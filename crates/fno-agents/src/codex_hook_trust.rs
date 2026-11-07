//! Codex plugin-hook trust: read the truth from codex, write it back through codex.
//!
//! codex trusts each plugin hook by a declaration hash plus a bundle
//! fingerprint (`trusted_hash` + `trusted_content_hash` in
//! `config.toml [hooks.state]`). A plugin update invalidates both, and codex
//! then refuses every fno hook ("plugin contents changed; hook approval
//! required") until the rows are refreshed. fno-launched CLI lanes already
//! bypass the check; daemon-hosted thread workers do not, so after a plugin
//! update they run with no fno hooks: no guards, no stop hook, no state.
//!
//! The fix never derives a hash itself. `hooks/list` returns each hook's
//! `current_hash` ("<declaration>/<content>", split server-side on write),
//! and `config/batchWrite` - the same RPC the codex TUI uses to trust a
//! hook - persists it with `reload_user_config: true`, so the daemon stays
//! the one writer of its own config. Only rows whose `plugin_id` starts
//! `fno@` are written; a user who disabled a fno hook keeps it disabled.

use futures_util::SinkExt;
use serde::Serialize;
use serde_json::{json, Value};
use std::path::Path;
use tokio_tungstenite::tungstenite::Message;

/// Plugin rows this module may rewrite.
pub const FNO_PLUGIN_PREFIX: &str = "fno@";

/// Per-round-trip wall clock bound. The listing walk fingerprints the whole
/// plugin bundle (measured 2.3-2.6s warm per root), so this is generous on
/// purpose; a wedged daemon still cannot park the caller forever.
const REQUEST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);

/// One trust verdict batch over every fno plugin hook codex can see.
#[derive(Debug, Clone, Serialize)]
pub struct HookTrustReport {
    /// `ok` (every enabled fno hook trusted), `untrusted` (some are not),
    /// `unsupported` (codex answered but has no hooks/list), `unreachable`
    /// (no daemon), `error` (protocol or write failure).
    pub status: &'static str,
    /// Enabled fno hooks codex reported.
    pub checked: usize,
    pub trusted: usize,
    /// Enabled fno hooks codex would refuse (untrusted or modified).
    pub not_trusted: usize,
    pub modified: usize,
    pub untrusted: usize,
    /// fno rows the user disabled; never written.
    pub disabled: usize,
    /// Why the batch did not run, when it did not.
    pub reason: Option<String>,
}

impl HookTrustReport {
    /// A batch that never ran: the daemon was absent, refused the method,
    /// or the protocol failed. Counts stay zero; the reason names it.
    fn skipped(status: &'static str, reason: String) -> Self {
        Self {
            status,
            checked: 0,
            trusted: 0,
            not_trusted: 0,
            modified: 0,
            untrusted: 0,
            disabled: 0,
            reason: Some(reason),
        }
    }

    /// The batch started but the protocol failed: `error`, reason named.
    fn skipped_error(reason: String) -> Self {
        Self::skipped("error", reason)
    }
}

impl HookTrustReport {
    /// Count the verdict over codex's rows. `checked` covers enabled hooks
    /// only; a user-disabled fno hook is counted in `disabled` and written
    /// never.
    fn counts(hooks: &[FnoHook]) -> Self {
        let mut report = Self {
            status: "ok",
            checked: 0,
            trusted: 0,
            not_trusted: 0,
            modified: 0,
            untrusted: 0,
            disabled: 0,
            reason: None,
        };
        for hook in hooks {
            match (hook.enabled, hook.trust_status.as_str()) {
                (false, _) => report.disabled += 1,
                (true, "trusted") => report.trusted += 1,
                (true, "modified") => {
                    report.modified += 1;
                    report.not_trusted += 1;
                }
                (true, _) => {
                    report.untrusted += 1;
                    report.not_trusted += 1;
                }
            }
        }
        report.checked = report.trusted + report.not_trusted;
        if report.not_trusted > 0 {
            report.status = "untrusted";
        }
        report
    }
}

