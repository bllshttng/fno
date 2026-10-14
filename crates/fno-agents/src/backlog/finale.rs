//! The finale arms: catalog commands whose wheel bodies outlived PR 3398's
//! wave-4 cutover. Each arm mirrors the wheel body's exits and renders (the
//! parity is the Python file this PR deletes), served with no compat forward.

use serde_json::{json, Map, Value};

use super::node_ref::{find_node, has_node_id_prefix};
use super::settings;
use super::workflows::{refuse_non_graph_backend, text_at};
use crate::claims;
use crate::graph_store;
use crate::lanes::LANE_SLOT_PREFIX;

// ---------------------------------------------------------------------------
// new - the scoped-add legacy word
// ---------------------------------------------------------------------------

pub fn run_new(tail: &[String]) -> i32 {
    // `new` resolves to add/scoped in the catalog (commands.rs from_legacy);
    // the create door already binds the project from the cwd, so the arm is
    // a straight delegate with no separate surface.
    super::create_cli::run(tail)
}

// ---------------------------------------------------------------------------
// lanes - the one-read parallel-lane rollup
// ---------------------------------------------------------------------------

const LANES_HELP: &str = "Usage: fno backlog lanes [--json|-J]

One-read parallel-lane rollup: live lane-slot claims joined with their graph
nodes (slug, status, PR), against the configured lane cap. Read-only.
";

pub fn run_lanes(tail: &[String]) -> i32 {
    if tail.iter().any(|a| a == "--help" || a == "-h") {
        print!("{LANES_HELP}");
        return 0;
    }
    let json_output = tail.iter().any(|a| a == "--json" || a == "-J");
    let max_lanes = super::advance_settings::load_merged(None)
        .pointer("/parallel/max_lanes")
        .and_then(Value::as_i64)
        .unwrap_or(1);
    // A graph read miss degrades to claims-only rows (the wheel's bare
    // except): the fleet shape still renders, node fields stay blank.
    let nodes: Map<String, Value> = {
        let store = super::api::Store::new(&settings::graph_path());
        match super::api::rows(&store) {
            Ok(rows) => rows
                .iter()
                .filter_map(|e| {
                    let id = text_at(e, "id")?;
                    Some((id.to_string(), e.clone()))
                })
                .collect(),
            Err(_) => Map::new(),
        }
    };
    let mut lane_claims = match claims::list(Some(LANE_SLOT_PREFIX), None, false) {
        Ok(rows) => rows,
        Err(e) => {
            eprintln!("lanes: claim read failed: {e}");
            return 1;
        }
    };
    lane_claims.sort_by(|a, b| a.key.cmp(&b.key));
    let mut lanes = Vec::with_capacity(lane_claims.len());
    for c in &lane_claims {
        let lane_id = c
            .metadata
            .get("lane_id")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let node = nodes.get(&lane_id).cloned().unwrap_or(Value::Null);
        let get_str = |k: &str| node.get(k).and_then(Value::as_str).map(String::from);
        lanes.push(json!({
            "slot": c.key,
            "lane_id": lane_id,
            "domain": c.metadata.get("domain").cloned().unwrap_or(Value::Null),
            "slug": get_str("slug"),
            "status": get_str("status"),
            "pr_number": node.get("pr_number").cloned().unwrap_or(Value::Null),
            "holder": c.holder,
        }));
    }
    let active = lanes.len();
    if json_output {
        println!(
            "{}",
            json!({"max_lanes": max_lanes, "active": active, "lanes": lanes})
        );
        return 0;
    }
    println!("lanes: {active}/{max_lanes} active");
    for ln in &lanes {
        let slot = ln["slot"].as_str().unwrap_or("");
        let lane_id = ln["lane_id"].as_str().unwrap_or("");
        let slug = ln["slug"]
            .as_str()
            .map(|s| format!("  {s}"))
            .unwrap_or_default();
        let domain = ln["domain"].as_str().unwrap_or("-");
        let status = ln["status"].as_str().unwrap_or("-");
        let pr = ln["pr_number"]
            .as_i64()
            .map(|n| format!("  pr#{n}"))
            .unwrap_or_default();
        println!("{slot}  {lane_id}{slug}  domain={domain}  {status}{pr}");
    }
    0
}

