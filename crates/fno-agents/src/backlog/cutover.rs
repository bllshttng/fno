//! The wave-4 cutover arms: catalog commands whose Python bodies were thin
//! clients of native machinery, served here with no compat forward. Each arm
//! mirrors the deleted wheel body's exits and lines (the parity is the
//! deleted file, so the renders quote it).
//!
//! Covered here: `version`, `status`, `cost`, `demand`, `collisions`,
//! `backfill-deferred-kind`, `unclaim`, `defer`, `undefer`, `retract`,
//! `unsupersede`, `relatedness`, and the completed one-shot
//! `archive-dedupe-ids`. Heavier bodies (decompose, intake, capture, batch,
//! task, provenance reads, the rich done surface, requeue, retro, carveout,
//! pick, discover, ready, lanes, stuck-epics, reprioritize, remove, project-
//! root, epic status, reconcile-findings) keep their documented dispositions
//! in the dispatcher until their own arms land.

use serde_json::{json, Map, Value};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use super::node_ref::{find_node, has_node_id_prefix};
use super::patch::{classify_deferred_reason, DEFERRED_KINDS};
use super::settings;
use super::workflows::{refuse_non_graph_backend, text_at};
use crate::graph_store;

// ---------------------------------------------------------------------------
// version
// ---------------------------------------------------------------------------

const VERSION_HELP: &str = "Usage: fno backlog version

Print the store's mutation counter: it grows one per write.
";

pub fn run_version(tail: &[String]) -> i32 {
    if tail.iter().any(|a| a == "--help" || a == "-h") {
        print!("{VERSION_HELP}");
        return 0;
    }
    if let Some(code) = refuse_non_graph_backend("version") {
        return code;
    }
    let store = super::api::Store::new(&settings::graph_path());
    match super::api::version(&store) {
        Ok(v) => {
            println!("{v}");
            0
        }
        Err(e) => {
            eprintln!("Error: {}", e.0);
            1
        }
    }
}

// ---------------------------------------------------------------------------
// status - the per-project progress summary (the cmd_status render)
// ---------------------------------------------------------------------------

const STATUS_HELP: &str = "Usage: fno backlog status [--project NAME] [--all] [--roadmap-id ID]

Per-project progress summary over the working graph: done/claimed/ready/idea
counts for feature nodes, plus a cost column and PR link per row.

Options:
  --project, -p NAME  Restrict to one project
  --all, -A           Show every project
  --roadmap-id ID     Restrict to one roadmap
  -h, --help          Print help
";

pub fn run_status(tail: &[String]) -> i32 {
    let mut project: Option<String> = None;
    let mut all = false;
    let mut roadmap_id: Option<String> = None;
    let mut it = tail.iter();
    while let Some(arg) = it.next() {
        let (flag, inline) = match arg.split_once('=') {
            Some((f, v)) if f.starts_with("--") => (f, Some(v.to_string())),
            _ => (arg.as_str(), None),
        };
        match flag {
            "-h" | "--help" => {
                print!("{STATUS_HELP}");
                return 0;
            }
            "--project" | "-p" => {
                project = Some(inline.or_else(|| it.next().cloned()).unwrap_or_default());
            }
            "--all" | "-A" => all = true,
            "--roadmap-id" => {
                roadmap_id = Some(inline.or_else(|| it.next().cloned()).unwrap_or_default());
            }
            other if other.starts_with('-') && other.len() > 1 => {
                eprintln!("No such option: {other}");
                return 2;
            }
            _ => {
                eprintln!("Got unexpected extra argument ({arg})");
                return 2;
            }
        }
    }
    if let Some(code) = refuse_non_graph_backend("status") {
        return code;
    }
    let graph = settings::graph_path();
    let Ok(rows) = graph_store::read_rows(&graph) else {
        eprintln!("Error: the backlog graph could not be read");
        return 1;
    };
    // The display read hides archived residents (wire_rows' default read).
    let entries: Vec<&Value> = rows.iter().filter(|e| !truthy(e, "archived_at")).collect();
    if entries.is_empty() {
        println!("No graph entries found. Run /megawalk vision.md to generate a roadmap.");
        return 0;
    }
    if let Some(rid) = &roadmap_id {
        let filtered: Vec<&Value> = entries
            .iter()
            .copied()
            .filter(|e| text_at(e, "roadmap_id") == Some(rid.as_str()))
            .collect();
        return render_status(&filtered, project.as_deref(), all);
    }
    render_status(&entries, project.as_deref(), all)
}

fn render_status(entries: &[&Value], project: Option<&str>, all: bool) -> i32 {
    // Group rows per the Python fold: one named project, every project, or
    // the detected project, falling back to one "(all)" bucket.
    let mut buckets: Vec<(String, Vec<&Value>)> = Vec::new();
    match project {
        Some(p) => buckets.push((
            p.to_string(),
            entries
                .iter()
                .copied()
                .filter(|e| text_at(e, "project") == Some(p))
                .collect(),
        )),
        None if all => {
            let mut by: BTreeMap<String, Vec<&Value>> = BTreeMap::new();
            for e in entries {
                let name = text_at(e, "project").unwrap_or("(no project)").to_string();
                by.entry(name).or_default().push(e);
            }
            buckets.extend(by);
        }
        None => {
            // No --project/--all: the deleted wheel detected the caller's
            // project from node cwd fields (graph/_intake.detect_project);
            // the first row's project value is row order, not detection.
            let owned: Vec<Value> = entries.iter().map(|e| (*e).clone()).collect();
            match super::triage::detect_project(&owned) {
                Some(p) => {
                    let detected: Vec<&Value> = entries
                        .iter()
                        .copied()
                        .filter(|e| text_at(e, "project") == Some(p.as_str()))
                        .collect();
                    buckets.push((p, detected));
                }
                None => buckets.push(("(all)".to_string(), entries.to_vec())),
            }
        }
    }
    let mut global_done = 0usize;
    let mut global_total = 0usize;
    let mut global_cost = 0.0f64;
    for (name, rows) in buckets.iter() {
        let features: Vec<&Value> = rows
            .iter()
            .copied()
            .filter(|e| text_at(e, "type") == Some("feature"))
            .collect();
        let count = |status: &str| {
            features
                .iter()
                .filter(|e| text_at(e, "status") == Some(status))
                .count()
        };
        let done = count("done");
        let claimed = count("in_progress");
        let ready = count("ready");
        let ideas = count("idea");
        let blocked = count("blocked");
        let deferred = count("deferred");
        let total = features.len();
        let cost: f64 = features
            .iter()
            .filter_map(|e| e.get("cost_usd").and_then(Value::as_f64))
            .sum();
        global_done += done;
        global_total += total;
        global_cost += cost;
        if all {
            let ideas_suffix = if ideas > 0 {
                format!(", ideas: {ideas}")
            } else {
                String::new()
            };
            let deferred_suffix = if deferred > 0 {
                format!(", deferred: {deferred}")
            } else {
                String::new()
            };
            println!(
                "\n=== {name} ({done}/{total} done{ideas_suffix}{deferred_suffix}, ${cost:.2}) ==="
            );
        } else {
            println!("Project: {name}");
            let roadmaps: Vec<&Value> = rows
                .iter()
                .copied()
                .filter(|e| text_at(e, "type") == Some("roadmap"))
                .collect();
            if let Some(r) = roadmaps.first() {
                let rid = text_at(r, "roadmap_id").unwrap_or("?");
                let rtitle = text_at(r, "title").unwrap_or("?");
                println!("Roadmap: {rid} ({rtitle})");
            }
            let ideas_suffix = if ideas > 0 {
                format!(" | ideas: {ideas} (use 'fno backlog ready --ideas' to list)")
            } else {
                String::new()
            };
            let deferred_suffix = if deferred > 0 {
                format!(
                    " | deferred: {deferred} (use 'fno backlog ready --include-deferred' to list)"
                )
            } else {
                String::new()
            };
            println!(
                "Progress: {done}/{total} done | {claimed} claimed | {ready} ready{ideas_suffix} | {blocked} blocked{deferred_suffix}"
            );
            println!("Cost: ${cost:.2}");
            println!();
        }
        println!(
            "{:<14} {:<30} {:<10} {:<10} {:>8}  {}",
            "ID", "Title", "Status", "Priority", "Cost", "PR"
        );
        println!("{}", "-".repeat(85));
        for e in &features {
            let id = text_at(e, "id").unwrap_or("?");
            let title: String = text_at(e, "title")
                .unwrap_or("?")
                .chars()
                .take(28)
                .collect();
            let st = text_at(e, "status").unwrap_or("?");
            let pri = text_at(e, "priority").unwrap_or("?");
            let c = e.get("cost_usd").and_then(Value::as_f64).unwrap_or(0.0);
            let pr = match e.get("pr_number").and_then(Value::as_i64) {
                Some(n) => format!("#{n}"),
                None => "-".to_string(),
            };
            println!(
                "{:<14} {:<30} {:<10} {:<10} {:>8.2}  {}",
                id, title, st, pri, c, pr
            );
        }
    }
    if all && buckets.len() > 1 {
        println!("\nTotal: {global_done}/{global_total} done, ${global_cost:.2}");
    }
    0
}

// ---------------------------------------------------------------------------
// cost - record one session's cost (a level, not an increment)
// ---------------------------------------------------------------------------

const COST_HELP: &str = "Usage: fno backlog cost TASK_ID --session-id ID --amount N

Record one session's cost on a node. A session's cost is a LEVEL that gets
re-reported, so the row for --session-id is replaced in place; cost_usd is
re-derived from the rows, never incremented.

Options:
  --session-id ID  Run id owning this cost.  [required]
  --amount N       Cost in USD.  [required]
  -h, --help       Print help
";

