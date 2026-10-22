//! Native `fno inbox user` commands, and the hidden `fno inbox operator`
//! spelling of the same group.
//!
//! The queue reader lives in the runtime crate as `fno-agents compaction
//! operator-turns`, so the front classifies the command and execs that action:
//! the Stop hook's status read costs one native process and no Python. A command
//! not yet native, any `--help`, and any flag the Python command would refuse
//! forward to the Python `fno inbox` group.

use std::ffi::OsString;

/// The commands answered natively, each with the flags it accepts.
const NATIVE_USER_COMMANDS: &[(&str, &[&str])] = &[("status", &["--json", "-J"])];

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
    let (_, flags) = NATIVE_USER_COMMANDS
        .iter()
        .find(|(name, _)| *name == command)?;
    let rest = &args[3..];
    if !rest
        .iter()
        .all(|arg| arg.to_str().is_some_and(|a| flags.contains(&a)))
    {
        return None;
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
    fn status_runs_native_and_everything_else_forwards() {
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
            assert!(classify_inbox_user(&mk(&["inbox", group, "list"])).is_none());
            assert!(classify_inbox_user(&mk(&["inbox", group])).is_none());
        }
        assert!(classify_inbox_user(&mk(&["inbox", "law", "status"])).is_none());
    }
}
