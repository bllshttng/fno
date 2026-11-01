//! The `fno evals` family's native dispatch: `fno-agents evals <command>`.
//!
//! The Python leaf (cli/src/fno/evals/) forwards every command here; the
//! fold commands route to their existing native owners (evals-trend,
//! evals-macro), and the remainder lands in this module as the leg deletes
//! (the port protocol in docs/architecture/dual-implementation-inventory.md).
//! Python owns transport only.

use serde_json::json;

pub mod bank;
pub mod failure_types;
pub mod grading;
pub mod graduate;
pub mod history;
pub mod research_grade;
pub mod runner;

pub fn run(args: &[String]) -> i32 {
    let Some(sub) = args.first() else {
        eprintln!(
            "usage: fno-agents evals <command> [options] (run | trend | report | macro | export | qualify | graduate | grade | candidates | drafts | gate-retry)"
        );
        return 2;
    };
    let rest = &args[1..];
    match sub.as_str() {
        "trend" => crate::evals_trend::run_evals_trend(&trend_argv(rest, "trend")),
        "report" => crate::evals_trend::run_evals_trend(&trend_argv(rest, "report")),
        "macro" => crate::evals_macro::run_evals_macro(&macro_argv(rest)),
        "grade" => crate::evals::research_grade::run(rest),
        "graduate" => crate::evals::graduate::run(rest),
        "run" => run_command(rest),
        "export" => export_command(rest),
        "qualify" => qualify_command(rest),
        "candidates" => crate::evals_candidates::run(rest),
        "drafts" => crate::evals_drafts::run(rest),
        "gate-retry" => crate::evals_gate::retry(rest),
        // The remainder lands with the port's later commits; the skeleton
        // answers every spelling so the dispatch shape compiles first.
        _ => {
            eprintln!(
                "usage: fno-agents evals <command> [options] (run | trend | report | macro | export | qualify | graduate | grade | candidates | drafts | gate-retry)"
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
        // config evals.stale_days rides along, the same read doctor.py and
        // pr_watch make; an unreadable config reads the 7 the leg used.
        let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
        let stale = crate::paths_cli::cfg_str(&cwd, &["evals", "stale_days"])
            .and_then(|v| v.trim().parse::<i64>().ok())
            .unwrap_or(7);
        argv.push("--stale-days".into());
        argv.push(stale.to_string());
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
/// <state_dir>/history/evals-history.jsonl. Cross-project like the ledger:
/// a per-machine ledger, never a repo-relative artifact.
pub fn history_path() -> std::path::PathBuf {
    // One authority: the same resolution eval_reap and triage_health read -
    // config.paths.evals_history, else <state>/history/evals-history.jsonl
    // (the Python leg's default carried the history/ dir).
    crate::eval_reap::eval_history_path().unwrap_or_else(|| {
        dirs_home()
            .join(".fno")
            .join("history")
            .join("evals-history.jsonl")
    })
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

/// The run command: bank discovery, the discipline gates, the confirm line
/// above 20 total runs, the cohort split validation, the sweep, and the
/// per-task loop. The exit codes are the leaf's: 0 graded, 1 refusal, 2 a
/// load-time discipline violation.
fn run_command(rest: &[String]) -> i32 {
    let repeat: u32 = match flag_lookup(rest, "--repeat")
        .or_else(|| flag_lookup(rest, "-k"))
        .and_then(|v| v.parse().ok())
    {
        Some(k) => k,
        None => 1,
    };
    if repeat < 1 {
        eprintln!("Error: --repeat must be >= 1");
        return 1;
    }
    let variant = flag_lookup(rest, "--variant").unwrap_or_else(|| "baseline".into());
    let ref_arg = flag_lookup(rest, "--ref");
    if !crate::evals::runner::variant_ok(&variant) {
        eprintln!("Error: --variant must be 'baseline' or 'v<N>', got '{variant}'");
        return 1;
    }
    if (variant == "baseline") != ref_arg.is_none() {
        eprintln!("Error: --variant v<N> and --ref REF must be used together");
        return 1;
    }
    let lane_coord = match flag_lookup(rest, "--lane") {
        Some(name) => match crate::evals::runner::resolve_lane(&name) {
            Ok(row) => Some(row),
            Err(e) => {
                eprintln!("Error: {e}");
                return 1;
            }
        },
        None => None,
    };
    let bank_dir = resolve_bank_dir(flag_lookup(rest, "--bank"));
    let tasks = match crate::evals::bank::discover_bank(&bank_dir) {
        Ok(tasks) => tasks,
        Err(e) => {
            if e.0.contains("not found") {
                eprintln!(
                    "Error: no bank at {} (expected evals/bank/*.yaml).",
                    bank_dir.display()
                );
                return 1;
            }
            eprintln!("Error: {e}");
            return 2;
        }
    };
    let task_filter = flag_lookup(rest, "--task");
    let tier_filter = flag_lookup(rest, "--tier");
    let tag_filter = flag_lookup(rest, "--tag");
    let workers: usize = flag_lookup(rest, "--workers")
        .and_then(|v| v.parse().ok())
        .unwrap_or(1)
        .max(1);
    let selected: Vec<_> = tasks
        .iter()
        .filter(|t| {
            task_filter
                .as_deref()
                .map(|f| f.split(',').any(|id| t.id == id))
                .unwrap_or(true)
        })
        .filter(|t| tier_filter.as_deref().map(|f| t.tier == f).unwrap_or(true))
        .filter(|t| {
            tag_filter
                .as_deref()
                .map(|f| t.tags.iter().any(|tag| tag == f))
                .unwrap_or(true)
        })
        .collect();
    if selected.is_empty() {
        eprintln!("Error: selection matched no bank tasks.");
        return 1;
    }
    let mut provider = flag_lookup(rest, "--provider");
    if provider.is_none() && lane_coord.is_none() && selected.iter().any(|t| t.prompt.is_some()) {
        provider = Some("claude".into());
        println!("no --provider given; defaulting prompt-bearing tasks to claude headless");
    }
    let total_runs = selected.len() as u32 * repeat;
    let yes = rest.iter().any(|a| a == "--yes" || a == "-y");
    if total_runs > 20 && !yes {
        let count = selected.len();
        println!("About to run {count} task(s) x {repeat} = {total_runs} live runs.");
        print!("Proceed? [y/N]: ");
        use std::io::Write as _;
        let _ = std::io::stdout().flush();
        let mut answer = String::new();
        let _ = std::io::stdin().read_line(&mut answer);
        let answer = answer.trim().to_lowercase();
        if answer != "y" && answer != "yes" {
            println!("Aborted.");
            return 0;
        }
    }
    let repo_root = canonical_repo_root();
    // The cohort split validates BEFORE any worker call: one native door
    // call (membership, overlap, duplicates, bank rev).
    let cohorts_yaml = bank_dir.join("cohorts.yaml");
    let gate_run = flag_lookup(rest, "--cohort").is_some_and(|c| c.get(..5) == Some("gate-"));
    // The split guards a held-out claim. A run picked by cohort name makes
    // none, and a bank edit on main must not refuse every gate run.
    if cohorts_yaml.exists() && !gate_run {
        match cohort_gate(&bank_dir, &tasks, &repo_root) {
            Ok(line) => println!("{line}"),
            // The refusal already carries the leg's full line shape.
            Err(text) => {
                eprintln!("{text}");
                return 2;
            }
        }
    }
    let swept = crate::evals::runner::sweep_orphans(&repo_root);
    if swept > 0 {
        println!("swept {swept} orphaned eval worktree(s) from a prior run");
    }
    let history = history_path();
    let cohort = flag_lookup(rest, "--cohort");
    let next = std::sync::atomic::AtomicUsize::new(0);
    let failed = std::sync::atomic::AtomicBool::new(false);
    let run_next = || {
        use std::sync::atomic::Ordering::Relaxed;
        while let Some(task) = selected.get(next.fetch_add(1, Relaxed)) {
            let results = crate::evals::runner::run_task(
                task,
                &repo_root,
                repeat,
                &history,
                provider.as_deref(),
                &variant,
                ref_arg.as_deref().unwrap_or(&task.repo_fixture),
                lane_coord.as_ref(),
                cohort.as_deref(),
            );
            let passes = results.iter().filter(|r| r.passed).count();
            let passk = if passes == repeat as usize {
                "PASS"
            } else {
                "FAIL"
            };
            println!(
                "  {:<11} {} [{}]: {}/{} pass  (pass^{}={})",
                task.tier, task.id, variant, passes, repeat, repeat, passk
            );
            for r in &results {
                if !r.passed {
                    println!("      run {}: {}", r.repeat_index, r.reason);
                }
            }
            if passes < repeat as usize {
                failed.store(true, Relaxed);
            }
        }
    };
    std::thread::scope(|scope| {
        for _ in 0..workers {
            scope.spawn(run_next);
        }
    });
    let all_passed = !failed.load(std::sync::atomic::Ordering::Relaxed);
    println!(
        "{}",
        if all_passed {
            "done."
        } else {
            "done (some runs failed; see history)."
        }
    );
    0
}

/// Superseded nodes, from the graph: the plan's own status line is the rung
/// authority's to read, and the node's status is the fact wanted here.
pub(crate) fn superseded_nodes(graph: &std::path::Path) -> std::collections::BTreeSet<String> {
    let query = crate::backlog::RowQuery {
        fields: Some(["id", "status"].into_iter().map(str::to_string).collect()),
        with_blockers: true,
        ..Default::default()
    };
    crate::graph_store::read_rows_where(graph, &query)
        .map(|rows| {
            rows.iter()
                .filter(|n| {
                    n.get("status").and_then(serde_json::Value::as_str) == Some("superseded")
                })
                .filter_map(|n| n.get("id").and_then(serde_json::Value::as_str))
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

/// The cohort split gate: one native door call, the same argv the Python
/// leaf assembled (membership, overlap, duplicates, bank rev).
fn cohort_gate(
    bank_dir: &std::path::Path,
    tasks: &[crate::evals::bank::TaskSpec],
    repo_root: &std::path::Path,
) -> Result<String, String> {
    let ids: Vec<String> = tasks.iter().map(|t| t.id.clone()).collect();
    let known = serde_json::to_string(&ids).unwrap_or_default();
    let exe = std::env::current_exe().unwrap_or_else(|_| std::path::PathBuf::from("fno-agents"));
    let out = std::process::Command::new(exe)
        .args(["evals-attempt"])
        .args(["--cohorts-yaml"])
        .arg(bank_dir.join("cohorts.yaml"))
        .args(["--known-ids", &known])
        .args(["--repo"])
        .arg(repo_root)
        .output()
        .map_err(|e| format!("Error: cohort split refused: native cohort door failed ({e})"))?;
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    let last = stdout.trim().lines().last().unwrap_or("");
    let payload: serde_json::Value = serde_json::from_str(last)
        .map_err(|_| "native cohort door returned unreadable output".to_string())?;
    if payload
        .get("error")
        .and_then(serde_json::Value::as_str)
        .map(|s| !s.is_empty())
        .unwrap_or(false)
    {
        return Err(format!(
            "Error: cohort split refused: {}",
            payload["error"].as_str().unwrap_or_default()
        ));
    }
    if !out.status.success() || payload.get("ok").and_then(serde_json::Value::as_bool) != Some(true)
    {
        // The leg printed one line per door error; the empty-errors
        // fallback names the door's exit code.
        let errs: Vec<String> = payload
            .get("errors")
            .and_then(serde_json::Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str())
                    .map(|e| format!("Error: cohort split refused: {e}"))
                    .collect()
            })
            .unwrap_or_default();
        let lines = if errs.is_empty() {
            vec![format!(
                "Error: cohort split refused: native cohort door failed ({})",
                out.status.code().unwrap_or(-1)
            )]
        } else {
            errs
        };
        return Err(lines.join("\n"));
    }
    let unchanged = payload
        .get("bank_unchanged")
        .and_then(serde_json::Value::as_bool);
    Ok(if unchanged == Some(false) {
        return Err(
            "Error: cohort split is pinned to a bank rev the bank has changed since; redeclare cohorts.yaml against the current bank."
                .into(),
        );
    } else if unchanged == Some(true) {
        "validated cohort split against the current bank".to_string()
    } else {
        "validated cohort split against its pin".to_string()
    })
}

/// One --name value / --name=value token from a raw argv tail.
pub(super) fn flag_lookup(args: &[String], name: &str) -> Option<String> {
    let mut iter = args.iter();
    while let Some(a) = iter.next() {
        if a == name {
            return iter.next().cloned();
        }
        if let Some(v) = a.strip_prefix(&format!("{name}=")) {
            return Some(v.to_string());
        }
    }
    None
}

/// The bank dir: explicit --bank, else <canonical-root>/evals/bank.
pub(crate) fn resolve_bank_dir(explicit: Option<String>) -> std::path::PathBuf {
    if let Some(b) = explicit {
        return std::path::PathBuf::from(b);
    }
    canonical_repo_root().join("evals").join("bank")
}

/// The canonical repo root the bank resolves against: FNO_REPO_ROOT, then
/// the first git worktree-list row, then cwd (the Python resolver's walk).
pub(super) fn canonical_repo_root() -> std::path::PathBuf {
    if let Some(root) = std::env::var_os("FNO_REPO_ROOT") {
        if !root.is_empty() {
            return std::path::PathBuf::from(root);
        }
    }
    let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
    let main_root = std::process::Command::new("git")
        .args(["worktree", "list", "--porcelain"])
        .current_dir(&cwd)
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| {
            String::from_utf8_lossy(&o.stdout)
                .lines()
                .find_map(|l| l.strip_prefix("worktree ").map(str::to_string))
        })
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| cwd.clone());
    main_root
}

/// One attempt-door call: (returncode, last-line JSON). An unreachable
/// binary or an unreadable answer returns (2, {"error": ...}) so every
/// caller refuses closed.
fn attempt_door(flags: &[String], tail: &[String]) -> (i32, serde_json::Value) {
    let exe = std::env::current_exe().unwrap_or_else(|_| std::path::PathBuf::from("fno-agents"));
    let out = std::process::Command::new(exe)
        .arg("evals-attempt")
        .args(flags)
        .args(tail)
        .output();
    let ok_json = |stdout: &str| -> Option<serde_json::Value> {
        stdout
            .trim()
            .lines()
            .last()
            .and_then(|l| serde_json::from_str::<serde_json::Value>(l).ok())
            .filter(|v| v.is_object())
    };
    match out {
        Err(e) => (
            2,
            json!({"error": format!("native cohort door failed: {e}")}),
        ),
        Ok(o) => match ok_json(&String::from_utf8_lossy(&o.stdout)) {
            Some(payload) => (o.status.code().unwrap_or(2), payload),
            None => (
                o.status.code().unwrap_or(2),
                json!({"error": "native cohort door returned unreadable output"}),
            ),
        },
    }
}

/// The shared export/qualify plumbing: resolve the bank, refuse an
/// undeclared split (named per command), and assemble the door tail
/// (declared split, bank-current known ids, rows source).
fn cohort_setup(rest: &[String], bank_dir: &std::path::Path) -> Result<Vec<String>, String> {
    let tasks = crate::evals::bank::discover_bank(bank_dir).map_err(|e| e.0)?;
    let repo_root = canonical_repo_root();
    let ids: Vec<String> = tasks.iter().map(|t| t.id.clone()).collect();
    let known = serde_json::to_string(&ids).unwrap_or_default();
    let history = flag_lookup(rest, "--history")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(history_path);
    Ok(vec![
        "--cohorts-yaml".into(),
        bank_dir.join("cohorts.yaml").display().to_string(),
        "--known-ids".into(),
        known,
        "--repo".into(),
        repo_root.display().to_string(),
        "--rows".into(),
        history.display().to_string(),
    ])
}

/// The export command: train-cohort history for prompt tuning. ONLY train
/// rows, so held-out trajectories never enter a tuning view. Exit 0
/// exported / 1 no declared split / 2 refused or setup error.
fn export_command(rest: &[String]) -> i32 {
    let Some(out_path) = flag_lookup(rest, "--out") else {
        eprintln!("Error: export requires --out FILE");
        return 2;
    };
    let bank_dir = resolve_bank_dir(flag_lookup(rest, "--bank"));
    if !bank_dir.join("cohorts.yaml").exists() {
        eprintln!(
            "Error: no declared cohort split in {} (cohorts.yaml): a tuning export must never be cut from an undeclared bank.",
            bank_dir.display()
        );
        return 1;
    }
    let tail = match cohort_setup(rest, &bank_dir) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("Error: {e}");
            return 2;
        }
    };
    let mut tail = tail;
    tail.push("--out".into());
    tail.push(out_path.clone());
    let (rc, payload) = attempt_door(&["--export-train".to_string()], &tail);
    if payload
        .get("error")
        .and_then(serde_json::Value::as_str)
        .map(|s| !s.is_empty())
        .unwrap_or(false)
    {
        eprintln!("Error: {}", payload["error"].as_str().unwrap_or_default());
        return 2;
    }
    if rc != 0 {
        eprintln!("Error: export door failed ({rc})");
        return 2;
    }
    let exported = payload
        .get("exported")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0);
    let train_tasks = payload
        .get("train_tasks")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0);
    println!(
        "exported {exported} train row(s) across {train_tasks} declared task(s) -> {out_path}"
    );
    0
}

