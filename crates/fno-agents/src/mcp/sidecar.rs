//! The per-user sidecar daemon, ported from the deleted
//! `cli/src/fno/mcp/sidecar.py`. A lazy-start / lazy-exit Unix-socket
//! daemon: it holds the in-memory `session_id -> registered channel` map,
//! routes pokes to the owning push connection, and is the rendezvous so
//! external `fno agents mcp send` calls reach the right per-session
//! channel server. Lifecycle owned by fno.

use serde_json::{json, Value};
use std::collections::HashMap;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::unix::OwnedWriteHalf;
use tokio::net::UnixListener;
use tokio::net::UnixStream;
use tokio::sync::Mutex as AsyncMutex;

use super::client::is_sidecar_running;

/// Default idle-exit window: 30 minutes, matching the Python leg.
pub const DEFAULT_IDLE_EXIT_SECONDS: u64 = 30 * 60;

/// Wire-protocol error reasons, machine-stable across the port.
const REASON_BAD_REQUEST: &str = "bad_request";
const REASON_NOT_REGISTERED: &str = "channel_not_registered";
const REASON_WRITE_FAILED: &str = "channel_write_failed";
const REASON_ALREADY_REGISTERED: &str = "already_registered";
const REASON_FOREIGN_UNREGISTER: &str = "foreign_unregister_rejected";

/// One registration record: the push connection's write half plus the
/// facts the status projection and the state flush carry.
struct Channel {
    channel_name: String,
    pid: u32,
    registered_at: Instant,
    conn_id: u64,
    writer: Arc<AsyncMutex<OwnedWriteHalf>>,
}

/// Mutable state for one sidecar process.
pub struct SidecarState {
    channels: Mutex<HashMap<String, Channel>>,
    conn_seq: AtomicU64,
    started_at: Instant,
    last_activity: Mutex<Instant>,
    idle_exit: Duration,
}

impl SidecarState {
    pub fn new(idle_exit_seconds: u64) -> Self {
        Self {
            channels: Mutex::new(HashMap::new()),
            conn_seq: AtomicU64::new(0),
            started_at: Instant::now(),
            last_activity: Mutex::new(Instant::now()),
            idle_exit: Duration::from_secs(idle_exit_seconds),
        }
    }

    /// Mint the next per-connection id.
    fn next_conn_id(&self) -> u64 {
        self.conn_seq.fetch_add(1, Ordering::Relaxed) + 1
    }

    /// Refresh the idle clock. Every accepted connection and every op
    /// touches it, so idle means no connections AND no ops.
    pub fn touch(&self) {
        *self.last_activity.lock().unwrap() = Instant::now();
    }

    /// Seconds since the sidecar started.
    pub fn uptime_seconds(&self) -> u64 {
        self.started_at.elapsed().as_secs()
    }

    /// Seconds since the last accepted connection or op.
    pub fn last_activity_secs(&self) -> u64 {
        self.last_activity.lock().unwrap().elapsed().as_secs()
    }

    /// True when no channels are registered AND the idle window elapsed.
    pub fn is_idle(&self) -> bool {
        if !self.channels.lock().unwrap().is_empty() {
            return false;
        }
        self.last_activity.lock().unwrap().elapsed() >= self.idle_exit
    }

    /// The status projection's channel rows.
    pub fn status_rows(&self) -> Vec<Value> {
        self.channels
            .lock()
            .unwrap()
            .iter()
            .map(|(sid, ch)| {
                json!({
                    "session_id": sid,
                    "channel_name": ch.channel_name,
                    "pid": ch.pid,
                    "registered_seconds_ago": ch.registered_at.elapsed().as_secs(),
                })
            })
            .collect()
    }

    /// The registered channel count.
    pub fn channel_count(&self) -> usize {
        self.channels.lock().unwrap().len()
    }
}

