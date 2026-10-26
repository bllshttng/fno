//! The synchronous sidecar client, ported from the deleted
//! `cli/src/fno/mcp/client.py` with the same error taxonomy (reason strings
//! byte-match the Python leg). The `--mcp-exec send` door body and the
//! daemon's `handle_push_to_channel` delivery leg both answer through this
//! client, so one socket protocol stays one implementation.

use serde_json::{json, Value};
use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Default per-call timeout. The sidecar is local; sub-100ms responses are
/// typical; 5s is the spec upper bound for probes.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(5);

/// The lazy-start subprocess bind window: the spawned sidecar must bind
/// within 5s of spawn.
pub const LAZY_START_BIND_TIMEOUT: Duration = Duration::from_secs(5);

/// Polling interval while waiting for the lazy-started sidecar to bind.
const LAZY_START_POLL_INTERVAL: Duration = Duration::from_millis(50);

/// The client error taxonomy, mirroring the Python leg's two exceptions.
/// `Unreachable` covers the socket path (machine-stable `reason`:
/// `server_not_listening`, `socket_missing`, `connect_error:{errno}`,
/// `write_error`, `read_timeout`, `read_error`, `empty_response`,
/// `parse_error:{kind}`, `lazy_start_spawn_failed`,
/// `lazy_start_timeout`); `Sidecar` covers an `ok: false` op reply.
#[derive(Debug)]
pub enum ClientError {
    Unreachable { reason: String },
    Sidecar { reason: String },
}

/// Resolved sidecar socket path: `$FNO_SIDECAR_SOCKET`, then
/// `$XDG_RUNTIME_DIR/fno/sidecar.sock`, then the state dir's
/// `sidecar.sock`. Re-exported so callers do not reach into the sidecar
/// module's internals, exactly as the Python client re-exported it.
pub fn socket_path() -> PathBuf {
    super::sidecar::socket_path()
}

/// Resolved sidecar state-flush path (`sidecar-state.json`).
pub fn state_path() -> PathBuf {
    super::sidecar::state_path()
}

/// One request/response over a fresh socket connection, closed after the
/// read. Returns the parsed reply (even when it carries `ok: false` - the
/// caller decides whether that is a [`ClientError::Sidecar`]).
fn rpc_once(sock_path: &Path, request: &Value, timeout: Duration) -> Result<Value, ClientError> {
    let mut stream = match UnixStream::connect(sock_path) {
        Ok(s) => s,
        Err(e) => return Err(unreachable_from_connect(e)),
    };
    stream
        .set_read_timeout(Some(timeout))
        .map_err(|e| ClientError::Unreachable {
            reason: format!("connect_error:{}", e.raw_os_error().unwrap_or(0)),
        })?;
    stream
        .set_write_timeout(Some(timeout))
        .map_err(|e| ClientError::Unreachable {
            reason: format!("connect_error:{}", e.raw_os_error().unwrap_or(0)),
        })?;
    let mut wire = serde_json::to_vec(&request).map_err(|e| ClientError::Unreachable {
        reason: format!("serialize_error:{e}"),
    })?;
    wire.push(b'\n');
    stream
        .write_all(&wire)
        .map_err(|_| ClientError::Unreachable {
            reason: "write_error".to_string(),
        })?;
    let mut buf = Vec::with_capacity(4096);
    let mut chunk = [0u8; 4096];
    loop {
        let n = stream
            .read(&mut chunk)
            .map_err(|_| ClientError::Unreachable {
                reason: "read_error".to_string(),
            })?;
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&chunk[..n]);
        if buf.contains(&b'\n') {
            break;
        }
    }
    if buf.is_empty() {
        return Err(ClientError::Unreachable {
            reason: "empty_response".to_string(),
        });
    }
    let line = &buf[..buf.iter().position(|b| *b == b'\n').unwrap_or(buf.len())];
    let parsed: Value = serde_json::from_slice(line).map_err(|e| ClientError::Unreachable {
        reason: format!("parse_error:{}", py_error_kind(&e)),
    })?;
    Ok(parsed)
}

