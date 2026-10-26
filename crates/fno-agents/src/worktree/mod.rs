//! The worktree family's native verbs, ported from the deleted
//! `cli/src/fno/worktree*.py`, `runtime/`, and `workspace/` Python legs.
//!
//! Each op is a raw `fno-agents` client intercept (client.rs), so the curated
//! menu sees none of them; the `fno` front door claims the `worktree`,
//! `workspace` and `agents workspace` argv shapes and execs here. The goldens
//! under `tests/golden/worktree/` were captured from the Python legs before
//! their deletion (step 2 of the port protocol,
//! docs/architecture/dual-implementation-inventory.md) and characterize these
//! ops through `worktree_parity.rs`.

pub mod ensure;
pub mod overlaps;
pub mod policy;
pub mod registry_worker;
pub mod status;
pub mod stranded;

use std::path::{Path, PathBuf};

/// The live-status vocabulary a registry row must carry for a session to own
/// its tree. Shared with `OWNERSHIP_LIVE_STATUSES`
/// (cli/src/fno/harness_identity.py); the parity corpus pins the split.
pub const OWNERSHIP_LIVE_STATUSES: [&str; 6] =
    ["spawning", "ready", "idle", "busy", "live", "restarting"];

/// Python's non-strict `Path.resolve()`: canonicalize the longest existing
/// ancestor, then append the remainder. `fs::canonicalize` refuses a missing
/// tail, which every default base path here has.
pub(crate) fn resolve_lenient(path: &Path) -> PathBuf {
    let mut existing = path.to_path_buf();
    let mut tail: Vec<std::ffi::OsString> = Vec::new();
    loop {
        if existing.symlink_metadata().is_ok() {
            break;
        }
        match existing.file_name() {
            Some(name) => {
                tail.push(name.to_os_string());
                existing = existing.parent().unwrap_or(Path::new("/")).to_path_buf();
            }
            None => break,
        }
    }
    let mut resolved = std::fs::canonicalize(&existing).unwrap_or(existing);
    for part in tail.iter().rev() {
        resolved.push(part);
    }
    resolved
}

/// `os.path.expanduser` for the one leading `~` (and bare `~`).
pub(crate) fn expanduser(raw: &str) -> PathBuf {
    if raw == "~" {
        return home_dir();
    }
    if let Some(rest) = raw.strip_prefix("~/") {
        return home_dir().join(rest);
    }
    PathBuf::from(raw)
}

fn home_dir() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/"))
}

/// The state root: `$FNO_STATE_DIR` when set, else `~/.fno` (paths.state_dir's
/// config-free arm).
pub(crate) fn state_root() -> PathBuf {
    match std::env::var_os("FNO_STATE_DIR") {
        Some(dir) if !dir.is_empty() => resolve_lenient(Path::new(&dir)),
        _ => resolve_lenient(&home_dir().join(".fno")),
    }
}

pub(crate) fn git_out(dir: &Path, args: &[&str]) -> Option<std::process::Output> {
    std::process::Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .ok()
}

/// The `git worktree list --porcelain` rows: (branch, path), bare rows
/// skipped, detached rows carrying no branch - the reader three modules
/// shared in the Python family.
pub(crate) fn parse_porcelain_rows(text: &str) -> Vec<(Option<String>, String)> {
    let mut rows: Vec<(Option<String>, String)> = Vec::new();
    let mut path: Option<String> = None;
    let mut branch: Option<String> = None;
    let mut detached = false;
    let mut bare = false;
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("worktree ") {
            if path.is_some() && !bare {
                rows.push((
                    if detached { None } else { branch.clone() },
                    path.clone().unwrap(),
                ));
            }
            path = Some(rest.to_string());
            branch = None;
            detached = false;
            bare = false;
        } else if let Some(rest) = line.strip_prefix("branch refs/heads/") {
            branch = Some(rest.to_string());
        } else if line == "detached" {
            detached = true;
        } else if line == "bare" {
            bare = true;
        }
    }
    if path.is_some() && !bare {
        rows.push((if detached { None } else { branch.clone() }, path.unwrap()));
    }
    rows
}

/// A `git` run bounded by `secs`; a timeout or spawn failure reads as None.
pub(crate) fn git_bounded(dir: &Path, args: &[&str], secs: u64) -> Option<std::process::Output> {
    let mut cmd = std::process::Command::new("git");
    cmd.args(args).current_dir(dir);
    run_bounded(cmd, secs)
}

/// Bound one child with a thread-side wait, like the Python legs' `timeout=`.
pub(crate) fn run_bounded(
    mut cmd: std::process::Command,
    secs: u64,
) -> Option<std::process::Output> {
    let (tx, rx) = std::sync::mpsc::channel();
    let _ = tx.clone();
    match cmd
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
    {
        Ok(mut child) => {
            let handle = std::thread::spawn(move || {
                let out = child.wait_with_output();
                let _ = tx.send(());
                out.ok()
            });
            match rx.recv_timeout(std::time::Duration::from_secs(secs)) {
                Ok(()) => handle.join().ok().flatten(),
                Err(_) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    None
                }
            }
        }
        Err(_) => None,
    }
}

/// Print one canonical envelope piece: sorted-key compact JSON + newline, the
/// serialization the capture driver froze with `sort_keys`.
pub(crate) fn worktrees_base() -> PathBuf {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    crate::paths_cli::worktrees_base(&cwd)
}
pub(crate) fn sorted_json(value: &serde_json::Value) -> String {
    fn write_sorted(v: &serde_json::Value, out: &mut String) {
        match v {
            serde_json::Value::Object(map) => {
                let mut keys: Vec<&String> = map.keys().collect();
                keys.sort();
                out.push('{');
                for (i, k) in keys.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    out.push_str(&serde_json::Value::String((*k).clone()).to_string());
                    out.push(':');
                    write_sorted(&map[*k], out);
                }
                out.push('}');
            }
            serde_json::Value::Array(items) => {
                out.push('[');
                for (i, item) in items.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    write_sorted(item, out);
                }
                out.push(']');
            }
            other => out.push_str(&other.to_string()),
        }
    }
    let mut out = String::new();
    write_sorted(value, &mut out);
    out
}
