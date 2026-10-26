//! The worktree family's front door. `fno worktree <leaf>`, `fno workspace
//! <leaf>`, and the `fno agents workspace` spelling all resolve here: the
//! lifecycle leaves exec the native ops (crates/fno-agents/src/worktree/),
//! status/cleanup/archive exec the guarded bash script, and an unknown leaf
//! refuses here because the Python mounts died with the family port.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

/// One resolved launch: the sibling binary's argv, or the lifecycle script.
pub enum Launch {
    /// Exec `fno-agents <argv>` (stdio inherited, exit code returned).
    Agents(Vec<String>),
    /// Exec `bash scripts/lib/worktree-lifecycle.sh <leaf args>` from the
    /// repo root, the composition the deleted Python verbs forwarded to.
    Lifecycle(Vec<String>),
    /// An argv shape the family owns but no leaf answers: refuse with the
    /// leaf list instead of forwarding to mounts that no longer exist.
    Refuse(String),
}

impl Launch {
    pub fn run(self) -> i32 {
        match self {
            Launch::Agents(argv) => {
                let bin = crate::digest_overlay::fno_agents_bin();
                let mut cmd = std::process::Command::new(bin);
                cmd.args(&argv);
                match cmd.status() {
                    Ok(status) => status.code().unwrap_or(1),
                    Err(e) => {
                        eprintln!("fno worktree: could not run fno-agents: {e}");
                        1
                    }
                }
            }
            Launch::Lifecycle(rest) => {
                let Some(script) = lifecycle_script() else {
                    eprintln!(
                        "fno worktree: no usable plugin root for \
                         scripts/lib/worktree-lifecycle.sh"
                    );
                    return 2;
                };
                let root = repo_root();
                let mut cmd = std::process::Command::new("bash");
                cmd.arg(script).args(&rest).current_dir(&root);
                match cmd.status() {
                    Ok(status) => status.code().unwrap_or(1),
                    Err(e) => {
                        eprintln!("fno worktree: could not run the lifecycle script: {e}");
                        2
                    }
                }
            }
            Launch::Refuse(message) => {
                eprintln!("{message}");
                2
            }
        }
    }
}

/// `scripts/lib/worktree-lifecycle.sh` under the plugin root: the env roots
/// first, then the install pointer file in the layout's marker order
/// (`install/plugin-root`, then the legacy spelling), and only a root
/// carrying the script answers - a stale pointer falls through rather than
/// naming a dead path.
fn lifecycle_script() -> Option<PathBuf> {
    for name in ["CLAUDE_PLUGIN_ROOT", "CODEX_PLUGIN_ROOT", "FNO_REPO_ROOT"] {
        if let Ok(root) = std::env::var(name) {
            if !root.is_empty() {
                return probe_root(PathBuf::from(root));
            }
        }
    }
    let root = match std::env::var_os("FNO_HOME") {
        Some(home) if !home.is_empty() => PathBuf::from(home),
        _ => home_dir().join(".fno"),
    };
    let folded = root.join("install").join("plugin-root");
    let legacy = root.join("plugin-root");
    for candidate in [folded, legacy] {
        if let Some(script) = probe_root(&candidate) {
            return Some(script);
        }
    }
    None
}

fn probe_root(root: &Path) -> Option<PathBuf> {
    let script = root
        .join("scripts")
        .join("lib")
        .join("worktree-lifecycle.sh");
    script.is_file().then_some(script.to_path_buf())
}

fn home_dir() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/"))
}

/// The three spellings reduce to one surface: `worktree <leaf>`,
/// `workspace worktree <leaf> | reap | register-worker`, and the same after
/// an `agents` prefix. Returns None for any other argv so unrelated
/// commands fall through untouched.
pub fn classify(args: &[OsString]) -> Option<Launch> {
    let strs: Vec<String> = args
        .iter()
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    let tail: &[String] = match strs.first().map(String::as_str) {
        Some("worktree") => &strs[1..],
        Some("workspace") if strs.get(1).map(String::as_str) == Some("worktree") => &strs[2..],
        Some("workspace") => {
            return workspace_leaf(&strs[1..]);
        }
        Some("agents") if strs.get(1).map(String::as_str) == Some("workspace") => {
            let after = &strs[2..];
            match after.first().map(String::as_str) {
                Some("worktree") => &after[1..],
                _ => return workspace_leaf(after),
            }
        }
        _ => return None,
    };
    worktree_leaf(tail)
}