/// Map a connect failure onto the Python leg's machine-stable reasons:
/// a missing socket file is `socket_missing`, a live socket with no
/// listener is `server_not_listening`, anything else is
/// `connect_error:{errno}`.
fn unreachable_from_connect(e: std::io::Error) -> ClientError {
    if e.kind() == std::io::ErrorKind::NotFound {
        return ClientError::Unreachable {
            reason: "socket_missing".to_string(),
        };
    }
    if e.raw_os_error() == Some(libc::ECONNREFUSED) {
        return ClientError::Unreachable {
            reason: "server_not_listening".to_string(),
        };
    }
    ClientError::Unreachable {
        reason: format!("connect_error:{}", e.raw_os_error().unwrap_or(0)),
    }
}

/// The Python exception class name for a JSON error, for the
/// `parse_error:{kind}` reason: `JSONDecodeError` for malformed JSON,
/// `UnicodeDecodeError` for invalid UTF-8 (serde's error kind matches).
fn py_error_kind(e: &serde_json::Error) -> &'static str {
    if e.is_syntax() || e.is_eof() {
        "JSONDecodeError"
    } else {
        "UnicodeDecodeError"
    }
}

/// Probe an existing sidecar with a short-timeout connect + ping. True only
/// when the peer answers within `timeout`; anything else (no socket file,
/// refused, garbled) is false, and the caller may unlink and re-bind.
pub fn is_sidecar_running(sock_path: &Path, timeout: Duration) -> bool {
    if !sock_path.exists() {
        return false;
    }
    let probe = rpc_once(sock_path, &json!({"op": "ping"}), timeout);
    matches!(probe, Ok(reply) if reply.get("ok").and_then(Value::as_bool) == Some(true))
}

/// Spawn the sidecar detached (new session, log appended to the XDG state
/// log), forwarding the requested socket path through `FNO_SIDECAR_SOCKET`
/// so a caller asking for a non-default path does not lazy-start a sidecar
/// at the default path and then time out polling the non-default path.
fn spawn_detached_sidecar(sock_path: &Path) -> Option<()> {
    use std::os::unix::process::CommandExt;
    let exe = worker_executable()?;
    let log_path = state_log_path();
    if let Some(parent) = log_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .ok()?;
    let log_err = log.try_clone().ok()?;
    let mut env: Vec<(String, String)> = std::env::vars_os()
        .filter_map(|(k, v)| {
            let k = k.to_string_lossy().into_owned();
            let v = v.to_string_lossy().into_owned();
            Some((k, v))
        })
        .collect();
    env.retain(|(k, _)| k != "FNO_SIDECAR_SOCKET");
    env.push((
        "FNO_SIDECAR_SOCKET".to_string(),
        sock_path.to_string_lossy().into_owned(),
    ));
    let mut command = std::process::Command::new(exe);
    command
        .arg("--mcp-exec")
        .arg("sidecar")
        .stdin(std::process::Stdio::null())
        .stdout(log)
        .stderr(log_err)
        .env_clear()
        .envs(env);
    // Detached: a new session so the lazy-started sidecar survives its
    // parent, mirroring the Python leg's `start_new_session=True`.
    // SAFETY: setsid(2) is async-signal-safe and runs between fork and
    // exec in the child.
    unsafe {
        command.pre_exec(|| {
            // SAFETY: setsid(2) is async-signal-safe.
            if libc::setsid() < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    command.spawn().ok().map(|_| ())
}

/// The sidecar's log path: `$XDG_STATE_HOME/fno/sidecar.log`, default
/// `~/.local/state/fno/sidecar.log`.
fn state_log_path() -> PathBuf {
    let state_home = std::env::var_os("XDG_STATE_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            std::env::var_os("HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("/"))
                .join(".local/state")
        });
    state_home.join("fno").join("sidecar.log")
}

/// The worker binary that carries the `--mcp-exec` lane: `$FNO_AGENTS_WORKER`,
/// the worker beside the current binary, then PATH. The lazy-start must name
/// the WORKER, never `current_exe`: when the client answers inside the
/// daemon, `current_exe` is `fno-agents-daemon`, which has no `--mcp-exec`
/// door, and the lazy-start would deterministically time out.
fn worker_executable() -> Option<PathBuf> {
    if let Ok(v) = std::env::var("FNO_AGENTS_WORKER") {
        let p = PathBuf::from(v);
        if p.is_file() {
            return Some(p);
        }
    }
    let exe = std::env::current_exe().ok()?;
    let sibling = exe.parent()?.join("fno-agents-worker");
    if sibling.is_file() {
        return Some(sibling);
    }
    find_on_path()
}

/// The first executable `fno-agents-worker` on PATH.
fn find_on_path() -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join("fno-agents-worker"))
        .find(|p| p.is_file())
}

