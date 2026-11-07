//! Native `fno inbox user` commands, the hidden `fno inbox operator` spelling
//! of the same group, and `fno inbox peers`.
//!
//! The queue reader lives in the runtime crate as `fno-agents compaction
//! operator-turns`, so the front classifies the command and execs that action:
//! the Stop hook's status read costs one native process and no Python. Any
//! `--help` and any flag the Python command would refuse forward to the Python
//! `fno inbox` group.

use std::ffi::OsString;

/// The commands answered natively: each with its switches and its value flags.
/// A value flag carries a positive count; any other value forwards, so the
/// Python command's own refusal answers it.
const NATIVE_USER_COMMANDS: &[(&str, &[&str], &[&str])] = &[
    ("status", &["--json", "-J"], &[]),
    ("list", &["--json", "-J", "--all"], &["--limit", "-L"]),
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
    if command == "ack" {
        return classify_ack(&args[3..]);
    }
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

/// Classify `fno inbox outstanding ...` and the hidden legacy `fno
/// outstanding` spelling: `Some(rest)` is the argv tail for `fno-agents
/// compaction outstanding`. The whole group runs native now the Python
/// package is deleted: the listing, its subcommands (`mine`, `ask`, `clear`,
/// `reindex`), and any unknown flag refuse through the runtime's own usage
/// line.
pub fn classify_inbox_outstanding(args: &[OsString]) -> Option<Vec<OsString>> {
    let rest = match args.first()?.to_str()? {
        "inbox" if args.get(1)?.to_str()? == "outstanding" => &args[2..],
        "outstanding" => &args[1..],
        _ => return None,
    };
    Some(rest.to_vec())
}

/// Classify `fno inbox approvals ls|show|decide ...`: the argv tail for
/// `fno-agents compaction approvals`. Any `--help` forwards to the Python
/// group, which only lists the commands; the runtime owns every other argument.
pub fn classify_inbox_approvals(args: &[OsString]) -> Option<Vec<OsString>> {
    if args.len() < 3 || args[0].to_str()? != "inbox" || args[1].to_str()? != "approvals" {
        return None;
    }
    if !matches!(args[2].to_str()?, "ls" | "show" | "decide") {
        return None;
    }
    if args[3..]
        .iter()
        .any(|a| matches!(a.to_str(), Some("--help" | "-h")))
    {
        return None;
    }
    let mut argv = vec![OsString::from("approvals")];
    argv.extend_from_slice(&args[2..]);
    Some(argv)
}

/// Classify `fno inbox day start|end`: the argv for `fno-agents day --kind
/// <k> --commit`. Only the two boundary words claim; anything else forwards,
/// where Python answers a group it no longer mounts with its own usage
/// refusal.
pub fn classify_inbox_day(args: &[OsString]) -> Option<Vec<OsString>> {
    if args.len() != 3 || args[0].to_str()? != "inbox" || args[1].to_str()? != "day" {
        return None;
    }
    let kind = args[2].to_str()?;
    if !matches!(kind, "start" | "end") {
        return None;
    }
    Some(vec![
        OsString::from("--kind"),
        OsString::from(kind),
        OsString::from("--commit"),
    ])
}

/// What `fno inbox` prints when no native command claims the words: a help
/// page, or a refusal. The Python `inbox` mount is deleted, so the front
/// answers here and nothing forwards.
#[derive(Debug, PartialEq, Eq)]
pub enum InboxMenu {
    /// A help page. `bare` is true when the caller named no command, which
    /// prints to stderr and exits 2 (the usage convention of a bare group).
    Help { text: String, bare: bool },
    /// A refusal line, printed to stderr with exit 2.
    Refuse(String),
}

const INBOX_ABOUT: &str = "What is waiting on a human: approvals, notify, outstanding \
carve-outs and questions, the lead board, decisions, law, and the user conversation queue.";

const INBOX_COMMANDS: &[(&str, &str)] = &[
    (
        "approvals",
        "Inspect and decide pending approvals for consequential effects.",
    ),
    ("board", "The lead board."),
    ("day", "Mark the start or end of the day: day start|end."),
    ("decide", "Record a decision."),
    ("decisions", "List recorded decisions."),
    ("law", "Set, stage and match standing law."),
    ("notify", "Send the user a notice: notify TITLE MESSAGE."),
    (
        "outstanding",
        "Carve-outs and questions waiting on a human.",
    ),
    (
        "peers",
        "Peer projects, their surfaces and surface globs: peers [--json].",
    ),
    (
        "user",
        "Queue of this session's undispositioned user turns, derived from the transcript.",
    ),
];

const USER_ABOUT: &str = "Queue of this session's undispositioned user turns, derived from the \
transcript by the Rust reader and acked to a per-session ledger under ~/.fno/operator-capture/ \
(the <session>.scan.json scan cursor is a cache; deleting it only forces a full rescan). Machine \
envelopes are refused and counted by name in that reader. Residual hole: raw mail (send --raw) \
reads as a user turn; over-counting is the safe direction there.";

const USER_COMMANDS: &[(&str, &str)] = &[
    (
        "list",
        "Undispositioned user turns, oldest first; --all lists every session's asks with their answer state: list [--all] [--limit N] [--json].",
    ),
    (
        "ack",
        "Dispose one user turn, naming what it produced: ack TURN --outcome OUTCOME [--why TEXT].",
    ),
    (
        "status",
        "Queue depth for this session - the number the capture hook reads: status [--json].",
    ),
];

const APPROVALS_ABOUT: &str = "Inspect and decide pending approvals for consequential effects.";

const APPROVALS_COMMANDS: &[(&str, &str)] = &[
    ("ls", "List approval requests (--pending, --json)."),
    (
        "show",
        "Show every bound field of one request: show DIGEST [--json].",
    ),
    (
        "decide",
        "Decide one exact request: decide DIGEST --as PRINCIPAL (--approve | --decline).",
    ),
];

fn group_page(path: &str, about: &str, commands: &[(&str, &str)]) -> String {
    let width = commands.iter().map(|(n, _)| n.len()).max().unwrap_or(0);
    let mut text = format!("Usage: {path} COMMAND [ARGS]...\n\n  {about}\n\nCommands:\n");
    for (name, line) in commands {
        text.push_str(&format!("  {name:width$}  {line}\n"));
    }
    text
}

fn no_such_command(path: &str, word: &str) -> InboxMenu {
    InboxMenu::Refuse(format!(
        "{path}: no such command '{word}'. `{path} --help` lists the commands."
    ))
}

fn is_help(word: &str) -> bool {
    matches!(word, "-h" | "--help")
}

/// Answer one two-level group (`user`, `approvals`) from its command table.
fn group_menu(path: &str, about: &str, commands: &[(&str, &str)], rest: &[&str]) -> InboxMenu {
    let Some(&first) = rest.first() else {
        return InboxMenu::Help {
            text: group_page(path, about, commands),
            bare: true,
        };
    };
    if is_help(first) {
        return InboxMenu::Help {
            text: group_page(path, about, commands),
            bare: false,
        };
    }
    let Some((name, line)) = commands.iter().find(|(n, _)| *n == first) else {
        return no_such_command(path, first);
    };
    if rest[1..].iter().any(|w| is_help(w)) {
        return InboxMenu::Help {
            text: format!("Usage: {path} {name}\n\n  {line}\n"),
            bare: false,
        };
    }
    InboxMenu::Refuse(format!(
        "{path} {name}: unrecognized arguments. `{path} {name} --help` shows the usage."
    ))
}

/// Answer every `fno inbox ...` argv the native classifiers did not claim.
/// Reached last: a claimed command never gets here, so each branch is a help
/// request or a refusal.
pub fn classify_inbox_menu(args: &[OsString]) -> Option<InboxMenu> {
    if args.first()?.to_str()? != "inbox" {
        return None;
    }
    let words: Vec<String> = args[1..]
        .iter()
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    let words: Vec<&str> = words.iter().map(String::as_str).collect();
    let Some(&first) = words.first() else {
        return Some(InboxMenu::Help {
            text: group_page("fno inbox", INBOX_ABOUT, INBOX_COMMANDS),
            bare: true,
        });
    };
    if is_help(first) {
        return Some(InboxMenu::Help {
            text: group_page("fno inbox", INBOX_ABOUT, INBOX_COMMANDS),
            bare: false,
        });
    }
    Some(match first {
        "user" => group_menu("fno inbox user", USER_ABOUT, USER_COMMANDS, &words[1..]),
        "operator" => group_menu("fno inbox operator", USER_ABOUT, USER_COMMANDS, &words[1..]),
        "approvals" => group_menu(
            "fno inbox approvals",
            APPROVALS_ABOUT,
            APPROVALS_COMMANDS,
            &words[1..],
        ),
        word if INBOX_COMMANDS.iter().any(|(n, _)| *n == word) => InboxMenu::Refuse(format!(
            "fno inbox {word}: unrecognized arguments. `fno inbox --help` lists the commands."
        )),
        word => no_such_command("fno inbox", word),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mk(args: &[&str]) -> Vec<OsString> {
        args.iter().map(OsString::from).collect()
    }

    #[test]
    fn the_user_commands_run_native_and_everything_else_forwards() {
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
                classify_inbox_user(&mk(&["inbox", group, "list", "--all"])),
                Some(mk(&["operator-turns", "--render", "list", "--all"]))
            );
            assert_eq!(
                classify_inbox_user(&mk(&["inbox", group, "list", "--all", "-L", "5"])),
                Some(mk(&[
                    "operator-turns",
                    "--render",
                    "list",
                    "--all",
                    "-L",
                    "5"
                ]))
            );
            assert!(classify_inbox_user(&mk(&["inbox", group, "status", "--all"])).is_none());
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
        assert_eq!(
            classify_inbox_outstanding(&mk(&["inbox", "outstanding"])),
            Some(Vec::new())
        );
        assert_eq!(
            classify_inbox_outstanding(&mk(&["inbox", "outstanding", "-J"])),
            Some(mk(&["-J"]))
        );
        assert_eq!(
            classify_inbox_outstanding(&mk(&["outstanding", "--json"])),
            Some(mk(&["--json"]))
        );
        for sub in [
            "mine",
            "mine do add text",
            "ask",
            "clear",
            "reindex",
            "--help",
            "--bogus",
        ] {
            let words: Vec<&str> = sub.split_whitespace().collect();
            let mut argv = mk(&["inbox", "outstanding"]);
            argv.extend(mk(&words));
            assert!(
                classify_inbox_outstanding(&argv).is_some(),
                "the whole outstanding group claims natively: {sub}"
            );
        }
        assert!(classify_inbox_outstanding(&mk(&["inbox", "law", "status"])).is_none());
        for sub in ["start", "end"] {
            assert_eq!(
                classify_inbox_day(&mk(&["inbox", "day", sub])),
                Some(mk(&["--kind", sub, "--commit"]))
            );
        }
        assert!(classify_inbox_day(&mk(&["inbox", "day"])).is_none());
        assert!(classify_inbox_day(&mk(&["inbox", "day", "nope"])).is_none());
        assert!(classify_inbox_day(&mk(&["inbox", "day", "start", "--json"])).is_none());
    }
}