pub fn run_cost(tail: &[String]) -> i32 {
    let mut task_id: Option<String> = None;
    let mut session: Option<String> = None;
    let mut amount: Option<String> = None;
    let mut it = tail.iter();
    while let Some(arg) = it.next() {
        let (flag, inline) = match arg.split_once('=') {
            Some((f, v)) if f.starts_with("--") => (f, Some(v.to_string())),
            _ => (arg.as_str(), None),
        };
        match flag {
            "-h" | "--help" => {
                print!("{COST_HELP}");
                return 0;
            }
            "--session-id" | "--session" => {
                session = Some(inline.or_else(|| it.next().cloned()).unwrap_or_default());
            }
            "--amount" => {
                amount = Some(inline.or_else(|| it.next().cloned()).unwrap_or_default());
            }
            other if other.starts_with('-') && other.len() > 1 => {
                eprintln!("No such option: {other}");
                return 2;
            }
            _ if task_id.is_none() => task_id = Some(arg.clone()),
            _ => {
                eprintln!("Got unexpected extra argument ({arg})");
                return 2;
            }
        }
    }
    let Some(task_id) = task_id else {
        eprintln!("Missing argument 'TASK_ID'.");
        return 2;
    };
    let Some(session) = session else {
        eprintln!("Missing option '--session-id'.");
        return 2;
    };
    let Some(amount) = amount else {
        eprintln!("Missing option '--amount'.");
        return 2;
    };
    if !has_node_id_prefix(&task_id) {
        eprintln!("Error: task_id must be a <prefix>-<4..8 hex> node id, got '{task_id}'");
        return 1;
    }
    let Ok(amount_f) = amount.parse::<f64>() else {
        eprintln!("Error: amount must be a number, got '{amount}'");
        return 1;
    };
    if let Some(code) = refuse_non_graph_backend("cost") {
        return code;
    }
    let graph = settings::graph_path();
    let found: std::cell::RefCell<Option<String>> = std::cell::RefCell::new(None);
    let applied = graph_store::mutate_rows(
        &graph,
        std::time::Duration::from_secs(5),
        None,
        None,
        |working| {
            let resolved = find_node(working, &task_id).map(|n| {
                text_at(n, "id")
                    .map(String::from)
                    .unwrap_or_else(|| task_id.clone())
            });
            let Some(resolved) = resolved else {
                return Err(graph_store::StoreError::Invalid(format!(
                    "feature {task_id} not found"
                )));
            };
            let node = working
                .iter_mut()
                .find(|e| text_at(e, "id").as_deref() == Some(resolved.as_str()))
                .expect("resolved row exists");
            upsert_cost_session(node, &session, amount_f);
            *found.borrow_mut() = Some(resolved);
            Ok(true)
        },
    );
    if let Err(err) = applied {
        eprintln!("Error: {err}");
        return 1;
    }
    if found.borrow().is_none() {
        eprintln!("Error: feature {task_id} not found");
        return 1;
    }
    println!("Recorded ${amount_f:.2} for {task_id} (session {session})");
    0
}

/// The cost-row upsert (the wheel's upsert_cost_session): one row per
/// session, replaced in place; cost_usd re-derived at the one stored
/// precision.
fn upsert_cost_session(node: &mut Value, session_id: &str, cost_usd: f64) {
    let obj = node.as_object_mut().expect("row is an object");
    let rows: Vec<Value> = obj
        .get("cost_sessions")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut kept: Vec<Value> = Vec::new();
    for row in rows {
        let matches = row
            .get("session_id")
            .and_then(Value::as_str)
            .map(|s| s == session_id)
            .unwrap_or(false);
        if !matches {
            kept.push(row);
        }
    }
    kept.push(json!({
        "session_id": session_id,
        "cost_usd": round4(cost_usd),
        "timestamp": graph_store::now_isoformat(),
    }));
    let total: f64 = kept
        .iter()
        .filter_map(|r| r.get("cost_usd").and_then(Value::as_f64))
        .sum();
    obj.insert("cost_sessions".into(), Value::Array(kept));
    obj.insert("cost_usd".into(), json!(round4(total)));
}

/// The one stored precision (the wheel's _COST_NDIGITS): a per-caller
/// precision made one node's total depend on which surface wrote it last.
fn round4(x: f64) -> f64 {
    (x * 10_000.0).round() / 10_000.0
}

// ---------------------------------------------------------------------------
// demand - where agent encounters and operator priority disagree
// ---------------------------------------------------------------------------

const DEMAND_HELP: &str = "Usage: fno backlog demand [--json]

Show where agent encounters and operator priority DISAGREE. One row per node
carrying at least one encounter, highest divergence first.

Options:
  -J, --json  Emit the rows as JSON.
  -h, --help  Print help
";

const DEMAND_EMPTY: &str = "no encounters recorded yet. An agent files one with `fno backlog encounter <node> --evidence \"<what it cost>\"`.";

pub fn run_demand(tail: &[String]) -> i32 {
    let json_out = tail.iter().any(|a| a == "--json" || a == "-J");
    if tail.iter().any(|a| a == "--help" || a == "-h") {
        print!("{DEMAND_HELP}");
        return 0;
    }
    // An encounter is footnote-minted metadata that lives only in the graph,
    // and `encounter` is already refused under an external backend.
    if let Some(code) = refuse_non_graph_backend("demand") {
        return code;
    }
    let graph = settings::graph_path();
    let Ok(rows) = graph_store::read_rows(&graph) else {
        eprintln!("Error: the backlog graph could not be read");
        return 1;
    };
    let entries: Vec<&Value> = rows.iter().filter(|e| !truthy(e, "archived_at")).collect();
    let board = super::projections::Board::new(
        &rows
            .iter()
            .filter(|e| !truthy(e, "archived_at"))
            .cloned()
            .collect::<Vec<_>>(),
        &std::collections::BTreeSet::new(),
    );
    let table = demand_rows(&entries, &board);
    if json_out {
        println!(
            "{}",
            serde_json::to_string(&table).unwrap_or_else(|_| "[]".into())
        );
        return 0;
    }
    if table.is_empty() {
        println!("{DEMAND_EMPTY}");
        return 0;
    }
    let enc_width = 12;
    println!(
        "{:>5}  {:<8} {:<4} {:<width$} {:>10}  {:<12} title",
        "score",
        "node",
        "pri",
        "enc",
        "dispatched",
        "status",
        width = enc_width
    );
    for row in &table {
        let split = if row.operator > 0 {
            format!(" ({}a/{}o)", row.agent, row.operator)
        } else {
            String::new()
        };
        let enc_cell = format!("{}{}", row.enc, split);
        println!(
            "{:>5}  {:<8} {:<4} {:<width$} {:>10}  {:<12} {}",
            row.score,
            row.node,
            row.pri,
            enc_cell,
            row.dispatched,
            row.status,
            row.title,
            width = enc_width
        );
    }
    0
}

#[derive(serde::Serialize)]
struct DemandRow {
    score: i64,
    node: String,
    pri: String,
    enc: usize,
    agent: usize,
    operator: usize,
    dispatched: usize,
    status: String,
    title: String,
}

fn truthy(row: &Value, key: &str) -> bool {
    match row.get(key) {
        Some(Value::Bool(b)) => *b,
        Some(Value::String(s)) => !s.is_empty() && s != "null" && s != "None" && s != "False",
        Some(Value::Null) | None => false,
        Some(_) => true,
    }
}

/// The demand fold (the deleted demand_rows): encounter weight AGAINST
/// operator priority, ties on node id, no decay, no normalization.
fn demand_rows(entries: &[&Value], board: &super::projections::Board) -> Vec<DemandRow> {
    let mut out: Vec<DemandRow> = Vec::new();
    for e in entries {
        let voters = encounter_voters(e);
        if voters.is_empty() {
            continue;
        }
        let operators = operator_voters(e);
        let id = text_at(e, "id").unwrap_or_default();
        let pri = board
            .effective()
            .get(id)
            .cloned()
            .unwrap_or_else(|| text_at(e, "priority").unwrap_or("p2").to_string());
        let weight = priority_weight(&pri);
        let dispatched = session_ids(e);
        let count = voters.len();
        out.push(DemandRow {
            score: divergence_score(e, weight, count),
            node: id.to_string(),
            pri,
            enc: count,
            agent: voters.difference(&operators).count(),
            operator: voters.intersection(&operators).count(),
            dispatched: voters.intersection(&dispatched).count(),
            status: text_at(e, "status").unwrap_or_default().to_string(),
            title: text_at(e, "title").unwrap_or_default().to_string(),
        });
    }
    out.sort_by(|a, b| b.score.cmp(&a.score).then(a.node.cmp(&b.node)));
    out
}

fn priority_weight(pri: &str) -> i64 {
    match pri {
        "p0" => 1,
        "p1" => 2,
        "p3" => 4,
        _ => 3,
    }
}

fn divergence_score(entry: &Value, weight: i64, voters: usize) -> i64 {
    let mut weight = weight;
    if !truthy(entry, "sessions") && !truthy(entry, "pr_number") {
        weight *= 2;
    }
    voters as i64 * weight
}

fn encounter_voters(entry: &Value) -> std::collections::BTreeSet<String> {
    let mut out = std::collections::BTreeSet::new();
    for r in entry
        .get("encounters")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let key = r
            .get("voter_key")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .or_else(|| r.get("session_id").and_then(Value::as_str))
            .unwrap_or("");
        if !key.is_empty() {
            out.insert(key.to_string());
        }
    }
    out
}

fn operator_voters(entry: &Value) -> std::collections::BTreeSet<String> {
    let mut out = std::collections::BTreeSet::new();
    for r in entry
        .get("encounters")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let kind = r.get("voter_kind").and_then(Value::as_str);
        if kind != Some("operator") {
            continue;
        }
        let key = r
            .get("voter_key")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .or_else(|| r.get("session_id").and_then(Value::as_str))
            .unwrap_or("");
        if !key.is_empty() {
            out.insert(key.to_string());
        }
    }
    out
}

