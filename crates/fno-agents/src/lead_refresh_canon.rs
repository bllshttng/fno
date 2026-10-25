//! `lead-refresh-canon`: the lead beat's canon-doc refresh, the native door
//! behind `fno agents org refresh-canon`.
//!
//! The beat used to run the hook through a caller-resolved `$PLUGIN_ROOT`,
//! chained from two env vars and the install pointer file. In a shell with
//! none of the three the word vanished, bash exited 127, and a beat that
//! sent its output to /dev/null hid the failure while the check-in read the
//! doc as covered from older writes. This door resolves the root itself,
//! keeps the hook's output on the caller's terminal, and refuses naming
//! every source it tried when none resolves.

use std::process::{Command, Stdio};

const EXIT_USAGE: i32 = 2;

const HOOK_REL: &str = "hooks/precompact-canon-doc.sh";

/// The pointer file the install command and the session-start hook write;
/// the same anchor `provider::plugin_root` reads.
fn pointer_path() -> std::path::PathBuf {
    let home = match std::env::var("FNO_HOME") {
        Ok(home) if !home.is_empty() => std::path::PathBuf::from(home),
        _ => crate::paths::dirs_home().join(".fno"),
    };
    crate::state_layout::place(&home, "plugin-root")
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
/// reader share, so the refusal and the exec cannot disagree.
fn hook_path(root: &std::path::Path) -> std::path::PathBuf {
    root.join(HOOK_REL)
}

/// `fno-agents lead-refresh-canon`: resolve the plugin root, run the hook,
/// keep its output. Takes no arguments; any argument is a usage refusal.
pub fn run_refresh_canon(args: &[String]) -> i32 {
    if !args.is_empty() {
        eprintln!("refresh-canon: takes no arguments, got {args:?}");
        return EXIT_USAGE;
    }
    let Some(root) = crate::provider::plugin_root() else {
        eprintln!("{}", refusal_message());
        return 1;
    };
    let hook = hook_path(&root);
    if !hook.is_file() {
        eprintln!(
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
            eprintln!("refresh-canon: could not run {}: {e}", hook.display());
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
        let hook = hook_path(std::path::Path::new("/stage/fno"));
        assert_eq!(
            hook,
            std::path::Path::new("/stage/fno/hooks/precompact-canon-doc.sh")
        );
    }
}
