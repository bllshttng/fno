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
// discover - the expired-deferral worklist (read-only)
// ---------------------------------------------------------------------------

const DISCOVER_HELP: &str = "Usage: fno backlog discover [--limit N] [--json|-J]

Review expired deferred nodes without changing backlog state. The output is
a ranked worklist for a human; it never closes, undefers, or writes a node.
";

/// The wheel's `fno.graph.discovery._PATH_RE` shape: a repo-relative path
/// token embedded in prose (no backticks, no anchors).
fn path_tokens(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let bytes = text.as_bytes();
    let n = bytes.len();
    let is_word = |b: u8| b.is_ascii_alphanumeric() || b == b'_' || b == b'.';
    let mut i = 0;
    while i < n {
        // A token starts after a non-word byte (the wheel's negative
        // lookbehind) and looks like a slash path.
        if is_word(bytes[i]) && (i == 0 || !is_word(bytes[i - 1])) {
            let start = i;
            let mut slashes = 0;
            let mut j = i;
            let mut last = start;
            while j < n {
                let b = bytes[j];
                if b == b'/' {
                    if j == start || slashes >= 16 {
                        break;
                    }
                    slashes += 1;
                    j += 1;
                } else if is_word(b) || b == b'-' {
                    j += 1;
                    last = j;
                } else {
                    break;
                }
            }
            if slashes >= 1 && last > start {
                out.push(text[start..last].to_string());
            }
            i = j.max(start + 1);
        } else {
            i += 1;
        }
    }
    out
}

/// The `fno backlog maintain` stale-drain stamp shape, at any staleness
/// days: the deferral is a 30-day park, not a decision.
fn stale_drain_reason_still_true(node: &Value) -> bool {
    use std::time::{SystemTime, UNIX_EPOCH};
    let Some(reason) = node.get("deferred_reason").and_then(Value::as_str) else {
        return false;
    };
    if !reason.starts_with("stale >") || !reason.ends_with("d, drained by maintain") {
        return false;
    }
    let Some(deferred_at) = node.get("deferred_at").and_then(Value::as_str) else {
        return false;
    };
    let Some(parked) =
        super::maintenance_archive::parse_ts(Some(&Value::String(deferred_at.to_string())))
    else {
        return false;
    };
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let age_days = (now - parked.timestamp()) as f64 / 86400.0;
    age_days >= 30.0
}

/// The wheel's `_file_evidence`: repo-relative paths the node's own text
/// names that exist on disk right now.
fn file_evidence(node: &Value) -> Vec<String> {
    let mut text = node
        .get("details")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    text.push(' ');
    text.push_str(node.get("title").and_then(Value::as_str).unwrap_or(""));
    path_tokens(&text)
        .into_iter()
        .filter(|p| std::path::Path::new(p).is_file())
        .collect()
}

struct Assessment {
    verdict: &'static str,
    evidence: Vec<Value>,
    reason: String,
}