fn session_ids(entry: &Value) -> std::collections::BTreeSet<String> {
    entry
        .get("sessions")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|r| r.get("session_id").and_then(Value::as_str))
        .filter(|s| !s.is_empty())
        .map(String::from)
        .collect()
}

// ---------------------------------------------------------------------------
// collisions check
// ---------------------------------------------------------------------------

const COLLISIONS_HELP: &str =
    "Usage: fno backlog collisions check PLAN_PATH [--self-id ID] [--json]

Check a plan against all pending nodes for file collisions.

Options:
  --self-id ID  Skip this node ID when comparing
  -J, --json    Emit structured JSON instead of human text
  -h, --help    Print help
";

pub fn run_collisions(tail: &[String]) -> i32 {
    let mut plan_path: Option<String> = None;
    let mut self_id: Option<String> = None;
    let mut json_out = false;
    // The help names the `check` action word; without this it parsed as
    // the plan path and the real path read as an extra argument.
    let tail = match tail.split_first() {
        Some((action, rest)) if action == "check" => rest,
        _ => tail,
    };
    let mut it = tail.iter();
    while let Some(arg) = it.next() {
        let (flag, inline) = match arg.split_once('=') {
            Some((f, v)) if f.starts_with("--") => (f, Some(v.to_string())),
            _ => (arg.as_str(), None),
        };
        match flag {
            "-h" | "--help" => {
                print!("{COLLISIONS_HELP}");
                return 0;
            }
            "--self-id" => {
                self_id = Some(inline.or_else(|| it.next().cloned()).unwrap_or_default());
            }
            "-J" | "--json" => json_out = true,
            other if other.starts_with('-') && other.len() > 1 => {
                eprintln!("No such option: {other}");
                return 2;
            }
            _ if plan_path.is_none() => plan_path = Some(arg.clone()),
            _ => {
                eprintln!("Got unexpected extra argument ({arg})");
                return 2;
            }
        }
    }
    let Some(plan_path) = plan_path else {
        eprintln!("Missing argument 'PLAN_PATH'.");
        return 2;
    };
    if let Some(code) = refuse_non_graph_backend("collisions") {
        return code;
    }
    let plan = PathBuf::from(&plan_path);
    let graph = settings::graph_path();
    let Ok(rows) = graph_store::read_rows(&graph) else {
        eprintln!("Error: the backlog graph could not be read");
        return 1;
    };
    let entries: Vec<Value> = rows
        .into_iter()
        .filter(|e| !truthy(e, "archived_at"))
        .collect();
    let evaluated = super::collision::has_file_surface(&plan);
    // The wheel's detector inferred the self-exclusion when --self-id was
    // omitted: a plan already intaked against one node would otherwise
    // collide with that node's own plan at HIGH, the severity meant to stop
    // an operator. An explicit flag (even empty) suppresses the inference.
    let self_effective: Option<String> = match self_id {
        Some(id) => Some(id),
        None => infer_collisions_self_id(&plan, &entries),
    };
    let thresholds = collision_thresholds();
    let collisions = if evaluated {
        super::collision::find_collisions(
            &plan,
            &entries,
            self_effective.as_deref().unwrap_or(""),
            &thresholds,
        )
    } else {
        Vec::new()
    };
    if json_out {
        let payload: Vec<Value> = collisions
            .iter()
            .map(|c| {
                json!({
                    "with_node_id": c.with_node_id,
                    "with_node_title": c.with_node_title,
                    "with_plan_path": c.with_plan_path,
                    "shared_files": c.shared_files,
                    "candidate_only_files": c.candidate_only_files,
                    "other_only_files": c.other_only_files,
                    "severity": severity_word(c),
                    "recommended_action": action_word(c),
                    "rationale": c.rationale,
                })
            })
            .collect();
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "status": if evaluated { "ok" } else { "unevaluated" },
                "collisions": payload,
            }))
            .unwrap_or_else(|_| "{}".into())
        );
        return 0;
    }
    if !evaluated {
        eprintln!(
            "UNEVALUATED: {plan_path} states no file surface, so nothing was \
             compared. Add a '## Files to Modify' or '## File Ownership Map' table."
        );
        return 0;
    }
    if collisions.is_empty() {
        println!("No collisions found for {plan_path}");
        return 0;
    }
    for c in &collisions {
        println!(
            "[{}] {} ({})",
            severity_word(c).to_uppercase(),
            c.with_node_id,
            c.with_node_title
        );
        println!("  shared: {}", c.shared_files.join(", "));
        println!("  recommended: {}", action_word(c));
        println!("  rationale: {}", c.rationale);
        println!();
    }
    0
}

fn severity_word(c: &super::collision::Collision) -> &'static str {
    match c.severity {
        super::collision::Severity::High => "high",
        super::collision::Severity::Medium => "medium",
        super::collision::Severity::Low => "low",
    }
}

fn action_word(c: &super::collision::Collision) -> &'static str {
    match c.recommended_action {
        super::collision::Action::Coordinate => "coordinate",
        super::collision::Action::Absorb => "absorb",
        super::collision::Action::Supersede => "supersede",
    }
}

/// The wheel detector's self-inference (graph/collision.py `_claims_default`
/// + the registered-plan check): with one claimed id on the plan, the node is
/// SELF only when this file is the plan it already registered, or it has
/// registered none yet; a DIFFERENT plan drafted against the same node
/// genuinely overlaps the node's own plan and must stay visible.
fn infer_collisions_self_id(plan: &Path, entries: &[Value]) -> Option<String> {
    let target = if plan.is_dir() {
        plan.join("00-INDEX.md")
    } else {
        plan.to_path_buf()
    };
    let claims = super::session_cli::plan_claims(target.to_str()?)?;
    if claims.len() != 1 {
        return None;
    }
    let claimed = claims.into_iter().next()?;
    let registered = entries
        .iter()
        .find(|e| text_at(e, "id").as_deref() == Some(claimed.as_str()))
        .and_then(|e| text_at(e, "plan_path"))
        .map(str::to_string);
    match registered {
        None => Some(claimed),
        Some(reg) if reg.is_empty() => Some(claimed),
        Some(reg) => {
            let root = super::collision::find_repo_root();
            let resolved = super::collision::resolve_plan_path(&reg, &root);
            let same = match (
                std::fs::canonicalize(&resolved),
                std::fs::canonicalize(plan),
            ) {
                (Ok(a), Ok(b)) => a == b,
                _ => resolved == plan,
            };
            Some(claimed).filter(|_| same)
        }
    }
}

/// Severity thresholds resolve from the config candidates (project beats
/// user), each key sanitized to the model default when malformed or
/// negative, the same fail-open contract the wheel's settings model held.
fn collision_thresholds() -> super::collision::Thresholds {
    let defaults = super::collision::Thresholds::default();
    for cfg in settings::config_candidates() {
        let Some(block) = cfg
            .get("collision")
            .and_then(|c| c.get("severity_thresholds"))
        else {
            continue;
        };
        let Ok(mut t) = serde_json::from_value::<super::collision::Thresholds>(block.clone())
        else {
            continue;
        };
        let nonneg = |v: f64, d: f64| if v.is_finite() && v >= 0.0 { v } else { d };
        t.high_count = nonneg(t.high_count, defaults.high_count);
        t.high_ratio = nonneg(t.high_ratio, defaults.high_ratio);
        t.medium_count = nonneg(t.medium_count, defaults.medium_count);
        t.medium_ratio = nonneg(t.medium_ratio, defaults.medium_ratio);
        return t;
    }
    defaults
}

// ---------------------------------------------------------------------------
// backfill-deferred-kind
// ---------------------------------------------------------------------------

const BACKFILL_HELP: &str =
    "Usage: fno backlog backfill-deferred-kind [--map FILE] [--apply] [--json]

Classify deferred rows by EXACT reason match. Never closes or undefers.
Existing kinds are never overwritten; non-deferred nodes are never touched.

Options:
  --map FILE  TSV file: kind<TAB>exact deferred_reason per row
  --apply     Write the classification. Default is a dry-run report.
  -J, --json  Emit JSON report
  -h, --help  Print help
";

