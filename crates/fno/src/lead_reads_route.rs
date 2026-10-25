//! The lead read verbs answered natively: `fno agents org` history, verdict,
//! faq, `chart shape`, `chart manifest-path`, and `fno inbox board`.
//!
//! The agents menu rewrites every `org` spelling to `agents lead <verb>`
//! first, so this route claims that argv, the deprecated root `fno lead
//! <verb>` (with the move notice the Python shim printed), and `fno inbox
//! board`. Each verb execs an argument of an existing `fno-agents` action,
//! because law d-fe66560a bars a new action. `board --last-run` stays with
//! the Python leg, which shares its predicate with the PR watcher's lead
//! wake: one owner until that caller ports.

use std::ffi::OsString;
use std::process::Command;

/// One claimed verb: its native argv prefix and its `--help` text.
struct Verb {
    name: &'static str,
    native: &'static [&'static str],
    help: &'static str,
}

const VERBS: &[Verb] = &[
    Verb {
        name: "history",
        native: &["lead-history"],
        help: "Usage: fno agents org history [--scope SCOPE] [--json|-J]\n\n\
               Read this role's recorded term: its check-ins, newest first, verbatim.\n",
    },
    Verb {
        name: "verdict",
        native: &["lead-history", "--verdict", "--user-verb"],
        help: "Usage: fno agents org verdict [SCOPE]\n\n\
               Read the term tenure verdict. SCOPE defaults to this session's own role.\n",
    },
    Verb {
        name: "faq",
        native: &["lead-state", "--faq"],
        help: "Usage: fno agents org faq list --scope SCOPE [--max-entries N]\n       \
               fno agents org faq add --question Q --answer A --specimen S --exit E [--scope SCOPE]\n\n\
               Durable answers a promoted session leaves behind.\n",
    },
    Verb {
        name: "shape",
        native: &["lead-shape", "--user-verb"],
        help: "Usage: fno agents org chart shape pass|team [--scope SCOPE]\n\n\
               Declare this term's shape: a pure pass, or a team holding workers.\n",
    },
    Verb {
        name: "manifest-path",
        native: &["lead-state", "--manifest-path"],
        help: "Usage: fno agents org chart manifest-path --harness-session-id ID \
               [--harness H] [--state-root DIR]\n\n\
               Print this live promoted session's existing scope manifest path.\n",
    },
];

const BOARD: Verb = Verb {
    name: "board",
    native: &["board", "--user-verb"],
    help:
        "Usage: fno inbox board [--json|-J] [--max-rows N] [--budget-ms MS] [--state PATH]\n       \
           fno inbox board --last-run [--since WINDOW]\n\n\
           Report every queue that would keep a lead working.\n",
};

/// What the front door does with a claimed argv.
#[derive(Debug, PartialEq, Eq)]
pub struct Claim {
    /// The move notice for a deprecated spelling, printed on stderr.
    pub notice: Option<String>,
    /// The `fno-agents` argv, or None to print `help` and exit 0.
    pub native: Option<Vec<OsString>>,
    pub help: &'static str,
}

fn claim(verb: &Verb, tail: &[OsString], notice: Option<String>) -> Claim {
    let help = tail
        .iter()
        .any(|a| matches!(a.to_str(), Some("--help") | Some("-h")));
    let native = (!help).then(|| {
        verb.native
            .iter()
            .map(OsString::from)
            .chain(tail.iter().cloned())
            .collect()
    });
    Claim {
        notice,
        native,
        help: verb.help,
    }
}