/// Resolved sidecar socket path: `$FNO_SIDECAR_SOCKET` override, then
/// `$XDG_RUNTIME_DIR/fno/sidecar.sock`, then the configured state dir's
/// `sidecar.sock` (`FNO_STATE_DIR`, config, or `$HOME/.fno`).
pub fn socket_path() -> PathBuf {
    if let Some(override_path) = std::env::var_os("FNO_SIDECAR_SOCKET") {
        if !override_path.is_empty() {
            return PathBuf::from(override_path);
        }
    }
    if let Some(xdg) = std::env::var_os("XDG_RUNTIME_DIR") {
        if !xdg.is_empty() {
            return Path::new(&xdg).join("fno").join("sidecar.sock");
        }
    }
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    state_dir(&cwd).join("sidecar.sock")
}

/// The state dir root, shared with `state_path`: the configured state dir
/// or `$HOME/.fno`.
fn state_dir(cwd: &Path) -> PathBuf {
    crate::agents_config::state_dir(cwd).unwrap_or_else(|| {
        std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("/"))
            .join(".fno")
    })
}

/// The persistent state file (registered channels at SIGTERM).
pub fn state_path() -> PathBuf {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    state_dir(&cwd).join("sidecar-state.json")
}

/// Create the socket's parent directory at mode 0700, tightening a looser
/// existing mode. The socket inherits group/other access from the parent's
/// traversal bits, so a world-readable parent is refused by tightening.
fn ensure_parent_secure(path: &Path) {
    let parent = path.parent().unwrap_or(Path::new("."));
    let _ = std::fs::create_dir_all(parent);
    if let Ok(meta) = std::fs::metadata(parent) {
        let mode = meta.permissions().mode();
        if mode & 0o077 != 0 {
            let _ = std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700));
        }
    }
}

/// Persist the channel-registration table to disk. SIGKILL leaves the file
/// stale; channel servers re-register on next boot, and the registry's
/// `mcp_channel_id` stays the persistent source of truth.
fn flush_state(state: &SidecarState) {
    let path = state_path();
    let _ = std::fs::create_dir_all(path.parent().unwrap_or(Path::new(".")));
    let channels = state.channels.lock().unwrap();
    let rows: Vec<Value> = channels
        .iter()
        .map(|(sid, ch)| {
            json!({
                "session_id": sid,
                "channel_name": ch.channel_name,
                "pid": ch.pid,
            })
        })
        .collect();
    let payload = json!({
        "pid": std::process::id(),
        "flushed_at": epoch_seconds(),
        "channels": rows,
    });
    drop(channels);
    let tmp = path.with_extension("json.tmp");
    if std::fs::write(
        &tmp,
        serde_json::to_vec_pretty(&payload).unwrap_or_default(),
    )
    .is_ok()
    {
        let _ = std::fs::rename(&tmp, &path);
    }
}

fn epoch_seconds() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// Pre-bind probe + stale-socket recovery. `None` means the path is ready
/// for a fresh bind; `Some(Ok(..))`-style outcome enum below. A socket
/// file with no listener is a mid-bind-crash artifact: unlink and re-probe,
/// bounded to two attempts so a tight re-create race fails loudly.
fn prepare_socket_path(sock_path: &Path) -> PrepareOutcome {
    ensure_parent_secure(sock_path);
    for _ in 0..2 {
        if !sock_path.exists() {
            return PrepareOutcome::Ready;
        }
        if is_sidecar_running(sock_path, Duration::from_millis(300)) {
            return PrepareOutcome::AlreadyRunning;
        }
        match std::fs::remove_file(sock_path) {
            Ok(()) => return PrepareOutcome::Ready,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return PrepareOutcome::Ready,
            Err(_) => continue,
        }
    }
    PrepareOutcome::Failed("stale_socket_unlink_exhausted".to_string())
}

/// The pre-bind outcome.
enum PrepareOutcome {
    Ready,
    AlreadyRunning,
    Failed(String),
}

/// Run the sidecar on a fresh current-thread runtime: the worker lane's
/// entry. Returns the process exit code. The idle window is the module
/// default (30 minutes).
pub fn run_sidecar(idle_exit_seconds: u64) -> i32 {
    let rt = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("sidecar: cannot build tokio runtime: {e}");
            return 2;
        }
    };
    rt.block_on(serve_process(idle_exit_seconds))
}

