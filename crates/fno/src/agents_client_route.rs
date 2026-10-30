//! The last `agents` door: a word no earlier door claims execs the fno-agents
//! client with its mapped argv. No Python process starts, except the one family
//! still native there: `agents claim` hands over to the claims CLI.

use std::ffi::OsString;

/// Words the client serves under another argv; every other word keeps its own
/// spelling, and the client refuses one it does not know.
const MAPPED: &[(&str, &[&str])] = &[
    ("capabilities", &["status", "--capabilities"]),
    ("target-family", &["status", "--target-family"]),
    ("register", &["status", "--register"]),
    ("name", &["status", "--name"]),
    ("whoami", &["status", "--whoami"]),
    ("registry-repair", &["status", "--registry-repair"]),
    ("heal-token", &["status", "--heal-token"]),
    ("pane-identity", &["status", "--pane-identity"]),
    ("watch", &["status", "--watch"]),
    ("truth", &["status", "--truth"]),
    ("peek", &["status", "--peek"]),
    ("team", &["status", "--team"]),
    ("role", &["status", "--role"]),
    ("liveness", &["status", "--liveness"]),
    ("top", &["census", "--workers"]),
    ("orphans", &["census", "--orphans"]),
    ("incident", &["fleet-incident"]),
    ("retask", &["rename", "retask"]),
    ("watchdog", &["census", "watchdog"]),
    ("sweep", &["census", "sweep"]),
    ("stale-escalate", &["census", "stale-escalate"]),
    ("friction-escalate", &["census", "friction-escalate"]),
    ("distress-verdicts", &["census", "distress-verdicts"]),
    ("gate-status", &["census", "gate-status"]),
    (
        "codex-session-for-pid",
        &["census", "codex-session-for-pid"],
    ),
    ("spawn-guard", &["census", "spawn-guard"]),
    ("discovered-json", &["census", "discovered-json"]),
];

#[derive(Debug, PartialEq)]
pub enum Route {
    /// The fno-agents argv to exec.
    Client(Vec<OsString>),
    /// The wheel argv to forward.
    Python(Vec<OsString>),
}

/// The route for `agents <word> ...`. A flag-led or bare `agents` is the menu's.
pub fn classify(args: &[OsString]) -> Option<Route> {
    if args.first()?.to_str()? != "agents" {
        return None;
    }
    let word = args.get(1)?.to_str()?;
    if word.starts_with('-') {
        return None;
    }
    let rest = &args[2..];
    if word == "claim" {
        let argv = std::iter::once(OsString::from("claim")).chain(rest.iter().cloned());
        return Some(Route::Python(argv.collect()));
    }
    if word == "harness" && matches!(rest.first().and_then(|a| a.to_str()), Some("--help" | "-h")) {
        return Some(Route::Client(prefixed(&["harness-probe", "--help"], &[])));
    }
    if word == "harness" && rest.first().and_then(|a| a.to_str()) == Some("probe") {
        return Some(Route::Client(prefixed(
            &["harness-probe", "fields"],
            &rest[1..],
        )));
    }
    let mapped = MAPPED.iter().find(|(from, _)| *from == word);
    Some(Route::Client(match mapped {
        Some((_, to)) => prefixed(to, rest),
        None => prefixed(&[word], rest),
    }))
}

fn prefixed(prefix: &[&str], rest: &[OsString]) -> Vec<OsString> {
    prefix
        .iter()
        .map(OsString::from)
        .chain(rest.iter().cloned())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn os(words: &[&str]) -> Vec<OsString> {
        words.iter().map(OsString::from).collect()
    }

    #[test]
    fn every_word_maps_to_the_client_argv_that_serves_it() {
        for (public, client) in [
            (
                &["agents", "truth", "w"][..],
                &["status", "--truth", "w"][..],
            ),
            (
                &["agents", "orphans", "--reap"],
                &["census", "--orphans", "--reap"],
            ),
            (
                &["agents", "incident", "status"],
                &["fleet-incident", "status"],
            ),
            (&["agents", "retask", "w"], &["rename", "retask", "w"]),
            (
                &["agents", "harness", "probe", "x"],
                &["harness-probe", "fields", "x"],
            ),
            (
                &["agents", "watchdog", "--json"],
                &["census", "watchdog", "--json"],
            ),
            (&["agents", "list"], &["list"]),
            (&["agents", "nope"], &["nope"]),
        ] {
            assert_eq!(classify(&os(public)), Some(Route::Client(os(client))));
        }
    }

    #[test]
    fn the_claim_family_stays_in_the_wheel_and_the_menu_keeps_flags() {
        assert_eq!(
            classify(&os(&["agents", "claim", "reap"])),
            Some(Route::Python(os(&["claim", "reap"])))
        );
        assert_eq!(classify(&os(&["agents", "--help"])), None);
        assert_eq!(classify(&os(&["agents"])), None);
        assert_eq!(classify(&os(&["backlog", "list"])), None);
    }
}
