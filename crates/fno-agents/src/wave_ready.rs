//! The /execute dispatch reads behind `fno-agents wave`: `ready` (the
//! work-stealing query a waves round runs before each dispatch),
//! `emit-boundary` (the task_done / blocked boundary emit plus the task-claim
//! settle), `product-gate` (the PRODUCT.md re-check before an impeccable
//! dispatch) and `constraints` (the project constraints a subagent prompt
//! carries). Transport-only, like `wave check|fork|join`.

use serde_json::{json, Map, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::Command;

const BANDS: [&str; 3] = ["low", "medium", "high"];
const PRODUCT_MD_PATHS: [&str; 3] = [
    "PRODUCT.md",
    ".agents/context/PRODUCT.md",
    "docs/PRODUCT.md",
];
const PRODUCT_MD_MIN_BYTES: usize = 200;
const PRODUCT_MD_TODO_RATIO: f64 = 0.25;
/// Boundary outcomes that release the task claim as done.
const TERMINAL_OUTCOMES: [&str; 2] = ["SUCCESS", "DONE_WITH_CONCERNS"];
/// Outcomes that give the task back to pending. Not "": `blocked` always
/// settles as FAILED, so an empty outcome can only come from a `task_done`
/// that omitted --outcome, and giving that just-committed task back would let
/// a peer claim and re-run it.
const GIVEBACK_OUTCOMES: [&str; 2] = ["FAILED", "BLOCKED"];
const SESSION_ENV: [&str; 7] = [
    "TARGET_SESSION_ID",
    "FNO_HARNESS_SESSION_ID",
    "CODEX_THREAD_ID",
    "CODEX_SESSION_ID",
    "CLAUDE_CODE_SESSION_ID",
    "GEMINI_SESSION_ID",
    "OPENCODE_SESSION_ID",
];

#[derive(Debug, Clone)]
pub(crate) struct Wave {
    pub(crate) number: i64,
    pub(crate) tasks: Vec<String>,
    pub(crate) difficulty: String,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct ReadyStrategy {
    pub(crate) waves: Vec<Wave>,
    /// Declared per-task edges. A task with no entry inherits the previous wave.
    pub(crate) blocked_by: BTreeMap<String, Vec<String>>,
    pub(crate) surfaces: BTreeMap<String, Vec<String>>,
}

fn text(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

fn band(value: Option<&Value>, fallback: &str) -> String {
    let named = value.map(text).unwrap_or_default().trim().to_lowercase();
    if BANDS.contains(&named.as_str()) {
        return named;
    }
    if BANDS.contains(&fallback) {
        fallback.to_string()
    } else {
        String::new()
    }
}

/// Present-but-invalid frontmatter fields, one report line each. A missing
/// required field is tolerated (a plan binds its node later) and status drift
/// is `fno do plan reconcile-status`'s job, so neither refuses a run.
fn frontmatter_report(frontmatter: &Value) -> Option<String> {
    let empty = Map::new();
    let fm = frontmatter.as_object().unwrap_or(&empty);
    let lines: Vec<String> = crate::plan_schema::validate_frontmatter(fm)
        .iter()
        .filter(|v| {
            v.get("message").and_then(Value::as_str) != Some("Field required")
                && v.get("field").and_then(Value::as_str) != Some("status")
        })
        .map(|v| {
            let field = v.get("field").map(text).filter(|f| !f.is_empty());
            let message = v.get("message").map(text).unwrap_or_default();
            let got = v.get("got").cloned().unwrap_or(Value::Null);
            format!(
                "  {}: {message} (got {got})",
                field.as_deref().unwrap_or("<root>")
            )
        })
        .collect();
    (!lines.is_empty()).then(|| lines.join("\n"))
}

/// Load the plan's Execution Strategy, or name on stderr why it cannot run.
pub(crate) fn load(plan: &Path) -> Option<ReadyStrategy> {
    let shown = plan.display();
    let doc = match std::fs::metadata(plan) {
        Err(e) => {
            eprintln!("BLOCKED blocked_reason=plan_unreadable: cannot read {shown}: {e}");
            return None;
        }
        Ok(_) => match crate::plan_doc::doc::load_plan(plan) {
            Ok(doc) => doc,
            Err(e) => {
                eprintln!("Error: malformed plan doc (frontmatter): {}", e.message);
                return None;
            }
        },
    };
    if let Some(report) = frontmatter_report(&doc.frontmatter) {
        eprintln!("BLOCKED blocked_reason=plan_frontmatter_invalid: {shown}\n{report}");
        return None;
    }
    let Some(section) = doc.get_section("Execution Strategy") else {
        eprintln!("Warning: No execution strategy section found in {shown}");
        return None;
    };
    let raw = match crate::plan_doc::brief::parse_execution_strategy(section) {
        Ok(raw) => raw,
        Err(e) => {
            eprintln!(
                "Error: malformed Execution Strategy YAML in {shown}: {}",
                e.0
            );
            return None;
        }
    };
    let edge_errors = crate::plan_doc::brief::validate_task_edges(&raw);
    if !edge_errors.is_empty() {
        eprintln!(
            "BLOCKED blocked_reason=plan_task_edges_invalid: {shown}\n{}",
            edge_errors.join("\n")
        );
        return None;
    }
    let strategy = from_raw(&raw, doc.frontmatter.get("difficulty"));
    if strategy.waves.is_empty() {
        eprintln!("Error: No valid waves found in Execution Strategy of {shown}");
        return None;
    }
    Some(strategy)
}

pub(crate) fn from_raw(raw: &Value, plan_difficulty: Option<&Value>) -> ReadyStrategy {
    let plan_band = band(plan_difficulty, "");
    let mut strategy = ReadyStrategy::default();
    for wave in raw
        .get("waves")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        if !wave.is_object() {
            continue;
        }
        let tasks = match wave.get("tasks") {
            Some(Value::Array(items)) => items.iter().map(text).collect(),
            Some(Value::Null) | None => Vec::new(),
            Some(one) => vec![text(one)],
        };
        let fallback = strategy.waves.len() as i64 + 1;
        let number = match wave.get("wave") {
            Some(Value::Number(n)) => n.as_i64().unwrap_or(fallback),
            Some(Value::String(s)) => s.trim().parse().unwrap_or(fallback),
            _ => fallback,
        };
        strategy.waves.push(Wave {
            number,
            tasks,
            difficulty: band(wave.get("difficulty"), &plan_band),
        });
    }
    for task in raw
        .get("tasks")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let id = task.get("id").map(text).unwrap_or_default();
        if id.is_empty() {
            continue;
        }
        let list = |key: &str| -> Vec<String> {
            task.get(key)
                .and_then(Value::as_array)
                .map(|a| a.iter().map(text).collect())
                .unwrap_or_default()
        };
        if task.get("blocked_by_declared").and_then(Value::as_bool) == Some(true) {
            strategy.blocked_by.insert(id.clone(), list("blocked_by"));
        }
        strategy.surfaces.insert(id, list("surface"));
    }
    strategy
}

/// File targets from a `### Task <id>` section's Files list: the fallback for
/// a plan that names ownership only in prose.
fn prose_targets(plan_text: &str, task_id: &str) -> Vec<String> {
    let header = format!("### Task {task_id}");
    let mut section: Vec<&str> = Vec::new();
    let mut inside = false;
    for line in plan_text.lines() {
        if let Some(after) = line.strip_prefix(&header) {
            if after.chars().next().is_none_or(|c| !c.is_ascii_digit()) {
                inside = true;
                section.clear();
                continue;
            }
        }
        if inside && line.starts_with("### Task ") {
            break;
        }
        if inside {
            section.push(line);
        }
    }
    let mut targets = Vec::new();
    let mut collecting = false;
    for raw in section {
        let line = raw.trim();
        if !collecting {
            collecting = [
                "**Files:**",
                "**File:**",
                "Files:",
                "File:",
                "## Files",
                "## File",
            ]
            .iter()
            .any(|p| line.starts_with(p));
            continue;
        }
        if [
            "**Acceptance Criteria",
            "Acceptance Criteria",
            "**Steps:",
            "Steps:",
            "---",
        ]
        .iter()
        .any(|p| line.starts_with(p))
        {
            break;
        }
        if let Some(item) = line.strip_prefix("- ").or_else(|| line.strip_prefix("* ")) {
            let mut item = item;
            for action in ["Create:", "Modify:", "Update:", "Delete:"] {
                if let Some(rest) = item.strip_prefix(action) {
                    item = rest;
                    break;
                }
            }
            let target = item.trim().trim_matches('`');
            if !target.is_empty() {
                targets.push(target.to_string());
            }
        }
    }
    targets
}

fn task_targets(strategy: &ReadyStrategy, plan_text: &str, task_id: &str) -> BTreeSet<String> {
    match strategy.surfaces.get(task_id) {
        Some(surface) if !surface.is_empty() => surface.iter().cloned().collect(),
        _ => prose_targets(plan_text, task_id).into_iter().collect(),
    }
}

/// Derived within-wave edges from the collision partition: inside a group,
/// task N waits on task N-1 in wave order; an unevaluated task waits on
/// every evaluated sibling.
fn partition_edges(
    strategy: &ReadyStrategy,
    plan_text: &str,
    wave: &Wave,
) -> Vec<(String, Vec<String>)> {
    let items: Vec<(String, BTreeSet<String>)> = wave
        .tasks
        .iter()
        .map(|t| (t.clone(), task_targets(strategy, plan_text, t)))
        .collect();
    let (groups, unevaluated) = crate::backlog::collision::partition(
        &items,
        crate::backlog::collision::HIDDEN_SHARED_OUTPUT_ROOTS,
    );
    let mut edges: Vec<(String, Vec<String>)> = Vec::new();
    let mut push = |task: &str, blockers: Vec<String>, replace: bool| match edges
        .iter_mut()
        .find(|(t, _)| t == task)
    {
        Some((_, have)) if replace => *have = blockers,
        Some((_, have)) => have.extend(blockers),
        None => edges.push((task.to_string(), blockers)),
    };
    for group in &groups {
        let ordered: Vec<&String> = wave.tasks.iter().filter(|t| group.contains(*t)).collect();
        for pair in ordered.windows(2) {
            push(pair[1], vec![pair[0].clone()], false);
        }
    }
    let evaluated: Vec<String> = wave
        .tasks
        .iter()
        .filter(|t| !unevaluated.contains(*t))
        .cloned()
        .collect();
    for task in &wave.tasks {
        if unevaluated.contains(task) && !evaluated.is_empty() {
            push(task, evaluated.clone(), true);
        }
    }
    edges
}

/// Union the partition edges into `blocked_by`. A declared list is extended,
/// never replaced; an undeclared task keeps its previous-wave inheritance as
/// the base, so a derived edge never floats it past an unfinished earlier
/// wave. Runs for every wave: `mode` means subagent fan-out inside one
/// worker, not a scheduling switch.
pub(crate) fn apply_partition_edges(strategy: &mut ReadyStrategy, plan_text: &str) {
    for pos in 0..strategy.waves.len() {
        let wave = strategy.waves[pos].clone();
        for (task, blockers) in partition_edges(strategy, plan_text, &wave) {
            let mut base: BTreeSet<String> = match strategy.blocked_by.get(&task) {
                Some(declared) => declared.iter().cloned().collect(),
                None => strategy
                    .waves
                    .iter()
                    .position(|w| w.tasks.contains(&task))
                    .filter(|&p| p > 0)
                    .map(|p| strategy.waves[p - 1].tasks.iter().cloned().collect())
                    .unwrap_or_default(),
            };
            base.extend(blockers);
            strategy.blocked_by.insert(task, base.into_iter().collect());
        }
    }
}

fn effective_blockers(strategy: &ReadyStrategy, task: &str) -> BTreeSet<String> {
    if let Some(declared) = strategy.blocked_by.get(task) {
        return declared.iter().cloned().collect();
    }
    match strategy
        .waves
        .iter()
        .position(|w| w.tasks.iter().any(|t| t == task))
    {
        Some(pos) if pos > 0 => strategy.waves[pos - 1].tasks.iter().cloned().collect(),
        _ => BTreeSet::new(),
    }
}

/// Tasks dispatchable now, in wave then declaration order: not completed,
/// not busy, every effective blocker completed.
pub(crate) fn ready_tasks(
    strategy: &ReadyStrategy,
    completed: &[String],
    busy: &[String],
) -> Vec<String> {
    let done: BTreeSet<&String> = completed.iter().collect();
    let mut ready = Vec::new();
    for wave in &strategy.waves {
        for task in &wave.tasks {
            if done.contains(task) || busy.contains(task) {
                continue;
            }
            if effective_blockers(strategy, task)
                .iter()
                .all(|b| done.contains(b))
            {
                ready.push(task.clone());
            }
        }
    }
    ready
}

/// `- [x] 1.1:` rows from STATE.md (ids like 1.1, 01, 02b).
pub(crate) fn completed_from_state(text: &str) -> Vec<String> {
    text.match_indices("- [x] ")
        .filter_map(|(at, marker)| {
            let rest = &text[at + marker.len()..];
            let digits = rest
                .find(|c: char| !(c.is_ascii_digit() || c == '.'))
                .unwrap_or(rest.len());
            if digits == 0 {
                return None;
            }
            let letters = rest[digits..]
                .find(|c: char| !c.is_ascii_alphabetic())
                .map(|n| digits + n)
                .unwrap_or(rest.len());
            rest[letters..]
                .starts_with(':')
                .then(|| rest[..letters].to_string())
        })
        .collect()
}

fn dedup(items: Vec<String>) -> Vec<String> {
    let mut seen = BTreeSet::new();
    items
        .into_iter()
        .filter(|i| seen.insert(i.clone()))
        .collect()
}

/// One best-effort `fno` read: a missing binary, a timeout or a nonzero exit
/// each print one stderr note and answer None, so a boundary side effect
/// never fails the task or the run.
fn shell_fno(argv: &[&str], what: &str, secs: u64) -> Option<Vec<u8>> {
    let mut cmd = Command::new("fno");
    cmd.args(argv);
    match crate::bounded_cmd::output_with_timeout_result(cmd, secs) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            eprintln!("wave: note: fno unavailable, skipped {what}");
            None
        }
        Err(e) => {
            eprintln!("wave: note: {what} failed (non-fatal): {e}");
            None
        }
        Ok(out) if !out.status.success() => {
            eprintln!(
                "wave: note: {what} rejected (non-fatal): {}",
                String::from_utf8_lossy(&out.stderr).trim()
            );
            None
        }
        Ok(out) => Some(out.stdout),
    }
}

