//! The lead commands, all answered natively: `fno agents org` history,
//! verdict, faq, init, checkin, cancel, term, done, escalate, drain, `chart
//! shape`, `chart manifest-path`, `chart ledger`, and `fno inbox board`.
//!
//! The agents menu rewrites every `org` spelling to `agents lead <command>`
//! first, so this route claims that argv, the deprecated root `fno lead
//! <command>` (with the move notice the Python shim printed), and `fno inbox
//! board`. Each command execs an argument of an existing `fno-agents` action,
//! because law d-fe66560a bars a new action.

use std::ffi::OsString;
use std::process::Command;

/// One claimed command: its native argv prefix and its `--help` text.
struct Route {
    name: &'static str,
    native: &'static [&'static str],
    help: &'static str,
}

const ROUTES: &[Route] = &[
    Route {
        name: "history",
        native: &["lead-history"],
        help: "Usage: fno agents org history [--scope SCOPE] [--json|-J]\n\n\
               Read this role's recorded term: its check-ins, newest first, verbatim.\n",
    },
    Route {
        name: "verdict",
        native: &["lead-history", "--verdict", "--user-command"],
        help: "Usage: fno agents org verdict [SCOPE]\n\n\
               Read the term tenure verdict. SCOPE defaults to this session's own role.\n",
    },
    Route {
        name: "faq",
        native: &["lead-state", "--faq"],
        help: "Usage: fno agents org faq list --scope SCOPE [--max-entries N] [--paths]\n       \
               fno agents org faq add --question Q --answer A --specimen S --exit E [--scope SCOPE]\n\n\
               Durable answers a promoted session leaves behind.\n",
    },
    Route {
        name: "shape",
        native: &["lead-shape", "--user-command"],
        help: "Usage: fno agents org chart shape pass|team [--scope SCOPE]\n\n\
               Declare this term's shape: a pure pass, or a team holding workers.\n",
    },
    Route {
        name: "manifest-path",
        native: &["lead-state", "--manifest-path"],
        help: "Usage: fno agents org chart manifest-path --harness-session-id ID \
               [--harness H] [--state-root DIR]\n\n\
               Print this live promoted session's existing scope manifest path.\n",
    },
    Route {
        name: "init",
        native: &["lead-state", "--init"],
        help: "Usage: fno agents org init --scope SCOPE [--scope SCOPE...] \
               --harness-session-id ID [--max-iterations N] [--respawn-ceiling N] [--force]\n\n\
               Write this role scope's manifest, which the lead loop arms read.\n",
    },
    Route {
        name: "checkin",
        native: &["lead-checkin", "--user-command"],
        help: "Usage: fno agents org checkin [--scope SCOPE] [--name NAME] [--theme THEME] \
               [--change TEXT] [--no-emit] [--json]\n\n\
               Run the term check-in: gather, print, diff against the last beat, journal.\n",
    },
    Route {
        name: "cancel",
        native: &["lead-state", "--cancel"],
        help: "Usage: fno agents org cancel --scope SCOPE [--clear]\n\n\
               Set or clear the cancel signal beside a role's manifest.\n",
    },
    Route {
        name: "term",
        native: &["lead-state", "--term"],
        help: "Usage: fno agents org term span:<N>[smhd]|compactions:<N> [--reason TEXT] \
               [--scope SCOPE]\n\n\
               Declare or extend this term: the bound past which the Stop hook asks \
               for a handoff or a reason.\n",
    },
    Route {
        name: "done",
        native: &["lead-state", "--done"],
        help: "Usage: fno agents org done [--scope SCOPE]\n\n\
               Expire this role: vacate the row and clear the scope manifest. \
               SCOPE defaults to this session's own role.\n",
    },
    Route {
        name: "escalate",
        native: &["lead-state", "--escalate"],
        help: "Usage: fno agents org escalate [SCOPE] [--stalled IDS] [--reason R]\n\n\
               Escalate to the presiding role or operator with work pending.\n",
    },
    Route {
        name: "drain",
        native: &["lead-state", "--drain"],
        help: "Usage: fno agents org drain SCOPE\n\n\
               Is the role drained: how many scope nodes are not done or superseded.\n",
    },
    Route {
        name: "ledger",
        native: &["lead-rundown", "--ledger"],
        help: "Usage: fno agents org ledger [--out PATH]\n\n\
               Render the term ledger page: every role, its territory, its nodes.\n",
    },
];