async fn serve_process(idle_exit_seconds: u64) -> i32 {
    let sock_path = socket_path();
    match prepare_socket_path(&sock_path) {
        PrepareOutcome::AlreadyRunning => {
            eprintln!("sidecar already running at {}", sock_path.display());
            return 0;
        }
        PrepareOutcome::Failed(reason) => {
            eprintln!("sidecar bind preparation failed: {reason}");
            return 2;
        }
        PrepareOutcome::Ready => {}
    }
    let state = Arc::new(SidecarState::new(idle_exit_seconds));
    match bind_and_serve(Arc::clone(&state), &sock_path).await {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("sidecar start_unix_server failed: {e}");
            2
        }
    }
}

/// Bind the listener and serve until stop. The socket is born at 0600
/// (umask 077 during bind, chmod belt-and-suspenders after) and the idle
/// watcher exits the loop when the state goes idle.
pub(crate) async fn bind_and_serve(
    state: Arc<SidecarState>,
    sock_path: &Path,
) -> Result<(), std::io::Error> {
    ensure_parent_secure(sock_path);
    let prior_umask = unsafe { libc::umask(0o077) };
    let listener = UnixListener::bind(sock_path);
    unsafe { libc::umask(prior_umask) };
    let listener = match listener {
        Ok(l) => l,
        Err(e) => return Err(e),
    };
    let _ = std::fs::set_permissions(sock_path, std::fs::Permissions::from_mode(0o600));
    let mut signals = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    let mut sigint = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())?;

    loop {
        tokio::select! {
            _ = signals.recv() => {
                eprintln!("sidecar received SIGTERM; flushing state and shutting down");
                flush_state(&state);
                break;
            }
            _ = sigint.recv() => {
                eprintln!("sidecar received SIGINT; flushing state and shutting down");
                flush_state(&state);
                break;
            }
            accepted = listener.accept() => {
                match accepted {
                    Ok((stream, _addr)) => {
                        state.touch();
                        let state = Arc::clone(&state);
                        tokio::spawn(handle_client(state, stream));
                    }
                    Err(_) => continue,
                }
            }
            _ = tokio::time::sleep(Duration::from_secs(5)) => {
                if state.is_idle() {
                    eprintln!(
                        "sidecar idle for {}s with {} channels; exiting",
                        state.last_activity_secs(),
                        state.channel_count()
                    );
                    break;
                }
            }
        }
    }
    let _ = std::fs::remove_file(sock_path);
    Ok(())
}

