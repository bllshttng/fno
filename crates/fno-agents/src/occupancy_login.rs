//! The idle login shell verdict (R6 of the worktree occupancy classifier).
//!
//! A login shell (`-/bin/zsh -l`) whose tty has had no input or output for
//! 24h pins nothing: the tree it sits in releases. The shell keeps running -
//! verdict inert, action keep, so the archive path (which signals only
//! `terminate` rows) never closes a terminal tab. Fail closed: a child
//! process, an active tty, or an unreadable tty all hold, and a login shell
//! with a child falls through to the classifier's own hold.
//!
//! `--classify` (occupancy_classify.rs) runs this lane first and hands the
//! classifier only the pids it did not answer. Rows share one TSV shape so
//! the shell bridge's row-count check stays whole.
use std::process::Command;

const SHELL_NAMES: &[&str] = &["zsh", "bash", "sh"];
/// A tty silent this long releases the tree.
const IDLE_SECS: u64 = 24 * 3600;

/// R6 shape: a login shell (`-/bin/zsh -l`, `zsh --login`, ...).
pub(crate) fn is_login_shell(argv: &[&str]) -> bool {
    let Some(argv0) = argv.first() else {
        return false;
    };
    let base = argv0.rsplit('/').next().unwrap_or(argv0);
    let base = base.strip_prefix('-').unwrap_or(base);
    if !SHELL_NAMES.contains(&base) {
        return false;
    }
    if argv0.starts_with('-') {
        return true;
    }
    argv[1..].iter().any(|a| *a == "-l" || *a == "--login")
}

/// The verdict for one probed login shell. `None` falls through to the
/// classifier: a login shell with a child keeps today's unclassified hold,
/// and the child still inherits through the descent rule.
pub(crate) fn decide(
    has_child: bool,
    tty_idle: Option<u64>,
    tty_name: &str,
) -> Option<(&'static str, String)> {
    if has_child {
        return None;
    }
    let tty = if tty_name.is_empty() { "-" } else { tty_name };
    match tty_idle {
        None => Some(("holds", "login shell: tty unreadable".to_string())),
        Some(idle) if idle < IDLE_SECS => Some((
            "holds",
            format!("login shell, tty {} active {}h ago", tty, idle / 3600),
        )),
        Some(idle) => Some((
            "inert",
            format!(
                "idle login shell, tty {} quiet {}h; left running",
                tty,
                idle / 3600
            ),
        )),
    }
}

fn ps_field(field: &str, pid: &str) -> Option<String> {
    let out = Command::new("ps")
        .args(["-o", field, "-p", pid])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if text.is_empty() {
        None
    } else {
        Some(text)
    }
}

fn has_live_child(pid: &str) -> bool {
    // Exit 1 means no children; any other failure reads as a child, fail
    // closed.
    match Command::new("pgrep").arg("-P").arg(pid).output() {
        Ok(out) if out.status.success() => !String::from_utf8_lossy(&out.stdout).trim().is_empty(),
        Ok(_) => false,
        Err(_) => true,
    }
}

/// Idle seconds on the shell's tty: now minus the newest atime/mtime. `None`
/// when the shell has no tty or the device does not answer.
fn tty_idle_secs(pid: &str) -> (Option<u64>, String) {
    let Some(tty) = ps_field("tty=", pid) else {
        return (None, String::new());
    };
    if tty == "?" || tty == "??" {
        return (None, String::new());
    }
    let path = format!("/dev/{}", tty);
    match std::fs::metadata(&path) {
        Ok(meta) => {
            use std::os::unix::fs::MetadataExt;
            let newest = meta.atime().max(meta.mtime()).max(0) as u64;
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0);
            (Some(now.saturating_sub(newest)), tty)
        }
        Err(_) => (None, tty),
    }
}

/// One `pid verdict action job reason cmd` row for a login shell this lane
/// answers; None leaves the pid with the classifier.
pub(crate) fn login_row(pid: u32) -> Option<String> {
    let pid = &pid.to_string();
    let argv_text = ps_field("args=", pid)?;
    let tokens: Vec<&str> = argv_text.split_whitespace().collect();
    if !is_login_shell(&tokens) {
        return None;
    }
    let (idle, tty_name) = tty_idle_secs(pid);
    let (verdict, reason) = decide(has_live_child(pid), idle, &tty_name)?;
    // R6 is always keep: the shell keeps running, only its cwd goes.
    Some(format!("{pid}\t{verdict}\tkeep\t-\t{reason}\t{argv_text}"))
}

/// `fno-agents occupancy-login <pid>...`: one TSV row per login shell this
/// lane answers. `--classify <wt> <pid>...` answers every pid, the login lane
/// first and the occupancy classifier for the rest.
pub fn run(args: &[String]) -> i32 {
    if args.first().map(String::as_str) == Some("--classify") {
        return crate::occupancy_classify::run(&args[1..]);
    }
    // Only a parsed pid reaches the row, never the raw argument text.
    for pid in args.iter().filter_map(|a| a.parse::<u32>().ok()) {
        if let Some(row) = login_row(pid) {
            println!("{row}");
        }
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn login_shell_shapes_match() {
        assert!(is_login_shell(&["-/bin/zsh", "-l"]));
        assert!(is_login_shell(&["/bin/zsh", "--login"]));
        assert!(is_login_shell(&["-/bin/bash"]));
        assert!(!is_login_shell(&["/bin/zsh"]));
        assert!(!is_login_shell(&[
            "/bin/bash",
            "-c",
            "source ~/snapshots/x"
        ]));
        assert!(!is_login_shell(&[]));
        assert!(!is_login_shell(&[
            "/usr/libexec/fno-agents-worker",
            "--pane"
        ]));
    }

    #[test]
    fn quiet_active_and_unreadable_ttys_decide() {
        let (verdict, reason) = decide(false, Some(145_000), "ttys045").unwrap();
        assert_eq!(verdict, "inert");
        assert!(reason.contains("quiet 40h"), "{reason}");
        assert!(reason.contains("ttys045"));

        let (verdict, reason) = decide(false, Some(3_600), "ttys046").unwrap();
        assert_eq!(verdict, "holds");
        assert!(reason.contains("active 1h ago"), "{reason}");

        let (verdict, reason) = decide(false, None, "").unwrap();
        assert_eq!(verdict, "holds");
        assert_eq!(reason, "login shell: tty unreadable");
    }
}
