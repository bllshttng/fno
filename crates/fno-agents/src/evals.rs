//! The `fno evals` family's native dispatch: `fno-agents evals <command>`.
//!
//! The Python leaf (cli/src/fno/evals/) forwards every command here; the
//! fold commands route to their existing native owners (evals-trend,
//! evals-macro), and the remainder lands in this module as the leg deletes
//! (the port protocol in docs/architecture/dual-implementation-inventory.md).
//! Python owns transport only.

pub mod research_grade;

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
        "grade" => crate::evals::research_grade::run(rest),
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

/// The event journals the macro fold reads, in the Python leaf's order:
/// the machine journal, the agents-home journal, and the canonical project
/// journal; each contributes its `.ephemeral` sibling when it exists plus
/// that base's numeric rotations (newest first), deduped after resolution.
pub fn event_journals() -> Vec<std::path::PathBuf> {
    let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
    let global = crate::paths_cli::global_events_json(&cwd);
    let agents = agents_home().join("events.jsonl");
    let project = crate::law_match::project_events_journal();
    let mut resolved: Vec<std::path::PathBuf> = Vec::new();
    let mut seen: std::collections::HashSet<std::path::PathBuf> = std::collections::HashSet::new();
    for live in [global, agents, project] {
        let resolved_live = live.canonicalize().unwrap_or_else(|_| absolute(&live));
        let mut bases = vec![resolved_live.clone()];
        let name = resolved_live
            .file_name()
            .map(|n| n.to_os_string())
            .unwrap_or_default();
        let ephemeral =
            resolved_live.with_file_name(format!("{}.ephemeral", name.to_string_lossy()));
        if ephemeral.exists() {
            bases.push(ephemeral);
        }
        for base in bases {
            let mut rotated: Vec<(u64, std::path::PathBuf)> = Vec::new();
            let prefix = format!(
                "{}.",
                base.file_name().unwrap_or_default().to_string_lossy()
            );
            if let Ok(entries) =
                std::fs::read_dir(base.parent().unwrap_or(std::path::Path::new(".")))
            {
                for entry in entries.flatten() {
                    let candidate = entry.path();
                    let candidate_name = candidate
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_default();
                    let Some(suffix) = candidate_name.strip_prefix(&prefix) else {
                        continue;
                    };
                    if suffix.chars().all(|c| c.is_ascii_digit())
                        && !suffix.is_empty()
                        && candidate.exists()
                    {
                        if let Ok(n) = suffix.parse::<u64>() {
                            rotated.push((n, candidate));
                        }
                    }
                }
            }
            rotated.sort_by(|a, b| b.0.cmp(&a.0));
            for (_, path) in &rotated {
                push_unique(&mut resolved, &mut seen, path);
            }
            push_unique(&mut resolved, &mut seen, &base);
        }
    }
    resolved
}

fn push_unique(
    out: &mut Vec<std::path::PathBuf>,
    seen: &mut std::collections::HashSet<std::path::PathBuf>,
    path: &std::path::Path,
) {
    let resolved = path.canonicalize().unwrap_or_else(|_| absolute(path));
    if seen.insert(resolved.clone()) {
        out.push(resolved);
    }
}

fn absolute(path: &std::path::Path) -> std::path::PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| std::path::PathBuf::from("."))
            .join(path)
    }
}

/// The agents runtime home: FNO_AGENTS_HOME, else $HOME/.fno/agents.
fn agents_home() -> std::path::PathBuf {
    if let Some(root) = std::env::var_os("FNO_AGENTS_HOME") {
        if !root.is_empty() {
            return std::path::PathBuf::from(root);
        }
    }
    dirs_home().join(".fno").join("agents")
}
