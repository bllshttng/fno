//! `epic status`: the one-table children rollup over an epic's children.
//!
//! The wheel's `cmd_epic_status` + its six private helpers, ported whole:
//! resolution, container refusal, the events union (global mirror + child
//! project journals), live-worker reads, merge-unconfirmed flags, receipt
//! rendering, breaker streaks, `--verify-merges`, the scope-growth metric
//! over entries + archive, and both output shapes.

use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

const EPIC_STATUS_HELP: &str = "Usage: fno backlog epic status <epic> [--json|-J] [--verify-merges]

One table over an epic's children: status, worker, PR, and an inline
dispatch receipt (or breaker streak) so an idle/deferred child is never a
silent blank. Refuses a non-container node by name.

A child reads `done` at finalize, not at merge, so a done child
whose PR GitHub never confirmed merged is flagged `merge unconfirmed` - the
state in which the graph lies at a wave gate. --verify-merges resolves those
rows against GitHub (one gh pr view per FLAGGED row only); without it the
whole table is local and needs no network.
";

/// Node-keyed dispatch receipts (all carry data.node_id). termination keys on
/// session_id, not node_id, so it never matches a child row and is left out.
const RECEIPT_TYPES: [&str; 6] = [
    "advance_dispatched",
    "advance_skipped",
    "advance_failed",
    "quota_deferred",
    "dispatch_deferred",
    "quota_rotation_declined",
];

// Below this fraction of the epic's window carrying a joinable origin, the
// growth figure is withheld: at low capture a small number is indistinguishable
// from a missed one, and it errs low. A constant rather than config, so a
// low-capture project cannot tune the check away.
const SCOPE_GROWTH_COVERAGE_FLOOR: f64 = 0.50;

/// ScopeGrowth (the wheel's rollup.ScopeGrowth): the follow-up work an epic
/// accumulated after decomposition, with the evidence that qualifies it.
struct ScopeGrowth {
    follow_up_ids: Option<Vec<String>>,
    window_total: usize,
    window_with_origin: usize,
    window_dangling: usize,
    coverage: f64,
    reportable: bool,
    realized_nodes: usize,
    realized_prs: usize,
    declared_size: Option<String>,
}

/// ``source_node_id`` inverted: origin id -> the nodes that name it.
fn origin_index(entries: &[Value]) -> BTreeMap<&str, Vec<&Value>> {
    let mut out: BTreeMap<&str, Vec<&Value>> = BTreeMap::new();
    for e in entries {
        if let Some(src) = e.get("source_node_id").and_then(Value::as_str) {
            out.entry(src).or_default().push(e);
        }
    }
    out
}

/// Every node under ``root_id`` by ``parent``, transitively.
fn parent_descendants(entries: &[Value], root_id: &str) -> BTreeSet<String> {
    let mut by_parent: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for e in entries {
        if let (Some(p), Some(id)) = (
            e.get("parent").and_then(Value::as_str),
            e.get("id").and_then(Value::as_str),
        ) {
            by_parent.entry(p).or_default().push(id);
        }
    }
    let mut out = BTreeSet::new();
    let mut frontier = vec![root_id.to_string()];
    while let Some(current) = frontier.pop() {
        if let Some(children) = by_parent.get(current.as_str()) {
            for child in children {
                if out.insert((*child).to_string()) {
                    frontier.push((*child).to_string());
                }
            }
        }
    }
    out
}

