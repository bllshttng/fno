//! `fno agents watch`: observe a held stream-json thread's turns in real time.
//! A read leaf with no action of its own: the router rewrites it into
//! `status --watch`, because the binary's action list is shrink-only.
//!
//! The command polls the worker's own `stream.read_frames` over its socket and
//! renders the turn lifecycle (delivered -> streaming -> reply -> complete), so
//! a stalled turn is visible rather than silent. It writes nothing.

use crate::client_commands::{py_repr_str, read_registry_entries, resolve_entry_with_heal};
use crate::paths::AgentsHome;
use serde_json::{json, Value};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

const USAGE: &str = "usage: fno agents watch NAME [--interval SECONDS]";

const READ_TIMEOUT: Duration = Duration::from_secs(5);

static INTERRUPTED: AtomicBool = AtomicBool::new(false);

// SAFETY: the handler touches only an AtomicBool. No allocations, no locks.
extern "C" fn on_sigint(_: libc::c_int) {
    INTERRUPTED.store(true, Ordering::SeqCst);
}

/// Python truthiness for a JSON value: null, false, 0, "", [] and {} are false.
fn truthy(value: Option<&Value>) -> bool {
    match value {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().is_some_and(|f| f != 0.0),
        Some(Value::String(s)) => !s.is_empty(),
        Some(Value::Array(a)) => !a.is_empty(),
        Some(Value::Object(o)) => !o.is_empty(),
    }
}

