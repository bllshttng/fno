//! Native `fno inbox user` commands, and the hidden `fno inbox operator`
//! spelling of the same group.
//!
//! The queue reader lives in the runtime crate as `fno-agents compaction
//! operator-turns`, so the front classifies the command and execs that action:
//! the Stop hook's status read costs one native process and no Python. A command
//! not yet native, any `--help`, and any flag the Python command would refuse
//! forward to the Python `fno inbox` group.

use std::ffi::OsString;

/// The commands answered natively: each with its switches and its value flags.
/// A value flag carries a positive count; any other value forwards, so the
/// Python command's own refusal answers it.
const NATIVE_USER_COMMANDS: &[(&str, &[&str], &[&str])] = &[
    ("status", &["--json", "-J"], &[]),
    ("list", &["--json", "-J"], &["--limit", "-L"]),
];

/// Classify `fno inbox user|operator <command> ...`: `Some(argv)` is the argv
/// tail for `fno-agents compaction`, `None` forwards to Python.
pub fn classify_inbox_user(args: &[OsString]) -> Option<Vec<OsString>> {
    if args.len() < 3 || args[0].to_str()? != "inbox" {
        return None;
    }
    if !matches!(args[1].to_str()?, "user" | "operator") {
        return None;
    }
    let command = args[2].to_str()?;
    let (_, switches, valued) = NATIVE_USER_COMMANDS
        .iter()
        .find(|(name, ..)| *name == command)?;
    let rest = &args[3..];
    let mut i = 0;
    while i < rest.len() {
        let arg = rest[i].to_str()?;
        if valued.contains(&arg) {
            let count = rest.get(i + 1)?.to_str()?.parse::<usize>().ok()?;
            if count == 0 {
                return None;
            }
            i += 2;
        } else if switches.contains(&arg) {
            i += 1;
        } else {
            return None;
        }
    }
    let mut argv: Vec<OsString> = ["operator-turns", "--render", command]
        .iter()
        .map(OsString::from)
        .collect();
    argv.extend_from_slice(rest);
    Some(argv)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mk(args: &[&str]) -> Vec<OsString> {
        args.iter().map(OsString::from).collect()
    }

    #[test]
    fn status_and_list_run_native_and_everything_else_forwards() {
        for group in ["user", "operator"] {
            assert_eq!(
                classify_inbox_user(&mk(&["inbox", group, "status", "--json"])),
                Some(mk(&["operator-turns", "--render", "status", "--json"]))
            );
            assert_eq!(
                classify_inbox_user(&mk(&["inbox", group, "status"])),
                Some(mk(&["operator-turns", "--render", "status"]))
            );
            assert!(classify_inbox_user(&mk(&["inbox", group, "status", "--help"])).is_none());
            assert!(
                classify_inbox_user(&mk(&["inbox", group, "status", "--session", "s"])).is_none()
            );
            assert_eq!(
                classify_inbox_user(&mk(&["inbox", group, "list", "-L", "2", "-J"])),
                Some(mk(&["operator-turns", "--render", "list", "-L", "2", "-J"]))
            );
            assert!(classify_inbox_user(&mk(&["inbox", group, "list", "--limit", "0"])).is_none());
            assert!(classify_inbox_user(&mk(&["inbox", group, "list", "--limit"])).is_none());
            assert!(
                classify_inbox_user(&mk(&["inbox", group, "status", "--limit", "1"])).is_none()
            );
            assert!(classify_inbox_user(&mk(&["inbox", group, "ack", "u-1"])).is_none());
            assert!(classify_inbox_user(&mk(&["inbox", group])).is_none());
        }
        assert!(classify_inbox_user(&mk(&["inbox", "law", "status"])).is_none());
    }
}
