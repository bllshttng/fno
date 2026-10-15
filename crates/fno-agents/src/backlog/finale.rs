//! The finale arms: catalog commands whose wheel bodies outlived PR 3398's
//! wave-4 cutover. Each arm mirrors the wheel body's exits and renders (the
//! parity is the Python file this PR deletes), served with no compat forward.

use serde_json::{json, Map, Value};

use super::node_ref::{find_node, find_node_index, has_node_id_prefix};
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
            || node.get("pr_merged") == Some(&Value::Bool(true))
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
        let mut merged_cmd = std::process::Command::new("gh");
        merged_cmd.args([
            "api",
            &format!("repos/{slug}/pulls/{number}"),
            "--jq",
            ".merged",
        ]);
        let out = crate::gh_budget::output(merged_cmd).ok()?;
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
                            "fts lane unavailable ({}); substring fallback ranked the candidates",
                            e.0
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
    // `new` is the scoped-create word (node_builder.cmd_new's contract, now
    // the create door's): difficulty defaults to medium where plain add
    // demands one, the door carries the scoped opt-outs and inbox provenance
    // flags (`fno inbox drain` files with them), and the birth stamps
    // source="fno-new" - the receipt shape matches the add door's.
    let has_difficulty = tail.windows(2).any(|w| w[0] == "--difficulty");
    let mut args: Vec<String> = tail.to_vec();
    if !has_difficulty {
        args.push("--difficulty".to_string());
        args.push("medium".to_string());
    }
    let mut parsed = match super::create_cli::parse(&args) {
        super::create_cli::ParsedAdd::Help => {
            print!("{}", super::create_cli::ADD_HELP);
            return 0;
        }
        super::create_cli::ParsedAdd::Refusal { message, exit } => {
            eprintln!("{message}");
            return exit;
        }
        super::create_cli::ParsedAdd::Args(a) => a,
    };
    parsed.scoped_new = true;
    match super::create_cli::create_node(&parsed, &|_| None) {
        Ok(born) => {
            println!(
                "{}",
                super::render::py_json_pretty(&serde_json::json!({
                    "id": born.id,
                    "title": parsed.title
                }))
            );
            0
        }
        Err(r) => {
            eprintln!("{}", r.message);
            r.exit
        }
    }
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
            positional[2..]
                .iter()
                .map(|s| s.as_str())
                .collect::<Vec<&str>>()
                .join(" ")
        );
        return 2;
    }
    let Some(task_id) = positional.first().map(|s| s.to_string()) else {
        eprintln!("Missing argument 'TASK_ID'.");
        return 2;
    };
    let Some(priority) = positional.get(1).map(|s| s.to_string()) else {
        eprintln!("Missing argument 'PRIORITY'.");
        return 2;
    };
    if !has_node_id_prefix(&task_id) {
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
    let old = find_node(&rows, &task_id)
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
            let Some(idx) = find_node_index(working, &id_value) else {
                return Ok(false);
            };
            let Some(obj) = working[idx].as_object_mut() else {
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

// ---------------------------------------------------------------------------
// pick - the fzf queue picker
// ---------------------------------------------------------------------------

const PICK_HELP: &str = "Usage: fno backlog pick [--project NAME] [--all] [--ideas/--no-ideas] [--blocked/--no-blocked] [--reason TEXT]

Interactively manage the backlog queue via fzf with live marker updates.
";

const PICK_RENDER_AWK: &str = r##"
# Reads two files:
#   ARGV[1] = pending.txt (lines: "Q ab-xxxx" / "U ab-xxxx" / "T ab-xxxx",
#                          plus an initial "# pending" sentinel)
#   ARGV[2] = cands.tsv   (tab-delimited candidate snapshot)
# Emits TAB-delimited fzf rows: "<url>\t<id>\t<visible row>".
BEGIN { FS = "\t" }
NR == FNR {
    if (length($0) >= 3) {
        kind = substr($0, 1, 1)
        if (kind == "Q" || kind == "U" || kind == "T") {
            rest = substr($0, 3)
            gsub(/[ \t\r\n]+$/, "", rest)
            gsub(/^[ \t]+/, "", rest)
            if (rest != "") {
                cnt = ++pending_count[rest]
                pending_seq[rest "|" cnt] = kind
            }
        }
    }
    next
}
{
    id = $1; title = $2; prio = $3; project = $4; status = $5
    q_initial = ($6 == "1") ? 1 : 0
    plan_path = $7; blocked_by = $8; url = $9

    queued = q_initial
    n = pending_count[id]
    for (i = 1; i <= n; i++) {
        k = pending_seq[id "|" i]
        if (k == "Q") queued = 1
        else if (k == "U") queued = 0
        else if (k == "T") queued = (1 - queued)
    }

    is_blocked = (status == "blocked")
    if (queued && is_blocked)       marker = "[Q!]"
    else if (queued)                 marker = "[Q]"
    else if (is_blocked)             marker = "[B]"
    else                             marker = "[ ]"

    kind_col = (plan_path == "") ? "idea" : "plan"

    if (length(project) > 22) project = substr(project, 1, 21) "."
    if (length(title)   > 75) title   = substr(title,   1, 74) "."

    while (length(marker) < 5)   marker = marker " "
    while (length(project) < 22) project = project " "

    blocker_suffix = ""
    if (is_blocked && blocked_by != "")
        blocker_suffix = "  (blocked by " blocked_by ")"

    printf "%s\t%s\t%s %s  %s  %s  %s  %s%s\n", \
        url, id, marker, kind_col, prio, project, id, title, blocker_suffix
}
"##;

fn tsv_safe(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c == '\t' || c == '\n' || c == '\r' {
                ' '
            } else {
                c
            }
        })
        .collect()
}

