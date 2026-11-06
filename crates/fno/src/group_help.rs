//! The root menu and the `config`, `do` and `doctor` group menus.
//!
//! These pages were the Python CLI's last answers. The root menu reads the
//! live rows of `command-names.tsv`, so it lists exactly what the front door
//! answers and what `fno doctor lint menu-caps` counts. The group menus are
//! plain tables: each word below is claimed by a Rust classifier in
//! `main.rs`, and the classifier's own door prints that command's help.

use std::ffi::OsString;

/// The `config` group's commands.
const CONFIG: &[(&str, &str)] = &[
    ("accounts", "harness accounts, failover and routing"),
    ("active-backlog", "print the backlog this project reads"),
    ("doctor", "check the config files and their paths"),
    ("get", "read a config key"),
    ("history", "the config change log"),
    ("paths", "the resolved state paths"),
    ("plugin", "install and inspect the plugin"),
    ("plugins", "the project's plugin list"),
    ("project", "the project registry"),
    ("route", "provider route lanes"),
    ("schema", "the config schema"),
    ("set", "write a config key"),
    ("setup", "the setup wizard"),
    ("status-sinks", "where status events go"),
    ("unset", "remove a config key"),
];

/// The `do` group's commands.
const DO: &[(&str, &str)] = &[
    ("ci", "CI runs for a PR"),
    ("delivery", "evaluate a plan's delivery evidence"),
    ("diff", "the diff a review reads"),
    ("feedback", "review feedback on a PR"),
    ("phase", "phase handoff artifacts"),
    ("plan", "plan files: path, parse, validate"),
    ("pr", "the PR lifecycle: create, status, wait, merge"),
    ("research", "research briefs"),
    ("resume", "resume receipts"),
    ("review", "the review lane"),
    ("show", "show one revision"),
    ("state", "the target session manifest"),
    ("target", "target sessions: start, init, status"),
    ("think", "the think inspect receipt"),
    ("wizard", "the guided setup flow"),
];

/// The `doctor` group's commands. Bare `fno doctor` prints the health report.
const DOCTOR: &[(&str, &str)] = &[
    ("bash-census", "count Bash calls and permission decisions"),
    ("builds", "build caches and their size"),
    ("bundle", "check skill bundle freshness"),
    ("evals", "golden-task evals"),
    ("event", "the event journal"),
    ("footprint", "fleet CPU and process count"),
    ("harness", "probe one harness"),
    ("harness-matrix", "the harness capability matrix"),
    ("intel", "the intel report"),
    ("lanes", "lane usage"),
    ("lint", "style, menu caps and repo lints"),
    ("observer", "skill evals over recorded sessions"),
    ("reclaim", "reclaim disk from stale state"),
    ("scratch", "sweep or report scratch files"),
    ("skill-diff", "skill drift between installs"),
    ("stalls", "stalled sessions"),
    ("test", "run tests with the worktree's paths"),
    ("update", "update fno"),
];

/// The `do target` group's commands.
const DO_TARGET: &[(&str, &str)] = &[
    ("blast-check", "size a plan's blast radius"),
    ("check-contained", "refuse a node folded into another"),
    ("check-dispatch-hold", "read a plan's dispatch hold"),
    ("check-review-gate", "check the configured reviewers"),
    ("denominator-ratio", "the review denominator ratio"),
    ("init", "write the session manifest and claim the node"),
    ("request-self-review", "request the inline review"),
    ("resolve-model", "the model a stage runs on"),
    ("resolve-owned-identity", "the identity that owns a node"),
    ("review-invocation", "the review command for this run"),
    ("start", "worktree off origin/main, then init"),
    ("status", "the session's state"),
];

fn table(path: &str) -> Option<&'static [(&'static str, &'static str)]> {
    match path {
        "config" => Some(CONFIG),
        "do" => Some(DO),
        "do target" => Some(DO_TARGET),
        "doctor" => Some(DOCTOR),
        _ => None,
    }
}

fn render(head: &str, rows: &[(&str, &str)], foot: &str) -> String {
    let width = rows.iter().map(|r| r.0.len()).max().unwrap_or(0);
    let mut out = format!("{head}\n\nCommands:\n");
    for (name, note) in rows {
        out.push_str(&format!("  {name:<width$}  {note}\n"));
    }
    out.push('\n');
    out.push_str(foot);
    out
}

/// `fno --help`: the live roots with their notes. `all` adds the retired
/// names and where each one went.
pub fn root(all: bool) -> String {
    let live = crate::command_names::live_root_rows();
    let mut text = render(
        "fno: idea to shipped PR.\n\nUsage: fno <command> [args]",
        &live,
        "Run fno <command> --help for a command's own help.\n",
    );
    if all {
        let retired = crate::command_names::retired_root_rows();
        let width = retired.iter().map(|r| r.0.len()).max().unwrap_or(0);
        text.push_str("\nRetired names:\n");
        for (name, now) in retired {
            text.push_str(&format!("  {name:<width$}  {now}\n"));
        }
    } else {
        text.push_str("Run fno --help --all for the retired names.\n");
    }
    text
}

/// The group menu for `fno <group> --help` (and a bare `fno config` or
/// `fno do`), or None when `args` asks for no group menu.
pub fn classify(args: &[OsString]) -> Option<String> {
    let words: Vec<&str> = args.iter().map(|a| a.to_str()).collect::<Option<_>>()?;
    let (path, rest) = match words.as_slice() {
        [a, b, rest @ ..] if table(&format!("{a} {b}")).is_some() => (format!("{a} {b}"), rest),
        [a, rest @ ..] => (a.to_string(), rest),
        [] => return None,
    };
    let rows = table(&path)?;
    let help = match rest {
        [] => path != "doctor",
        ["--help" | "-h"] => true,
        _ => false,
    };
    help.then(|| {
        render(
            &format!("Usage: fno {path} <command> [args]"),
            rows,
            &format!("Run fno {path} <command> --help for a command's own help.\n"),
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn os(words: &[&str]) -> Vec<OsString> {
        words.iter().map(OsString::from).collect()
    }

    #[test]
    fn root_and_group_menus_answer_natively() {
        let menu = root(false);
        for name in crate::command_names::live_roots() {
            assert!(
                menu.contains(&format!("  {name} ")),
                "{name} missing:\n{menu}"
            );
        }
        assert!(!menu.contains("Retired names"));
        assert!(root(true).contains("Retired names"));
        assert!(classify(&os(&["do", "--help"]))
            .unwrap()
            .contains("  target "));
        assert!(classify(&os(&["config"])).unwrap().contains("  set "));
        // Bare `fno doctor` is the health report, not the menu.
        assert!(classify(&os(&["doctor"])).is_none());
        assert!(classify(&os(&["doctor", "-h"])).is_some());
        assert!(classify(&os(&["do", "target", "--help"]))
            .unwrap()
            .contains("  start "));
        assert!(classify(&os(&["do", "target", "start", "--help"])).is_none());
        assert!(classify(&os(&["backlog", "--help"])).is_none());
    }
}