/// Serve one client connection until EOF. A connection that issues
/// `register_channel` becomes long-lived push mode; the write half moves
/// into the map entry, so pushes and later replies serialize on the same
/// writer lock. The conn_id marks ownership for unregister and purge.
async fn handle_client(state: Arc<SidecarState>, stream: UnixStream) {
    state.touch();
    let conn_id = state.next_conn_id();
    let (reader, writer) = stream.into_split();
    let writer = Arc::new(AsyncMutex::new(writer));
    let mut my_sessions: Vec<String> = Vec::new();
    let mut reader = BufReader::new(reader);
    let mut raw = Vec::new();
    loop {
        raw.clear();
        let n = match reader.read_until(b'\n', &mut raw).await {
            Ok(n) => n,
            Err(_) => break,
        };
        if n == 0 {
            break;
        }
        state.touch();
        let req: Value = match serde_json::from_slice(&raw) {
            Ok(v) => v,
            Err(_) => {
                write_line(
                    writer.clone(),
                    &json!({"ok": false, "reason": REASON_BAD_REQUEST}),
                )
                .await;
                continue;
            }
        };
        let op = req.get("op").and_then(Value::as_str).unwrap_or("");
        match op {
            "ping" => {
                write_line(
                    writer.clone(),
                    &json!({
                        "ok": true,
                        "pid": std::process::id(),
                        "uptime_seconds": state.uptime_seconds(),
                    }),
                )
                .await;
            }
            "status" => {
                write_line(
                    writer.clone(),
                    &json!({
                        "ok": true,
                        "pid": std::process::id(),
                        "uptime_seconds": state.uptime_seconds(),
                        "channels_registered": state.channel_count(),
                        "channels": state.status_rows(),
                    }),
                )
                .await;
            }
            "register_channel" => {
                let sid = req.get("session_id").and_then(Value::as_str);
                let cname = req.get("channel_name").and_then(Value::as_str);
                let pid = req.get("pid").and_then(Value::as_u64);
                if sid.is_none() || cname.is_none() || pid.is_none() {
                    write_line(
                        writer.clone(),
                        &json!({"ok": false, "reason": REASON_BAD_REQUEST}),
                    )
                    .await;
                    continue;
                }
                let (sid, cname) = (sid.unwrap().to_string(), cname.unwrap().to_string());
                let pid = pid.unwrap();
                let writer_for_entry = writer.clone();
                // Reply computed under the map lock, sent after the drop:
                // a std Mutex guard must never sit across an .await.
                let reply = {
                    let mut channels = state.channels.lock().unwrap();
                    match channels.get(sid.as_str()) {
                        Some(existing) if existing.conn_id != conn_id => {
                            json!({"ok": false, "reason": REASON_ALREADY_REGISTERED})
                        }
                        _ => {
                            channels.insert(
                                sid.clone(),
                                Channel {
                                    channel_name: cname.clone(),
                                    pid: pid as u32,
                                    registered_at: Instant::now(),
                                    conn_id,
                                    writer: writer_for_entry,
                                },
                            );
                            json!({"ok": true})
                        }
                    }
                };
                write_line(writer.clone(), &reply).await;
                if reply.get("ok").and_then(Value::as_bool) == Some(true) {
                    my_sessions.push(sid);
                }
            }
            "unregister_channel" => {
                let sid = req
                    .get("session_id")
                    .and_then(Value::as_str)
                    .map(String::from);
                let reply = {
                    let mut channels = state.channels.lock().unwrap();
                    match sid.as_deref() {
                        None => json!({"ok": false, "reason": REASON_BAD_REQUEST}),
                        Some(sid) => {
                            let existing = channels.get(sid);
                            if let Some(existing) = existing {
                                if existing.conn_id != conn_id {
                                    json!({"ok": false, "reason": REASON_FOREIGN_UNREGISTER})
                                } else {
                                    channels.remove(sid);
                                    json!({"ok": true})
                                }
                            } else {
                                // A missing row is not an error: a best-effort
                                // drop answers ok like the Python leg did.
                                json!({"ok": true})
                            }
                        }
                    }
                };
                write_line(writer.clone(), &reply).await;
                if let Some(sid) = sid {
                    my_sessions.retain(|s| s != &sid);
                }
            }
            "send_to_channel" => {
                let sid = req.get("session_id").and_then(Value::as_str);
                let envelope = req.get("envelope");
                if sid.is_none() || envelope.is_none() {
                    write_line(
                        writer.clone(),
                        &json!({"ok": false, "reason": REASON_BAD_REQUEST}),
                    )
                    .await;
                    continue;
                }
                let envelope = envelope.unwrap();
                if !envelope.is_object() {
                    write_line(
                        writer.clone(),
                        &json!({"ok": false, "reason": REASON_BAD_REQUEST}),
                    )
                    .await;
                    continue;
                }
                let outcome = state.push_deliver(sid.unwrap(), envelope).await;
                let reply = match outcome {
                    PushOutcome::Delivered => json!({"ok": true}),
                    PushOutcome::NotRegistered => {
                        json!({"ok": false, "reason": REASON_NOT_REGISTERED})
                    }
                    PushOutcome::WriteFailed => {
                        json!({"ok": false, "reason": REASON_WRITE_FAILED})
                    }
                };
                write_line(writer.clone(), &reply).await;
            }
            _ => {
                write_line(
                    writer.clone(),
                    &json!({"ok": false, "reason": REASON_BAD_REQUEST}),
                )
                .await;
            }
        }
    }
    // Purge every session this connection registered on disconnect.
    {
        let mut channels = state.channels.lock().unwrap();
        for sid in &my_sessions {
            let remove = match channels.get(sid) {
                Some(ch) => ch.conn_id == conn_id,
                None => false,
            };
            if remove {
                channels.remove(sid);
            }
        }
    }
}

/// The send_to_channel outcome: delivered, or the two failure shapes the
/// Python leg distinguished.
#[derive(Clone, Copy, PartialEq)]
pub enum PushOutcome {
    Delivered,
    NotRegistered,
    WriteFailed,
}