/// Percent-encode with a safe set (urllib.parse.quote's shapes: no safe
/// chars for the vault, `/` for the file path).
fn uri_encode(text: &str, safe_slash: bool) -> String {
    let mut out = String::with_capacity(text.len());
    for b in text.bytes() {
        let keep = b.is_ascii_alphanumeric()
            || matches!(b, b'-' | b'_' | b'.' | b'~')
            || (safe_slash && b == b'/');
        if keep {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// The global config's `[obsidian]` block: vault when enabled (the wheel's
/// read_global_block shape). The global config is TOML; serde_json's
/// `from_str` would refuse it and the opener would silently never arm.
fn obsidian_vault() -> Option<String> {
    let home = std::env::var("HOME").ok()?;
    let text =
        std::fs::read_to_string(std::path::Path::new(&home).join(".fno/config.toml")).ok()?;
    let value: toml::Value = toml::from_str(&text).ok()?;
    let obs = value.get("obsidian")?;
    if obs.get("enabled").and_then(toml::Value::as_bool) != Some(true) {
        return None;
    }
    obs.get("vault")
        .and_then(toml::Value::as_str)
        .filter(|v| !v.is_empty())
        .map(String::from)
}

fn obsidian_url(vault: &str, plan_path: &str) -> Option<String> {
    let mut p = plan_path.trim().to_string();
    if !p.starts_with("internal/") {
        let marker = format!("/{vault}/");
        let vault_cut = p.rfind(&marker).map(|idx| {
            let rest = p[idx + marker.len()..].to_string();
            rest.starts_with("internal/").then_some(rest)
        });
        match vault_cut {
            Some(Some(rest)) => p = rest,
            _ => {
                let internal_cut = p.rfind("/internal/").map(|i| p[i + 1..].to_string());
                p = internal_cut?;
            }
        }
    }
    let p = p.trim_end_matches('/');
    if !p.ends_with(".md") {
        return None;
    }
    Some(format!(
        "obsidian://open?vault={}&file={}",
        uri_encode(vault, false),
        uri_encode(p.trim_end_matches(".md"), true)
    ))
}

/// The wheel's entries-based project detection: the first node whose cwd
/// (expanduser + normpath) equals the repo root names the project; else the
/// first node under it.
fn detect_project_from_entries(entries: &[Value], cwd: &std::path::Path) -> Option<String> {
    let norm_root = cwd.to_string_lossy().to_string();
    let root_with_sep = format!("{}/", norm_root.trim_end_matches('/'));
    let mut fallback: Option<String> = None;
    for e in entries {
        let Some(entry_cwd) = e.get("cwd").and_then(Value::as_str) else {
            continue;
        };
        let expanded = shellexpand_tilde(entry_cwd);
        let norm = std::path::Path::new(&expanded)
            .to_string_lossy()
            .to_string();
        if norm == norm_root {
            return e.get("project").and_then(Value::as_str).map(String::from);
        }
        if fallback.is_none() && norm.starts_with(&root_with_sep) {
            fallback = e.get("project").and_then(Value::as_str).map(String::from);
        }
    }
    fallback
}

fn shellexpand_tilde(path: &str) -> String {
    if let Some(rest) = path.strip_prefix("~/") {
        if let Ok(home) = std::env::var("HOME") {
            return format!("{home}/{rest}");
        }
    }
    path.to_string()
}

fn priority_rank(p: Option<&str>) -> i64 {
    match p {
        Some("p0") => 0,
        Some("p1") => 1,
        Some("p3") => 3,
        _ => 2,
    }
}

pub fn run_pick(tail: &[String]) -> i32 {
    if tail.iter().any(|a| a == "--help" || a == "-h") {
        print!("{PICK_HELP}");
        return 0;
    }
    let mut project: Option<String> = None;
    let mut all = false;
    let mut include_ideas = true;
    let mut include_blocked = false;
    let mut reason: Option<String> = None;
    let mut it = tail.iter();
    while let Some(arg) = it.next() {
        let (flag, inline) = match arg.split_once('=') {
            Some((f, v)) if f.starts_with("--") => (f, Some(v.to_string())),
            _ => (arg.as_str(), None),
        };
        match flag {
            "--project" | "-p" => {
                project = Some(inline.or_else(|| it.next().cloned()).unwrap_or_default());
            }
            "--all" | "-A" => all = true,
            "--ideas" => include_ideas = true,
            "--no-ideas" => include_ideas = false,
            "--blocked" | "-b" => include_blocked = true,
            "--no-blocked" => include_blocked = false,
            "--reason" | "-R" => {
                reason = inline.or_else(|| it.next().cloned());
            }
            other => {
                eprintln!("pick: unknown argument '{other}'");
                return 2;
            }
        }
    }
    let Some(fzf) = which("fzf") else {
        eprintln!(
            "Error: fzf not found on PATH. Install with `brew install fzf` (macOS) or your package manager."
        );
        return 1;
    };
    let Some(awk_bin) = which("awk") else {
        eprintln!("Error: awk not found on PATH (needed for live marker updates).");
        return 1;
    };
    let graph = settings::graph_path();
    let store = super::api::Store::new(&graph);
    let entries = match super::api::rows(&store) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("Error: {}", e.0);
            return 1;
        }
    };
    let mut candidates: Vec<Value> = entries
        .iter()
        .filter(|e| {
            let s = e.get("status").and_then(Value::as_str).unwrap_or("");
            s == "ready" || (include_ideas && s == "idea") || (include_blocked && s == "blocked")
        })
        .cloned()
        .collect();
    if let Some(p) = &project {
        candidates.retain(|e| e.get("project").and_then(Value::as_str) == Some(p.as_str()));
    } else if !all {
        let cwd = std::env::current_dir().unwrap_or_default();
        if let Some(detected) = detect_project_from_entries(&candidates, &cwd) {
            candidates
                .retain(|e| e.get("project").and_then(Value::as_str) == Some(detected.as_str()));
        }
    }
    if candidates.is_empty() {
        let mut scope: Vec<&str> = vec!["ready"];
        if include_ideas {
            scope.push("idea");
        }
        if include_blocked {
            scope.push("blocked");
        }
        scope.sort_unstable();
        println!("No {} rows to pick from in this scope.", scope.join("/"));
        return 0;
    }
    let currently_queued: std::collections::HashSet<String> = candidates
        .iter()
        .filter(|e| e.get("queued_at").is_some_and(|v| !v.is_null()))
        .filter_map(|e| text_at(e, "id").map(String::from))
        .collect();
    candidates.sort_by(|a, b| {
        // Queued rows sort to the TOP (0 before 1): the wheel's picker kept
        // the work being actively managed first, then priority, then age.
        let qa = i64::from(!currently_queued.contains(text_at(a, "id").unwrap_or("")));
        let qb = i64::from(!currently_queued.contains(text_at(b, "id").unwrap_or("")));
        qa.cmp(&qb)
            .then_with(|| {
                priority_rank(a.get("priority").and_then(Value::as_str))
                    .cmp(&priority_rank(b.get("priority").and_then(Value::as_str)))
            })
            .then_with(|| {
                a.get("created_at")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .cmp(b.get("created_at").and_then(Value::as_str).unwrap_or(""))
            })
    });

    let vault = obsidian_vault();
    let open_cmd = if cfg!(target_os = "macos") {
        "open".to_string()
    } else {
        which("xdg-open").unwrap_or_else(|| "xdg-open".to_string())
    };

    let tmp = std::env::temp_dir();
    let uniq = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let cand_path = tmp.join(format!("fno-pick-{uniq}.cands.tsv"));
    let pend_path = tmp.join(format!("fno-pick-{uniq}.pending.txt"));
    let awk_path = tmp.join(format!("fno-pick-{uniq}.awk"));
    let cand_for_cleanup = cand_path.clone();
    let pend_for_cleanup = pend_path.clone();
    let awk_for_cleanup = awk_path.clone();
    let cleanup = move || {
        for p in [&cand_for_cleanup, &pend_for_cleanup, &awk_for_cleanup] {
            let _ = std::fs::remove_file(p);
        }
    };
    // Seed pending.txt with the sentinel comment line: awk's NR==FNR test
    // misfires when the first file is empty.
    if std::fs::write(&pend_path, "# pending\n").is_err() {
        eprintln!("pick: could not write the pending file");
        return 1;
    }
    let mut cands_tsv = String::new();
    for e in &candidates {
        let id = text_at(e, "id").unwrap_or("");
        let title = e.get("title").and_then(Value::as_str).unwrap_or("");
        let prio = e.get("priority").and_then(Value::as_str).unwrap_or("p2");
        let proj = e.get("project").and_then(Value::as_str).unwrap_or("-");
        let status = e.get("status").and_then(Value::as_str).unwrap_or("ready");
        let queued = if e.get("queued_at").is_some_and(|v| !v.is_null()) {
            "1"
        } else {
            "0"
        };
        let plan_path = e.get("plan_path").and_then(Value::as_str).unwrap_or("");
        let blockers = e
            .get("blocked_by")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    .join(",")
            })
            .unwrap_or_default();
        let mut url = String::new();
        if let (Some(v), Some(plan)) =
            (vault.as_deref(), e.get("plan_path").and_then(Value::as_str))
        {
            url = obsidian_url(v, plan).unwrap_or_default();
        }
        cands_tsv.push_str(&format!(
            "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\n",
            // The awk renderer's column contract: id first, url last (the
            // wheel's cmd_pick wrote this exact order).
            id,
            tsv_safe(title),
            prio,
            tsv_safe(proj),
            status,
            queued,
            tsv_safe(plan_path),
            blockers,
            url
        ));
    }
    if std::fs::write(&cand_path, &cands_tsv).is_err() {
        eprintln!("pick: could not write the candidate snapshot");
        return 1;
    }
    if std::fs::write(&awk_path, PICK_RENDER_AWK).is_err() {
        eprintln!("pick: could not write the renderer");
        cleanup();
        return 1;
    }

    let qp = shell_quote(&pend_path.to_string_lossy());
    let qc = shell_quote(&cand_path.to_string_lossy());
    let qs = shell_quote(&awk_path.to_string_lossy());
    let qa = shell_quote(&awk_bin);
    let render_cmd = format!("{qa} -f {qs} {qp} {qc}");
    let mut header = format!(
        "q=queue  u=unqueue  space=toggle  o=open plan  Enter=commit  Ctrl-C=cancel  ({} rows, {} queued initially)\nMarkers update in-place as you press keys: [ ] not queued  [Q] queued  [B] blocked  [Q!] queued+blocked",
        candidates.len(),
        currently_queued.len()
    );
    if vault.is_none() {
        header.push_str("\n(set config.obsidian.vault in settings.yaml to enable 'o' opener)");
    }

    // Initial row set: run the renderer once with empty pending.
    let initial = std::process::Command::new(&awk_bin)
        .arg("-f")
        .arg(&awk_path)
        .arg(&pend_path)
        .arg(&cand_path)
        .output();
    let initial_rows = match initial {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).to_string(),
        _ => String::new(),
    };

    // rc 130 = Ctrl-C / Esc - the pending file drops unread.
    let spawned = std::process::Command::new(&fzf)
        .args([
            "--no-multi",
            "--delimiter",
            "\t",
            "--with-nth",
            "3..",
            "--nth",
            "3..",
            "--bind",
            &format!("q:execute-silent(printf 'Q %s\\n' {{2}} >> {qp})+reload({render_cmd})+down"),
            "--bind",
            &format!("u:execute-silent(printf 'U %s\\n' {{2}} >> {qp})+reload({render_cmd})+down"),
            "--bind",
            &format!(
                "space:execute-silent(printf 'T %s\\n' {{2}} >> {qp})+reload({render_cmd})+down"
            ),
            "--bind",
            "enter:accept",
            "--bind",
            &format!("o:execute-silent(u={{1}}; [[ -n \"$u\" ]] && {open_cmd} \"$u\")"),
            "--header",
            &header,
            "--prompt",
            "pick> ",
            "--height",
            "85%",
            "--reverse",
            "--no-sort",
        ])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn();
    let mut out = match spawned {
        Ok(mut child) => {
            use std::io::Write;
            if let Some(stdin) = child.stdin.take() {
                let mut stdin = stdin;
                let _ = stdin.write_all(initial_rows.as_bytes());
            }
            match child.wait_with_output() {
                Ok(o) => o,
                Err(_) => {
                    cleanup();
                    println!("Cancelled.");
                    return 0;
                }
            }
        }
        Err(_) => {
            eprintln!("pick: fzf failed to start");
            cleanup();
            return 1;
        }
    };
    let _ = &mut out;
    if out.status.code() == Some(130) {
        // rc 130 = Ctrl-C / Esc: drop pending unread, exactly the wheel's.
        cleanup();
        println!("Cancelled.");
        return 0;
    }

    // Parse pending.txt BEFORE cleanup deletes it, preserving order so T
    // flips the running state (matches what awk rendered to the screen).
    let mut ordered_intents: Vec<(String, char)> = Vec::new();
    if let Ok(pending) = std::fs::read_to_string(&pend_path) {
        for raw in pending.lines() {
            if raw.len() < 3 {
                continue;
            }
            let kind = raw.chars().next().unwrap_or(' ');
            if !matches!(kind, 'Q' | 'U' | 'T') {
                continue;
            }
            let rest = raw[2..].trim();
            if has_node_id_prefix(rest) {
                ordered_intents.push((rest.to_string(), kind));
            }
        }
    }
    cleanup();
    if ordered_intents.is_empty() {
        println!("No changes.");
        return 0;
    }
    let mut final_queued: std::collections::BTreeMap<String, bool> = Default::default();
    for (tid, kind) in &ordered_intents {
        match kind {
            'Q' => {
                final_queued.insert(tid.clone(), true);
            }
            'U' => {
                final_queued.insert(tid.clone(), false);
            }
            _ => {
                let prev = final_queued
                    .get(tid)
                    .copied()
                    .unwrap_or(currently_queued.contains(tid));
                final_queued.insert(tid.clone(), !prev);
            }
        }
    }
    let mut to_queue: Vec<String> = Vec::new();
    let mut to_unqueue: Vec<String> = Vec::new();
    for (tid, want) in &final_queued {
        let was = currently_queued.contains(tid);
        if *want && !was {
            to_queue.push(tid.clone());
        } else if was && !*want {
            to_unqueue.push(tid.clone());
        }
    }
    if to_queue.is_empty() && to_unqueue.is_empty() {
        println!("No changes (marks ended at original state).");
        return 0;
    }
    let cleaned_reason = reason
        .as_deref()
        .map(str::trim)
        .filter(|r| !r.is_empty())
        .map(String::from);
    let now = graph_store::now_isoformat();
    let queue_ids = to_queue.clone();
    let unqueue_ids = to_unqueue.clone();
    let reason_value = cleaned_reason.clone();
    let applied = graph_store::mutate_rows(
        &graph,
        std::time::Duration::from_secs(5),
        None,
        None,
        |working| {
            let mut any = false;
            for tid in &queue_ids {
                // Silent skip when a node is missing or already in the
                // target state: the snapshot-to-lock race loses one mark,
                // never the user's other valid ones.
                let Some(idx) = find_node_index(working, tid) else {
                    continue;
                };
                let node = &mut working[idx];
                if node.get("queued_at").is_some_and(|v| !v.is_null()) {
                    continue;
                }
                let Some(obj) = node.as_object_mut() else {
                    continue;
                };
                obj.insert("queued_at".into(), Value::String(now.clone()));
                if let Some(r) = &reason_value {
                    obj.insert("queued_reason".into(), Value::String(r.clone()));
                }
                any = true;
            }
            for tid in &unqueue_ids {
                let Some(idx) = find_node_index(working, tid) else {
                    continue;
                };
                let node = &mut working[idx];
                if !node.get("queued_at").is_some_and(|v| !v.is_null()) {
                    continue;
                }
                let Some(obj) = node.as_object_mut() else {
                    continue;
                };
                obj.insert("queued_at".into(), Value::Null);
                obj.insert("queued_reason".into(), Value::Null);
                any = true;
            }
            Ok(any)
        },
    );
    if applied.is_err() {
        eprintln!("pick: the queue write failed");
        return 1;
    }
    let suffix = cleaned_reason
        .as_ref()
        .map(|r| format!(": \"{r}\""))
        .unwrap_or_default();
    for tid in &to_queue {
        println!("Queued {tid}{suffix}");
    }
    for tid in &to_unqueue {
        println!("Unqueued {tid}");
    }
    println!("({} queued, {} unqueued)", to_queue.len(), to_unqueue.len());
    0
}

fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

fn which(name: &str) -> Option<String> {
    let path = std::env::var("PATH").ok()?;
    for dir in path.split(':') {
        let candidate = std::path::Path::new(dir).join(name);
        if candidate.is_file() {
            return Some(candidate.to_string_lossy().into_owned());
        }
    }
    None
}

// ---------------------------------------------------------------------------
// requeue - return a wedged in_progress node to the queue
// ---------------------------------------------------------------------------

const REQUEUE_HELP: &str = "Usage: fno backlog requeue NODE [--json|-J]

Return a node wedged in_progress by a dead worker to the queue. The re-lock
door retired with the graph claim mirror; re-acquisition is a claim store
acquire, not a backlog verb.
";

/// The wheel's allowlist, fail closed: suspect is still owned, corrupted is
/// unreadable, and a state added later must refuse.
const REQUEUEABLE_CLAIM_STATES: [&str; 2] = ["free", "stale"];

/// Fixed-width-4 humanized age (the wheel's `_humanize_age`): an unread
/// probe renders `?`, never a zero.
fn humanize_age(seconds: Option<i64>) -> String {
    let Some(seconds) = seconds else {
        return "   ?".to_string();
    };
    let (unit, n) = if seconds < 60 {
        ("s", seconds)
    } else if seconds < 3600 {
        ("m", seconds / 60)
    } else if seconds < 86400 {
        ("h", seconds / 3600)
    } else {
        ("d", (seconds / 86400).min(999))
    };
    format!("{:>4}", format!("{n}{unit}"))
}