/// The qualify command: aggregate held-out qualification results: counts and
/// coverage only, never a held-out prompt or trace. Reports
/// {"qualified": false, "reason"} (exit 0) when the split is absent,
/// refused, stale, or empty. Exit 2 = setup error.
fn qualify_command(rest: &[String]) -> i32 {
    let bank_dir = resolve_bank_dir(flag_lookup(rest, "--bank"));
    if !bank_dir.join("cohorts.yaml").exists() {
        println!(
            "{}",
            py_compact(
                &json!({"qualified": false, "reason": "no declared cohort split (cohorts.yaml)"})
            )
        );
        return 0;
    }
    let tail = match cohort_setup(rest, &bank_dir) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("Error: {e}");
            return 2;
        }
    };
    let (rc, payload) = attempt_door(&["--aggregate".to_string()], &tail);
    if payload
        .get("error")
        .and_then(serde_json::Value::as_str)
        .map(|s| !s.is_empty())
        .unwrap_or(false)
    {
        eprintln!("Error: {}", payload["error"].as_str().unwrap_or_default());
        return 2;
    }
    if rc != 0 {
        eprintln!("Error: qualify door failed ({rc})");
        return 2;
    }
    println!("{}", py_compact(&payload));
    0
}

/// The Python json.dumps default shape (", " and ": " separators, insertion
/// order): the leaf's echo bytes.
fn py_compact(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::Null => "null".into(),
        serde_json::Value::Bool(b) => b.to_string(),
        serde_json::Value::Number(n) => n.to_string(),
        serde_json::Value::String(s) => serde_json::to_string(s).unwrap_or_default(),
        serde_json::Value::Array(a) => {
            let items: Vec<String> = a.iter().map(py_compact).collect();
            format!("[{}]", items.join(", "))
        }
        serde_json::Value::Object(m) => {
            let items: Vec<String> = m
                .iter()
                .map(|(k, v)| {
                    format!(
                        "{}: {}",
                        serde_json::to_string(k).unwrap_or_default(),
                        py_compact(v)
                    )
                })
                .collect();
            format!("{{{}}}", items.join(", "))
        }
    }
}