/// One task claim as `(live, holder)`. `None` live means unreadable, which
/// callers keep fail-closed so an unreadable claim never dispatches.
fn claim_state(node: &str, task: &str) -> (Option<bool>, String) {
    let key = format!("task:{node}:{task}");
    let Some(out) = shell_fno(
        &["agents", "claim", "status", &key, "--json"],
        &format!("task claim read for {node}/{task}"),
        15,
    ) else {
        return (None, String::new());
    };
    let Ok(Value::Object(payload)) = serde_json::from_slice::<Value>(&out) else {
        eprintln!("wave: note: unreadable task claim for {node}/{task}; suppressing dispatch");
        return (None, String::new());
    };
    let holder = payload.get("holder").map(text).unwrap_or_default();
    match payload.get("state").and_then(Value::as_str) {
        Some("live" | "suspect") => (Some(true), holder),
        Some("free" | "stale" | "expired" | "dead") => (Some(false), holder),
        other => {
            eprintln!(
                "wave: note: unknown task claim state {other:?} for {node}/{task}; suppressing dispatch"
            );
            (None, holder)
        }
    }
}

struct NodeRows {
    completed: Vec<String>,
    claimed: Vec<String>,
    blocked: Vec<String>,
}

/// Fold the node's task rows: `done` joins completed; a live foreign claim on
/// an `in_progress` row is claimed; a stale or self-owned claim is re-offered;
/// an unknown status never dispatches. An unreadable list degrades to STATE.md.
fn node_rows(node: &str) -> NodeRows {
    let mut rows = NodeRows {
        completed: vec![],
        claimed: vec![],
        blocked: vec![],
    };
    let Some(out) = shell_fno(
        &["backlog", "task", "list", node, "--json"],
        &format!("task list read for node {node}"),
        15,
    ) else {
        return rows;
    };
    let tasks = match serde_json::from_slice::<Value>(&out) {
        Ok(Value::Object(m)) if m.get("tasks").is_some_and(Value::is_array) => {
            m["tasks"].as_array().cloned().unwrap_or_default()
        }
        _ => {
            eprintln!(
                "wave: note: unreadable task rows for node {node}; proceeding with STATE.md only"
            );
            return rows;
        }
    };
    let own: BTreeSet<String> = SESSION_ENV
        .iter()
        .filter_map(|k| std::env::var(k).ok())
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
        .collect();
    for row in tasks.iter().filter(|r| r.is_object()) {
        let id = row.get("id").map(text).unwrap_or_default();
        match row.get("status").and_then(Value::as_str) {
            Some("done") => rows.completed.push(id),
            Some("pending") => {}
            Some("in_progress") => match claim_state(node, &id) {
                (Some(false), _) => {}
                (Some(true), holder)
                    if own.contains(&holder)
                        || own.contains(&row.get("owner").map(text).unwrap_or_default()) => {}
                _ => rows.claimed.push(id),
            },
            other => {
                eprintln!(
                    "wave: note: task {node}/{id} has unsupported status {other:?}; suppressing dispatch"
                );
                rows.blocked.push(id);
            }
        }
    }
    rows
}