/// The `worktree <leaf>` table. The native ops pass flags through verbatim;
/// the three lifecycle leaves carry the leaf word the bash script reads.
fn worktree_leaf(tail: &[String]) -> Option<Launch> {
    match tail.first().map(String::as_str) {
        Some("policy") => agents_launch("worktree-policy", &tail[1..]),
        Some("ensure") => agents_launch("worktree-ensure", &tail[1..]),
        Some("stranded") => agents_launch("worktree-stranded", &tail[1..]),
        Some("reapable") => agents_launch("worktree-reapable", &tail[1..]),
        Some("overlap-record") => agents_launch("worktree-overlap-record", &tail[1..]),
        Some("overlaps") => agents_launch("worktree-overlaps", &tail[1..]),
        Some("status") => Some(Launch::Lifecycle(vec!["status".into()])),
        Some("cleanup") => Some(Launch::Lifecycle(tail.to_vec())),
        Some("archive") => Some(Launch::Lifecycle(tail.to_vec())),
        _ => Some(Launch::Refuse(
            "fno worktree: unknown leaf. Leaves: policy ensure status cleanup archive \
             stranded reapable overlap-record overlaps"
                .to_string(),
        )),
    }
}

/// The `workspace` surface's own two leaves (the worktree app is folded in
/// above; these are the workspace CLI's remaining commands).
fn workspace_leaf(tail: &[String]) -> Option<Launch> {
    match tail.first().map(String::as_str) {
        Some("reap") => {
            let mut argv = vec!["reap".to_string(), "--state-files-only".to_string()];
            argv.extend(tail[1..].iter().cloned());
            Some(Launch::Agents(argv))
        }
        Some("register-worker") => agents_launch("workspace-register-worker", &tail[1..]),
        _ => Some(Launch::Refuse(
            "fno workspace: unknown leaf. Leaves: worktree <leaf> | reap | register-worker"
                .to_string(),
        )),
    }
}

/// The op argv with its leading word: the sibling binary's raw intercepts.
fn agents_launch(op: &str, rest: &[String]) -> Option<Launch> {
    let mut argv = vec![op.to_string()];
    argv.extend(rest.iter().cloned());
    Some(Launch::Agents(argv))
}

/// The git root of the cwd, so the lifecycle script's relative paths (the
/// `.claude/worktrees/<name>` arm) resolve from a subdirectory too.
fn repo_root() -> PathBuf {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let out = std::process::Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .current_dir(&cwd)
        .output();
    match out {
        Ok(o) if o.status.success() => PathBuf::from(String::from_utf8_lossy(&o.stdout).trim()),
        _ => cwd,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(words: &[&str]) -> Vec<OsString> {
        words.iter().map(OsString::from).collect()
    }

    #[test]
    fn workspace_reap_builds_the_state_files_only_argv() {
        // The workspace reap leaf never touches mux tabs: the door inserts
        // --state-files-only ahead of whatever the caller passed.
        let launch = classify(&args(&["agents", "workspace", "reap", "--json"])).unwrap();
        match launch {
            Launch::Agents(argv) => {
                assert_eq!(argv, vec!["reap", "--state-files-only", "--json"])
            }
            other => panic!("expected an agents launch, got {other:?}"),
        }
    }

    #[test]
    fn unknown_workspace_leaf_refuses_with_the_leaf_list() {
        let launch = classify(&args(&["workspace", "reassign"])).unwrap();
        match launch {
            Launch::Refuse(msg) => {
                assert!(msg.contains("worktree <leaf>"), "refusal said: {msg}");
                assert!(msg.contains("register-worker"), "refusal said: {msg}");
            }
            other => panic!("expected a refusal, got {other:?}"),
        }
    }
}
