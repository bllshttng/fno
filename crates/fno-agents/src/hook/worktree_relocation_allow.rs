//! PermissionRequest hook: allow EnterWorktree into a worktree under the
//! resolved `paths.worktrees_base`.
//!
//! Claude Code asks before EnterWorktree moves the session to a path outside
//! `<repo>/.claude/worktrees` ("permission-root relocation"). The ask is a
//! safety check the auto-mode classifier may not approve, and an allow rule
//! is never read for it, so an auto-mode worker that launches from the repo
//! root stops at the prompt. A PermissionRequest hook is the one answer short
//! of bypassPermissions. Allowing only the fno base keeps the ask for every
//! other path.
//!
//! Both paths are canonicalized before the prefix test, so a symlink under
//! the base cannot point the session elsewhere. Anything else prints `{}`,
//! which leaves the prompt in place: this hook never denies.
//!
//! The base holds every repo's worktrees, yet the allow cannot carry a
//! session into another repo: Claude Code refuses any path that is not a
//! registered worktree of the session's own repo, after this hook allows.

use serde_json::{json, Value};
use std::path::{Path, PathBuf};

/// Entry: read one PermissionRequest payload, decide, print, always exit 0.
pub fn run(_args: &[String]) -> i32 {
    let payload: Value = serde_json::from_str(super::read_stdin().trim()).unwrap_or(Value::Null);
    let cwd = payload_cwd(&payload);
    if allows(&payload, &crate::paths_cli::worktrees_base(&cwd)) {
        let out = json!({
            "hookSpecificOutput": {
                "hookEventName": "PermissionRequest",
                "decision": {"behavior": "allow"}
            }
        });
        println!("{out}");
        0
    } else {
        super::emit_allow()
    }
}

fn payload_cwd(payload: &Value) -> PathBuf {
    payload
        .get("cwd")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_else(|| PathBuf::from("."))
}

/// True when the payload is an EnterWorktree `path` call whose real target
/// sits strictly under the real base. Separated from `run` so the tests
/// exercise the same predicate with no stdin or config in the loop.
fn allows(payload: &Value, base: &Path) -> bool {
    if payload.get("tool_name").and_then(Value::as_str) != Some("EnterWorktree") {
        return false;
    }
    let Some(raw) = payload
        .get("tool_input")
        .and_then(|ti| ti.get("path"))
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
    else {
        return false;
    };
    let target = payload_cwd(payload).join(raw);
    let (Ok(target), Ok(base)) = (target.canonicalize(), base.canonicalize()) else {
        return false;
    };
    target != base && target.starts_with(&base)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn call(cwd: &Path, path: &str) -> Value {
        json!({
            "hook_event_name": "PermissionRequest",
            "cwd": cwd.to_string_lossy(),
            "tool_name": "EnterWorktree",
            "tool_input": {"path": path}
        })
    }

    #[test]
    fn allows_only_a_real_path_strictly_under_the_base() {
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path().join("worktrees");
        let wt = base.join("repo").join("node");
        let outside = tmp.path().join("elsewhere");
        std::fs::create_dir_all(&wt).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        let repo = tmp.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();

        assert!(allows(&call(&repo, &wt.to_string_lossy()), &base));
        // Relative paths resolve against the payload cwd.
        assert!(allows(&call(&base, "repo/node"), &base));
        // The base itself, an outside dir, a missing dir: the prompt stays.
        assert!(!allows(&call(&repo, &base.to_string_lossy()), &base));
        assert!(!allows(&call(&repo, &outside.to_string_lossy()), &base));
        assert!(!allows(
            &call(&repo, &base.join("gone").to_string_lossy()),
            &base
        ));
        // A `..` escape canonicalizes out of the base.
        let escape = format!("{}/repo/../../elsewhere", base.display());
        assert!(!allows(&call(&repo, &escape), &base));
        // Another tool, or the create form with no path, never allows.
        let mut other = call(&repo, &wt.to_string_lossy());
        other["tool_name"] = json!("Bash");
        assert!(!allows(&other, &base));
        let create = json!({"tool_name": "EnterWorktree", "tool_input": {"name": "x"}});
        assert!(!allows(&create, &base));
    }

    #[cfg(unix)]
    #[test]
    fn a_symlink_under_the_base_pointing_out_keeps_the_prompt() {
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path().join("worktrees");
        let outside = tmp.path().join("elsewhere");
        std::fs::create_dir_all(&base).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        let link = base.join("link");
        std::os::unix::fs::symlink(&outside, &link).unwrap();
        assert!(!allows(&call(tmp.path(), &link.to_string_lossy()), &base));
    }
}