pub fn run_backfill_deferred_kind(tail: &[String]) -> i32 {
    let mut map_file: Option<String> = None;
    let mut apply = false;
    let mut json_out = false;
    let mut it = tail.iter();
    while let Some(arg) = it.next() {
        let (flag, inline) = match arg.split_once('=') {
            Some((f, v)) if f.starts_with("--") => (f, Some(v.to_string())),
            _ => (arg.as_str(), None),
        };
        match flag {
            "-h" | "--help" => {
                print!("{BACKFILL_HELP}");
                return 0;
            }
            "--map" => {
                map_file = Some(inline.or_else(|| it.next().cloned()).unwrap_or_default());
            }
            "--apply" => apply = true,
            "-J" | "--json" => json_out = true,
            other if other.starts_with('-') && other.len() > 1 => {
                eprintln!("No such option: {other}");
                return 2;
            }
            _ => {
                eprintln!("Got unexpected extra argument ({arg})");
                return 2;
            }
        }
    }
    let mut extra_map: BTreeMap<String, String> = BTreeMap::new();
    if let Some(map_path) = &map_file {
        match load_deferred_kind_map(Path::new(map_path)) {
            Ok(m) => extra_map = m,
            Err(e) => {
                eprintln!("Error: {e}");
                return 1;
            }
        }
    }
    if let Some(code) = refuse_non_graph_backend("backfill-deferred-kind") {
        return code;
    }
    let graph = settings::graph_path();
    if !apply {
        let Ok(rows) = graph_store::read_rows(&graph) else {
            eprintln!("Error: the backlog graph could not be read");
            return 1;
        };
        let mut would: BTreeMap<String, usize> = BTreeMap::new();
        let mut unclassified = 0usize;
        for e in &rows {
            if text_at(e, "status") != Some("deferred")
                || text_at(e, "deferred_kind").is_some_and(|k| !k.is_empty())
            {
                continue;
            }
            match classify(e, &extra_map) {
                Some(kind) => *would.entry(kind.to_string()).or_default() += 1,
                None => unclassified += 1,
            }
        }
        if json_out {
            let would_json: Map<String, Value> =
                would.into_iter().map(|(k, v)| (k, json!(v))).collect();
            println!(
                "{}",
                serde_json::to_string(&json!({
                    "dry_run": true,
                    "would_stamp": would_json,
                    "unclassified": unclassified,
                }))
                .unwrap_or_else(|_| "{}".into())
            );
        } else {
            for (kind, n) in &would {
                println!("  {kind}: {n}");
            }
            println!("  unclassified (stays unknown): {unclassified}");
            println!("  dry run; pass --apply to write");
        }
        return 0;
    }
    let counts: std::cell::RefCell<BTreeMap<String, usize>> =
        std::cell::RefCell::new(BTreeMap::new());
    let applied = graph_store::mutate_rows(
        &graph,
        std::time::Duration::from_secs(5),
        None,
        None,
        |working| {
            for node in working.iter_mut() {
                if text_at(node, "status") != Some("deferred")
                    || text_at(node, "deferred_kind").is_some_and(|k| !k.is_empty())
                {
                    continue;
                }
                if let Some(kind) = classify(node, &extra_map) {
                    if let Some(obj) = node.as_object_mut() {
                        obj.insert("deferred_kind".into(), json!(kind));
                        *counts.borrow_mut().entry(kind.to_string()).or_default() += 1;
                    }
                }
            }
            Ok(true)
        },
    );
    if let Err(err) = applied {
        eprintln!("Error: {err}");
        return 1;
    }
    let counts = counts.into_inner();
    if json_out {
        let stamped: Map<String, Value> =
            counts.iter().map(|(k, v)| (k.clone(), json!(v))).collect();
        println!(
            "{}",
            serde_json::to_string(&json!({"applied": true, "stamped": stamped}))
                .unwrap_or_else(|_| "{}".into())
        );
    } else {
        for (kind, n) in &counts {
            println!("  stamped {kind}: {n}");
        }
        let total: usize = counts.values().sum();
        println!(
            "  total stamped: {total} (existing kinds preserved; unclassified left unclassified)"
        );
    }
    0
}

fn classify(node: &Value, extra: &BTreeMap<String, String>) -> Option<&'static str> {
    let reason = text_at(node, "deferred_reason")?;
    if let Some(kind) = extra.get(reason) {
        // The extra map's values are validated against DEFERRED_KINDS at
        // load; find the matching static so the return type holds.
        return DEFERRED_KINDS.iter().copied().find(|k| k == &kind.as_str());
    }
    classify_deferred_reason(reason)
}

/// The operator TSV: `kind<TAB>exact reason` per row. Hard error before any
/// graph read; duplicate reasons with conflicting kinds are refused.
fn load_deferred_kind_map(path: &Path) -> Result<BTreeMap<String, String>, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut mapping: BTreeMap<String, String> = BTreeMap::new();
    for (i, raw) in text.lines().enumerate() {
        let lineno = i + 1;
        let line = raw.trim_end_matches(['\r']);
        if line.trim().is_empty() || line.trim_start().starts_with('#') {
            continue;
        }
        let Some((kind, reason)) = line.split_once('\t') else {
            return Err(format!(
                "{}:{lineno}: expected 'kind<TAB>reason', got no tab",
                path.display()
            ));
        };
        let kind = kind.trim();
        let reason = reason.trim();
        if !DEFERRED_KINDS.contains(&kind) {
            return Err(format!(
                "{}:{lineno}: unknown kind '{kind}' (valid: {})",
                path.display(),
                DEFERRED_KINDS.join(", ")
            ));
        }
        if reason.is_empty() {
            return Err(format!("{0}:{1}: empty reason", path.display(), lineno));
        }
        if let Some(prev) = mapping.get(reason) {
            if prev != kind {
                return Err(format!(
                    "{}:{lineno}: reason maps to two kinds ({:?})",
                    path.display(),
                    reason.chars().take(60).collect::<String>()
                ));
            }
        }
        mapping.insert(reason.to_string(), kind.to_string());
    }
    Ok(mapping)
}

// ---------------------------------------------------------------------------
// archive-dedupe-ids - the completed one-shot
// ---------------------------------------------------------------------------

const ARCHIVE_DEDUPE_HELP: &str = "Usage: fno backlog archive-dedupe-ids

The one-shot archive id-repair ran to completion; nothing is left to do.
The repair's receipt is the `backlog_archive_dedupe_completed` event.
";

pub fn run_archive_dedupe_ids(tail: &[String]) -> i32 {
    if tail.iter().any(|a| a == "--help" || a == "-h") {
        print!("{ARCHIVE_DEDUPE_HELP}");
        return 0;
    }
    eprintln!(
        "Error: archive-dedupe-ids was a one-shot repair that ran to \
         completion; there is nothing left for it to do."
    );
    2
}

// ---------------------------------------------------------------------------
// unclaim - free a wedged node's claim
// ---------------------------------------------------------------------------

const UNCLAIM_HELP: &str = "Usage: fno backlog unclaim TASK_ID

Free a node (reverts claimed -> ready, releases the lockfile).
";

pub fn run_unclaim(tail: &[String]) -> i32 {
    let args: Vec<&String> = tail
        .iter()
        .filter(|a| *a != "--help" && *a != "-h")
        .collect();
    if tail.iter().any(|a| a == "--help" || a == "-h") {
        print!("{UNCLAIM_HELP}");
        return 0;
    }
    let Some(task_id) = args.first() else {
        eprintln!("Missing argument 'TASK_ID'.");
        return 2;
    };
    if !has_node_id_prefix(task_id) {
        eprintln!("Error: task_id must be a <prefix>-<4..8 hex> node id, got '{task_id}'");
        return 1;
    }
    let node_id = task_id.to_string();
    if let Some(code) = refuse_non_graph_backend("unclaim") {
        return code;
    }
    let graph = settings::graph_path();
    let Ok(rows) = graph_store::read_rows(&graph) else {
        eprintln!("Error: the backlog graph could not be read");
        return 1;
    };
    if find_node(&rows, &node_id).is_none() {
        eprintln!("Error: graph node {node_id} not found");
        return 1;
    }
    // A legacy short id (`ab-1234`) resolves through find_node's unique
    // prefix tier; the claim key and the settle below must use the row's
    // canonical id, never the abbreviation that found it.
    let node_id = find_node(&rows, &node_id)
        .and_then(|n| text_at(n, "id"))
        .unwrap_or(&node_id)
        .to_string();
    // The lockfile side is best-effort; the graph settle still runs.
    let lock_note = release_node_lockfile(&node_id);
    if let Err(err) = settle_released_node(&graph, &node_id) {
        eprintln!("Error: {err}");
        return 1;
    }
    if !(lock_note.starts_with("lockfile left")
        || lock_note.starts_with("lockfile changed")
        || lock_note.starts_with("lockfile untouched"))
    {
        let Ok(after) = graph_store::read_rows(&graph) else {
            eprintln!("Error: the backlog graph could not be read");
            return 1;
        };
        if let Some(node) = find_node(&after, &node_id) {
            if text_at(node, "persisted_status") == Some("in_progress") {
                let open_do = open_do_rows(node);
                let plural = if open_do == 1 { "" } else { "s" };
                eprintln!(
                    "unclaim: {node_id} still reads in_progress after clearing the claim \
                     ({open_do} open do row{plural}). The claim was not what held it. \
                     Use: fno backlog requeue {node_id}"
                );
                return 3;
            }
        }
    }
    println!("Unclaimed {node_id} ({lock_note})");
    0
}

fn open_do_rows(node: &Value) -> usize {
    node.get("sessions")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|r| is_open_do_row(r))
        .count()
}

fn is_open_do_row(row: &Value) -> bool {
    // Identified (harness + session_id), bounded (started_at), not yet closed.
    let nonempty_str = |key: &str| {
        row.get(key)
            .and_then(Value::as_str)
            .is_some_and(|s| !s.trim().is_empty())
    };
    text_at(row, "phase") == Some("execute")
        && nonempty_str("harness")
        && nonempty_str("session_id")
        && nonempty_str("started_at")
        && row.get("ended_at").map(|v| v.is_null()).unwrap_or(true)
}

/// Best-effort release of the `node:<id>` lockfile. Releases stale or own
/// holders; keeps a LIVE foreign holder. Never fails the verb.
pub(crate) fn release_node_lockfile(node_id: &str) -> String {
    let key = format!("node:{node_id}");
    let roots: Vec<Option<PathBuf>> = {
        let mut roots = vec![crate::claims::claims_root_for(&key), None];
        roots.dedup();
        roots
    };
    for root in &roots {
        let (state, record) = crate::claims::status(&key, root.as_deref());
        match state {
            crate::claims::ClaimState::Free => return "no lockfile".to_string(),
            crate::claims::ClaimState::Stale => {
                let holder = record
                    .as_ref()
                    .map(|r| r.holder.clone())
                    .unwrap_or_default();
                let note = match crate::claims::release(&key, &holder, root.as_deref(), None) {
                    Ok(()) => "released stale lockfile".to_string(),
                    Err(_) => "lockfile changed".to_string(),
                };
                return note;
            }
            crate::claims::ClaimState::Corrupted => {
                eprintln!(
                    "warning: lockfile {key} is corrupted; graph claim cleared but \
                     lockfile left intact. Use `fno agents claim release {key} --force \
                     -R <why>` to repair."
                );
                return "lockfile left (corrupted)".to_string();
            }
            crate::claims::ClaimState::Live | crate::claims::ClaimState::Suspect => {
                let holder = record
                    .as_ref()
                    .map(|r| r.holder.clone())
                    .unwrap_or_default();
                if holder == invoking_claim_holder() {
                    return match crate::claims::release(&key, &holder, root.as_deref(), None) {
                        Ok(()) => "released own lockfile".to_string(),
                        Err(_) => "lockfile changed".to_string(),
                    };
                }
                eprintln!(
                    "warning: lockfile {key} held by LIVE holder {holder:?}; lockfile left \
                     intact. Use `fno agents claim release {key} --force -R <why>` to override."
                );
                return "lockfile left (live foreign holder)".to_string();
            }
        }
    }
    "lockfile untouched".to_string()
}