/// Split `ready` at the worker's band, highest band first within each half;
/// an unbanded task ranks below every band.
fn band_partition(
    strategy: &ReadyStrategy,
    ready: &[String],
    bands: &Map<String, Value>,
    worker: &str,
) -> (Vec<String>, Vec<String>) {
    let rank = |t: &String| -> i64 {
        let b = bands.get(t).and_then(Value::as_str).unwrap_or("");
        BANDS
            .iter()
            .position(|n| *n == b)
            .map(|p| p as i64)
            .unwrap_or(-1)
    };
    let mine = BANDS.iter().position(|n| *n == worker).unwrap_or(0) as i64;
    let wave_of = |t: &String| {
        strategy
            .waves
            .iter()
            .find(|w| w.tasks.contains(t))
            .map(|w| w.number)
            .unwrap_or(0)
    };
    let mut below: Vec<String> = ready.iter().filter(|t| rank(t) <= mine).cloned().collect();
    let mut above: Vec<String> = ready.iter().filter(|t| rank(t) > mine).cloned().collect();
    for half in [&mut below, &mut above] {
        half.sort_by(|a, b| (-rank(a), wave_of(a), a).cmp(&(-rank(b), wave_of(b), b)));
    }
    (below, above)
}

pub(crate) fn ready_payload(
    strategy: &ReadyStrategy,
    completed: Vec<String>,
    claimed: Vec<String>,
    blocked: Vec<String>,
    worker_band: Option<&str>,
) -> Value {
    let completed = dedup(completed);
    let busy: Vec<String> = claimed.iter().chain(blocked.iter()).cloned().collect();
    let ready = ready_tasks(strategy, &completed, &busy);
    let mut blocked_on = Map::new();
    for wave in &strategy.waves {
        for task in &wave.tasks {
            if completed.contains(task) || busy.contains(task) || ready.contains(task) {
                continue;
            }
            let outstanding: Vec<String> = effective_blockers(strategy, task)
                .into_iter()
                .filter(|b| !completed.contains(b))
                .collect();
            if !outstanding.is_empty() {
                blocked_on.insert(task.clone(), json!(outstanding));
            }
        }
    }
    let mut bands = Map::new();
    for wave in &strategy.waves {
        for task in &wave.tasks {
            bands.insert(task.clone(), json!(wave.difficulty));
        }
    }
    let mut payload = json!({
        "ready": ready,
        "completed": completed,
        "claimed": claimed,
        "blocked": blocked,
        "blocked_on": blocked_on,
        "bands": bands,
    });
    if let Some(worker) = worker_band {
        let (mine, above) = band_partition(strategy, &ready, &bands, worker);
        payload["ready"] = json!(mine);
        payload["above_band"] = json!(above);
    }
    payload
}