/// The row fno needs from codex's `hooks/list`.
#[derive(Debug, Clone)]
struct FnoHook {
    key: String,
    current_hash: String,
    trust_status: String,
    enabled: bool,
}

/// Ask codex which fno plugin hooks it refuses. Read-only.
pub fn check(home: &Path) -> HookTrustReport {
    run(home, /*write*/ false)
}

/// Refresh every untrusted enabled fno plugin hook, then re-read the verdict.
pub fn trust(home: &Path) -> HookTrustReport {
    run(home, /*write*/ true)
}

fn run(home: &Path, write: bool) -> HookTrustReport {
    let socket = crate::codex_inject::codex_app_server_socket_path();
    if let Err(error) = crate::codex_inject::ensure_codex_daemon() {
        return HookTrustReport::skipped("unreachable", error);
    }
    let socket_for_thread = socket.clone();
    let home = home.to_path_buf();
    let outcome = std::thread::spawn(move || {
        let runtime = match tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        {
            Ok(runtime) => runtime,
            Err(error) => return Err(format!("trust runtime: {error}")),
        };
        runtime.block_on(trust_flow(&socket_for_thread, &home, write))
    })
    .join()
    .unwrap_or_else(|_| Err("trust thread panicked".to_string()));
    outcome.unwrap_or_else(HookTrustReport::skipped_error)
}

async fn trust_flow(socket: &Path, home: &Path, write: bool) -> Result<HookTrustReport, String> {
    let (mut sink, mut stream) = crate::codex_inject::connect_app_server(socket)
        .await
        .map_err(|e| format!("daemon connect: {e}"))?;
    let hooks = request_hooks_list(&mut sink, &mut stream, home).await?;
    let fno = fno_rows(&hooks);
    let report = HookTrustReport::counts(&fno);
    if !write || report.not_trusted == 0 {
        return Ok(report);
    }
    write_hook_trust(&mut sink, &mut stream, &fno).await?;
    let hooks = request_hooks_list(&mut sink, &mut stream, home).await?;
    let fno = fno_rows(&hooks);
    let mut report = HookTrustReport::counts(&fno);
    if report.not_trusted > 0 {
        report.reason = Some("wrote trust rows; some hooks still untrusted".to_string());
    }
    Ok(report)
}

/// One `hooks/list` round trip. Failures name the failure, never the guess.
async fn request_hooks_list(
    sink: &mut crate::codex_inject::AppServerSink,
    stream: &mut crate::codex_inject::AppServerStream,
    home: &Path,
) -> Result<Vec<Value>, String> {
    sink.send(Message::Text(
        json!({
            "id": "fno-hook-trust-list",
            "method": "hooks/list",
            "params": {"cwds": [home]}
        })
        .to_string()
        .into(),
    ))
    .await
    .map_err(|error| format!("hooks/list send: {error}"))?;
    let raw = tokio::time::timeout(
        REQUEST_TIMEOUT,
        crate::codex_inject::read_until_id(stream, &json!("fno-hook-trust-list")),
    )
    .await
    .map_err(|_| "hooks/list timed out".to_string())?
    .map_err(str::to_string)?;
    let frame: Value = serde_json::from_str(&raw).map_err(|e| format!("hooks/list frame: {e}"))?;
    if let Some(error) = frame.get("error") {
        return Err(format!("hooks/list refused: {error}"));
    }
    let data = frame
        .pointer("/result/data")
        .and_then(Value::as_array)
        .ok_or_else(|| "hooks/list answered without data".to_string())?;
    let hooks: Vec<Value> = data
        .iter()
        .flat_map(|entry| {
            entry
                .get("hooks")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default()
        })
        .collect();
    Ok(hooks)
}