/// Seconds since the row last showed life: its start, or a note its own
/// session wrote (the wheel's `_do_row_idle_s`).
fn do_row_idle_s(entry: &Value, row: &Value, now_s: f64) -> Option<i64> {
    let parse = |value: Option<&str>| -> Option<i64> {
        super::maintenance_archive::parse_ts(Some(&Value::String(value?.to_string())))
            .map(|dt| dt.timestamp())
    };
    let session = row.get("session_id").and_then(Value::as_str).unwrap_or("");
    let mut stamps: Vec<i64> = Vec::new();
    if let Some(started) = parse(row.get("started_at").and_then(Value::as_str)) {
        stamps.push(started);
    }
    if let Some(notes) = entry.get("progress_notes").and_then(Value::as_array) {
        for n in notes {
            let same_session = n.get("source_session_id").and_then(Value::as_str) == Some(session);
            if same_session {
                if let Some(ts) = parse(n.get("ts").and_then(Value::as_str)) {
                    stamps.push(ts);
                }
            }
        }
    }
    stamps.iter().max().map(|max| (now_s as i64) - max)
}

/// Why the abandoned-do-row sweep holds this row, or None when it reads
/// gone (the wheel's `_execute_row_hold_reason` order: the roster is read
/// only once some open row is past the bound, a reachable worker outranks
/// the idle clock, and an unread roster holds).
fn execute_row_hold_reason(
    entry: &Value,
    row: &Value,
    now_s: f64,
    bound_s: i64,
    workers: Option<&Vec<String>>,
    roster_consulted: bool,
    roster_reason: &str,
) -> Option<String> {
    let status = entry.get("status").and_then(Value::as_str).unwrap_or("");
    if crate::graph_store::TERMINAL_RUNGS.contains(&status) || entry.get("superseded_by").is_some()
    {
        return None;
    }
    let idle = do_row_idle_s(entry, row, now_s);
    let past_bound = idle.is_some_and(|i| i > bound_s);
    let mut worker_names: Option<Vec<String>> = None;
    if past_bound {
        if !roster_consulted {
            return Some(format!("roster unread ({roster_reason})"));
        }
        worker_names = workers.filter(|w| !w.is_empty()).cloned();
    }
    if let Some(names) = worker_names {
        return Some(format!("reachable worker on node {}", names.join(", ")));
    }
    match idle {
        Some(idle) if idle > bound_s => None,
        _ => Some(format!(
            "row idle {}, inside the {}h bound",
            idle.map(|i| format!("{}h", i / 3600))
                .unwrap_or_else(|| "?".to_string()),
            bound_s / 3600
        )),
    }
}