fn flag<'a>(args: &'a [String], name: &str) -> Result<Option<&'a str>, String> {
    match args.iter().position(|a| a == name) {
        None => Ok(None),
        Some(at) => args
            .get(at + 1)
            .map(|v| Some(v.as_str()))
            .ok_or_else(|| format!("{name} requires a value")),
    }
}

const READY_USAGE: &str = "usage: fno-agents wave ready <plan> [--state <STATE.md>] [--node <id>] [--band low|medium|high]
  Prints {ready, completed, claimed, blocked, blocked_on, bands} as one JSON object;
  --band splits ready at the worker's band and adds above_band.";

fn run_ready(args: &[String]) -> i32 {
    if args.first().is_none_or(|a| a == "-h" || a == "--help") {
        println!("{READY_USAGE}");
        return if args.is_empty() { 2 } else { 0 };
    }
    let plan = PathBuf::from(&args[0]);
    let (state, node, worker_band) = match (
        flag(args, "--state"),
        flag(args, "--node"),
        flag(args, "--band"),
    ) {
        (Ok(s), Ok(n), Ok(b)) => (s, n, b.map(|b| b.trim().to_lowercase())),
        (Err(e), ..) | (_, Err(e), _) | (.., Err(e)) => {
            eprintln!("Error: {e}");
            return 1;
        }
    };
    if let Some(b) = &worker_band {
        if !BANDS.contains(&b.as_str()) {
            eprintln!("Error: --band must be one of low|medium|high, got {b:?}");
            return 1;
        }
    }
    let Some(mut strategy) = load(&plan) else {
        println!("No execution strategy found in {}", plan.display());
        return 1;
    };
    let plan_text = std::fs::read_to_string(&plan).unwrap_or_default();
    apply_partition_edges(&mut strategy, &plan_text);
    let mut completed = state
        .and_then(|s| std::fs::read_to_string(s).ok())
        .map(|t| completed_from_state(&t))
        .unwrap_or_default();
    let (mut claimed, mut blocked) = (Vec::new(), Vec::new());
    if let Some(node) = node.filter(|n| !n.is_empty()) {
        let rows = node_rows(node);
        completed.extend(rows.completed);
        claimed = dedup(rows.claimed);
        blocked = dedup(rows.blocked);
    }
    println!(
        "{}",
        ready_payload(
            &strategy,
            completed,
            claimed,
            blocked,
            worker_band.as_deref()
        )
    );
    0
}

