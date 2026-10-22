//! Native `fno inbox user` verbs, the hidden `fno inbox operator` spelling
//! of the same group, and `fno inbox peers`.
//!
//! The queue reader lives in the runtime crate as `fno-agents compaction
//! operator-turns`, so the front classifies the verb and execs that action:
//! the Stop hook's status read costs one native process and no Python. Any
//! `--help` and any flag the Python verb would refuse forward to the Python
//! `fno inbox` group.

use std::ffi::OsString;

/// The verbs answered natively: each with its switches and its value flags.
/// A value flag carries a positive count; any other value forwards, so the
/// Python verb's own refusal answers it.
const NATIVE_USER_VERBS: &[(&str, &[&str], &[&str])] = &[
    ("status", &["--json", "-J"], &[]),
    ("list", &["--json", "-J"], &["--limit", "-L"]),
];

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
    if verb == "ack" {
        return classify_ack(&args[3..]);
    }
    let (_, switches, valued) = NATIVE_USER_VERBS.iter().find(|(name, ..)| *name == verb)?;
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
    let mut argv: Vec<OsString> = ["operator-turns", "--render", verb]
        .iter()
        .map(OsString::from)
        .collect();
    argv.extend_from_slice(rest);
    Some(argv)
}

/// `ack <turn> --outcome <o> [--why <w>]`, in any order: one turn word and
/// a non-empty `--outcome`. Any other shape forwards to Python.
fn classify_ack(rest: &[OsString]) -> Option<Vec<OsString>> {
    let (mut turn, mut outcome) = (false, false);
    let mut i = 0;
    while i < rest.len() {
        match rest[i].to_str()? {
            flag @ ("--outcome" | "--why") => {
                let value = rest.get(i + 1)?.to_str()?;
                if value.is_empty() || value.starts_with('-') {
                    return None;
                }
                outcome |= flag == "--outcome";
                i += 2;
            }
            word if !word.starts_with('-') && !turn => {
                turn = true;
                i += 1;
            }
            _ => return None,
        }
    }
    if !(turn && outcome) {
        return None;
    }
    let mut argv = vec![OsString::from("ack")];
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
    fn the_user_verbs_run_native_and_everything_else_forwards() {
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
            assert_eq!(
                classify_inbox_user(&mk(&["inbox", group, "ack", "u-1", "--outcome", "nothing"])),
                Some(mk(&["ack", "u-1", "--outcome", "nothing"]))
            );
            assert!(classify_inbox_user(&mk(&["inbox", group, "ack", "u-1"])).is_none());
            assert!(classify_inbox_user(&mk(&[
                "inbox",
                group,
                "ack",
                "u-1",
                "u-2",
                "--outcome",
                "nothing"
            ]))
            .is_none());
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