/// The holder id this invocation would hold: the target manifest's recorded
/// claim holder, else `target-session:<the resolved session id>`.
pub(crate) fn invoking_claim_holder() -> String {
    let manifest = std::env::current_dir()
        .ok()
        .map(|cwd| cwd.join(".fno/target-state.md"));
    if let Some(path) = manifest {
        if let Ok(text) = std::fs::read_to_string(&path) {
            for line in text.lines() {
                if let Some(value) = line.trim().strip_prefix("target_claim_holder:") {
                    let value = value.trim().trim_matches(['"', '\'']);
                    if !value.is_empty() && value != "null" {
                        return value.to_string();
                    }
                }
            }
        }
    }
    let (session, _) = crate::claims::resolve_identity();
    match session {
        Some(sid) => format!("target-session:{sid}"),
        None => String::new(),
    }
}

/// Recompute + persist the released node's derived status (the settle).
pub(crate) fn settle_released_node(graph: &Path, node_id: &str) -> Result<(), String> {
    graph_store::mutate_rows(
        graph,
        std::time::Duration::from_secs(5),
        None,
        None,
        |working| {
            if working.iter().any(|e| text_at(e, "id") == Some(node_id)) {
                graph_store::recompute_statuses_with_plan_rungs(working, None);
            }
            Ok(true)
        },
    )
    .map(|_| ())
    .map_err(|e| e.to_string())
}

// ---------------------------------------------------------------------------
// defer / undefer / retract - the lifecycle trio
// ---------------------------------------------------------------------------

const DEFER_HELP: &str = "Usage: fno backlog defer TASK_IDS... --reason TEXT [--kind KIND]

Mark one or more backlog nodes as deferred. Sets deferred_at + deferred_reason.
Atomic across the batch: if any ID is unknown, none are deferred.

Options:
  -R, --reason TEXT  Why these nodes are being deferred.  [required]
  -K, --kind KIND    expired|blocked|wont_do|retracted|superseded|later|
                     contingent|carveout|internal_only|junk. Omitted: stamped
                     only when the reason exactly matches a known
                     machine-stamped string.
  -h, --help         Print help
";

const UNDEFER_HELP: &str = "Usage: fno backlog undefer TASK_IDS...

Clear deferred state on one or more backlog nodes. Idempotent. Atomic across
the batch. A node still superseded refuses and names the door route.
";

const RETRACT_HELP: &str = "Usage: fno backlog retract TASK_IDS... REASON

Retract one or more backlog nodes: defer + stamp deferred_kind: retracted.
The reason rides a positional, not a flag.
";

struct IdsAndFlags {
    ids_raw: Vec<String>,
    reason: Option<String>,
    kind: Option<String>,
}

fn parse_ids_and_flags(tail: &[String], help: &str) -> Result<IdsAndFlags, i32> {
    let mut out = IdsAndFlags {
        ids_raw: Vec::new(),
        reason: None,
        kind: None,
    };
    let mut it = tail.iter();
    while let Some(arg) = it.next() {
        let (flag, inline) = match arg.split_once('=') {
            Some((f, v)) if f.starts_with("--") => (f, Some(v.to_string())),
            _ => (arg.as_str(), None),
        };
        match flag {
            "-h" | "--help" => {
                print!("{help}");
                return Err(0);
            }
            "--reason" | "-R" => {
                out.reason = Some(inline.or_else(|| it.next().cloned()).unwrap_or_default());
            }
            "--kind" | "-K" => {
                out.kind = Some(inline.or_else(|| it.next().cloned()).unwrap_or_default());
            }
            _ => out.ids_raw.push(arg.clone()),
        }
    }
    Ok(out)
}

pub fn run_defer(tail: &[String]) -> i32 {
    let parsed = match parse_ids_and_flags(tail, DEFER_HELP) {
        Ok(p) => p,
        Err(code) => return code,
    };
    if let Some(code) = refuse_non_graph_backend("defer") {
        return code;
    }
    let Some(reason) = parsed
        .reason
        .map(|r| r.trim().to_string())
        .filter(|r| !r.is_empty())
    else {
        eprintln!("Error: --reason cannot be blank");
        return 1;
    };
    if let Some(kind) = &parsed.kind {
        if !DEFERRED_KINDS.contains(&kind.as_str()) {
            eprintln!(
                "Error: --kind must be one of {}, got '{kind}'",
                DEFERRED_KINDS.join(", ")
            );
            return 1;
        }
    }
    defer_batch(
        &parsed.ids_raw,
        &reason,
        parsed
            .kind
            .as_deref()
            .or_else(|| classify_deferred_reason(&reason)),
        "Deferred",
    )
}

pub fn run_retract(tail: &[String]) -> i32 {
    // The reason rides a positional, not a flag: `retract <ids> "<premise>"`.
    let (ids_raw, reason) = match tail.first() {
        None => {
            print!("{RETRACT_HELP}");
            return 2;
        }
        Some(first) if first == "--help" || first == "-h" => {
            print!("{RETRACT_HELP}");
            return 0;
        }
        Some(_) => {
            let split = tail.iter().position(|a| a == "--reason" || a == "-R");
            match split {
                Some(pos) => (
                    tail[..pos].to_vec(),
                    tail.get(pos + 1).cloned().unwrap_or_default(),
                ),
                None => {
                    if tail.len() < 2 {
                        eprintln!(
                            "retract: at least one TASK_ID and a reason are required: \
                             fno backlog retract <ids> \"<the false premise>\""
                        );
                        return 2;
                    }
                    (
                        tail[..tail.len() - 1].to_vec(),
                        tail.last().cloned().unwrap_or_default(),
                    )
                }
            }
        }
    };
    if let Some(code) = refuse_non_graph_backend("retract") {
        return code;
    }
    let reason = reason.trim().to_string();
    if reason.is_empty() {
        eprintln!("Error: --reason cannot be blank");
        return 1;
    }
    defer_batch(&ids_raw, &reason, Some("retracted"), "Retracted")
}

/// The shared defer write: validate every id, warn on dependents, stamp
/// status + deferred_reason (+ kind) per node, one locked mutation.
fn defer_batch(ids_raw: &[String], reason: &str, kind: Option<&str>, verb: &str) -> i32 {
    let graph = settings::graph_path();
    let Ok(rows) = graph_store::read_rows(&graph) else {
        eprintln!("Error: the backlog graph could not be read");
        return 1;
    };
    let mut ids: Vec<String> = Vec::new();
    for raw in ids_raw {
        for piece in raw.split(',') {
            let piece = piece.trim();
            if piece.is_empty() {
                continue;
            }
            if !has_node_id_prefix(piece) {
                eprintln!("Error: task_id must be a <prefix>-<4..8 hex> node id, got '{piece}'");
                return 1;
            }
            ids.push(piece.to_string());
        }
    }
    if ids.is_empty() {
        eprintln!("Error: at least one TASK_ID is required");
        return 2;
    }
    // Resolve every id and abort naming ALL missing ones before any write.
    for id in &ids {
        if find_node(&rows, id).is_none() {
            eprintln!("Error: feature(s) not found: {id}");
            return 1;
        }
        let dependents: Vec<String> = rows
            .iter()
            .filter(|e| {
                e.get("blocked_by")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str)
                    .any(|b| b == id.as_str())
            })
            .filter_map(|e| text_at(e, "id"))
            .map(String::from)
            .collect();
        if !dependents.is_empty() {
            eprintln!("WARN: Deferring {id} blocks: {}", dependents.join(", "));
        }
    }
    let ids_for_apply = ids.clone();
    let reason_value = reason.to_string();
    let kind_value = kind.map(String::from);
    let applied = graph_store::mutate_rows(
        &graph,
        std::time::Duration::from_secs(5),
        None,
        None,
        |working| {
            for id in &ids_for_apply {
                let Some(node) = working
                    .iter_mut()
                    .find(|e| text_at(e, "id").as_deref() == Some(id.as_str()))
                else {
                    return Err(graph_store::StoreError::Invalid(format!(
                        "feature(s) not found: {id}"
                    )));
                };
                let obj = node.as_object_mut().expect("row is an object");
                obj.insert("status".into(), json!("deferred"));
                obj.insert("deferred_at".into(), json!(graph_store::now_isoformat()));
                obj.insert("deferred_reason".into(), json!(reason_value));
                obj.insert(
                    "deferred_kind".into(),
                    match &kind_value {
                        Some(k) => json!(k),
                        None => Value::Null,
                    },
                );
            }
            Ok(true)
        },
    );
    if let Err(err) = applied {
        eprintln!("Error: {err}");
        return 1;
    }
    for id in &ids {
        println!("{verb} {id}: \"{reason}\"");
    }
    project_plans_for(&ids);
    0
}