/// The bound node from the session manifest: `state_path` as given when it
/// resolves, then under `FNO_REPO_ROOT`, then walking up from cwd but never
/// past the repo root, because a walk that leaves the repo finds a
/// stranger's manifest and task ids like 1.1 exist in nearly every plan.
pub(crate) fn manifest_node(cwd: &Path) -> String {
    let given = Path::new(".fno/target-state.md");
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Some(root) = std::env::var_os("FNO_REPO_ROOT").filter(|v| !v.is_empty()) {
        candidates.push(PathBuf::from(root).join(given));
    }
    let root = cwd.ancestors().find(|d| d.join(".git").exists());
    if let Some(root) = root {
        for dir in cwd.ancestors() {
            candidates.push(dir.join(given));
            if dir == root {
                break;
            }
        }
    }
    let Some(found) = candidates.iter().find(|c| c.is_file()) else {
        return String::new();
    };
    let Ok(bytes) = std::fs::read(found) else {
        return String::new();
    };
    let text = String::from_utf8_lossy(&bytes);
    for line in text.lines() {
        if let Some(value) = line.trim().strip_prefix("graph_node_id:") {
            let value = value.trim().trim_matches('"').trim();
            if !value.is_empty() && value != "null" {
                return value.to_string();
            }
        }
    }
    String::new()
}

