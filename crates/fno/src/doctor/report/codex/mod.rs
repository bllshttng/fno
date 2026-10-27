//! The codex collectors: app-server probe, hooks inspection and
//! context-window arithmetic. The plugin freshness verdict reads from
//! `fno-agents plugin-install codex --status` (one owner).

mod context_window;
mod hooks;

pub(crate) use context_window::*;
pub(crate) use hooks::*;

use serde_json::{json, Value};

use super::codex_home;

/// Whether a codex app-server daemon is listening on its control socket
/// (doctor.py:3190-3210). The socket file survives its daemon, so liveness is
/// a bounded connect probe, never the file's existence.
pub(crate) fn codex_app_server_report() -> Value {
    let socket_path = codex_home().join("app-server-control/app-server-control.sock");
    // Bounded connect probe on a worker thread: recv_timeout supplies the one
    // second the socket-level connect timeout used to, and a hung connect
    // strands at most this probe's thread until the process exits.
    let (tx, rx) = std::sync::mpsc::channel();
    let probe_path = socket_path.clone();
    std::thread::spawn(move || {
        let _ = tx.send(std::os::unix::net::UnixStream::connect(probe_path).is_ok());
    });
    let present = rx
        .recv_timeout(std::time::Duration::from_secs(1))
        .unwrap_or(false);
    json!({
        "present": present,
        "socket_path": socket_path.to_string_lossy(),
    })
}

/// The surface-report view of the hooks inspection (doctor.py:3812-3825):
/// dual layers plus any foreign hooks in `~/.codex/hooks.json`.
pub(crate) fn codex_hooks_dual_and_foreign() -> (bool, Value) {
    let codex = codex_hooks_report();
    let duplicate = codex.get("duplicate_layers") == Some(&Value::Bool(true));
    let footnote = codex
        .get("footnote_json_hooks")
        .and_then(Value::as_array)
        .map(|a| !a.is_empty())
        .unwrap_or(false);
    let foreign = codex
        .get("foreign_json_hooks")
        .cloned()
        .unwrap_or_else(|| Value::Array(vec![]));
    (duplicate && footnote, foreign)
}