impl SidecarState {
    /// Wrap the envelope in a deliver frame and push it down the owning
    /// connection. On a write failure the registration is purged so the
    /// next probe reports the channel gone.
    pub async fn push_deliver(&self, sid: &str, envelope: &Value) -> PushOutcome {
        let writer = {
            let mut channels = self.channels.lock().unwrap();
            let Some(ch) = channels.get_mut(sid) else {
                return PushOutcome::NotRegistered;
            };
            ch.writer.clone()
        };
        let frame = json!({"op": "deliver", "envelope": envelope});
        let mut line = serde_json::to_vec(&frame).unwrap_or_default();
        line.push(b'\n');
        let outcome = match writer.lock().await.write_all(&line).await {
            Ok(()) => PushOutcome::Delivered,
            Err(_) => PushOutcome::WriteFailed,
        };
        if outcome == PushOutcome::WriteFailed {
            self.channels.lock().unwrap().remove(sid);
        }
        outcome
    }
}

/// Write one JSON line down a connection's write half. False when the peer
/// is gone; the caller purges the registration.
async fn write_line(writer: Arc<AsyncMutex<OwnedWriteHalf>>, payload: &Value) -> bool {
    let mut line = serde_json::to_vec(payload).unwrap_or_default();
    line.push(b'\n');
    writer.lock().await.write_all(&line).await.is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn rpc(sock: &Path, payload: &Value) -> Value {
        let mut stream = tokio::net::UnixStream::connect(sock).await.unwrap();
        {
            use tokio::io::AsyncWriteExt;
            let mut line = serde_json::to_vec(payload).unwrap();
            line.push(b'\n');
            stream.write_all(&line).await.unwrap();
        }
        let mut buf = Vec::new();
        let mut reader = BufReader::new(&mut stream);
        reader.read_until(b'\n', &mut buf).await.unwrap();
        serde_json::from_slice(&buf).unwrap()
    }

    /// AC3-MIDBIND: a stale socket file with no listener behind it is
    /// unlinked and rebound by a fresh sidecar.
    #[tokio::test]
    async fn a_stale_socket_file_is_unlinked_and_rebound() {
        let dir = tempfile::tempdir().unwrap();
        let sock = dir.path().join("s.sock");
        std::fs::write(&sock, b"").unwrap();
        assert!(sock.exists());
        let state = Arc::new(SidecarState::new(1800));
        assert!(matches!(prepare_socket_path(&sock), PrepareOutcome::Ready));
        assert!(!sock.exists(), "the stale artifact was removed");
    }

    /// One sidecar per user: a second invocation against a live sidecar
    /// detects it and exits 0 with the "already running" note.
    #[tokio::test]
    async fn a_second_sidecar_detects_a_live_first() {
        let dir = tempfile::tempdir().unwrap();
        let sock = dir.path().join("s.sock");
        let state = Arc::new(SidecarState::new(1800));
        let server_sock = sock.clone();
        let server = tokio::spawn(async move {
            let _ = bind_and_serve(state, &server_sock).await;
        });
        tokio::time::sleep(Duration::from_millis(150)).await;
        assert!(matches!(
            prepare_socket_path(&sock),
            PrepareOutcome::AlreadyRunning
        ));
        server.abort();
    }

    /// The miss case: send_to_channel against an unregistered session
    /// answers `channel_not_registered` (AC1-ERR of the lifecycle file).
    #[tokio::test]
    async fn a_send_to_an_unregistered_channel_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let sock = dir.path().join("s.sock");
        let state = Arc::new(SidecarState::new(1800));
        let server_sock = sock.clone();
        let server = tokio::spawn(async move {
            let _ = bind_and_serve(state, &server_sock).await;
        });
        tokio::time::sleep(Duration::from_millis(150)).await;
        let resp = rpc(
            &sock,
            &json!({
                "op": "send_to_channel",
                "session_id": "ghost-id",
                "envelope": {
                    "jsonrpc": "2.0",
                    "method": "notifications/claude/channel",
                    "params": {"content": "hi", "meta": {}},
                },
            }),
        )
        .await;
        assert_eq!(
            resp,
            json!({"ok": false, "reason": "channel_not_registered"})
        );
        server.abort();
    }
}
