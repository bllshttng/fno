//! `fno agents org refresh-canon`: the lead beat's canon-doc refresh, run in
//! process behind the [`crate::agents_history`] classification pattern.
//!
//! The beat used to run the hook through a caller-resolved `$PLUGIN_ROOT`,
//! chained from two env vars and the install pointer file. In a shell with
//! none of the three the word vanished, bash exited 127, and a beat that
//! sent its output to /dev/null hid the failure while the check-in read the
//! doc as covered from older writes. This door resolves the root itself,
//! keeps the hook's output on the caller's terminal, and refuses naming
//! every source it tried when none resolves.
//!
//! The pointer resolution mirrors the sibling crate's provider reader (env
//! chain, then the state-root layout's `plugin-root` marker row). The two
//! crates do not depend on each other; when the layout row moves, move this
//! mirror with it.

use std::ffi::OsString;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const HOOK_REL: &str = "hooks/precompact-canon-doc.sh";

/// `$HOME`, or `/` when the environment lost it - the same anchor the
/// sibling provider reader uses.
fn dirs_home() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/"))
}

/// The install pointer file under the state root, in the layout's marker
/// order: the folded destination while it exists, the legacy spelling while
/// it exists, else the folded destination (a fresh install).
fn pointer_path() -> PathBuf {
    let root = match std::env::var_os("FNO_HOME") {
        Some(home) if !home.is_empty() => PathBuf::from(home),
        _ => dirs_home().join(".fno"),
    };
    let new = root.join("install").join("plugin-root");
    let legacy = root.join("plugin-root");
    if new.exists() {
        new
    } else if legacy.exists() {
        legacy
    } else {
        new
    }
}

/// The resolved plugin root, or None: the env roots first, then the pointer
/// file, and only a root carrying the plugin manifest answers - a stale
/// pointer falls through rather than naming a dead path.
fn plugin_root() -> Option<PathBuf> {
    for name in ["CLAUDE_PLUGIN_ROOT", "CODEX_PLUGIN_ROOT", "FNO_REPO_ROOT"] {
        if let Ok(root) = std::env::var(name) {
            if !root.is_empty() {
                return Some(PathBuf::from(root));
            }
        }
    }
    let text = std::fs::read_to_string(pointer_path()).ok()?;
    let root = PathBuf::from(text.trim());
    (root.join(".claude-plugin").join("plugin.json").is_file()).then_some(root)
}

/// The refusal when no source resolves. It names every source, so the beat
/// can fix the one that is wrong instead of guessing.
fn refusal_message() -> String {
    format!(
        "refresh-canon: no usable plugin root: tried $CLAUDE_PLUGIN_ROOT, \
$CODEX_PLUGIN_ROOT, $FNO_REPO_ROOT, and the pointer file {}",
        pointer_path().display()
    )
}

/// The hook path under a resolved root: the one composition writer and
/// reader share, so the refusal and the run cannot disagree.
fn hook_path(root: &Path) -> PathBuf {
    root.join(HOOK_REL)
}

/// Run the beat refresh and return the process exit code. Takes no
/// arguments; any argument is a usage refusal.
pub fn run(args: &[OsString]) -> i32 {
    let stderr = io::stderr();
    run_to(args, &mut stderr.lock())
}

/// The door's own writes go to stderr only: the hook's stdout is the beat's
/// output and the child inherits it, so the caller never forwards one.
fn run_to(args: &[OsString], stderr: &mut impl Write) -> i32 {
    if !args.is_empty() {
        let _ = writeln!(stderr, "refresh-canon: takes no arguments");
        return 2;
    }
    let Some(root) = plugin_root() else {
        let _ = writeln!(stderr, "{}", refusal_message());
        return 1;
    };
    let hook = hook_path(&root);
    if !hook.is_file() {
        let _ = writeln!(
            stderr,
            "refresh-canon: the resolved plugin root {} carries no {HOOK_REL}",
            root.display()
        );
        return 1;
    }
    // The beat reads what the hook prints: stdio is inherited, never a
    // redirect a caller can silence. The hook never blocks and degrades to
    // exit 0 on unreadable inputs, so its code is the beat's code.
    match Command::new("bash")
        .arg(&hook)
        .stdin(Stdio::null())
        .status()
    {
        Ok(status) => status.code().unwrap_or(1),
        Err(e) => {
            let _ = writeln!(
                stderr,
                "refresh-canon: could not run {}: {e}",
                hook.display()
            );
            1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refusal_names_every_source_and_the_hook_path_composes() {
        let message = refusal_message();
        assert!(message.contains("$CLAUDE_PLUGIN_ROOT"), "{message}");
        assert!(message.contains("$CODEX_PLUGIN_ROOT"), "{message}");
        assert!(message.contains("$FNO_REPO_ROOT"), "{message}");
        assert!(message.contains("plugin-root"), "{message}");
        let hook = hook_path(Path::new("/stage/fno"));
        assert_eq!(hook, Path::new("/stage/fno/hooks/precompact-canon-doc.sh"));
    }

    #[test]
    fn extra_arguments_are_a_usage_refusal() {
        let mut err: Vec<u8> = Vec::new();
        let code = run_to(&[OsString::from("--extra")], &mut err);
        assert_eq!(code, 2);
        assert!(String::from_utf8_lossy(&err).contains("takes no arguments"));
    }
}