/// The scope-growth metric over entries (working graph + archive): the
/// follow-up set is every node reachable from the epic's descendants by
/// ``source_node_id``, minus the descendants themselves, and coverage is
/// measured over the epic's window (nodes created at or after it).
fn scope_growth(entries: &[Value], epic_id: &str) -> ScopeGrowth {
    let descendants = parent_descendants(entries, epic_id);
    let by_origin = origin_index(entries);

    let mut follow_ups: BTreeSet<String> = BTreeSet::new();
    let mut seen: BTreeSet<String> = descendants.clone();
    seen.insert(epic_id.to_string());
    let mut frontier: Vec<String> = descendants.iter().cloned().collect();
    while let Some(current) = frontier.pop() {
        if let Some(kids) = by_origin.get(current.as_str()) {
            for child in kids {
                let Some(child_id) = child.get("id").and_then(Value::as_str) else {
                    continue;
                };
                if seen.insert(child_id.to_string()) {
                    follow_ups.insert(child_id.to_string());
                    frontier.push(child_id.to_string());
                }
            }
        }
    }

    let by_id: BTreeMap<&str, &Value> = entries
        .iter()
        .filter_map(|e| e.get("id").and_then(Value::as_str).map(|i| (i, e)))
        .collect();
    let epic_born = by_id
        .get(epic_id)
        .and_then(|e| e.get("created_at").and_then(Value::as_str))
        .unwrap_or("");
    // An undated epic has no window to measure, so coverage is undefined
    // rather than total: a >= comparison over "" would otherwise admit the
    // entire graph and report a confident number over a population that was
    // never the epic's.
    let window: Vec<&Value> = if epic_born.is_empty() {
        Vec::new()
    // Counted by JOINABILITY, not truthiness: an origin naming a node the
    // graph no longer has contributes nothing to growth.
    } else {
        entries
            .iter()
            .filter(|e| {
                e.get("id").and_then(Value::as_str) != Some(epic_id)
                    && (e.get("created_at").and_then(Value::as_str).unwrap_or("")) >= epic_born
            })
            .collect()
    };
    let with_origin = window
        .iter()
        .filter(|e| {
            e.get("source_node_id")
                .and_then(Value::as_str)
                .is_some_and(|s| by_id.contains_key(s))
        })
        .count();
    let dangling = window
        .iter()
        .filter(|e| {
            e.get("source_node_id")
                .and_then(Value::as_str)
                .is_some_and(|s| !by_id.contains_key(s))
        })
        .count();
    let coverage = if window.is_empty() {
        0.0
    } else {
        with_origin as f64 / window.len() as f64
    };
    let reportable = coverage >= SCOPE_GROWTH_COVERAGE_FLOOR;
    let realized: Vec<&Value> = entries
        .iter()
        .filter(|e| {
            e.get("id")
                .and_then(Value::as_str)
                .is_some_and(|i| descendants.contains(i))
        })
        .collect();
    ScopeGrowth {
        follow_up_ids: if reportable {
            Some(follow_ups.into_iter().collect())
        } else {
            None
        },
        window_total: window.len(),
        window_with_origin: with_origin,
        window_dangling: dangling,
        coverage,
        reportable,
        realized_nodes: realized.len(),
        realized_prs: distinct_pr_refs(&realized),
        declared_size: by_id
            .get(epic_id)
            .and_then(|e| e.get("size").and_then(Value::as_str))
            .map(String::from),
    }
}

/// Distinct PR references across ``rows``, scoped by project (the wheel's
/// rollup._pr_refs): the primary pr_number plus one per additional_prs row.
fn distinct_pr_refs(rows: &[&Value]) -> usize {
    let mut refs: BTreeSet<(Option<&str>, i64)> = BTreeSet::new();
    for e in rows {
        let project = e.get("project").and_then(Value::as_str);
        if let Some(num) = e.get("pr_number").and_then(Value::as_i64) {
            refs.insert((project, num));
        }
        if let Some(extras) = e.get("additional_prs").and_then(Value::as_array) {
            for extra in extras {
                if let Some(num) = extra.get("number").and_then(Value::as_i64) {
                    refs.insert((project, num));
                }
            }
        }
    }
    refs.len()
}

// ---------------------------------------------------------------------------
// Events: the union the child notes read
// ---------------------------------------------------------------------------
/// Tolerant JSONL read: a malformed line is skipped, never raised - the
/// events union is advisory and must not break the table over one corrupt row.
fn read_jsonl(path: &std::path::Path) -> Vec<Value> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    text.lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .collect()
}