/// Return the live sidecar socket path, lazy-starting if needed. Returns
/// `lazy_start_spawn_failed` when the spawn fails and `lazy_start_timeout`
/// when the child misses the bind window.
pub fn ensure_sidecar(sock_path: Option<&Path>) -> Result<PathBuf, ClientError> {
    let resolved = sock_path.map(Path::to_path_buf).unwrap_or_else(socket_path);
    if is_sidecar_running(&resolved, Duration::from_millis(300)) {
        return Ok(resolved);
    }
    spawn_detached_sidecar(&resolved).ok_or(ClientError::Unreachable {
        reason: "lazy_start_spawn_failed".to_string(),
    })?;
    let deadline = std::time::Instant::now() + LAZY_START_BIND_TIMEOUT;
    while std::time::Instant::now() < deadline {
        if is_sidecar_running(&resolved, Duration::from_millis(200)) {
            return Ok(resolved);
        }
        std::thread::sleep(LAZY_START_POLL_INTERVAL);
    }
    Err(ClientError::Unreachable {
        reason: "lazy_start_timeout".to_string(),
    })
}

/// Probe the sidecar with a `ping` (no lazy-start).
pub fn ping(sock_path: Option<&Path>, timeout: Duration) -> Result<Value, ClientError> {
    rpc_once(
        sock_path.unwrap_or(&socket_path()),
        &json!({"op": "ping"}),
        timeout,
    )
}

/// Full sidecar status: the shape the reachability probe slices.
pub fn status(sock_path: Option<&Path>, timeout: Duration) -> Result<Value, ClientError> {
    rpc_once(
        sock_path.unwrap_or(&socket_path()),
        &json!({"op": "status"}),
        timeout,
    )
}

/// Route `envelope` to the channel registered under `session_id`.
pub fn send_to_channel(
    session_id: &str,
    envelope: &Value,
    sock_path: Option<&Path>,
    timeout: Duration,
) -> Result<(), ClientError> {
    let sock_path = match sock_path {
        Some(p) => p.to_path_buf(),
        None => ensure_sidecar(None)?,
    };
    let resp = rpc_once(
        &sock_path,
        &json!({
            "op": "send_to_channel",
            "session_id": session_id,
            "envelope": envelope,
        }),
        timeout,
    )?;
    if resp.get("ok").and_then(Value::as_bool) != Some(true) {
        return Err(ClientError::Sidecar {
            reason: resp
                .get("reason")
                .and_then(Value::as_str)
                .unwrap_or("unknown")
                .to_string(),
        });
    }
    Ok(())
}

/// Drop a registration (best-effort; a missing row is not an error).
pub fn unregister(session_id: &str, sock_path: Option<&Path>, timeout: Duration) -> () {
    let sock_path = sock_path.map(Path::to_path_buf).unwrap_or_else(socket_path);
    let probe = rpc_once(
        &sock_path,
        &json!({"op": "unregister_channel", "session_id": session_id}),
        timeout,
    );
    let _ = probe;
}

/// The `--mcp-exec send` door body: stdin envelope bytes in, exit code out.
/// Exit 0 on delivery; 1 on a sidecar/delivery failure; 2 on a malformed
/// envelope, before any sidecar contact. The stderr lines carry the Python
/// leg's wording.
pub fn send_from_bytes(session_id: &str, bytes: &[u8]) -> i32 {
    send_at(session_id, bytes, None)
}