// ---------------------------------------------------------------------------
// stuck-epics - the read-only stuck-epic rollup
// ---------------------------------------------------------------------------

const STUCK_EPICS_HELP: &str = "Usage: fno backlog stuck-epics [--json|-J]

Epics whose only incomplete children are deferred/superseded. Read-only
surface for the operator: it NEVER closes, undefers, or re-parents anything.
";

/// The wheel's holds_epic_open: a child still represents outstanding work
/// unless its entry is terminal (done/superseded are never holders).
fn holds_epic_open(child: &Value) -> bool {
    !graph_store::is_terminal_entry(child)
}

pub fn run_stuck_epics(tail: &[String]) -> i32 {
    if tail.iter().any(|a| a == "--help" || a == "-h") {
        print!("{STUCK_EPICS_HELP}");
        return 0;
    }
    let json_output = tail.iter().any(|a| a == "--json" || a == "-J");
    let store = super::api::Store::new(&settings::graph_path());
    let rows = match super::api::rows(&store) {
        Ok(rows) => rows,
        Err(e) => {
            eprintln!("Error: {}", e.0);
            return 1;
        }
    };
    let by_id: Map<String, Value> = rows
        .iter()
        .filter_map(|e| {
            let id = text_at(e, "id")?;
            Some((id.to_string(), e.clone()))
        })
        .collect();
    let mut stuck: Vec<Value> = Vec::new();
    for e in &rows {
        let Some(kids) = e.get("children").and_then(Value::as_array) else {
            continue;
        };
        if kids.is_empty() {
            continue;
        }
        let epic_status = e.get("status").and_then(Value::as_str);
        if matches!(epic_status, Some("done") | Some("superseded")) {
            continue;
        }
        let mut holders: Vec<Value> = Vec::new();
        let mut all_blocked = true;
        for k in kids {
            let Some(cid) = k.get("id").and_then(Value::as_str) else {
                continue;
            };
            let Some(child) = by_id.get(cid) else {
                // A child id pointing nowhere is ignored, same as
                // store._compute_children (no phantom summary).
                continue;
            };
            match child.get("status").and_then(Value::as_str) {
                Some("done") => continue,
                Some(s @ ("deferred" | "superseded")) => {
                    if holds_epic_open(child) {
                        holders.push(json!({
                            "id": child.get("id"),
                            "status": s,
                            "deferred_kind": child.get("deferred_kind"),
                        }));
                    }
                }
                // Any other status (in_progress, ready, idea, ...) is work
                // that could still advance: the epic is not stuck.
                _ => all_blocked = false,
            }
        }
        if !all_blocked {
            continue;
        }
        let closable = epic_status != Some("deferred") && holders.is_empty();
        let held_open_by: Vec<&str> = holders.iter().filter_map(|h| h["id"].as_str()).collect();
        stuck.push(json!({
            "id": e.get("id"),
            "status": e.get("status"),
            "title": e.get("title"),
            "closable": closable,
            "holders": holders.clone(),
            "held_open_by": held_open_by,
        }));
    }
    if json_output {
        println!("{}", json!({"stuck_epics": stuck}));
        return 0;
    }
    if stuck.is_empty() {
        println!("no stuck epics");
        return 0;
    }
    for row in &stuck {
        let id = row["id"].as_str().unwrap_or("");
        let status = row["status"].as_str().unwrap_or("");
        let title = row["title"].as_str().unwrap_or("");
        let verdict = if row["closable"].as_bool().unwrap_or(false) {
            "closable".to_string()
        } else {
            let ids: Vec<String> = row["held_open_by"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(Value::as_str)
                        .map(|s| format!("'{s}'"))
                        .collect()
                })
                .unwrap_or_default();
            format!("held open by [{}]", ids.join(", "))
        };
        println!("  {id} [{status}] {title} - {verdict}");
        for h in row["holders"].as_array().unwrap_or(&Vec::new()) {
            let kind = h["deferred_kind"]
                .as_str()
                .map(|k| format!(", {k}"))
                .unwrap_or_default();
            println!(
                "      {} [{}{}]",
                h["id"].as_str().unwrap_or(""),
                h["status"].as_str().unwrap_or(""),
                kind
            );
        }
    }
    println!(
        "  read-only: closing or undefering any of these is an operator ruling, never automatic"
    );
    0
}