/// The rows whose plugin owns them: `fno@`-prefixed plugin ids only.
fn fno_rows(hooks: &[Value]) -> Vec<FnoHook> {
    hooks
        .iter()
        .filter_map(|hook| {
            let plugin_id = hook.get("pluginId")?.as_str()?;
            if !plugin_id.starts_with(FNO_PLUGIN_PREFIX) {
                return None;
            }
            Some(FnoHook {
                key: hook.get("key")?.as_str()?.to_string(),
                current_hash: hook.get("currentHash")?.as_str()?.to_string(),
                trust_status: hook
                    .get("trustStatus")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown")
                    .to_string(),
                enabled: hook.get("enabled").and_then(Value::as_bool).unwrap_or(true),
            })
        })
        .collect()
}

/// The TUI-shaped trust write: one `config/batchWrite` upsert of
/// `hooks.state.{key}.trusted_hash = current_hash`, with
/// `reload_user_config: true` so the daemon re-reads its own config. codex
/// splits the combined "<declaration>/<content>" server-side.
async fn write_hook_trust(
    sink: &mut crate::codex_inject::AppServerSink,
    stream: &mut crate::codex_inject::AppServerStream,
    fno: &[FnoHook],
) -> Result<(), String> {
    let mut state: serde_json::Map<String, Value> = serde_json::Map::new();
    for hook in fno
        .iter()
        .filter(|h| h.enabled && h.trust_status != "trusted")
    {
        state.insert(hook.key.clone(), json!({"trusted_hash": hook.current_hash}));
    }
    if state.is_empty() {
        return Ok(());
    }
    sink.send(Message::Text(
        json!({
            "id": "fno-hook-trust-write",
            "method": "config/batchWrite",
            "params": {
                "edits": [{
                    "keyPath": "hooks.state",
                    "value": Value::Object(state),
                    "mergeStrategy": "upsert",
                }],
                "filePath": null,
                "expectedVersion": null,
                "reloadUserConfig": true,
            }
        })
        .to_string()
        .into(),
    ))
    .await
    .map_err(|error| format!("config/batchWrite send: {error}"))?;
    let raw = tokio::time::timeout(
        REQUEST_TIMEOUT,
        crate::codex_inject::read_until_id(stream, &json!("fno-hook-trust-write")),
    )
    .await
    .map_err(|_| "config/batchWrite timed out".to_string())?
    .map_err(str::to_string)?;
    let frame: Value = serde_json::from_str(&raw).map_err(|e| format!("batchWrite frame: {e}"))?;
    if let Some(error) = frame.get("error") {
        return Err(format!("config/batchWrite refused: {error}"));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// CLI
// ---------------------------------------------------------------------------

/// `fno-agents plugin-install codex --trust-hooks [--check] [--json]`.
/// Always exits 0: the status string carries the verdict, and a failure here
/// must never fail an install (the caller prints the remedy itself).
pub fn run_cli(check_only: bool, json_out: bool) -> i32 {
    let home = crate::codex_converge::codex_home();
    let report = if check_only {
        check(&home)
    } else {
        trust(&home)
    };
    if json_out {
        println!("{}", serde_json::to_string(&report).unwrap_or_default());
    } else {
        print_human(&report);
    }
    0
}

pub(crate) fn print_human(report: &HookTrustReport) {
    match report.status {
        "ok" => println!(
            "codex hook trust: ok ({} fno hooks trusted)",
            report.trusted
        ),
        "untrusted" => println!(
            "codex hook trust: {} of {} fno hooks untrusted; run `fno doctor --codex-hooks --fix`",
            report.not_trusted, report.checked
        ),
        "unsupported" => println!(
            "codex hook trust: skipped ({}); run `fno doctor --codex-hooks --fix`",
            report
                .reason
                .as_deref()
                .unwrap_or("codex has no hooks/list")
        ),
        status => println!(
            "codex hook trust: skipped ({}); run `fno doctor --codex-hooks --fix`",
            report.reason.as_deref().unwrap_or(status)
        ),
    }
}

#[cfg(test)]
#[path = "codex_hook_trust_tests.rs"]
mod tests;
