//! The client-side `ask` provider hooks, split out of `bin/client.rs` for
//! the file budget: same functions, one `mod` line at the entrypoint.
//! `use super::*` sees the entrypoint's imports, so the bodies moved
//! verbatim and the call sites did not change.

use super::*;

/// Read the `timeout` param as whole seconds; `None` when absent, negative, or fractional.
pub(super) fn opt_timeout(params: &Value) -> Option<std::time::Duration> {
    params
        .get("timeout")
        .and_then(|v| v.as_u64())
        .map(std::time::Duration::from_secs)
}

/// Read the `yolo` param as a bool, defaulting to false when absent.
/// Route a claude `ask` to the client-side `claude --bg` path, bypassing the
/// daemon. Returns `Some(exit_code)` when the target is claude
/// (resolved from an existing registry row, else the `--provider` flag), or
/// `None` to fall through to the daemon RPC for codex/gemini.
pub(super) fn maybe_run_claude_ask(home: &AgentsHome, params: &Value, name: &str) -> Option<i32> {
    use fno_agents::claude_ask::{dispatch_claude_ask, ClaudeHome};
    use fno_agents::state::load_registry;

    let provider_param = params.get("provider").and_then(|v| v.as_str());
    let registry = load_registry(&home.registry_json()).unwrap_or_default();
    let existing_provider = registry
        .find_name_or_full_session_id(name)
        .map(|e| e.harness_name().to_string());

    // Provider mismatch: an existing claude agent plus a conflicting --provider
    // flag. Python's select_provider rejects this as a mismatch; without the
    // check the registry value silently wins and the message is delivered to
    // the wrong provider/session on a stale or mistyped flag (Codex P2).
    if let (Some(ep), Some(pp)) = (existing_provider.as_deref(), provider_param) {
        if ep == "claude" && pp != "claude" {
            eprintln!(
                "fno-agents: agent {name:?} already exists with provider 'claude'; refusing to override with --provider {pp}"
            );
            return Some(2);
        }
    }

    let resolved = existing_provider.as_deref().or(provider_param);
    if resolved != Some("claude") {
        return None; // not a claude target; the daemon path handles it
    }

    let message = params.get("message").and_then(|v| v.as_str()).unwrap_or("");
    let from_name_stamped =
        fno_agents::mail_cli::send::stamp_from(params.get("from_name").and_then(|v| v.as_str()));
    let from_name = from_name_stamped.as_str();
    // ask is a follow-up to an already-registered session: dispatch_claude_ask
    // takes the cwd as `_cwd` and never launches in it. So resolve only an
    // explicit --cwd (canonicalized, Python's `Path(cwd).resolve()`; empty is
    // absent) or the caller cwd -- NEVER the canonical default or the redirect
    // note, which would be a false diagnostic for an operation that does not
    // consume the launch dir (review).
    let cwd = params
        .get("cwd")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(fno_agents::spawn_cwd::canonicalize_cwd)
        .unwrap_or_else(|| {
            std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."))
        });
    let timeout = opt_timeout(params);

    let claude_home = ClaudeHome::from_env();
    let outcome = dispatch_claude_ask(
        home,
        &claude_home,
        name,
        message,
        from_name,
        &cwd,
        false,
        timeout,
        &[],
    );
    // stderr/stdout carry exact bytes (newlines baked in); write verbatim.
    if !outcome.stderr.is_empty() {
        eprint!("{}", outcome.stderr);
    }
    if !outcome.stdout.is_empty() {
        print!("{}", outcome.stdout);
    }
    Some(outcome.exit_code)
}

/// Route a codex `ask` to the client-side `codex exec` path, bypassing the
/// daemon. Returns `Some(exit_code)` when the target is codex
/// (resolved from an existing registry row, else the `--provider` flag), or
/// `None` to fall through to the next provider hook.
pub(super) fn maybe_run_codex_ask(home: &AgentsHome, params: &Value, name: &str) -> Option<i32> {
    fno_agents::codex_ask::maybe_run_codex_ask(home, params, name)
}

/// Route a gemini `ask` to the client-side `gemini -p` path, bypassing the
/// daemon. Returns `Some(exit_code)` when the target is gemini,
/// or `None` to fall through to the unresolvable-`ask` surface.
pub(super) fn maybe_run_gemini_ask(home: &AgentsHome, params: &Value, name: &str) -> Option<i32> {
    fno_agents::gemini_ask::maybe_run_gemini_ask(home, params, name)
}

/// Route an agy `ask` to the client-side stateless guard (Phase C). agy is
/// plain-text with no session id, so a stateful resume is unsupported; this
/// returns `Some(2)` with a redirect error for an agy target, else `None`.
pub(super) fn maybe_run_agy_ask(home: &AgentsHome, params: &Value, name: &str) -> Option<i32> {
    fno_agents::agy_ask::maybe_run_agy_ask(home, params, name)
}

/// Route an opencode `ask` to the client-side pane-only guard.
/// opencode is hosted as a pane with no client-side stateful resume; this
/// returns `Some(2)` with a redirect error for an opencode target, else `None`.
pub(super) fn maybe_run_opencode_ask(home: &AgentsHome, params: &Value, name: &str) -> Option<i32> {
    fno_agents::opencode_ask::maybe_run_opencode_ask(home, params, name)
}