/// The post-write plan projection the lifecycle trio shares (best-effort,
/// the same posture the Python tail had).
fn project_plans_for(ids: &[String]) {
    let graph = settings::graph_path();
    let Ok(rows) = graph_store::read_rows(&graph) else {
        return;
    };
    let _ = crate::plan_doc::project::project_graph_nodes(&rows, ids, None, None, None, None);
}

pub fn run_undefer(tail: &[String]) -> i32 {
    if tail.is_empty() {
        print!("{UNDEFER_HELP}");
        return 2;
    }
    let parsed = match parse_ids_and_flags(tail, UNDEFER_HELP) {
        Ok(p) => p,
        Err(code) => return code,
    };
    if let Some(code) = refuse_non_graph_backend("undefer") {
        return code;
    }
    let graph = settings::graph_path();
    let Ok(rows) = graph_store::read_rows(&graph) else {
        eprintln!("Error: the backlog graph could not be read");
        return 1;
    };
    let mut ids: Vec<String> = Vec::new();
    for raw in &parsed.ids_raw {
        for piece in raw.split(',') {
            let piece = piece.trim();
            if piece.is_empty() {
                continue;
            }
            if !has_node_id_prefix(piece) {
                eprintln!("Error: task_id must be a <prefix>-<4..8 hex> node id, got '{piece}'");
                return 1;
            }
            ids.push(piece.to_string());
        }
    }
    if ids.is_empty() {
        eprintln!("Error: at least one TASK_ID is required");
        return 2;
    }
    for id in &ids {
        if find_node(&rows, id).is_none() {
            eprintln!("Error: feature(s) not found: {id}");
            return 1;
        }
    }
    // A park riding on a supersession is not undefer's to lift.
    for id in &ids {
        if let Some(node) = find_node(&rows, id) {
            if let Some(replacer) = text_at(node, "superseded_by") {
                eprintln!(
                    "refused: {id} is superseded by {replacer}; the park rides on the \
                     supersession. Supply it with: fno backlog update {id} --status idea \
                     (or fno backlog unsupersede {id})"
                );
                return 2;
            }
        }
    }
    let ids_for_apply = ids.clone();
    // Per-node plans anchor: the wheel scanned rulings in the TARGET node's
    // project (plans_content_dir(Path(node.cwd)) if the row carries one),
    // so a foreign plansDirectory override keeps holding; the invoking cwd
    // would read the wrong project's plans.
    let node_cwds: Vec<(String, Option<String>)> = ids
        .iter()
        .map(|id| {
            let cwd = find_node(&rows, id).and_then(|n| text_at(n, "cwd").map(str::to_string));
            (id.clone(), cwd)
        })
        .collect();
    let notes: std::cell::RefCell<Vec<String>> = std::cell::RefCell::new(Vec::new());
    let unchanged: std::cell::RefCell<Vec<String>> = std::cell::RefCell::new(Vec::new());
    let applied = graph_store::mutate_rows(
        &graph,
        std::time::Duration::from_secs(5),
        None,
        None,
        |working| {
            for id in &ids_for_apply {
                let Some(node) = working
                    .iter_mut()
                    .find(|e| text_at(e, "id").as_deref() == Some(id.as_str()))
                else {
                    return Err(graph_store::StoreError::Invalid(format!(
                        "feature(s) not found: {id}"
                    )));
                };
                let was_deferred = truthy(node, "deferred_at");
                let kind = text_at(node, "deferred_kind")
                    .unwrap_or("no kind")
                    .to_string();
                let reason = text_at(node, "deferred_reason").unwrap_or("").to_string();
                if !was_deferred {
                    unchanged.borrow_mut().push(id.clone());
                }
                {
                    let obj = node.as_object_mut().expect("row is an object");
                    obj.insert("deferred_at".into(), Value::Null);
                    obj.insert("deferred_reason".into(), Value::Null);
                    obj.insert("deferred_kind".into(), Value::Null);
                    obj.remove("status");
                }
                if was_deferred {
                    notes.borrow_mut().push(format!("{id}\t{kind}\t{reason}"));
                }
            }
            // The patch-door derivation: a cleared node re-derives from its
            // plan rung (none reads idea), never from apply_defaults' ready.
            let rungs: BTreeMap<String, String> = working
                .iter()
                .filter_map(|e| {
                    graph_store::entry_id(e).map(|i| {
                        (
                            i.to_string(),
                            crate::backlog_ready::plan_rung(e).to_string(),
                        )
                    })
                })
                .collect();
            graph_store::recompute_statuses_with_plan_rungs(working, Some(&rungs));
            Ok(true)
        },
    );
    if let Err(err) = applied {
        eprintln!("Error: {err}");
        return 1;
    }
    for id in &unchanged.into_inner() {
        println!("{id} unchanged");
        eprintln!("warning: {id} was not deferred");
    }
    for note in notes.into_inner() {
        let mut parts = note.splitn(3, '\t');
        let id = parts.next().unwrap_or_default();
        let kind = parts.next().unwrap_or("no kind");
        let reason = parts.next().unwrap_or("");
        // The streak-reset boundary: a human-recovered node needs fresh
        // failures before auto-defer re-triggers.
        super::workflows::emit_event("node_undeferred", json!({"unit_id": id}));
        if !reason.is_empty() {
            eprintln!("undefer: cleared the deferral of {id} ({kind}): {reason}");
        }
        // The rulings scan anchors on the target node's project, the anchor
        // the park was recorded under; a foreign plansDirectory override
        // keeps holding.
        let node_cwd = node_cwds
            .iter()
            .find(|(nid, _)| nid == id)
            .and_then(|(_, cwd)| cwd.as_deref());
        for line in ruling_lines_for(id, "undefer", node_cwd) {
            eprintln!("{line}");
        }
        println!("Undeferred {id}");
    }
    project_plans_for(&ids);
    0
}

/// The reversal-form ruling lines (`{prefix}: {id} is rejected by {by} in
/// {plan}: {reason}`), over the decisions door's own scan. `anchor_cwd`
/// pins the plans probe to the target node's project (the deleted verb's
/// `plans_content_dir(Path(node.cwd))`); None anchors on the invoking cwd.
fn ruling_lines_for(node_id: &str, prefix: &str, anchor_cwd: Option<&str>) -> Vec<String> {
    let dir = match anchor_cwd {
        Some(cwd) => {
            let expanded = shellexpand_home(cwd);
            match super::decisions_cli::plans_dir_for_listing_from(Path::new(&expanded)) {
                Ok(dir) => dir,
                Err(detail) => {
                    return vec![format!(
                        "{prefix}: plan rulings for {node_id} not read ({detail})"
                    )]
                }
            }
        }
        None => match super::decisions_cli::plans_dir_for_listing() {
            Ok(dir) => dir,
            Err(detail) => {
                return vec![format!(
                    "{prefix}: plan rulings for {node_id} not read ({detail})"
                )]
            }
        },
    };
    let result = super::decisions_cli::plan_rulings_in(&dir, node_id);
    if result.get("status").and_then(Value::as_str) == Some("unavailable") {
        let dir = result.get("dir").and_then(Value::as_str).unwrap_or("");
        let detail = result.get("detail").and_then(Value::as_str).unwrap_or("");
        return vec![format!(
            "{prefix}: plan rulings for {node_id} not read ({dir}: {detail})"
        )];
    }
    let mut lines = Vec::new();
    for ruling in result
        .get("rulings")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let by = ruling
            .get("by")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .collect::<Vec<_>>()
            .join(", ");
        let by = if by.is_empty() {
            "(unclaimed)".to_string()
        } else {
            by
        };
        let plan = ruling
            .get("plan_path")
            .and_then(Value::as_str)
            .unwrap_or("");
        let reason = ruling.get("reason").and_then(Value::as_str).unwrap_or("");
        lines.push(format!(
            "{prefix}: {node_id} is rejected by {by} in {plan}: {reason}"
        ));
    }
    lines
}

// ---------------------------------------------------------------------------
// unsupersede
// ---------------------------------------------------------------------------

const UNSUPERSEDE_HELP: &str = "Usage: fno backlog unsupersede NODE_ID

Reverse a supersede on NODE_ID. Idempotent in the safe direction.
";