/// The task status a boundary outcome settles to, or None for a stray spelling: an
/// unknown outcome settles nothing, because mapping strays to the give-back
/// would release a finished task's claim mid-flight.
pub(crate) fn settle_status(outcome: &str) -> Option<&'static str> {
    if TERMINAL_OUTCOMES.contains(&outcome) {
        Some("done")
    } else if GIVEBACK_OUTCOMES.contains(&outcome) {
        Some("pending")
    } else {
        None
    }
}

/// Always exits 0: a boundary emit never fails the caller's task or run.
fn run_emit_boundary(args: &[String]) -> i32 {
    let Some(kind) = args.first() else {
        eprintln!("Error: emit-boundary requires an event type");
        return 0;
    };
    let mut opts: BTreeMap<&str, String> = ["--run", "--node", "--task", "--outcome"]
        .into_iter()
        .map(|k| (k, String::new()))
        .collect();
    let mut data = "{}".to_string();
    let mut i = 1;
    while i < args.len() {
        let key = args[i].as_str();
        if i + 1 < args.len() && (opts.contains_key(key) || key == "--data") {
            if key == "--data" {
                data = args[i + 1].clone();
            } else {
                opts.insert(key, args[i + 1].clone());
            }
            i += 2;
        } else {
            i += 1;
        }
    }
    let data = serde_json::from_str::<Value>(&data).unwrap_or_else(|_| json!({}));
    let mut argv: Vec<String> = ["doctor", "event", "emit", "-t", kind, "-d"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    argv.extend([data.to_string(), "--source".into(), "target".into()]);
    for (flag, value) in &opts {
        if !value.is_empty() {
            argv.extend([flag.to_string(), value.clone()]);
        }
    }
    let argv_ref: Vec<&str> = argv.iter().map(String::as_str).collect();
    let _ = shell_fno(&argv_ref, &format!("{kind} emit"), 15);
    // The boundary every outcome passes through also settles the task claim
    // taken at dispatch. The event type is gated: a typo'd type carrying
    // --task must not reach the give-back and release a live worker's claim.
    let task = &opts["--task"];
    if task.is_empty() || !(kind == "task_done" || kind == "blocked") {
        return 0;
    }
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let node = Some(opts["--node"].clone())
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| manifest_node(&cwd));
    if node.is_empty() {
        eprintln!("wave: note: no node for task {task}; task claim not settled");
        return 0;
    }
    let outcome = if kind == "task_done" {
        opts["--outcome"].as_str()
    } else {
        "FAILED"
    };
    match settle_status(outcome) {
        Some(status) => {
            // A settle is a full graph mutation behind the fleet flock, and a
            // kill mid-settle strands a claim nothing releases: the long bound.
            let _ = shell_fno(
                &["backlog", "task", "update", &node, task, "--status", status],
                "task claim settle",
                120,
            );
        }
        None => eprintln!("wave: note: unknown task outcome {outcome:?}; task claim not settled"),
    }
    0
}