/// The events union (the wheel's _epic_events): the global mirror plus each
/// child project's journal, deduped by content, ts-sorted (None-safe) so
/// newest-wins holds across files.
fn epic_events(children: &[&Value]) -> Vec<Value> {
    let mut seen_paths: BTreeSet<String> = BTreeSet::new();
    let mut seen_env: BTreeSet<String> = BTreeSet::new();
    let mut out: Vec<Value> = Vec::new();
    let mut ingest = |path: &std::path::Path| {
        if !seen_paths.insert(path.display().to_string()) {
            return;
        }
        for e in read_jsonl(path) {
            let key = serde_json::to_string(&e).unwrap_or_else(|_| e.to_string());
            if seen_env.insert(key) {
                out.push(e);
            }
        }
    };

    // The global mirror: the configured state dir, then the agents-home
    // mirror when it differs (the wheel's _default_event_paths pair).
    let mut global_paths: Vec<std::path::PathBuf> = Vec::new();
    if let Some(state) = super::settings::state_dir() {
        global_paths.push(state.join("events.jsonl"));
    }
    if let Ok(home) = std::env::var("HOME") {
        global_paths.push(std::path::Path::new(&home).join(".fno/events.jsonl"));
    }
    for p in &global_paths {
        ingest(p);
    }
    for c in children {
        let root = c
            .get("project")
            .and_then(Value::as_str)
            .and_then(super::settings::project_root)
            .or_else(|| {
                c.get("_resolved_cwd")
                    .and_then(Value::as_str)
                    .map(String::from)
            })
            .or_else(|| c.get("cwd").and_then(Value::as_str).map(String::from));
        if let Some(root) = root {
            let journal = std::path::Path::new(&root).join(".fno/events.jsonl");
            ingest(&journal);
        }
    }
    out.sort_by(|a, b| {
        a.get("ts")
            .and_then(Value::as_str)
            .unwrap_or("")
            .cmp(b.get("ts").and_then(Value::as_str).unwrap_or(""))
    });
    out
}

// ---------------------------------------------------------------------------
// Child notes: streaks, receipts, the live worker, merge unconfirmed
// ---------------------------------------------------------------------------

/// One node-scoped event classification (the wheel's failure._Ev): a failure
/// inflates the breaker streak, a reset (a success close or an undefer) ends
/// it, and everything else rides through.
struct Ev {
    node_id: String,
    kind_fail: bool,
}

fn classify(raw: &Value) -> Option<Ev> {
    let etype = raw
        .get("type")
        .and_then(Value::as_str)
        .or_else(|| raw.get("kind").and_then(Value::as_str))?;
    let default_data = serde_json::Map::new();
    let data = raw
        .get("data")
        .and_then(Value::as_object)
        .unwrap_or(&default_data);
    let node_id = data
        .get("unit_id")
        .or_else(|| data.get("node_id"))
        .or_else(|| data.get("graph_node_id"))
        .and_then(Value::as_str)?;
    if node_id.is_empty() {
        return None;
    }
    let kind_fail = match etype {
        "node_failed" => true,
        "node_undeferred" => false,
        "node_closed" => match data.get("close").and_then(Value::as_str) {
            Some("parked") => true,
            Some("closed") => false,
            _ => return None,
        },
        _ => return None,
    };
    Some(Ev {
        node_id: node_id.to_string(),
        kind_fail,
    })
}

/// Consecutive failure events since the most recent reset boundary, scanning
/// newest -> oldest (the wheel's failure.consecutive_failures).
fn consecutive_failures(node_id: &str, events: &[Value]) -> usize {
    let mut count = 0;
    for raw in events.iter().rev() {
        let Some(ev) = classify(raw) else {
            continue;
        };
        if ev.node_id != node_id {
            continue;
        }
        if !ev.kind_fail {
            break;
        }
        count += 1;
    }
    count
}

/// The most recent dispatch/skip/failure receipt for a node, or None (the
/// wheel's _latest_receipt): events are ts-sorted oldest -> newest, so the
/// last matching envelope is the newest receipt.
fn latest_receipt(node_id: &str, events: &[Value]) -> Option<String> {
    let mut latest: Option<&Value> = None;
    for e in events {
        let Some(etype) = e.get("type").and_then(Value::as_str) else {
            continue;
        };
        if !RECEIPT_TYPES.contains(&etype) {
            continue;
        }
        let data = e.get("data")?;
        if data.get("node_id").and_then(Value::as_str) != Some(node_id) {
            continue;
        }
        latest = Some(e);
    }
    latest.map(|e| {
        format_receipt(
            e.get("type").and_then(Value::as_str).unwrap_or(""),
            e.get("data").unwrap_or(&Value::Null),
        )
    })
}