// ---------------------------------------------------------------------------
// reprioritize - the priority write
// ---------------------------------------------------------------------------

const REPRIORITIZE_HELP: &str =
    "Usage: fno backlog reprioritize TASK_ID PRIORITY [--blocks-everything]

Set a node's priority: p0|p1|p2|p3. P0 refuses without the
--blocks-everything acknowledgement.
";

pub fn run_reprioritize(tail: &[String]) -> i32 {
    if tail.iter().any(|a| a == "--help" || a == "-h") {
        print!("{REPRIORITIZE_HELP}");
        return 0;
    }
    if let Some(code) = refuse_non_graph_backend("reprioritize") {
        return code;
    }
    let mut positional: Vec<&String> = Vec::new();
    let mut blocks_everything = false;
    for a in tail {
        match a.as_str() {
            "--blocks-everything" => blocks_everything = true,
            other if other.starts_with('-') => {
                eprintln!("reprioritize: unknown flag '{other}'");
                return 2;
            }
            _ => positional.push(a),
        }
    }
    if positional.len() > 2 {
        eprintln!(
            "Got unexpected extra arguments: {}",
            positional[2..].join(" ")
        );
        return 2;
    }
    let Some(task_id) = positional.first() else {
        eprintln!("Missing argument 'TASK_ID'.");
        return 2;
    };
    let Some(priority) = positional.get(1) else {
        eprintln!("Missing argument 'PRIORITY'.");
        return 2;
    };
    if !has_node_id_prefix(task_id) {
        eprintln!("Error: task_id must be a <prefix>-<4..8 hex> node id, got '{task_id}'");
        return 1;
    }
    if !matches!(priority.as_str(), "p0" | "p1" | "p2" | "p3") {
        eprintln!("Error: invalid priority '{priority}'. Must be: p0, p1, p2, p3");
        return 1;
    }
    if priority.as_str() == "p0" && !blocks_everything {
        eprintln!("Error: p0 blocks everything else, usually a bug. Use --blocks-everything only when a broken service or fleet-wide resource leak blocks all downstream work. If it does, say so with --blocks-everything; otherwise file it p1.");
        return 2;
    }
    let graph = settings::graph_path();
    let Ok(rows) = graph_store::read_rows(&graph) else {
        eprintln!("Error: the backlog graph could not be read");
        return 1;
    };
    let old = find_node(&rows, task_id)
        .and_then(|n| text_at(n, "priority"))
        .unwrap_or("p2")
        .to_string();
    let priority_value = priority.clone();
    let id_value = task_id.clone();
    let applied = graph_store::mutate_rows(
        &graph,
        std::time::Duration::from_secs(5),
        None,
        None,
        |working| {
            let Some(node) = find_node(working, &id_value) else {
                return Ok(false);
            };
            let Some(obj) = node.as_object_mut() else {
                return Ok(false);
            };
            obj.insert("priority".into(), Value::String(priority_value.clone()));
            if priority_value == "p0" {
                obj.insert("blocks_everything".into(), Value::Bool(true));
            }
            Ok(true)
        },
    );
    match applied {
        Ok(Some(_)) => {
            println!("Reprioritized {task_id}: {old} -> {priority}");
            0
        }
        _ => {
            eprintln!("Error: feature {task_id} not found");
            1
        }
    }
}