/// Stale: under 200 bytes, or `[TODO]` markers over a quarter of the bytes.
pub(crate) fn product_md_stale(content: &str) -> bool {
    let bytes = content.len();
    if bytes < PRODUCT_MD_MIN_BYTES {
        return true;
    }
    let todo = content.matches("[TODO]").count() * "[TODO]".len();
    todo as f64 / bytes as f64 > PRODUCT_MD_TODO_RATIO
}

/// Exit 0 when PRODUCT.md is present and fresh; else print the `<help>` tag
/// that pauses the loop and exit 1.
fn run_product_gate(args: &[String]) -> i32 {
    let (root, plan, stages) = match (
        flag(args, "--repo"),
        flag(args, "--plan"),
        flag(args, "--stages"),
    ) {
        (Ok(r), Ok(p), Ok(s)) => (r.unwrap_or("."), p.unwrap_or(""), s.unwrap_or("")),
        _ => {
            eprintln!(
                "usage: fno-agents wave product-gate --repo <root> --plan <plan> --stages a,b,c"
            );
            return 2;
        }
    };
    let root = Path::new(root);
    let mut suffix = String::new();
    if let Some(found) = PRODUCT_MD_PATHS
        .iter()
        .map(|r| root.join(r))
        .find(|p| p.exists())
    {
        match std::fs::read(&found).map(String::from_utf8) {
            Ok(Ok(content)) if !product_md_stale(&content) => return 0,
            Ok(Ok(_)) => {}
            Ok(Err(_)) => suffix = ", read_error=UnicodeDecodeError".into(),
            Err(_) => suffix = ", read_error=OSError".into(),
        }
    }
    let stages = stages
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join(", ");
    println!(
        "<help reason=\"missing-product-md\" evidence=\"{plan}, stages: [{stages}]{suffix}\">\nPRODUCT.md required by /impeccable but missing or stale.\nRun /impeccable teach, then resume target.\n</help>"
    );
    1
}

/// The `  constraints:` list under the first settings.yaml that exists
/// (project, then user), as a prompt section; nothing when there is none.
pub(crate) fn constraints_section(settings: &[PathBuf]) -> String {
    let Some(text) = settings
        .iter()
        .find_map(|p| std::fs::read_to_string(p).ok())
    else {
        return String::new();
    };
    let mut items = Vec::new();
    let mut inside = false;
    for line in text.lines() {
        if line.starts_with("  constraints:") {
            inside = true;
            continue;
        }
        let key = line
            .strip_prefix("  ")
            .and_then(|r| r.split_once(':'))
            .map(|(k, _)| k);
        if inside
            && key.is_some_and(|k| {
                !k.is_empty() && k.chars().all(|c| c.is_ascii_alphabetic() || c == '_')
            })
        {
            break;
        }
        if inside {
            if let Some(item) = line.strip_prefix("    - ") {
                items.push(item.trim().trim_matches('"').to_string());
            }
        }
    }
    if items.is_empty() {
        return String::new();
    }
    let mut out = vec![
        String::new(),
        "## Project Constraints".into(),
        String::new(),
    ];
    out.extend(items.into_iter().map(|c| format!("- {c}")));
    out.join("\n")
}

fn run_constraints() -> i32 {
    let mut settings = vec![PathBuf::from(".fno/settings.yaml")];
    if let Some(home) = std::env::var_os("HOME") {
        settings.push(PathBuf::from(home).join(".claude/fno/settings.yaml"));
    }
    let section = constraints_section(&settings);
    if !section.is_empty() {
        println!("{section}");
    }
    0
}