pub fn run_unsupersede(tail: &[String]) -> i32 {
    let args: Vec<&String> = tail
        .iter()
        .filter(|a| *a != "--help" && *a != "-h")
        .collect();
    if tail.iter().any(|a| a == "--help" || a == "-h") {
        print!("{UNSUPERSEDE_HELP}");
        return 0;
    }
    let Some(node_id) = args.first() else {
        eprintln!("Missing argument 'NODE_ID'.");
        return 2;
    };
    if let Some(code) = refuse_non_graph_backend("unsupersede") {
        return code;
    }
    let graph = settings::graph_path();
    let Ok(rows) = graph_store::read_rows(&graph) else {
        eprintln!("Error: the backlog graph could not be read");
        return 1;
    };
    // References resolve format-agnostically: exact id, legacy short
    // prefix, dash variant, or the row's immutable slug.
    let resolved =
        find_node(&rows, node_id).or_else(|| super::node_ref::resolve_tiers(&rows, node_id));
    let Some(node) = resolved else {
        eprintln!("Error: node {node_id} not found");
        return 1;
    };
    let canonical = text_at(node, "id").unwrap_or(node_id).to_string();
    let replacer = find_node(&rows, &canonical)
        .and_then(|n| text_at(n, "superseded_by"))
        .unwrap_or("")
        .to_string();
    let cause = find_node(&rows, &canonical)
        .and_then(|n| n.get("supersession"))
        .filter(|v| v.is_object())
        .and_then(|s| s.get("cause").and_then(Value::as_str))
        .unwrap_or("")
        .to_string();
    // The reverse is a --leave-superseded mutation over the single-row write
    // seam: clear the supersession facts, recompute, persist.
    let target = canonical.clone();
    let changed: std::cell::RefCell<bool> = std::cell::RefCell::new(false);
    let applied = graph_store::mutate_rows(
        &graph,
        std::time::Duration::from_secs(5),
        None,
        None,
        |working| {
            let Some(idx) = working
                .iter()
                .position(|e| text_at(e, "id") == Some(target.as_str()))
            else {
                return Err(graph_store::StoreError::Invalid(format!(
                    "node {target} not found"
                )));
            };
            let had = text_at(&working[idx], "superseded_by").is_some();
            // The door's own clear: the supersession pair plus the
            // replacer's supersedes backref, one implementation.
            let mut changes: Vec<super::patch::Change> = Vec::new();
            super::patch::clear_supersession_facts(working, idx, &mut changes);
            // The patch-door derivation: a cleared node re-derives from its
            // plan rung (none reads idea), never from apply_defaults' ready.
            let rungs: BTreeMap<String, String> = working
                .iter()
                .filter_map(|e| {
                    graph_store::entry_id(e).map(|i| {
                        (
                            i.to_string(),
                            crate::backlog_ready::plan_rung(e).to_string(),
                        )
                    })
                })
                .collect();
            graph_store::recompute_statuses_with_plan_rungs(working, Some(&rungs));
            *changed.borrow_mut() = had;
            Ok(true)
        },
    );
    if let Err(err) = applied {
        eprintln!("Error: {err}");
        return 1;
    }
    if !changed.into_inner() {
        eprintln!("warning: {canonical} was not superseded");
        return 0;
    }
    // Same streak-reset boundary undefer gives a human-recovered node.
    super::workflows::emit_event("node_undeferred", json!({"unit_id": canonical}));
    eprintln!("unsupersede: cleared the supersession of {canonical} by {replacer}: {cause}");
    for line in ruling_lines_for(&canonical, "unsupersede", None) {
        eprintln!("{line}");
    }
    println!("Unsuperseded {node_id}");
    // Heal the plan doc off its terminal superseded rung, then recompute so
    // the derived status follows the corrected plan.
    project_plans_for(&[canonical.clone()]);
    let _ = settle_released_node(&graph, &canonical);
    0
}

// ---------------------------------------------------------------------------
// relatedness build / get
// ---------------------------------------------------------------------------

const RELATEDNESS_BUILD_HELP: &str =
    "Usage: fno backlog relatedness build [--project NAME] [--top-k K] [--json]

Build the relatedness sidecar from graph signals (read-only on the graph).
";

const RELATEDNESS_GET_HELP: &str = "Usage: fno backlog relatedness get NODE_ID [--top-k K] [--json]

Print the top related nodes for one node (the consumer API).
No map -> exit non-zero, empty stdout. Present map, no edges -> exit 0.
";

fn relatedness_path() -> PathBuf {
    settings::graph_path()
        .parent()
        .map(|p| p.join("relatedness.json"))
        .unwrap_or_else(|| PathBuf::from("relatedness.json"))
}

pub fn run_relatedness(tail: &[String]) -> i32 {
    let sub = tail.first().map(String::as_str);
    let rest = if tail.is_empty() {
        &tail[..]
    } else {
        &tail[1..]
    };
    match sub {
        Some("build") => relatedness_build(rest),
        Some("get") => relatedness_get(rest),
        _ => {
            print!("{RELATEDNESS_BUILD_HELP}");
            print!("{RELATEDNESS_GET_HELP}");
            if sub.is_none() {
                2
            } else {
                eprintln!("fno backlog relatedness: unknown subcommand '{sub:?}'");
                2
            }
        }
    }
}

fn relatedness_build(tail: &[String]) -> i32 {
    let mut project: Option<String> = None;
    let mut top_k = 5usize;
    let mut json_out = false;
    let mut it = tail.iter();
    while let Some(arg) = it.next() {
        let (flag, inline) = match arg.split_once('=') {
            Some((f, v)) if f.starts_with("--") => (f, Some(v.to_string())),
            _ => (arg.as_str(), None),
        };
        match flag {
            "-h" | "--help" => {
                print!("{RELATEDNESS_BUILD_HELP}");
                return 0;
            }
            "--project" | "-p" => {
                project = Some(inline.or_else(|| it.next().cloned()).unwrap_or_default());
            }
            "--top-k" | "-K" => {
                top_k = inline
                    .or_else(|| it.next().cloned())
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(5);
            }
            "-J" | "--json" => json_out = true,
            other if other.starts_with('-') && other.len() > 1 => {
                eprintln!("No such option: {other}");
                return 2;
            }
            _ => {
                eprintln!("Got unexpected extra argument ({arg})");
                return 2;
            }
        }
    }
    if let Some(code) = refuse_non_graph_backend("relatedness build") {
        return code;
    }
    let graph = settings::graph_path();
    let Ok(rows) = graph_store::read_rows(&graph) else {
        eprintln!("Error: the backlog graph could not be read");
        return 1;
    };
    let entries: Vec<Value> = rows
        .into_iter()
        .filter(|e| !truthy(e, "archived_at"))
        .filter(|e| match &project {
            Some(p) => text_at(e, "project") == Some(p.as_str()),
            None => true,
        })
        .collect();
    // The map is the recall surface (rollup scoring, epic bonus on): the
    // filing-time dedup net would silently drop the epic-sibling edges the
    // readers replay.
    let mapping = super::relatedness::build_map(&entries, top_k);
    let path = relatedness_path();
    let write = serde_json::to_string_pretty(&mapping).unwrap_or_else(|_| "{}".into());
    // Temp + rename: a concurrent filing-time reader never sees a truncated
    // sidecar (it degrades to the all-live fallback on a partial read).
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let tmp = path.with_extension("json.tmp");
    if let Err(e) = std::fs::write(&tmp, &write) {
        eprintln!("Error: could not write {tmp:?}: {e}");
        return 1;
    }
    if let Err(e) = std::fs::rename(&tmp, &path) {
        let _ = std::fs::remove_file(&tmp);
        eprintln!("Error: could not replace {path:?}: {e}");
        return 1;
    }
    if json_out {
        println!(
            "{}",
            serde_json::to_string_pretty(&mapping).unwrap_or_else(|_| "{}".into())
        );
    } else {
        let edges: usize = mapping.values().map(Vec::len).sum();
        println!(
            "relatedness: {} nodes, {edges} edges -> {}",
            mapping.len(),
            path.display()
        );
    }
    0
}

fn relatedness_get(tail: &[String]) -> i32 {
    let mut node_id: Option<String> = None;
    let mut top_k = 5usize;
    let mut json_out = false;
    let mut it = tail.iter();
    while let Some(arg) = it.next() {
        let (flag, inline) = match arg.split_once('=') {
            Some((f, v)) if f.starts_with("--") => (f, Some(v.to_string())),
            _ => (arg.as_str(), None),
        };
        match flag {
            "-h" | "--help" => {
                print!("{RELATEDNESS_GET_HELP}");
                return 0;
            }
            "--top-k" | "-K" => {
                top_k = inline
                    .or_else(|| it.next().cloned())
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(5);
            }
            "-J" | "--json" => json_out = true,
            other if other.starts_with('-') && other.len() > 1 => {
                eprintln!("No such option: {other}");
                return 2;
            }
            _ if node_id.is_none() => node_id = Some(arg.clone()),
            _ => {
                eprintln!("Got unexpected extra argument ({arg})");
                return 2;
            }
        }
    }
    let Some(node_id) = node_id else {
        eprintln!("Missing argument 'NODE_ID'.");
        return 2;
    };
    let path = relatedness_path();
    let Ok(text) = std::fs::read_to_string(&path) else {
        // No map: the caller's fallback signal.
        return 1;
    };
    let Ok(mapping) = serde_json::from_str::<BTreeMap<String, Vec<Value>>>(&text) else {
        return 1;
    };
    let edges = mapping.get(&node_id).cloned().unwrap_or_default();
    let edges: Vec<&Value> = edges.iter().take(top_k).collect();
    if json_out {
        println!(
            "{}",
            serde_json::to_string_pretty(&edges).unwrap_or_else(|_| "[]".into())
        );
        return 0;
    }
    for e in edges {
        let id = e.get("id").and_then(Value::as_str).unwrap_or("");
        let score = e.get("score").map(|s| s.to_string()).unwrap_or_default();
        let reason = e.get("reason").and_then(Value::as_str).unwrap_or("");
        println!("{id}\t{score}\t{reason}");
    }
    0
}

// ---------------------------------------------------------------------------
// ready - the selection read (the keeper's own ready op, in-process)
// ---------------------------------------------------------------------------

const READY_HELP: &str = "Usage: fno backlog ready [--project NAME] [--all] [--roadmap-id ID] [--parent ID] [--ideas] [--include-deferred] [--mission ID] [--json]

List dispatch-ready rows (the same filters and ranking `next` runs, without
the claim). Ideas ride only with --ideas; deferred only with
--include-deferred.

Options:
  --project, -p NAME      Filter by project
  --all, -A               Every project
  --roadmap-id ID         Restrict to one roadmap
  --parent ID             Transitive children of this epic
  --ideas, -I             Also list idea-stage rows
  --include-deferred      Also list deferred rows
  --mission ID            Restrict to nodes whose mission_id matches
  -J, --json              JSON rows (default; the flag is accepted for parity)
  -h, --help              Print help
";