/// The wheel's `reclaimable_note`: when the refused claim turns reclaimable.
fn reclaimable_note(reclaimable_at_ms: Option<i64>, now_ms: i64) -> Option<String> {
    let stamp = reclaimable_at_ms?;
    let remaining_s = (stamp - now_ms) / 1000;
    if remaining_s <= 0 {
        return Some("reclaimable now".to_string());
    }
    let minutes = ((remaining_s as f64) / 60.0).round().max(1.0) as i64;
    use chrono::TimeZone;
    let utc = chrono::Utc::now();
    let _ = utc;
    let stamped = chrono::Utc
        .timestamp_opt(stamp / 1000, ((stamp % 1000) * 1_000_000) as u32)
        .single()
        .map(|dt| dt.format("%Y-%m-%dT%H:%M:%SZ").to_string())
        .unwrap_or_default();
    Some(format!("reclaimable at {stamped} (in {minutes} min)"))
}

pub fn run_requeue(tail: &[String]) -> i32 {
    if tail.iter().any(|a| a == "--help" || a == "-h") {
        print!("{REQUEUE_HELP}");
        return 0;
    }
    let json_out = tail.iter().any(|a| a == "--json" || a == "-J");
    let positional: Vec<&String> = tail.iter().filter(|a| !a.starts_with('-')).collect();
    let Some(node) = positional.first() else {
        eprintln!("Missing argument 'NODE'.");
        return 2;
    };
    if let Some(code) = refuse_non_graph_backend("requeue") {
        return code;
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
    let Some(row) = find_node(&rows, node) else {
        eprintln!("requeue: no exact node matches {node:?}.");
        return 2;
    };
    let entry = row.clone();
    let Some(node_id) = text_at(&entry, "id").map(String::from) else {
        eprintln!("requeue: no exact node matches {node:?}.");
        return 2;
    };
    let status_before = entry
        .get("persisted_status")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();

    // has_pr outranks open_do in the derivation, so an in_review node never
    // reaches the settle; refusing here also keeps requeue off any PR.
    if status_before != "in_progress" {
        eprintln!(
            "requeue: {node_id} reads status {status_before:?}, not in_progress; only an in_progress node can be returned to the queue."
        );
        return 2;
    }

    // The claim reader wants the record, and the composite verdict still
    // reads unknown when an unresolved roster row's worktree names this node.
    let key = format!("node:{node_id}");
    let (state, record) = claims::status(&key, None);
    let state_str = state.as_str();
    if !REQUEUEABLE_CLAIM_STATES.contains(&state_str) {
        let claim_row = record
            .as_ref()
            .map(super::super::claim_verbs::claim_status_value)
            .unwrap_or(Value::Null);
        let holder = claim_row
            .get("holder")
            .and_then(Value::as_str)
            .unwrap_or("");
        let holder_note = if holder.is_empty() {
            String::new()
        } else {
            format!(", holder {holder:?}")
        };
        let basis = claim_row.get("basis").and_then(Value::as_str).unwrap_or("");
        let basis_note = if basis.is_empty() {
            String::new()
        } else {
            format!(" ({basis})")
        };
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);
        let note = reclaimable_note(
            claim_row.get("reclaimable_at").and_then(Value::as_i64),
            now_ms,
        );
        let grace_note = note
            .map(|n| format!("; {n}: re-run fno backlog requeue {node_id} then"))
            .unwrap_or_default();
        eprintln!(
            "requeue: claim {key} reads {state_str}{basis_note}{holder_note}; only {} may requeue{grace_note}.",
            REQUEUEABLE_CLAIM_STATES.join(" or ")
        );
        return 3;
    }

    // Reachability over the open do rows: session truth per row. The wheel
    // passed falsifier=None so a falsifier can never invent a refusal; the
    // payload's verdict only ever lowers.
    let open_rows: Vec<Value> = entry
        .get("sessions")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter(|r| graph_store::is_open_do_row(r))
                .cloned()
                .collect()
        })
        .unwrap_or_default();
    let now_s = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0);
    let stores = crate::session_truth::Stores::ambient();
    let mut cursors = crate::session_truth::cursor::TruthCursors::new();
    struct RowTruth {
        harness: String,
        session_id: String,
        state: String,
        reachability: String,
        samples: Option<i64>,
        last_event_at: Value,
        age: String,
        row_idle_s: Option<i64>,
    }
    let mut pairs: Vec<(Value, RowTruth)> = Vec::new();
    for r in &open_rows {
        let sid = r.get("session_id").and_then(Value::as_str).unwrap_or("");
        let payload =
            crate::session_truth::resolve_payload(None, sid, now_s, &stores, &mut cursors);
        let reachability = payload
            .get("reachability")
            .and_then(Value::as_str)
            .unwrap_or("unknown")
            .to_string();
        let samples = payload
            .get("observed_model")
            .and_then(|o| o.get("kind"))
            .and_then(Value::as_str)
            .map(|kind| {
                if kind == "no-model-yet" {
                    Some(0i64)
                } else if kind == "observed" {
                    o_samples(&payload)
                } else {
                    None
                }
            })
            .flatten();
        pairs.push((
            r.clone(),
            RowTruth {
                harness: r
                    .get("harness")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
                session_id: sid.to_string(),
                state: payload
                    .get("state")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown")
                    .to_string(),
                reachability,
                samples,
                last_event_at: payload.get("last_event_at").cloned().unwrap_or(Value::Null),
                age: humanize_age(payload.get("last_activity_age_s").and_then(Value::as_i64)),
                row_idle_s: do_row_idle_s(&entry, r, now_s),
            },
        ));
    }
    let merged = super::advance_settings::load_merged(None);
    let bound_hours = merged["backlog"]["maintain"]["abandoned_do_row_hours"]
        .as_i64()
        .filter(|n| *n > 0)
        .unwrap_or(24);
    let bound_s = bound_hours * 3600;
    let mut reachable_workers: Option<Vec<String>> = None;
    let mut roster_consulted = true;
    let mut roster_reason = String::new();
    let any_past_bound = pairs
        .iter()
        .any(|(r, _)| do_row_idle_s(&entry, r, now_s).unwrap_or(0) > bound_s);
    if any_past_bound {
        match super::worked::reachable_workers_by_node() {
            Ok(map) => {
                reachable_workers = map.get(&node_id).cloned();
            }
            Err(reason) => {
                roster_consulted = false;
                roster_reason = reason;
            }
        }
    }
    for (r, truth) in &pairs {
        let reach = truth.reachability == "reachable";
        if reach {
            let why = execute_row_hold_reason(
                &entry,
                r,
                now_s,
                bound_s,
                reachable_workers.as_ref(),
                roster_consulted,
                &roster_reason,
            );
            let Some(why) = why else { continue };
            let now = graph_store::now_isoformat();
            eprintln!(
                "requeue: {}:{} reads reachable; a reachable worker still owns the execute window. If that session is yours and has stopped this node: fno backlog session add {node_id} --phase execute --ended-at {now}. The execute row stays: {why}.",
                truth.harness, truth.session_id
            );
            return 3;
        }
    }

    // Reap the open rows: every phase fills ended_at and keeps the row.
    for r in &open_rows {
        let harness = r.get("harness").and_then(Value::as_str).unwrap_or("");
        let sid = r.get("session_id").and_then(Value::as_str).unwrap_or("");
        let _ = super::api::session_end(
            &store,
            &node_id,
            sid,
            "reap",
            Some("execute"),
            Some(harness),
            None,
        );
    }

    let note = super::cutover::release_node_lockfile(&node_id);
    if note.starts_with("lockfile") {
        eprintln!("requeue: {node_id} claim was not released ({note}); the node stays claimed.");
        return 3;
    }
    if note.starts_with("released") || note.starts_with("no lockfile") {
        let _ = super::cutover::settle_released_node(&graph, &node_id);
    }

    let after = super::api::rows(&store).ok().and_then(|rows| {
        rows.iter()
            .find(|e| text_at(e, "id") == Some(node_id.as_str()))
            .cloned()
    });
    let status_after = after
        .as_ref()
        .and_then(|a| a.get("persisted_status"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let remaining = after
        .as_ref()
        .and_then(|a| a.get("sessions"))
        .and_then(Value::as_array)
        .map(|a| a.iter().filter(|r| graph_store::is_open_do_row(r)).count())
        .unwrap_or(0);
    if after.is_none() || status_after == "in_progress" {
        eprintln!("requeue: {node_id} still reads in_progress after settling ({remaining} open do row(s) remain).");
        return 1;
    }

    let settled: Vec<Value> = pairs
        .iter()
        .map(|(_, t)| {
            json!({
                "harness": t.harness,
                "session_id": t.session_id,
                "state": t.state,
                "samples": t.samples,
                "last_event_at": t.last_event_at,
                "age": t.age,
                "row_idle_s": t.row_idle_s,
            })
        })
        .collect();
    if json_out {
        println!(
            "{}",
            json!({
                "node_id": node_id,
                "status_before": status_before,
                "status_after": status_after,
                "settled": settled,
            })
        );
        return 0;
    }
    println!("requeued {node_id} ({status_before} -> {status_after})");
    for (_, t) in &pairs {
        let samples = t
            .samples
            .map(|s| s.to_string())
            .unwrap_or_else(|| "?".to_string());
        println!(
            "  {}:{} state={} samples={samples} last_event_at={} age={}",
            t.harness, t.session_id, t.state, t.last_event_at, t.age
        );
    }
    0
}

/// The observed-model sample count off a truth payload.
fn o_samples(payload: &Value) -> Option<i64> {
    payload
        .get("observed_model")
        .and_then(|o| o.get("samples"))
        .and_then(Value::as_i64)
}
