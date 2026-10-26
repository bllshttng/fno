//! The codex collectors: app-server probe, hooks inspection, context-window
//! arithmetic, and the plugin freshness verdict.

mod context_window;
mod freshness;
mod hooks;

pub(crate) use context_window::*;
pub(crate) use freshness::*;
pub(crate) use hooks::*;

use serde_json::{json, Value};

use super::codex_home;

/// Whether a codex app-server daemon is listening on its control socket
/// (doctor.py:3190-3210). The socket file survives its daemon, so liveness is
/// a bounded connect probe, never the file's existence.
pub(crate) fn codex_app_server_report() -> Value {
    let socket_path = codex_home().join("app-server-control/app-server-control.sock");
    let present = std::os::unix::net::SocketAddr::from_pathname(&socket_path)
        .ok()
        .and_then(|addr| {
            std::os::unix::net::UnixStream::connect_timeout(&addr, std::time::Duration::from_secs(1))
                .ok()
        })
        .is_some();
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
