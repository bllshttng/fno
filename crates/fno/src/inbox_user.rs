//! Native `fno inbox user` verbs, the hidden `fno inbox operator` spelling
//! of the same group, and `fno inbox peers`.
//!
//! The queue reader lives in the runtime crate as `fno-agents compaction
//! operator-turns`, so the front classifies the verb and execs that action:
//! the Stop hook's status read costs one native process and no Python. A verb
//! not yet native, any `--help`, and any flag the Python verb would refuse
//! forward to the Python `fno inbox` group.

use std::ffi::OsString;

/// The verbs answered natively, each with the flags it accepts.
const NATIVE_USER_VERBS: &[(&str, &[&str])] = &[("status", &["--json", "-J"])];

/// Classify `fno inbox user|operator <verb> ...`: `Some(argv)` is the argv
/// tail for `fno-agents compaction`, `None` forwards to Python.
pub fn classify_inbox_user(args: &[OsString]) -> Option<Vec<OsString>> {
    if args.len() < 3 || args[0].to_str()? != "inbox" {
        return None;
    }
    if !matches!(args[1].to_str()?, "user" | "operator") {
        return None;
    }
    let verb = args[2].to_str()?;
    let (_, flags) = NATIVE_USER_VERBS.iter().find(|(name, _)| *name == verb)?;
    let rest = &args[3..];
    if !rest
        .iter()
        .all(|arg| arg.to_str().is_some_and(|a| flags.contains(&a)))
    {
        return None;
    }
    let mut argv: Vec<OsString> = ["operator-turns", "--render", verb]
        .iter()
        .map(OsString::from)
        .collect();
    argv.extend_from_slice(rest);
    Some(argv)
}

/// Classify `fno inbox peers [--json|-J|--help]`: the argv tail for
/// `fno-agents compaction inbox-peers`. Python has no peers command, so any
/// other argument still runs native and the runtime prints the usage refusal.
pub fn classify_inbox_peers(args: &[OsString]) -> Option<Vec<OsString>> {
    if args.len() < 2 || args[0].to_str()? != "inbox" || args[1].to_str()? != "peers" {
        return None;
    }
    let mut argv = vec![OsString::from("inbox-peers")];
    argv.extend_from_slice(&args[2..]);
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
        assert_eq!(
            classify_inbox_peers(&mk(&["inbox", "peers", "--json"])),
            Some(mk(&["inbox-peers", "--json"]))
        );
        assert!(classify_inbox_peers(&mk(&["inbox", "user", "status"])).is_none());
    }
}