/// The wheel's assess ladder: merged PR, then recorded files, then a top
/// candidate, then the still-true stale drain, then undecided.
fn assess_one(
    node: &Value,
    cands: &[Value],
    pr_merged: &dyn Fn(i64) -> Option<bool>,
) -> Assessment {
    let pr_number = node.get("pr_number").and_then(Value::as_i64);
    if let Some(pr) = pr_number {
        let satisfied = node.get("status").and_then(Value::as_str) == Some("done")
            || node.get("completed_at").is_some()
            || node.get("merged_at").is_some()
            || node.get("pr_merged") == Some(Value::Bool(true))
            || pr_merged(pr).unwrap_or(false);
        if satisfied {
            return Assessment {
                verdict: "satisfied",
                evidence: vec![json!(format!("PR#{pr}"))],
                reason: "merged PR recorded on node".into(),
            };
        }
    }
    let has_pr = pr_number.is_some();
    let files = file_evidence(node);
    if !files.is_empty() && (node.get("status").and_then(Value::as_str) == Some("done") || has_pr) {
        return Assessment {
            verdict: "satisfied",
            evidence: files.into_iter().map(Value::String).collect(),
            reason: "existing file recorded by node".into(),
        };
    }
    if let Some(top) = cands.first() {
        let lanes = top["lanes"]
            .as_array()
            .map(|a| {
                let mut names: Vec<&str> = a.iter().filter_map(Value::as_str).collect();
                names.sort_unstable();
                names.join(", ")
            })
            .unwrap_or_default();
        return Assessment {
            verdict: "duplicate",
            evidence: vec![top["node_id"].clone()],
            reason: format!("candidate surfaced by {lanes}"),
        };
    }
    if stale_drain_reason_still_true(node) {
        let reason_text = node
            .get("deferred_reason")
            .and_then(Value::as_str)
            .unwrap_or("");
        return Assessment {
            verdict: "still_real",
            evidence: vec![json!(format!("deferred reason: {reason_text}"))],
            reason: "the exact expiry condition remains true".into(),
        };
    }
    Assessment {
        verdict: "undecided",
        evidence: vec![],
        reason: "no duplicate or completion evidence, and the original condition is not provable"
            .into(),
    }
}

/// The wheel's `_fts_search` degraded lane: a substring pass over title,
/// slug and details (also the positive control's fallback).
fn substring_matches(query: &str, pool: &[Value]) -> Vec<String> {
    let lowered = query.to_lowercase();
    pool.iter()
        .filter_map(|e| {
            let id = text_at(e, "id")?;
            let hay = format!(
                "{} {} {}",
                e.get("title").and_then(Value::as_str).unwrap_or(""),
                e.get("slug").and_then(Value::as_str).unwrap_or(""),
                e.get("details").and_then(Value::as_str).unwrap_or("")
            )
            .to_lowercase();
            hay.contains(&lowered).then(|| id.to_string())
        })
        .collect()
}