pub fn run_ready(tail: &[String]) -> i32 {
    let mut project: Option<String> = None;
    let mut all = false;
    let mut roadmap_id: Option<String> = None;
    let mut parent: Option<String> = None;
    let mut mission: Option<String> = None;
    let mut include_ideas = false;
    let mut include_deferred = false;
    let mut it = tail.iter();
    while let Some(arg) = it.next() {
        let (flag, inline) = match arg.split_once('=') {
            Some((f, v)) if f.starts_with("--") => (f, Some(v.to_string())),
            _ => (arg.as_str(), None),
        };
        match flag {
            "-h" | "--help" => {
                print!("{READY_HELP}");
                return 0;
            }
            "--project" | "-p" => {
                project = Some(inline.or_else(|| it.next().cloned()).unwrap_or_default());
            }
            "--all" | "-A" => all = true,
            "--roadmap-id" => {
                roadmap_id = Some(inline.or_else(|| it.next().cloned()).unwrap_or_default());
            }
            "--parent" => {
                parent = Some(inline.or_else(|| it.next().cloned()).unwrap_or_default());
            }
            "--mission" => {
                mission = Some(inline.or_else(|| it.next().cloned()).unwrap_or_default());
            }
            "--ideas" | "-I" | "--include-ideas" => include_ideas = true,
            "--include-deferred" => include_deferred = true,
            "--json" | "-J" => {}
            other if other.starts_with('-') && other.len() > 1 => {
                eprintln!("No such option: {other}");
                return 2;
            }
            _ => {
                eprintln!("Got unexpected extra argument ({arg})");
                return 2;
            }
        }
    }
    // External backends share the Rust filters and ranking with `next`; the
    // joined-candidate read keeps the wheel until its own arm lands.
    if super::workflows::active_backend_name() != "graph" {
        return super::cli::forward_to_python("ready", tail);
    }
    let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
    let repo_root = super::create_cli::repo_root(&cwd);
    let config_dir = crate::claude_roster::config_dir();
    let staleness_days = crate::backlog_ready::configured_staleness_days(&config_dir);
    let fno_dir = settings::graph_path()
        .parent()
        .map(std::path::Path::to_path_buf)
        .unwrap_or_else(|| cwd.clone());
    let held = crate::needs::held_map(&fno_dir, &cwd);
    let claimed: std::collections::BTreeSet<String> =
        match crate::claims::list_strict(Some("node:"), None, false) {
            Ok(records) => records
                .iter()
                .filter_map(|rec| rec.key.strip_prefix("node:").map(str::to_string))
                .collect(),
            Err(e) => {
                eprintln!("Error: live claim state is unavailable; ready selection refused: {e}");
                return 1;
            }
        };
    let opts = crate::backlog_ready::ReadyOpts {
        project,
        all,
        roadmap_id,
        parent,
        mission,
        include_ideas,
        include_deferred,
        repo_root: Some(repo_root),
        claimed,
        staleness_days,
        now_ms: crate::claims::now_ms(),
        held,
    };
    let graph = settings::graph_path();
    let Ok(rows) = graph_store::read_rows(&graph) else {
        eprintln!("Error: the backlog graph could not be read");
        return 1;
    };
    match crate::backlog_ready::select(&rows, &opts) {
        Ok(reply) => {
            println!(
                "{}",
                serde_json::to_string_pretty(&reply.rows).unwrap_or_else(|_| "[]".into())
            );
            0
        }
        Err(crate::backlog_ready::NoSuchParent(parent)) => {
            eprintln!("Error: no such node '{parent}'");
            1
        }
    }
}

// ---------------------------------------------------------------------------
// remove - the hard delete
// ---------------------------------------------------------------------------

const REMOVE_HELP: &str = "Usage: fno backlog remove TASK_ID [--force]

Delete a node from the graph permanently. This verb exists and works.

Options:
  -F, --force  Skip cascade warning
  -h, --help   Print help
";

pub fn run_remove(tail: &[String]) -> i32 {
    let mut task_id: Option<String> = None;
    let mut force = false;
    let mut it = tail.iter();
    while let Some(arg) = it.next() {
        let (flag, _inline) = match arg.split_once('=') {
            Some((f, v)) if f.starts_with("--") => (f, Some(v.to_string())),
            _ => (arg.as_str(), None),
        };
        match flag {
            "-h" | "--help" => {
                print!("{REMOVE_HELP}");
                return 0;
            }
            "--force" | "-F" => force = true,
            other if other.starts_with('-') && other.len() > 1 => {
                eprintln!("No such option: {other}");
                return 2;
            }
            _ if task_id.is_none() => task_id = Some(arg.clone()),
            _ => {
                eprintln!("Got unexpected extra argument ({arg})");
                return 2;
            }
        }
    }
    let Some(task_id) = task_id else {
        eprintln!("Missing argument 'TASK_ID'.");
        return 2;
    };
    if !has_node_id_prefix(&task_id) {
        eprintln!("Error: task_id must be a <prefix>-<4..8 hex> node id, got '{task_id}'");
        return 1;
    }
    if let Some(code) = refuse_non_graph_backend("remove") {
        return code;
    }
    let graph = settings::graph_path();
    let Ok(rows) = graph_store::read_rows(&graph) else {
        eprintln!("Error: the backlog graph could not be read");
        return 1;
    };
    let dependents: Vec<String> = rows
        .iter()
        .filter(|e| {
            e.get("blocked_by")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .any(|b| b == task_id)
        })
        .filter_map(|e| text_at(e, "id"))
        .map(String::from)
        .collect();
    if !dependents.is_empty() && !force {
        eprintln!(
            "Removing {task_id} will orphan blocked_by in: {}",
            dependents.join(", ")
        );
        eprintln!("Use --force to confirm.");
        return 1;
    }
    let target = task_id.clone();
    let freed: std::cell::RefCell<Vec<String>> = std::cell::RefCell::new(Vec::new());
    let applied = graph_store::mutate_rows(
        &graph,
        std::time::Duration::from_secs(5),
        None,
        None,
        |working| {
            if find_node(working, &target).is_none() {
                return Err(graph_store::StoreError::Invalid(format!(
                    "feature {target} not found"
                )));
            }
            for e in working.iter_mut() {
                if let Some(blocked) = e.get_mut("blocked_by").and_then(Value::as_array_mut) {
                    blocked.retain(|b| b.as_str() != Some(target.as_str()));
                }
                // related is symmetric; a peer naming a node the graph no
                // longer has cannot repair itself.
                if let Some(related) = e.get_mut("related").and_then(Value::as_array_mut) {
                    related.retain(|r| r.as_str() != Some(target.as_str()));
                }
                let obj = match e.as_object_mut() {
                    Some(o) => o,
                    None => continue,
                };
                // A dependent's origin must resolve, never dangle.
                if obj.get("source_node_id").and_then(Value::as_str) == Some(target.as_str()) {
                    obj.insert("source_node_id".into(), Value::Null);
                }
                // Containment: a freed child is reported, never orphaned
                // silently.
                if obj.get("contained_in").and_then(Value::as_str) == Some(target.as_str()) {
                    obj.insert("contained_in".into(), Value::Null);
                    if let Some(id) = obj.get("id").and_then(Value::as_str) {
                        freed.borrow_mut().push(id.to_string());
                    }
                }
            }
            working.retain(|e| text_at(e, "id") != Some(target.as_str()));
            Ok(true)
        },
    );
    if let Err(err) = applied {
        eprintln!("Error: {err}");
        return 1;
    }
    for id in &freed.into_inner() {
        eprintln!("freed contained child {id} (owner removed)");
    }
    if dependents.is_empty() {
        println!("Removed {task_id}");
    } else {
        println!(
            "Removed {task_id} (orphaned deps in {})",
            dependents.join(", ")
        );
    }
    0
}

// ---------------------------------------------------------------------------
// project-root - the raw work-map lookup
// ---------------------------------------------------------------------------

const PROJECT_ROOT_HELP: &str = "Usage: fno backlog project-root PROJECT

Print a project's work-map root, or exit 1 (empty stdout) if unmapped.
The pure lookup, no cwd fallback: empty means exactly \"unmapped\".
";

pub fn run_project_root(tail: &[String]) -> i32 {
    let args: Vec<&String> = tail
        .iter()
        .filter(|a| *a != "--help" && *a != "-h")
        .collect();
    if tail.iter().any(|a| a == "--help" || a == "-h") {
        print!("{PROJECT_ROOT_HELP}");
        return 0;
    }
    let Some(project) = args.first() else {
        eprintln!("Missing argument 'PROJECT'.");
        return 2;
    };
    match project_root_from_settings(project) {
        Some(root) => {
            println!("{root}");
            0
        }
        None => 1,
    }
}

/// The inverse work-map lookup (`_intake.project_root_from_settings`): the
/// same candidate walk as the forward direction, first `name` match wins,
/// abspath with the home expanded. Pure map lookup: no stat, no git.
fn project_root_from_settings(project: &str) -> Option<String> {
    if project.is_empty() {
        return None;
    }
    for doc in settings::config_candidates() {
        let Some(workspaces) = doc
            .get("work")
            .and_then(|w| w.get("workspaces"))
            .and_then(Value::as_object)
        else {
            continue;
        };
        for (_, ws) in workspaces {
            let Some(projects) = ws.get("projects").and_then(Value::as_array) else {
                continue;
            };
            for p in projects {
                let name = p.get("name").and_then(Value::as_str).unwrap_or("");
                if name != project {
                    continue;
                }
                if let Some(path) = p.get("path").and_then(Value::as_str) {
                    let expanded = shellexpand_home(path);
                    return Some(absolutize(&expanded));
                }
            }
        }
    }
    None
}

fn shellexpand_home(path: &str) -> String {
    if let Some(rest) = path.strip_prefix('~') {
        if let Some(home) = std::env::var_os("HOME") {
            return format!("{}{}", home.to_string_lossy(), rest);
        }
    }
    path.to_string()
}

fn absolutize(path: &str) -> String {
    let p = Path::new(path);
    if p.is_absolute() {
        return path.to_string();
    }
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    cwd.join(p).display().to_string()
}
