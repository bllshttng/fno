//! The `fno evals` family's native dispatch: `fno-agents evals <command>`.
//!
//! The Python leaf (cli/src/fno/evals/) forwards every command here; the
//! fold commands route to their existing native owners (evals-trend,
//! evals-macro), and the remainder lands in this module as the leg deletes
//! (the port protocol in docs/architecture/dual-implementation-inventory.md).
//! Python owns transport only.

pub fn run(args: &[String]) -> i32 {
    let Some(sub) = args.first() else {
        eprintln!(
            "usage: fno-agents evals <command> [options] (run | trend | report | macro | export | qualify | graduate | grade)"
        );
        return 2;
    };
    let rest = &args[1..];
    match sub.as_str() {
        "trend" => crate::evals_trend::run_evals_trend(&trend_argv(rest, "trend")),
        "report" => crate::evals_trend::run_evals_trend(&trend_argv(rest, "report")),
        "macro" => crate::evals_arm::run_evals_arm(&macro_argv(rest)),
        // The remainder lands with the port's later commits; the skeleton
        // answers every spelling so the dispatch shape compiles first.
        _ => {
            eprintln!(
                "usage: fno-agents evals <command> [options] (run | trend | report | macro | export | qualify | graduate | grade)"
            );
            return 2;
        }
    }
}

/// The fold commands' default resolution the Python leaf did: the history
/// path and the stale window ride along unless the caller named them.
fn trend_argv(rest: &[String], mode: &str) -> Vec<String> {
    let mut argv: Vec<String> = vec!["--mode".into(), mode.into()];
    if !rest
        .iter()
        .any(|a| a == "--history" || a.starts_with("--history="))
    {
        argv.push("--history".into());
        argv.push(
            crate::evals::history_path()
                .into_os_string()
                .into_string()
                .unwrap_or_default(),
        );
    }
    if !rest
        .iter()
        .any(|a| a == "--stale-days" || a.starts_with("--stale-days="))
    {
        argv.push("--stale-days".into());
        argv.push("7".into());
    }
    argv.extend(rest.iter().cloned());
    argv
}

/// The macro arm takes raw argv; only the journal default rides along when
/// the caller passed no --events.
fn macro_argv(rest: &[String]) -> Vec<String> {
    let mut argv: Vec<String> = Vec::new();
    if !rest
        .iter()
        .any(|a| a == "--events" || a.starts_with("--events="))
    {
        for path in crate::evals::event_journals() {
            argv.push("--events".into());
            argv.push(path.into_os_string().into_string().unwrap_or_default());
        }
    }
    argv.extend(rest.iter().cloned());
    argv
}

/// The eval history path: config.paths.evals_history, else
/// <state_dir>/evals-history.jsonl. Cross-project like the ledger: a
/// per-machine ledger, never a repo-relative artifact.
pub fn history_path() -> std::path::PathBuf {
    // The settings read lives behind the config module once the port's
    // config seam lands; the skeleton resolves the env/default chain.
    if let Ok(carrier) = std::env::var("FNO_STATE_DIR") {
        let mut p = std::path::PathBuf::from(carrier);
        p.push("evals-history.jsonl");
        return p;
    }
    let mut p = dirs_home();
    p.push(".fno");
    p.push("evals-history.jsonl");
    p
}

fn dirs_home() -> std::path::PathBuf {
    std::env::var_os("HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::PathBuf::from("."))
}

/// The event journals the macro fold reads, in the Python leaf's order.
pub fn event_journals() -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    let state = if let Ok(carrier) = std::env::var("FNO_STATE_DIR") {
        std::path::PathBuf::from(carrier)
    } else {
        let mut p = dirs_home();
        p.push(".fno");
        p
    };
    for name in ["events.jsonl", "events.jsonl.ephemeral"] {
        let p = state.join(name);
        if p.exists() {
            out.push(p);
        }
    }
    out
}