/// The receipt cell (the wheel's _format_receipt).
fn format_receipt(etype: &str, data: &Value) -> String {
    if etype == "advance_dispatched" {
        let who = data
            .get("agent_name")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .or_else(|| data.get("short_id").and_then(Value::as_str))
            .unwrap_or("worker");
        return format!("dispatched {who}");
    }
    if etype == "advance_skipped" {
        let reason = data.get("reason").and_then(Value::as_str).unwrap_or("?");
        return format!("skipped: {reason}");
    }
    if etype == "advance_failed" {
        let err = data
            .get("error")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim()
            .to_string();
        return if err.is_empty() {
            "failed".to_string()
        } else {
            let cut = err
                .char_indices()
                .nth(80)
                .map(|(i, _)| i)
                .unwrap_or(err.len());
            format!("failed: {}", &err[..cut])
        };
    }
    if etype == "quota_rotation_declined" {
        let prov = data.get("provider").and_then(Value::as_str).unwrap_or("");
        return if prov.is_empty() {
            "declined".to_string()
        } else {
            format!("declined: {prov}")
        };
    }
    // quota_deferred / dispatch_deferred
    let prov = data
        .get("provider")
        .and_then(Value::as_str)
        .or_else(|| data.get("owner_harness").and_then(Value::as_str))
        .unwrap_or("");
    if prov.is_empty() {
        "deferred".to_string()
    } else {
        format!("deferred: {prov}")
    }
}

/// True when a done child carries a PR that GitHub never confirmed merged
/// (the wheel's _merge_unconfirmed): the graph lie this table exists to flag.
fn merge_unconfirmed(child: &Value) -> bool {
    child.get("status").and_then(Value::as_str) == Some("done")
        && child.get("pr_number").and_then(Value::as_i64).is_some()
        && child.get("merge_status").and_then(Value::as_str) != Some("merged")
}

/// Ask GitHub what actually happened to a flagged child's PR (the wheel's
/// _verify_merge). A probe that could not answer returns None and the caller
/// leaves the row FLAGGED: a failed probe is not evidence the PR merged.
fn verify_merge(child: &Value) -> Option<String> {
    let pr = child.get("pr_number")?.to_string();
    let root = child
        .get("project")
        .and_then(Value::as_str)
        .and_then(super::settings::project_root)
        .or_else(|| {
            child
                .get("_resolved_cwd")
                .and_then(Value::as_str)
                .map(String::from)
        })
        .or_else(|| child.get("cwd").and_then(Value::as_str).map(String::from))?;
    let cwd = std::path::PathBuf::from(root);
    let (ok, out, err) = crate::pr_push::run_labeled(
        "epic-status",
        "gh",
        &["pr", "view", &pr, "--json", "state,mergedAt"],
        &cwd,
        std::time::Duration::from_secs(30),
    )
    .ok()?;
    if !ok || out.trim().is_empty() {
        return None;
    }
    let payload: Value = serde_json::from_str(out.trim()).ok()?;
    if payload.get("mergedAt").and_then(Value::as_str).is_some()
        || payload.get("state").and_then(Value::as_str) == Some("MERGED")
    {
        return Some(format!("merged (unstamped) #{pr}"));
    }
    let state = payload
        .get("state")
        .and_then(Value::as_str)
        .unwrap_or("UNKNOWN");
    Some(format!("{state} #{pr}"))
}

/// The holder of a live/suspect node:<id> claim, else None (the wheel's
/// _live_worker): a status read must never crash the table.
fn live_worker(node_id: &str) -> Option<String> {
    let key = format!("node:{node_id}");
    let (state, record) = crate::claims::status(&key, None);
    match state {
        crate::claims::ClaimState::Live | crate::claims::ClaimState::Suspect => {
            record.map(|r| r.holder)
        }
        _ => None,
    }
}

/// The inline note for a child row (the wheel's _child_note): streak
/// (deferred), receipt (idle ready), merge-unconfirmed (done, unstamped PR),
/// or "-". Never blank for an idle ready child.
fn child_note(child: &Value, events: &[Value], worker: &Option<String>) -> String {
    let Some(node_id) = child.get("id").and_then(Value::as_str) else {
        return "-".to_string();
    };
    match child.get("status").and_then(Value::as_str) {
        Some("deferred") => {
            format!("streak {}", consecutive_failures(node_id, events))
        }
        Some("ready") if worker.is_none() => {
            latest_receipt(node_id, events).unwrap_or_else(|| "no receipt found".to_string())
        }
        _ if merge_unconfirmed(child) => {
            format!(
                "merge unconfirmed #{}",
                child
                    .get("pr_number")
                    .map(|v| v.to_string())
                    .unwrap_or_default()
            )
        }
        _ => "-".to_string(),
    }
}

// ---------------------------------------------------------------------------
// The verb
// ---------------------------------------------------------------------------