/// Python `str()` of a decoded JSON scalar, as the f-strings render it.
fn py_str(value: &Value) -> String {
    match value {
        Value::Null => "None".to_string(),
        Value::Bool(true) => "True".to_string(),
        Value::Bool(false) => "False".to_string(),
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// Map one stream frame to a display line (`None` = nothing to show).
pub fn render_frame(frame: &Value) -> Option<String> {
    let field = |key: &str| frame.get(key).map(py_str).unwrap_or_default();
    match frame.get("kind").and_then(Value::as_str)? {
        "system" => Some(format!("  · session ready ({})", field("subtype"))),
        "user_echo" => Some("  · delivered (turn received)".to_string()),
        "stream_event" => truthy(frame.get("delta")).then(|| format!("  · {}", field("delta"))),
        "assistant" => Some(format!("  -> {}", field("text"))),
        "result" if truthy(frame.get("is_error")) => Some("  x turn errored".to_string()),
        "result" => Some("  v turn complete".to_string()),
        "malformed" => Some("  · (skipped malformed frame)".to_string()),
        _ => None,
    }
}

/// One length-prefixed JSON RPC to a worker socket. `None` on any transport
/// failure or error reply, like the Python `rpc_roundtrip` it replaces.
fn worker_rpc(sock: &Path, method: &str, params: Value) -> Option<Value> {
    use std::io::{Read, Write};
    use std::os::unix::net::UnixStream;
    // std has no connect timeout for a unix socket; a listener that never
    // accepts still fails the bounded write or read below.
    let mut stream = UnixStream::connect(sock).ok()?;
    stream.set_read_timeout(Some(READ_TIMEOUT)).ok()?;
    stream.set_write_timeout(Some(READ_TIMEOUT)).ok()?;
    let body = serde_json::to_vec(&json!({ "id": 1, "method": method, "params": params })).ok()?;
    stream.write_all(&(body.len() as u32).to_le_bytes()).ok()?;
    stream.write_all(&body).ok()?;
    let mut len = [0u8; 4];
    stream.read_exact(&mut len).ok()?;
    let len = u32::from_le_bytes(len);
    if len > crate::protocol::MAX_FRAME_BYTES {
        return None;
    }
    let mut reply = vec![0u8; len as usize];
    stream.read_exact(&mut reply).ok()?;
    let reply: Value = serde_json::from_slice(&reply).ok()?;
    if reply.get("error").is_some() {
        return None;
    }
    reply.get("result").filter(|r| !r.is_null()).cloned()
}

/// Poll the frame log and render turns until the thread exits.
///
/// `read_frames(cursor)` is injected so the loop runs without a socket.
/// Returns 0 on a clean exit (child gone or Ctrl-C), 1 when the worker is
/// unreachable.
pub fn watch_loop(
    mut read_frames: impl FnMut(u64) -> Option<Value>,
    mut sleep: impl FnMut(),
    interrupted: impl Fn() -> bool,
) -> i32 {
    let mut cursor = 0u64;
    loop {
        if interrupted() {
            eprintln!("\n  -- watch stopped");
            return 0;
        }
        let Some(res) = read_frames(cursor) else {
            eprintln!("fno agents watch: thread not live (worker unreachable)");
            return 1;
        };
        cursor = res.get("next").and_then(Value::as_u64).unwrap_or(cursor);
        for frame in res
            .get("frames")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            if let Some(line) = render_frame(frame) {
                println!("{line}");
            }
        }
        let alive = res.get("child_alive");
        if alive.is_some() && !truthy(alive) {
            println!("  -- thread exited");
            return 0;
        }
        sleep();
    }
}

pub fn run_watch(args: &[String]) -> i32 {
    let mut name: Option<String> = None;
    let mut interval = 0.25_f64;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let (flag, inline) = match a.split_once('=') {
            Some((f, v)) if f.starts_with("--") => (f, Some(v.to_string())),
            _ => (a.as_str(), None),
        };
        match flag {
            "-h" | "--help" => {
                println!("{USAGE}");
                return 0;
            }
            "--interval" | "-i" => {
                let parsed = inline
                    .or_else(|| it.next().cloned())
                    .and_then(|v| v.parse::<f64>().ok())
                    .filter(|v| v.is_finite() && *v >= 0.0);
                match parsed {
                    Some(v) => interval = v,
                    None => {
                        eprintln!("watch: {flag} needs a number of seconds, zero or more\n{USAGE}");
                        return 2;
                    }
                }
            }
            other if other.starts_with('-') && other.len() > 1 => {
                eprintln!("watch: no such option: {other}\n{USAGE}");
                return 2;
            }
            _ if name.is_none() => name = Some(a.clone()),
            _ => {
                eprintln!("watch: unexpected extra argument {a:?}\n{USAGE}");
                return 2;
            }
        }
    }
    let Some(name) = name else {
        eprintln!("watch: missing argument NAME\n{USAGE}");
        return 2;
    };

    let home = AgentsHome::from_env();
    let registry = home.registry_json();
    let entries = match read_registry_entries(&registry) {
        Ok(e) => e,
        Err(exc) => {
            eprintln!(
                "fno agents watch: registry unreadable ({exc}); cannot resolve {}",
                py_repr_str(&name)
            );
            return 2;
        }
    };
    let entry = match resolve_entry_with_heal(&entries, &name, &registry) {
        Ok(e) => e,
        Err(err) => {
            eprintln!("fno agents watch: {}", err.message());
            return 2;
        }
    };
    let Some(short_id) = entry
        .get("short_id")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
    else {
        let row_name = entry.get("name").and_then(Value::as_str).unwrap_or("");
        eprintln!(
            "fno agents watch: agent {} has no worker short id on file; nothing to watch",
            py_repr_str(row_name)
        );
        return 2;
    };
    let sock = home.worker_sock(short_id);

    // SAFETY: on_sigint is async-signal-safe (one atomic store).
    unsafe {
        libc::signal(libc::SIGINT, on_sigint as *const () as libc::sighandler_t);
    }
    let pause = Duration::from_secs_f64(interval);
    watch_loop(
        |cursor| worker_rpc(&sock, "stream.read_frames", json!({ "cursor": cursor })),
        || {
            // Sleep in short slices so a Ctrl-C ends the watch promptly.
            let slice = Duration::from_millis(50);
            let mut left = pause;
            while !left.is_zero() && !INTERRUPTED.load(Ordering::SeqCst) {
                let step = left.min(slice);
                std::thread::sleep(step);
                left -= step;
            }
        },
        || INTERRUPTED.load(Ordering::SeqCst),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_frame_maps_each_kind() {
        let r = |v: Value| render_frame(&v);
        assert_eq!(
            r(json!({"kind": "system"})).unwrap(),
            "  · session ready ()"
        );
        assert_eq!(
            r(json!({"kind": "system", "subtype": null})).unwrap(),
            "  · session ready (None)"
        );
        assert_eq!(r(json!({"kind": "stream_event", "delta": 0})), None);
        assert_eq!(r(json!({"kind": "assistant"})).unwrap(), "  -> ");
        assert_eq!(
            r(json!({"kind": "result", "is_error": 1})).unwrap(),
            "  x turn errored"
        );
        assert_eq!(r(json!({"kind": 7})), None);
        assert_eq!(r(json!({})), None);
    }

    #[test]
    fn watch_loop_keeps_cursor_and_stops_on_interrupt() {
        let mut seen = Vec::new();
        let mut polls = 0;
        let rc = watch_loop(
            |c| {
                seen.push(c);
                Some(json!({"frames": [], "next": 4}))
            },
            || polls += 1,
            || polls >= 2,
        );
        assert_eq!(rc, 0);
        assert_eq!(seen, vec![0, 4]);
        assert_eq!(
            watch_loop(|_| Some(json!({"child_alive": null})), || {}, || false),
            0
        );
    }
}