/// Claim `agents lead <verb>`, `lead <verb>` and `inbox board`.
pub fn classify(args: &[OsString]) -> Option<Claim> {
    let words: Vec<&str> = args
        .iter()
        .take(3)
        .map(|a| a.to_str().unwrap_or(""))
        .collect();
    let (root_lead, name, tail) = match words.as_slice() {
        ["agents", "lead", name, ..] => (false, *name, &args[3..]),
        ["inbox", "board", ..] => (false, "board", &args[2..]),
        ["lead", name, ..] => (true, *name, &args[2..]),
        _ => return None,
    };
    if name == "board" {
        // Only the root `lead board` and `inbox board` spellings name it.
        if words[0] == "agents" || tail.iter().any(|a| a.to_str() == Some("--last-run")) {
            return None;
        }
        let notice = root_lead.then(|| "fno lead board is now fno inbox board".to_string());
        return Some(claim(&BOARD, tail, notice));
    }
    let verb = VERBS.iter().find(|v| v.name == name)?;
    let notice = root_lead.then(|| "fno lead is now fno agents lead".to_string());
    Some(claim(verb, tail, notice))
}

/// Exec the claimed verb; stdout, stderr and the exit code pass through.
pub fn run(claim: &Claim) -> i32 {
    if let Some(notice) = &claim.notice {
        eprintln!("{notice}");
    }
    let Some(native) = &claim.native else {
        print!("{}", claim.help);
        return 0;
    };
    match Command::new(crate::digest_overlay::fno_agents_bin())
        .args(native)
        .status()
    {
        Ok(status) => status.code().unwrap_or(1),
        Err(e) => {
            eprintln!(
                "fno agents lead: could not run fno-agents: {e}. Reinstall fno, run \
                 `fno doctor update --rust`, or set FNO_AGENTS_BIN."
            );
            2
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn os(words: &[&str]) -> Vec<OsString> {
        words.iter().map(OsString::from).collect()
    }

    fn native(args: &[&str]) -> Option<Vec<OsString>> {
        classify(&os(args)).and_then(|c| c.native)
    }

    #[test]
    fn every_org_verb_execs_an_argument_of_an_existing_action() {
        let cases: &[(&[&str], &[&str])] = &[
            (
                &["agents", "lead", "history", "--scope", "s"],
                &["lead-history", "--scope", "s"],
            ),
            (
                &["agents", "lead", "verdict", "s"],
                &["lead-history", "--verdict", "--user-verb", "s"],
            ),
            (
                &["agents", "lead", "faq", "list", "--scope", "s"],
                &["lead-state", "--faq", "list", "--scope", "s"],
            ),
            (
                &["agents", "lead", "shape", "team"],
                &["lead-shape", "--user-verb", "team"],
            ),
            (
                &[
                    "agents",
                    "lead",
                    "manifest-path",
                    "--harness-session-id",
                    "x",
                ],
                &["lead-state", "--manifest-path", "--harness-session-id", "x"],
            ),
            (
                &["inbox", "board", "--json"],
                &["board", "--user-verb", "--json"],
            ),
        ];
        for (argv, want) in cases {
            assert_eq!(native(argv), Some(os(want)), "{argv:?}");
        }
    }

    #[test]
    fn the_root_lead_spelling_keeps_its_move_notice() {
        let c = classify(&os(&["lead", "manifest-path", "--harness-session-id", "x"])).unwrap();
        assert_eq!(c.notice.as_deref(), Some("fno lead is now fno agents lead"));
        let c = classify(&os(&["lead", "board"])).unwrap();
        assert_eq!(
            c.notice.as_deref(),
            Some("fno lead board is now fno inbox board")
        );
        assert_eq!(
            classify(&os(&["agents", "lead", "history"]))
                .unwrap()
                .notice,
            None
        );
    }

    #[test]
    fn the_python_owned_spellings_stay_unclaimed() {
        // --last-run shares its predicate with the PR watcher's lead wake.
        assert_eq!(classify(&os(&["inbox", "board", "--last-run"])), None);
        // The lifecycle verbs are still Python's.
        for verb in [
            "init", "checkin", "done", "cancel", "escalate", "drain", "term",
        ] {
            assert_eq!(classify(&os(&["agents", "lead", verb])), None, "{verb}");
        }
        assert_eq!(classify(&os(&["agents", "lead", "board"])), None);
    }

    #[test]
    fn help_answers_here_without_an_exec() {
        let c = classify(&os(&["agents", "lead", "faq", "--help"])).unwrap();
        assert_eq!(c.native, None);
        assert!(c.help.starts_with("Usage: fno agents org faq list"));
    }
}