/// One table over an epic's children (the wheel's cmd_epic_status).
pub fn run_epic(tail: &[String]) -> i32 {
    let Some(sub) = tail.first() else {
        print!("{EPIC_STATUS_HELP}");
        return 0;
    };
    if sub != "status" {
        eprintln!("No such command '{sub}'.");
        return 2;
    }
    let rest = &tail[1..];
    if rest.iter().any(|a| a == "--help" || a == "-h") {
        print!("{EPIC_STATUS_HELP}");
        return 0;
    }
    let json_out = rest.iter().any(|a| a == "--json" || a == "-J");
    let verify = rest.iter().any(|a| a == "--verify-merges");
    let Some(epic) = rest.iter().find(|a| !a.starts_with('-')) else {
        eprintln!("Missing argument 'EPIC'.");
        return 2;
    };
    let store = super::api::Store::new(&super::settings::graph_path());
    let rows = match super::api::rows(&store) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("Error: {}", e.0);
            return 1;
        }
    };
    // The working-graph view: archived residents join the METRIC read only
    // (api::rows returns every row; the caller filters), so a swept child
    // keeps counting and the number does not quietly change with grooming.
    let live: Vec<&Value> = rows
        .iter()
        .filter(|r| r.get("archived_at").and_then(Value::as_str).is_none())
        .collect();
    let resolved = live
        .iter()
        .copied()
        .find(|e| e.get("id").and_then(Value::as_str) == Some(epic))
        .or_else(|| {
            live.iter()
                .copied()
                .find(|e| e.get("slug").and_then(Value::as_str) == Some(epic))
        })
        .or_else(|| super::node_ref::resolve_tiers(&rows, epic));
    let Some(epic_row) = resolved else {
        eprintln!("epic status: no node matches '{epic}'");
        return 1;
    };
    let epic_id = epic_row
        .get("id")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    // A container is a node with children OR an epic-typed node not yet
    // decomposed (childless but legitimately queryable -> "(no children)").
    // A genuine leaf (feature/bug/... with no children) is refused by name.
    let container_ids: BTreeSet<&str> = live
        .iter()
        .filter_map(|e| {
            let p = e.get("parent")?.as_str()?;
            (e.get("contained_in").and_then(Value::as_str) != Some(p)).then_some(p)
        })
        .collect();
    let is_epic_type = epic_row.get("type").and_then(Value::as_str) == Some("epic");
    if !container_ids.contains(epic_id.as_str()) && !is_epic_type {
        eprintln!(
            "epic status: {epic_id} is a leaf, not a container (an epic's work lives in its children)."
        );
        return 1;
    }

    let mut children: Vec<&Value> = live
        .iter()
        .filter(|e| e.get("parent").and_then(Value::as_str) == Some(epic_id.as_str()))
        .collect();
    children.sort_by_key(|c| c.get("id").and_then(Value::as_str).unwrap_or(""));
    let events = epic_events(&children);

    let total = children.len();
    let done = children
        .iter()
        .filter(|c| c.get("status").and_then(Value::as_str) == Some("done"))
        .count();
    struct EpicRow {
        slug: String,
        id: String,
        project: String,
        status: String,
        worker: Option<String>,
        pr: Option<String>,
        pr_value: Value,
        unconfirmed: bool,
        note: String,
    }
    let mut table_rows: Vec<EpicRow> = Vec::new();
    for c in &children {
        let node_id = c.get("id").and_then(Value::as_str).unwrap_or("");
        let worker = live_worker(node_id);
        let unconfirmed = merge_unconfirmed(c);
        let mut note = child_note(c, &events, &worker);
        if unconfirmed && verify {
            // A probe that could not answer leaves the row flagged; only a
            // real answer replaces the note.
            if let Some(answer) = verify_merge(c) {
                note = answer;
            }
        }
        table_rows.push(EpicRow {
            slug: c
                .get("slug")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
            id: node_id.to_string(),
            project: c
                .get("project")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
            status: c
                .get("status")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
            worker,
            pr: c
                .get("pr_number")
                .filter(|v| !v.is_null())
                .map(|v| v.to_string()),
            pr_value: c.get("pr_number").cloned().unwrap_or(Value::Null),
            unconfirmed,
            note,
        });
    }
    let unconfirmed_total = table_rows.iter().filter(|r| r.unconfirmed).count();

    // The scope-growth metric reads through the archive (entries + archive =
    // the one rows read this arm already holds); the children table stays
    // working-graph only.
    let growth = scope_growth(&rows, &epic_id);

    if json_out {
        let children_json: Vec<Value> = table_rows
            .iter()
            .map(|r| {
                serde_json::json!({
                    "id": r.id,
                    "slug": r.slug,
                    "project": r.project,
                    "status": r.status,
                    "worker": r.worker,
                    "pr_number": r.pr_value,
                    "merge_unconfirmed": r.unconfirmed,
                    "receipt": r.note,
                })
            })
            .collect();
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "epic": epic_id,
                "slug": epic_row.get("slug").and_then(Value::as_str),
                "children_total": total,
                "children_done": done,
                "children_merge_unconfirmed": unconfirmed_total,
                "merges_verified": verify,
                "children": children_json,
                "scope_growth": {
                    "follow_ups": growth.follow_up_ids.as_ref().map(|f| f.len()),
                    "follow_up_ids": growth.follow_up_ids,
                    "reportable": growth.reportable,
                    "coverage": (growth.coverage * 10000.0).round() / 10000.0,
                    "window_total": growth.window_total,
                    "window_with_origin": growth.window_with_origin,
                    "window_dangling": growth.window_dangling,
                    "realized_nodes": growth.realized_nodes,
                    "realized_prs": growth.realized_prs,
                    "declared_size": growth.declared_size,
                },
            }))
            .unwrap_or_else(|_| "{}".to_string())
        );
        return 0;
    }

    let mut header = format!(
        "epic: {epic_id} ({})  {done}/{total} done",
        epic_row.get("slug").and_then(Value::as_str).unwrap_or("")
    );
    if unconfirmed_total > 0 {
        // Qualifies the done count in the same breath rather than a line
        // below it: an unqualified "3/5 done" is what let the graph read as
        // landed at a wave gate.
        header += &format!("  ({unconfirmed_total} merge unconfirmed)");
    }
    println!("{header}");
    println!("  {}", scope_growth_line(&growth));
    if table_rows.is_empty() {
        println!("  (no children)");
        return 0;
    }
    let headers = ["child", "project", "status", "worker", "PR", "note"];
    let cells_of = |r: &EpicRow| -> Vec<String> {
        vec![
            if r.slug.is_empty() {
                r.id.clone()
            } else {
                format!("{} ({})", r.slug, r.id)
            },
            r.project.clone(),
            r.status.clone(),
            r.worker.clone().unwrap_or_else(|| "-".to_string()),
            r.pr.clone().unwrap_or_else(|| "-".to_string()),
            r.note.clone(),
        ]
    };
    let mut table: Vec<Vec<String>> = vec![headers.iter().map(|h| h.to_string()).collect()];
    table.extend(table_rows.iter().map(cells_of));
    let width = |col: usize| -> usize {
        table
            .iter()
            .map(|row| row[col].chars().count())
            .max()
            .unwrap_or(0)
    };
    let widths: Vec<usize> = (0..headers.len()).map(width).collect();
    for row in &table {
        let line: Vec<String> = row
            .iter()
            .enumerate()
            .map(|(i, cell)| {
                let pad = widths[i].saturating_sub(cell.chars().count());
                format!("{cell}{}", " ".repeat(pad))
            })
            .collect();
        println!("  {}", line.join("  "));
    }
    0
}

/// One line: the growth figure with its coverage, or why it is withheld (the
/// wheel's _scope_growth_line). The coverage clause is never dropped - a bare
/// count reads as measured.
fn scope_growth_line(growth: &ScopeGrowth) -> String {
    let mut pct = format!(
        "{:.0}% of {} window nodes",
        growth.coverage * 100.0,
        growth.window_total
    );
    if growth.window_dangling > 0 {
        pct += &format!(", {} dangling", growth.window_dangling);
    }
    let cost = format!(
        "realized {} nodes / {} PRs{}",
        growth.realized_nodes,
        growth.realized_prs,
        growth
            .declared_size
            .as_deref()
            .map(|s| format!(" vs size {s}"))
            .unwrap_or_default()
    );
    if !growth.reportable {
        return format!(
            "scope growth: withheld (origin capture {pct}, below the 50% floor)  |  {cost}"
        );
    }
    let count = growth.follow_up_ids.as_ref().map(Vec::len).unwrap_or(0);
    format!("scope growth: {count} follow-ups (origin capture {pct})  |  {cost}")
}