/// The door body over an explicit socket (the test seam; the door itself
/// reads `FNO_SIDECAR_SOCKET` through the default socket resolution).
pub fn send_at(session_id: &str, bytes: &[u8], sock_path: Option<&Path>) -> i32 {
    let parsed: Value = match serde_json::from_slice(bytes) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("invalid envelope JSON: {e}");
            return 2;
        }
    };
    if !parsed.is_object() {
        eprintln!("envelope must be a JSON object");
        return 2;
    }
    match send_to_channel(session_id, &parsed, sock_path, DEFAULT_TIMEOUT) {
        Ok(()) => 0,
        Err(ClientError::Sidecar { reason }) => {
            eprintln!("delivery failed: {reason}");
            1
        }
        Err(ClientError::Unreachable { reason }) => {
            eprintln!("sidecar unreachable: {reason}");
            1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::channel::build_channel_notification;
    use super::*;

    #[test]
    fn a_malformed_envelope_exits_2_before_any_sidecar_contact() {
        let dir = tempfile::tempdir().unwrap();
        let sock = dir.path().join("absent.sock");
        let code = send_at("s", b"not json", Some(&sock));
        assert_eq!(code, 2);
        assert!(!sock.exists(), "no sidecar may be lazy-started");
    }

    /// The end-to-end delivery contract the deleted
    /// `TestMcpSendCli::test_send_command_delivers_to_registered_channel`
    /// pinned: a stdin envelope crosses the sidecar to a registered channel,
    /// byte-equal including meta keys, exit 0.
    /// Two workers model the two processes production has: `send_at` probes
    /// the live sidecar with a blocking ping, and on one worker the spawned
    /// server it probes could never answer, so the probe read it as stale,
    /// unlinked it, and lazy-started an empty replacement.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_send_door_delivers_to_a_registered_channel() {
        let dir = tempfile::tempdir().unwrap();
        let sock = dir.path().join("s.sock");
        let state = std::sync::Arc::new(super::super::sidecar::SidecarState::new(1800));
        let server_sock = sock.clone();
        let server = tokio::spawn(async move {
            let _ = super::super::sidecar::bind_and_serve(state, &server_sock).await;
        });
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while !sock.exists() && std::time::Instant::now() < deadline {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        let mut conn_a = tokio::net::UnixStream::connect(&sock).await.unwrap();
        let register = json!({
            "op": "register_channel",
            "session_id": "sess-hit",
            "channel_name": "test-channel",
            "pid": std::process::id(),
        });
        {
            use tokio::io::AsyncWriteExt;
            let mut line = serde_json::to_vec(&register).unwrap();
            line.push(b'\n');
            conn_a.write_all(&line).await.unwrap();
        }
        let ack = read_frame(&mut conn_a).await;
        assert_eq!(ack, json!({"ok": true}));
        let envelope = build_channel_notification(
            "hi",
            Some(
                &serde_json::from_value::<serde_json::Map<String, Value>>(json!({"k": "v"}))
                    .unwrap(),
            ),
            None,
        )
        .unwrap();
        let code = send_at(
            "sess-hit",
            serde_json::to_vec(&envelope).unwrap().as_slice(),
            Some(&sock),
        );
        assert_eq!(code, 0);
        let frame = read_frame(&mut conn_a).await;
        assert_eq!(frame["op"], json!("deliver"));
        assert_eq!(frame["envelope"]["params"]["meta"], json!({"k": "v"}));
        server.abort();
    }

    async fn read_frame(stream: &mut tokio::net::UnixStream) -> Value {
        use tokio::io::AsyncReadExt;
        let mut buf = vec![0u8; 65536];
        let n = tokio::time::timeout(Duration::from_secs(2), stream.read(&mut buf))
            .await
            .unwrap()
            .unwrap();
        let line = &buf[..n];
        let end = line.iter().position(|b| *b == b'\n').unwrap_or(n);
        serde_json::from_slice(&line[..end]).unwrap()
    }
}
