//! The lead-guard wire layer: payload boundary helpers and shared refusal
//! text.
//!
//! [`super::lead_guard`] owns the decision core (registry, manifest, roots,
//! mode). This module owns everything that crosses the payload boundary:
//! the file-edit target derivation (claude file_path payloads and codex
//! apply_patch bodies), the codex patch-header walk, and the two-line
//! refusal text both wires print. The agy payload translation moved to
//! [`super::adapter::agy::to_claude_pretool`], which the pretool host and
//! this guard's claude wire both serve from.

use serde_json::Value;
use std::path::Path;

/// The two-line refusal. The shell twin is a pure exec shim, so this text is
/// the only copy of the rule it enforces.
pub(super) fn deny_text(target: &str, repo_root: &Path) -> String {
    format!(
        "lead-delegation-guard: write target '{target}' is inside the repo ({repo}), and a promoted session does not write SOURCE.\n\
         A lead operates the machine and does not author it: deploy and repair commands (fno config plugin install, fno doctor update) run, build output and everything outside the repo allow, repo source does not. Delegate the edit or escalate. An operator can list an in-repo path in config.lead.write_roots.\n",
        repo = repo_root.display(),
    )
}

// ── File edit classification ────────────────────────────────────────────────

/// The denied write, if any, for the file-editing tools: a `file_path` (or
/// `notebook_path`) payload judges by its one path; a codex patch payload
/// (body in `tool_input.command`) judges by its header paths. The marker
/// gate keeps an arbitrary command from being read as a patch; the deny
/// names the first denied path.
pub(super) fn read_denied(
    tool: &str,
    ti: &Value,
    targets: &[String],
    allowed: &dyn Fn(&str) -> bool,
) -> Option<String> {
    if tool == "Bash" {
        return targets.iter().find(|t| !allowed(t)).map(|t| t.to_string());
    }
    let file = ti
        .get("file_path")
        .or_else(|| ti.get("notebook_path"))
        .and_then(Value::as_str)
        .unwrap_or("");
    if !file.is_empty() {
        return (!allowed(file)).then(|| file.to_string());
    }
    let cmd = ti.get("command").and_then(Value::as_str).unwrap_or("");
    let targets = if cmd.contains("*** Begin Patch") {
        patch_targets(cmd)
    } else {
        Vec::new()
    };
    targets.iter().find(|t| !allowed(t)).map(|t| t.to_string())
}

/// Write targets a codex apply_patch body binds: the four header prefixes,
/// the same four `hooks/lib/write-targets.sh` reads. A patch BODY is file
/// content, never re-scanned as shell; only the headers name paths.
pub(super) fn patch_targets(command: &str) -> Vec<String> {
    command
        .lines()
        .filter_map(|line| {
            let path = line
                .strip_prefix("*** Add File: ")
                .or_else(|| line.strip_prefix("*** Update File: "))
                .or_else(|| line.strip_prefix("*** Delete File: "))
                .or_else(|| line.strip_prefix("*** Move to: "))?;
            let path = path.trim_end_matches('\r');
            (!path.is_empty()).then(|| path.to_string())
        })
        .collect()
}

// ── Limb signatures ──────────────────────────────────────────────────────────

pub(super) fn is_subagent_transcript(transcript: &str, sid: &str) -> bool {
    let Some(parent) = Path::new(transcript).parent() else {
        return false;
    };
    parent.file_name().is_some_and(|n| n == "subagents")
        && parent
            .parent()
            .and_then(|g| g.file_name())
            .is_some_and(|n| n == sid)
}

/// The sync-limb shape: the transcript's newest tool_use is Task/Agent with no
/// tool_result yet. Tail-only (the open entry sits at the end of a live
/// transcript); unreadable falls through fail-closed. The pairing is the one
/// shared walk (`interrupt_classify::trailing_open_call`), not a private leg.
pub(super) fn transcript_is_open_spawn(transcript: &str) -> bool {
    if transcript.is_empty() {
        return false;
    }
    crate::tail_text_strict(Path::new(transcript), 262_144)
        .and_then(|tail| {
            crate::interrupt_classify::trailing_open_call(&tail)
                .filter(|c| c.name == "Task" || c.name == "Agent")
        })
        .is_some()
}