pub fn run_discover(tail: &[String]) -> i32 {
    if tail.iter().any(|a| a == "--help" || a == "-h") {
        print!("{DISCOVER_HELP}");
        return 0;
    }
    let mut limit: i64 = 20;
    let mut json_output = false;
    let mut it = tail.iter();
    while let Some(arg) = it.next() {
        let (flag, inline) = match arg.split_once('=') {
            Some((f, v)) if f.starts_with("--") => (f, Some(v.to_string())),
            _ => (arg.as_str(), None),
        };
        match flag {
            "--limit" | "-L" => {
                let raw = inline.or_else(|| it.next().cloned()).unwrap_or_default();
                limit = raw.parse().unwrap_or(0);
                if limit < 1 {
                    eprintln!("discover: --limit must be >= 1");
                    return 2;
                }
            }
            "--json" | "-J" => json_output = true,
            other => {
                eprintln!("discover: unknown argument '{other}'");
                return 2;
            }
        }
    }
    let graph = settings::graph_path();
    let store = super::api::Store::new(&graph);
    let rows = match super::api::rows(&store) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("Error: {}", e.0);
            return 1;
        }
    };
    let expired: Vec<&Value> = rows
        .iter()
        .filter(|e| {
            e.get("status").and_then(Value::as_str) == Some("deferred")
                && e.get("deferred_kind").and_then(Value::as_str) == Some("expired")
        })
        .collect();
    let excluded_by_kind = rows
        .iter()
        .filter(|e| {
            e.get("status").and_then(Value::as_str) == Some("deferred")
                && e.get("deferred_kind").and_then(Value::as_str) != Some("expired")
        })
        .count();
    // The gh-verified merge state, cached per PR (deferral clears every
    // node-side completion field, so the check is the only truth).
    let repo = crate::finalize::repo_identity_from_git_remote(
        &std::env::current_dir().unwrap_or_default(),
    )
    .map(|full| {
        full.splitn(3, '/')
            .nth(1)
            .map(|rest| {
                let idx = full.find(rest).unwrap_or(0);
                full[idx..].to_string()
            })
            .unwrap_or(full.clone())
    });
    let pr_merged = |number: i64| -> Option<bool> {
        let Some(slug) = &repo else {
            return None;
        };
        let out = std::process::Command::new("gh")
            .args([
                "api",
                &format!("repos/{slug}/pulls/{number}"),
                "--jq",
                ".merged",
            ])
            .output()
            .ok()?;
        let text = String::from_utf8_lossy(&out.stdout).trim().to_string();
        match text.as_str() {
            "true" => Some(true),
            "false" => Some(false),
            _ => None,
        }
    };

    let mut worklist: Vec<Value> = Vec::new();
    let mut degraded_warnings: Vec<String> = Vec::new();
    let mut fts_degraded = false;
    for entry in &expired {
        let title = entry.get("title").and_then(Value::as_str).unwrap_or("");
        let details = entry.get("details").and_then(Value::as_str).unwrap_or("");
        let exclude = text_at(entry, "id").unwrap_or("");
        let pool: Vec<Value> = rows
            .iter()
            .filter(|e| text_at(e, "id").map(|i| i != exclude).unwrap_or(false))
            .cloned()
            .collect();
        // The FTS lane: one query, no cap (the wheel passes limit=None).
        let query = format!("{} {}", title, details);
        let (fts_ids, fts_warning): (Vec<String>, Option<String>) =
            match super::search::search(&store, &query, None) {
                Ok(ids) => (ids, None),
                Err(e) => {
                    fts_degraded = true;
                    (
                        substring_matches(&query, &pool),
                        Some(format!(
                            "fts lane unavailable ({e}); substring fallback ranked the candidates"
                        )),
                    )
                }
            };
        if let Some(w) = &fts_warning {
            if !degraded_warnings.contains(w) {
                degraded_warnings.push(w.clone());
            }
        }
        let by_id: std::collections::BTreeMap<String, Value> = pool
            .iter()
            .filter_map(|e| text_at(e, "id").map(|i| (i.to_string(), e.clone())))
            .collect();
        let incoming = json!({
            "id": "__incoming__",
            "title": title,
            "details": details,
            "domain": entry.get("domain").and_then(Value::as_str).unwrap_or("code"),
        });
        let related = super::relatedness::similar_nodes(&incoming, &pool, usize::MAX, None);
        let mut all_ids: Vec<String> = Vec::new();
        all_ids.extend(fts_ids.iter().filter(|i| by_id.contains_key(*i)).cloned());
        all_ids.extend(related.iter().map(|(id, _, _)| id.clone()));
        all_ids.sort();
        all_ids.dedup();
        let mut cands: Vec<Value> = Vec::new();
        for node_id in &all_ids {
            let Some(row) = by_id.get(node_id) else {
                continue;
            };
            let (score, reason) = related
                .iter()
                .find(|(id, _, _)| id == node_id)
                .map(|(_, s, r)| (*s, r.clone()))
                .unwrap_or_else(|| {
                    let (s, r) = super::relatedness::score_pair(&incoming, row, false);
                    (s, r)
                });
            let lanes: Vec<&str> = [
                ("fts", fts_ids.contains(node_id)),
                (
                    "relatedness",
                    related.iter().any(|(id, _, _)| id == node_id),
                ),
            ]
            .into_iter()
            .filter(|(_, present)| *present)
            .map(|(name, _)| name)
            .collect();
            cands.push(json!({
                "node_id": node_id,
                "score": score,
                "lanes": lanes,
                "reason": reason,
            }));
        }
        cands.sort_by(|a, b| {
            b["score"]
                .as_f64()
                .unwrap_or(0.0)
                .partial_cmp(&a["score"].as_f64().unwrap_or(0.0))
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| {
                    a["node_id"]
                        .as_str()
                        .unwrap_or("")
                        .cmp(b["node_id"].as_str().unwrap_or(""))
                })
        });
        cands.truncate(limit.max(0) as usize);
        let assessment = assess_one(entry, &cands, &pr_merged);
        worklist.push(json!({
            "id": entry.get("id"),
            "title": entry.get("title"),
            "verdict": assessment.verdict,
            "evidence": assessment.evidence,
            "reason": assessment.reason,
            "candidates": cands,
        }));
    }
    worklist.sort_by(|a, b| {
        let sa = a["candidates"]
            .as_array()
            .and_then(|c| c.first())
            .and_then(|c| c["score"].as_f64())
            .unwrap_or(0.0);
        let sb = b["candidates"]
            .as_array()
            .and_then(|c| c.first())
            .and_then(|c| c["score"].as_f64())
            .unwrap_or(0.0);
        sb.partial_cmp(&sa)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| {
                a["id"]
                    .as_str()
                    .unwrap_or("")
                    .cmp(b["id"].as_str().unwrap_or(""))
            })
    });
    if !worklist.is_empty()
        && worklist
            .iter()
            .all(|r| r["candidates"].as_array().is_some_and(|c| !c.is_empty()))
    {
        eprintln!("failed instrument: every expired node matched a candidate; refusing to report an all-match worklist");
        return 2;
    }
    let mut positive: Option<Value> = None;
    if !worklist.is_empty()
        && worklist
            .iter()
            .all(|r| r["candidates"].as_array().is_some_and(|c| c.is_empty()))
    {
        let control_query = expired
            .first()
            .map(|e| {
                e.get("title")
                    .and_then(Value::as_str)
                    .map(String::from)
                    .unwrap_or_else(|| text_at(e, "id").unwrap_or("").to_string())
            })
            .unwrap_or_default();
        let matches = match super::search::search(&store, &control_query, None) {
            Ok(ids) => ids,
            Err(_) => {
                fts_degraded = true;
                substring_matches(&control_query, &rows)
            }
        };
        if matches.is_empty() {
            eprintln!("failed instrument: the positive control matched nothing, so the all-empty worklist is not trusted");
            return 2;
        }
        positive = Some(json!({
            "query": control_query,
            "matches": matches,
            "lane": if fts_degraded { "substring" } else { "fts" },
            "degraded": fts_degraded,
        }));
    }
    if json_output {
        let report = json!({
            "population": "deferred_kind=expired",
            "assessed": expired.len(),
            "excluded_by_kind": excluded_by_kind,
            "degraded": !degraded_warnings.is_empty(),
            "warnings": degraded_warnings,
            "positive_control": positive,
            "worklist": worklist,
        });
        println!(
            "{}",
            serde_json::to_string_pretty(&report).unwrap_or_default()
        );
        return 0;
    }
    println!(
        "discover: assessed={} excluded-by-kind={}",
        expired.len(),
        excluded_by_kind
    );
    for row in &worklist {
        let evidence = row["evidence"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    .join(", ")
            })
            .unwrap_or_default();
        let evidence = if evidence.is_empty() {
            row["reason"].as_str().unwrap_or("").to_string()
        } else {
            evidence
        };
        println!(
            "{}\t{}\t{}",
            row["id"].as_str().unwrap_or(""),
            row["verdict"].as_str().unwrap_or(""),
            evidence
        );
    }
    if let Some(p) = &positive {
        println!(
            "positive control: {:?} -> {} ({})",
            p["query"].as_str().unwrap_or(""),
            p["matches"]
                .as_array()
                .map(|a| a
                    .iter()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    .join(", "))
                .unwrap_or_else(|| "no matches".to_string()),
            p["lane"].as_str().unwrap_or("")
        );
    }
    for w in &degraded_warnings {
        eprintln!("degraded: {w}");
    }
    0
}

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