/// `Some(exit)` when `command` is one of this module's leaves.
pub(crate) fn dispatch(command: &str, rest: &[String]) -> Option<i32> {
    match command {
        "ready" => Some(run_ready(rest)),
        "emit-boundary" => Some(run_emit_boundary(rest)),
        "product-gate" => Some(run_product_gate(rest)),
        "constraints" => Some(run_constraints()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strategy(yaml: &str) -> ReadyStrategy {
        let raw = crate::plan_doc::brief::parse_execution_strategy(yaml).unwrap();
        from_raw(&raw, None)
    }

    const THREE_WAVES: &str = "waves:\n  - {wave: 1, tasks: ['1.1', '1.2']}\n  - {wave: 2, tasks: ['2.1']}\n  - {wave: 3, tasks: ['3.1']}\ntasks:\n  - {id: '1.1'}\n  - {id: '1.2'}\n  - {id: '2.1'}\n  - {id: '3.1', blocked_by: ['1.1']}\n";

    #[test]
    fn edgeless_waves_barrier_and_a_declared_edge_floats_its_task() {
        let s = strategy(THREE_WAVES);
        assert_eq!(ready_tasks(&s, &[], &[]), vec!["1.1", "1.2"]);
        // 3.1 declares only 1.1, so it floats past the open 1.2 and 2.1.
        let done = vec!["1.1".to_string()];
        assert_eq!(ready_tasks(&s, &done, &[]), vec!["1.2", "3.1"]);
        let payload = ready_payload(&s, done, vec!["1.2".into()], vec![], None);
        assert_eq!(payload["ready"], json!(["3.1"]));
        assert_eq!(payload["blocked_on"], json!({"2.1": ["1.2"]}));
    }

    #[test]
    fn overlap_serializes_in_wave_order_and_cannot_pass_an_earlier_wave() {
        let yaml = "waves:\n  - {wave: 1, tasks: ['1.1']}\n  - {wave: 2, mode: parallel, tasks: ['2.1', '2.2', '2.3']}\ntasks:\n  - {id: '1.1', surface: ['a.rs']}\n  - {id: '2.1', surface: ['x.rs']}\n  - {id: '2.2', surface: ['x.rs']}\n  - {id: '2.3'}\n";
        let mut s = strategy(yaml);
        apply_partition_edges(&mut s, "");
        assert_eq!(s.blocked_by["2.2"], vec!["1.1", "2.1"]);
        // An unevaluated task waits on every evaluated sibling plus wave 1.
        assert_eq!(s.blocked_by["2.3"], vec!["1.1", "2.1", "2.2"]);
        assert_eq!(ready_tasks(&s, &["1.1".into()], &[]), vec!["2.1"]);
    }

    #[test]
    fn band_splits_the_pull_highest_first() {
        let yaml = "waves:\n  - {wave: 1, difficulty: high, tasks: ['1.1']}\n  - {wave: 2, difficulty: low, tasks: ['2.1']}\ntasks:\n  - {id: '1.1', blocked_by: []}\n  - {id: '2.1', blocked_by: []}\n";
        let s = strategy(yaml);
        let p = ready_payload(&s, vec![], vec![], vec![], Some("medium"));
        assert_eq!(p["ready"], json!(["2.1"]));
        assert_eq!(p["above_band"], json!(["1.1"]));
        assert_eq!(p["bands"], json!({"1.1": "high", "2.1": "low"}));
    }

    #[test]
    fn state_rows_and_prose_targets_parse() {
        let state = "- [x] 1.1: a\n- [x] 02b: b\n- [ ] 3.1: c\n- [x] nope: d\n";
        assert_eq!(completed_from_state(state), vec!["1.1", "02b"]);
        let plan = "### Task 1.1\n**Files:**\n- Modify: `src/a.rs`\n- b.rs\n**Steps:**\n- c.rs\n### Task 1.10\n";
        assert_eq!(prose_targets(plan, "1.1"), vec!["src/a.rs", "b.rs"]);
    }

    #[test]
    fn outcome_vocabulary_is_closed() {
        assert_eq!(settle_status("SUCCESS"), Some("done"));
        assert_eq!(settle_status("DONE_WITH_CONCERNS"), Some("done"));
        assert_eq!(settle_status("BLOCKED"), Some("pending"));
        assert_eq!(settle_status(""), None);
        assert_eq!(settle_status("success"), None);
    }

    #[test]
    fn manifest_walk_stops_at_the_repo_root() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".fno")).unwrap();
        std::fs::write(
            dir.path().join(".fno/target-state.md"),
            "graph_node_id: x-stranger\n",
        )
        .unwrap();
        let repo = dir.path().join("repo/sub");
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::create_dir_all(dir.path().join("repo/.git")).unwrap();
        assert_eq!(manifest_node(&repo), "");
        std::fs::create_dir_all(dir.path().join("repo/.fno")).unwrap();
        std::fs::write(
            dir.path().join("repo/.fno/target-state.md"),
            "graph_node_id: \"x-mine\"\n",
        )
        .unwrap();
        assert_eq!(manifest_node(&repo), "x-mine");
    }

    #[test]
    fn product_md_staleness_and_constraints_section() {
        assert!(product_md_stale("short"));
        assert!(!product_md_stale(&"a".repeat(300)));
        assert!(product_md_stale(&format!(
            "{}{}",
            "[TODO]".repeat(20),
            "a".repeat(200)
        )));
        let dir = tempfile::tempdir().unwrap();
        let settings = dir.path().join("settings.yaml");
        std::fs::write(&settings, "project:\n  constraints:\n    - \"solo founder\"\n    - no new deps\n  vision: x\n    - not this\n").unwrap();
        assert_eq!(
            constraints_section(&[dir.path().join("absent.yaml"), settings]),
            "\n## Project Constraints\n\n- solo founder\n- no new deps"
        );
    }
}