const BOARD: Route = Route {
    name: "board",
    native: &["board", "--user-command"],
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

fn claim(route: &Route, tail: &[OsString], notice: Option<String>) -> Claim {
    let help = tail
        .iter()
        .any(|a| matches!(a.to_str(), Some("--help") | Some("-h")));
    let native = (!help).then(|| {
        route
            .native
            .iter()
            .map(OsString::from)
            .chain(tail.iter().cloned())
            .collect()
    });
    Claim {
        notice,
        native,
        help: route.help,
    }
}

/// Claim `agents lead <command>`, `lead <command>` and `inbox board`.
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
        if words[0] == "agents" {
            return None;
        }
        let notice = root_lead.then(|| "fno lead board is now fno inbox board".to_string());
        return Some(claim(&BOARD, tail, notice));
    }
    let route = ROUTES.iter().find(|v| v.name == name)?;
    let notice = root_lead.then(|| "fno lead is now fno agents lead".to_string());
    Some(claim(route, tail, notice))
}

/// Exec the claimed command; stdout, stderr and the exit code pass through.
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
    fn every_org_command_execs_an_argument_of_an_existing_action() {
        let cases: &[(&[&str], &[&str])] = &[
            (
                &["agents", "lead", "history", "--scope", "s"],
                &["lead-history", "--scope", "s"],
            ),
            (
                &["agents", "lead", "verdict", "s"],
                &["lead-history", "--verdict", "--user-command", "s"],
            ),
            (
                &["agents", "lead", "faq", "list", "--scope", "s"],
                &["lead-state", "--faq", "list", "--scope", "s"],
            ),
            (
                &["agents", "lead", "shape", "team"],
                &["lead-shape", "--user-command", "team"],
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
                &["agents", "lead", "init", "--scope", "s"],
                &["lead-state", "--init", "--scope", "s"],
            ),
            (
                &["agents", "lead", "cancel", "--scope", "s"],
                &["lead-state", "--cancel", "--scope", "s"],
            ),
            (
                &["agents", "lead", "term", "span:4h"],
                &["lead-state", "--term", "span:4h"],
            ),
            (
                &["agents", "lead", "checkin", "--no-emit"],
                &["lead-checkin", "--user-command", "--no-emit"],
            ),
            (
                &["inbox", "board", "--json"],
                &["board", "--user-command", "--json"],
            ),
            (
                &["inbox", "board", "--last-run", "--since", "2h"],
                &["board", "--user-command", "--last-run", "--since", "2h"],
            ),
            (
                &["agents", "lead", "done", "--scope", "s"],
                &["lead-state", "--done", "--scope", "s"],
            ),
            (
                &["agents", "lead", "escalate", "s", "--stalled", "x-1"],
                &["lead-state", "--escalate", "s", "--stalled", "x-1"],
            ),
            (
                &["agents", "lead", "drain", "s"],
                &["lead-state", "--drain", "s"],
            ),
            (
                &["agents", "lead", "ledger", "--out", "p"],
                &["lead-rundown", "--ledger", "--out", "p"],
            ),
        ];
        for (argv, want) in cases {
            assert_eq!(native(argv), Some(os(want)), "{argv:?}");
        }
        // Help answers at the door: the Python registrations are gone.
        assert_eq!(native(&["agents", "lead", "faq", "--help"]), None);
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
    fn only_the_inbox_spellings_name_the_board() {
        assert_eq!(classify(&os(&["agents", "lead", "board"])), None);
    }
}
