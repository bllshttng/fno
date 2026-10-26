//! The scoreboard view folds, ported from `cli/src/fno/scoreboard/fold.py`
//! (build_scoreboard, build_calibration, build_skill_scoreboard, build_lanes,
//! build_efficiency, build_plan_fidelity and their helpers). The one
//! delivery classification rides [`crate::scoreboard::classify`] in process,
//! the same answer the keeper op returns, so the views cannot drift into a
//! second classifier. `--by-provider` and the registry occupancy keep their
//! pre-existing native legs (`scoreboard_provider`, `state::load_registry`).
//!
//! One deliberate normalizer: the main view's `delivery_classes` object
//! froze from Python SET iteration, whose order is process hash luck and no
//! part of the contract. The Rust fold emits it key-sorted; the parity test
//! sorts that one object on both sides before comparing.

use super::readers::{event_data, num, num_opt, parse_ts, pct, read_graph_nodes};
use serde_json::{json, Map, Value};
use std::collections::{BTreeMap, BTreeSet, HashMap};

/// `_WEDGE_REASONS`: the stuck-terminal set. Everything else outside the
/// delivered vocabulary lands in `other`, so the spend split always
/// reconciles to the window total.
const WEDGE_REASONS: [&str; 3] = ["NoProgress", "Budget", "Aborted"];

const SURVIVAL_FOLLOWUP_DAYS: i64 = 14;

/// `_PLAN_PHASES`: a plan-only thread produces planning output and no build
/// phase. The discriminator is the phase SET, never the terminal reason.
const PLAN_PHASES: [&str; 2] = ["think", "plan"];

/// The delivered-terminal check `_is_shipped_reason` makes: the wire name
/// parses into the one `TerminationReason` vocabulary and reads delivered.
/// Unparseable junk (a hand-edited row) is never a ship reason.
pub(crate) fn is_shipped_reason(termination_reason: Option<&Value>) -> bool {
    let Some(name) = termination_reason.and_then(Value::as_str) else {
        return false;
    };
    serde_json::from_value::<crate::loopcheck::TerminationReason>(Value::String(name.to_string()))
        .map(|t| t.is_delivered())
        .unwrap_or(false)
}

/// `_row_shipped`: a terminal proves the ROW shipped only when its node
/// delivered; a row with no node keeps the terminal check. A reconcile
/// backstop row (created after a merged PR) ships by construction.
pub(crate) fn row_shipped(row: &Value, deliveries: Option<&Value>) -> bool {
    let termination_reason = row.get("termination_reason");
    let is_backstop = termination_reason.and_then(Value::as_str) == Some("reconcile-backstop")
        && row.get("pr_number").is_some();
    if !is_shipped_reason(termination_reason) && !is_backstop {
        return false;
    }
    let nid = row
        .get("graph_node_id")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty());
    let Some(nid) = nid else { return true };
    let Some(deliveries) = deliveries else {
        return true;
    };
    deliveries
        .get(nid)
        .and_then(|c| c.get("delivered"))
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

/// `_is_planned_row`.
pub(crate) fn is_planned_row(row: &Value) -> bool {
    if let Some(phases) = row.get("phases_completed").and_then(Value::as_array) {
        if !phases.is_empty()
            && phases.iter().all(|p| {
                p.as_str()
                    .map(|p| PLAN_PHASES.contains(&p))
                    .unwrap_or(false)
            })
        {
            return true;
        }
    }
    matches!(
        row.get("type").and_then(Value::as_str),
        Some("think") | Some("plan")
    )
}

fn in_window(
    ts_raw: Option<&Value>,
    cutoff: chrono::DateTime<chrono::Local>,
    now: chrono::DateTime<chrono::Local>,
) -> bool {
    matches!(parse_ts(ts_raw), Some(dt) if cutoff <= dt && dt <= now)
}

/// The delivery classification a fold needs: `by_node` plus the survival
/// payload, classified once per command run.
pub(crate) struct Classified {
    pub by_node: Value,
    pub survival: Value,
}

pub(crate) fn classify_deliveries(
    graph_nodes: &[Value],
    rows: &[Value],
    project: Option<&str>,
    now_iso: Option<&str>,
    since_days: Option<i64>,
) -> Result<Value, String> {
    let mut params = json!({
        "entries": graph_nodes,
        "rows": rows,
    });
    if let Some(project) = project {
        params["project"] = json!(project);
    }
    if let Some(now_iso) = now_iso {
        params["now"] = json!(now_iso);
    }
    if let Some(days) = since_days {
        params["since_days"] = json!(days);
    }
    crate::scoreboard::classify(&params)
}

/// One classify call per command run, shared by every view fold.
pub(crate) struct RunInputs {
    pub rows: Vec<Value>,
    pub graph_nodes: Vec<Value>,
    pub events_paths: Vec<std::path::PathBuf>,
    pub ledger_path: std::path::PathBuf,
    pub classified: Value,
    pub by_node: Value,
}

pub(crate) fn load_run(project: Option<&str>) -> Result<RunInputs, String> {
    let ledger_path = super::readers::ledger_path();
    let rows = match super::readers::load_ledger_rows(&ledger_path) {
        Ok(rows) => rows,
        Err(broken) => return Err(format!("BROKEN_LEDGER\u{1}{}", broken.message())),
    };
    let graph_nodes = read_graph_nodes(&super::readers::graph_db_path());
    let classified = classify_deliveries(&graph_nodes, &rows, project, None, None)
        .map_err(|e| format!("classifier: {e}"))?;
    let by_node = classified
        .get("by_node")
        .cloned()
        .unwrap_or_else(|| json!({}));
    Ok(RunInputs {
        events_paths: super::readers::events_paths(&ledger_path),
        ledger_path,
        rows,
        graph_nodes,
        classified,
        by_node,
    })
}

/// `_node_outcome`: reverted flag, else a caused_by fix-node within the
/// follow-up window (or un-time-boundable, conservative), else merged_clean.
pub(crate) fn node_outcome(
    nid: &str,
    shipped_at: Option<chrono::DateTime<chrono::Local>>,
    by_id: &BTreeMap<String, &Value>,
    fixes: &HashMap<String, Vec<Value>>,
) -> &'static str {
    if by_id
        .get(nid)
        .and_then(|n| n.get("reverted"))
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        return "reverted";
    }
    if let Some(fix_list) = fixes.get(nid) {
        for fix in fix_list {
            let fix_at = parse_ts(fix.get("created_at"));
            match (shipped_at, fix_at) {
                (Some(shipped), Some(fix_at)) => {
                    let delta = fix_at - shipped;
                    if delta >= chrono::Duration::zero()
                        && delta <= chrono::Duration::days(SURVIVAL_FOLLOWUP_DAYS)
                    {
                        return "bounced";
                    }
                }
                _ => return "bounced",
            }
        }
    }
    "merged_clean"
}

/// The `by_id` + `fixes` maps every outcome-deriving view builds.
pub(crate) struct NodeMaps<'a> {
    pub by_id: BTreeMap<String, &'a Value>,
    pub fixes: HashMap<String, Vec<Value>>,
    pub w4_available: bool,
}

pub(crate) fn node_maps(graph_nodes: &[Value]) -> NodeMaps<'_> {
    let mut by_id = BTreeMap::new();
    for node in graph_nodes {
        let Some(obj) = node.as_object() else {
            continue;
        };
        let Some(id) = obj.get("id").and_then(Value::as_str) else {
            continue;
        };
        by_id.insert(id.to_string(), node);
    }
    let mut fixes: HashMap<String, Vec<Value>> = HashMap::new();
    for node in graph_nodes {
        if let Some(origin) = node.get("caused_by").and_then(Value::as_str) {
            if !origin.is_empty() {
                fixes
                    .entry(origin.to_string())
                    .or_default()
                    .push(node.clone());
            }
        }
    }
    let w4_available = graph_nodes.iter().any(|n| {
        n.as_object()
            .map(|o| o.contains_key("reverted") || o.contains_key("caused_by"))
            .unwrap_or(false)
    });
    NodeMaps {
        by_id,
        fixes,
        w4_available,
    }
}

/// Python `round(x, 2)` at two decimals: f64 in, f64 out.
pub(crate) fn round2(value: f64) -> f64 {
    format!("{value:.2}").parse().unwrap_or(value)
}

// ── the main scoreboard fold ──────────────────────────────────────────

pub(crate) fn build_scoreboard(
    inputs: &RunInputs,
    touch_events: &[Value],
    since_days: i64,
    now: chrono::DateTime<chrono::Local>,
) -> Value {
    let cutoff = now - chrono::Duration::days(since_days);
    let windowed: Vec<&Value> = inputs
        .rows
        .iter()
        .filter(|r| in_window(r.get("completed"), cutoff, now))
        .collect();
    let total = windowed.len() as i64;
    let deliveries = &inputs.by_node;
    let survival =
        inputs.classified.get("survival").cloned().unwrap_or_else(
            || json!({"available": false, "reason": "classifier gave no survival"}),
        );
    let window_node_ids: BTreeSet<&str> = windowed
        .iter()
        .filter_map(|r| r.get("graph_node_id").and_then(Value::as_str))
        .filter(|s| !s.is_empty())
        .collect();
    // Every candidate the union `set(deliveries) | window_node_ids` holds,
    // then the delivered-and-windowed filter. The BTreeSet keeps the
    // candidate iteration deterministic where Python iterated a set.
    let mut candidates: BTreeSet<&str> = window_node_ids.clone();
    if let Some(map) = deliveries.as_object() {
        for nid in map.keys() {
            candidates.insert(nid.as_str());
        }
    }
    let delivered_nodes: BTreeSet<String> = candidates
        .into_iter()
        .filter(|nid| {
            let c = deliveries.get(nid);
            c.and_then(|c| c.get("delivered"))
                .and_then(Value::as_bool)
                .unwrap_or(false)
                && in_window(c.and_then(|c| c.get("ship_ts")), cutoff, now)
        })
        .map(str::to_string)
        .collect();

    if total == 0 && delivered_nodes.is_empty() {
        return json!({"state": "no_data", "since_days": since_days, "rows": 0});
    }

    let with_tr = windowed
        .iter()
        .filter(|r| {
            r.get("termination_reason")
                .and_then(Value::as_str)
                .map(|s| !s.is_empty())
                .unwrap_or(false)
        })
        .count() as i64;
    let with_node = windowed
        .iter()
        .filter(|r| {
            r.get("graph_node_id")
                .and_then(Value::as_str)
                .map(|s| !s.is_empty())
                .unwrap_or(false)
        })
        .count() as i64;
    let coverage = json!({
        "rows": total,
        "termination_reason_pct": pct(with_tr, total),
        "node_linkage_pct": pct(with_node, total),
    });

    // First-seen order over the windowed rows, the same order the Python
    // Counter froze: serde's preserve_order Map keeps it for the JSON view.
    let mut stop_cause = Map::new();
    for r in &windowed {
        if let Some(tr) = r.get("termination_reason").and_then(Value::as_str) {
            if !tr.is_empty() {
                let counter = stop_cause.entry(tr.to_string()).or_insert(json!(0));
                *counter = json!(counter.as_i64().unwrap_or(0) + 1);
            }
        }
    }

    let ship_rows: Vec<&Value> = windowed
        .iter()
        .filter(|r| row_shipped(r, Some(deliveries)))
        .copied()
        .collect();
    let terminal_shipped: BTreeSet<&str> = ship_rows
        .iter()
        .filter_map(|r| r.get("graph_node_id").and_then(Value::as_str))
        .collect();

    // Python built this dict from SET iteration (hash order, not part of
    // the contract); the Rust fold emits key-sorted order and the parity
    // test sorts the object on both sides.
    let mut delivery_classes = Map::new();
    for nid in &delivered_nodes {
        if let Some(class) = deliveries
            .get(nid)
            .and_then(|c| c.get("class"))
            .and_then(Value::as_str)
        {
            let counter = delivery_classes
                .entry(class.to_string())
                .or_insert(json!(0));
            *counter = json!(counter.as_i64().unwrap_or(0) + 1);
        }
    }
    let delivery_classes = sort_object(delivery_classes);

    // Spend follows the node, not the reason: a run whose PR merged is ship
    // spend regardless of how its session stopped. A row with no node keeps
    // the reason-based split (nothing else is known about it).
    let mut ship_cost = 0.0f64;
    let mut wedge_cost = 0.0f64;
    let mut other_cost = 0.0f64;
    for r in &windowed {
        let cost = num(r.get("cost_usd"));
        let tr = r.get("termination_reason").and_then(Value::as_str);
        let nid = r
            .get("graph_node_id")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty());
        if let Some(nid) = nid {
            if delivered_nodes.contains(nid) {
                ship_cost += cost;
            } else if tr.map(|t| WEDGE_REASONS.contains(&t)).unwrap_or(false) {
                wedge_cost += cost;
            } else {
                other_cost += cost;
            }
        } else if is_shipped_reason(r.get("termination_reason")) {
            ship_cost += cost;
        } else if tr.map(|t| WEDGE_REASONS.contains(&t)).unwrap_or(false) {
            wedge_cost += cost;
        } else {
            other_cost += cost;
        }
    }
    let spend = json!({
        "ship_terminal_usd": round2(ship_cost),
        "wedge_terminal_usd": round2(wedge_cost),
        "other_usd": round2(other_cost),
    });

    let autonomy = autonomy(touch_events, &delivered_nodes, cutoff, now);
    let full = coverage["termination_reason_pct"] == json!(100)
        && autonomy["available"] == json!(true)
        && survival["available"] == json!(true);
    json!({
        "state": if full { "full" } else { "partial" },
        "since_days": since_days,
        "coverage": coverage,
        "shipped_nodes": delivered_nodes.len(),
        "shipped_by_terminal": terminal_shipped.len(),
        "merged_nodes_without_ledger_row": delivered_nodes
            .iter()
            .filter(|nid| {
                deliveries.get(nid.as_str()).and_then(|c| c.get("class")).and_then(Value::as_str)
                    == Some("merged")
                    && !window_node_ids.contains(nid.as_str())
            })
            .count(),
        "delivery_classes": delivery_classes,
        "stop_cause": Value::Object(stop_cause),
        "spend": spend,
        "autonomy": autonomy,
        "survival": survival,
    })
}

/// `_autonomy`.
fn autonomy(
    touch_events: &[Value],
    shipped_nodes: &BTreeSet<String>,
    cutoff: chrono::DateTime<chrono::Local>,
    now: chrono::DateTime<chrono::Local>,
) -> Value {
    if touch_events.is_empty() {
        return json!({"available": false, "reason": "no human_touch signals (Wave 4 not shipped)"});
    }
    if shipped_nodes.is_empty() {
        return json!({"available": false, "reason": "nothing shipped in the window"});
    }
    let in_window: Vec<&Value> = touch_events
        .iter()
        .filter(|e| event_in_window(e, cutoff, now))
        .collect();
    json!({
        "available": true,
        "touches": in_window.len(),
        "shipped_nodes": shipped_nodes.len(),
        "touches_per_shipped_node": round2(in_window.len() as f64 / shipped_nodes.len() as f64),
    })
}

/// `_event_in_window`: an undated event has no window to belong to.
fn event_in_window(
    event: &Value,
    cutoff: chrono::DateTime<chrono::Local>,
    now: chrono::DateTime<chrono::Local>,
) -> bool {
    let ts_raw = event
        .get("ts")
        .or_else(|| event.get("data").and_then(|d| d.get("ts")));
    matches!(parse_ts(ts_raw), Some(dt) if cutoff <= dt && dt <= now)
}

/// Sort an object's keys (the one set-derived object the fold emits).
pub(crate) fn sort_object(map: Map<String, Value>) -> Value {
    let sorted: BTreeMap<String, Value> = map.into_iter().collect();
    Value::Object(sorted.into_iter().collect())
}

// ── verifier calibration (W6) ─────────────────────────────────────────

const CALIBRATION_MIN_VERDICTS: usize = 10;
const COUNTABLE_VERDICTS: [&str; 3] = ["pass", "concerns", "fail"];
const OUTCOMES: [&str; 3] = ["merged_clean", "bounced", "reverted"];

pub(crate) fn build_calibration(
    verdict_events: &[Value],
    inputs: &RunInputs,
    since_days_unused: i64,
    node_maps: &NodeMaps,
) -> Value {
    let _ = since_days_unused;
    let mut final_verdict: Vec<(String, &str)> = Vec::new();
    let mut unattributed = 0i64;
    for e in verdict_events {
        let data = event_data(e);
        let Some(v) = data.get("verdict").and_then(Value::as_str) else {
            continue;
        };
        if !COUNTABLE_VERDICTS.contains(&v) && v != "error" && v != "not_applicable" {
            continue;
        }
        let Some(nid) = data.get("graph_node_id").and_then(Value::as_str) else {
            unattributed += 1;
            continue;
        };
        // Append order: last write wins.
        final_verdict.retain(|(existing, _)| existing != nid);
        final_verdict.push((nid.to_string(), v));
    }
    let mut excluded = Map::new();
    for (_, v) in &final_verdict {
        if !COUNTABLE_VERDICTS.contains(v) {
            let counter = excluded.entry(v.to_string()).or_insert(json!(0));
            *counter = json!(counter.as_i64().unwrap_or(0) + 1);
        }
    }
    let counted: Vec<(String, &str)> = final_verdict
        .iter()
        .filter(|(_, v)| COUNTABLE_VERDICTS.contains(v))
        .cloned()
        .collect();
    let n = counted.len() as i64;
    if (n as usize) < CALIBRATION_MIN_VERDICTS {
        return json!({
            "state": "insufficient",
            "need": CALIBRATION_MIN_VERDICTS,
            "n": n,
            "excluded": Value::Object(excluded),
            "unattributed": unattributed,
        });
    }

    let deliveries = &inputs.by_node;
    let mut ship_ts: BTreeMap<&str, chrono::DateTime<chrono::Local>> = BTreeMap::new();
    if let Some(map) = deliveries.as_object() {
        for (nid, c) in map {
            if !c.get("delivered").and_then(Value::as_bool).unwrap_or(false) {
                continue;
            }
            if let Some(dt) = parse_ts(c.get("ship_ts")) {
                ship_ts.insert(nid.as_str(), dt);
            }
        }
    }

    // Table rows keep the Python literal's fixed key order; the verdict x
    // outcome cells stay i64.
    let mut table: Map<String, Value> = Map::new();
    for v in COUNTABLE_VERDICTS {
        let mut row = Map::new();
        for o in OUTCOMES {
            row.insert(o.to_string(), json!(0));
        }
        table.insert(v.to_string(), Value::Object(row));
    }
    for (nid, v) in &counted {
        let outcome = node_outcome(
            nid,
            ship_ts.get(nid.as_str()).copied(),
            &node_maps.by_id,
            &node_maps.fixes,
        );
        let cell = table
            .get_mut(*v)
            .and_then(|row| row.get_mut(outcome))
            .unwrap_or_else(|| panic!("calibration table cell missing {v}/{outcome}"));
        *cell = json!(cell.as_i64().unwrap_or(0) + 1);
    }
    let untimed = counted
        .iter()
        .filter(|(nid, _)| !ship_ts.contains_key(nid.as_str()))
        .count();
    let cells = |v: &str, o: &str| -> i64 { table[v].get(o).and_then(Value::as_i64).unwrap_or(0) };
    let fp = cells("pass", "bounced") + cells("pass", "reverted");
    let passes = COUNTABLE_VERDICTS
        .iter()
        .map(|o| cells("pass", o))
        .sum::<i64>();
    json!({
        "state": "ok",
        "n": n,
        "excluded": Value::Object(excluded),
        "unattributed": unattributed,
        "untimed_outcomes": untimed,
        "table": Value::Object(table),
        "false_positive": {
            "count": fp,
            "of_pass": passes,
            "rate_pct": pct(fp, passes),
        },
    })
}

// ── skill-outcome attribution ─────────────────────────────────────────

/// `_PHASE_TO_SKILL`: /target's phase names route to skill ids; "do" is the
/// retired spelling whose key stays until the ledger rows age out.
const PHASE_TO_SKILL: [(&str, &str); 8] = [
    ("think", "fno:think"),
    ("plan", "fno:blueprint"),
    ("do", "fno:do"),
    ("execute", "fno:execute"),
    ("review", "fno:review"),
    ("docs", "fno:ship-docs"),
    ("ship", "fno:ship"),
    ("external", "fno:ship"),
];

/// `_skills_from_transcript`: the `input.skill` id of each Skill tool_use
/// block (duplicates kept).
pub(crate) fn skills_from_transcript(lines: &[&str]) -> Vec<String> {
    let mut found = Vec::new();
    for line in lines {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(obj) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let Some(content) = obj
            .get("message")
            .and_then(|m| m.get("content"))
            .and_then(Value::as_array)
        else {
            continue;
        };
        for block in content {
            let is_skill = block.get("type").and_then(Value::as_str) == Some("tool_use")
                && block.get("name").and_then(Value::as_str) == Some("Skill");
            if !is_skill {
                continue;
            }
            if let Some(skill) = block
                .get("input")
                .and_then(|i| i.get("skill"))
                .and_then(Value::as_str)
            {
                if !skill.is_empty() {
                    found.push(skill.to_string());
                }
            }
        }
    }
    found
}

/// `_default_read_transcript`: the session transcript across all project
/// dirs, split into lines; None when it does not resolve.
pub(crate) fn read_transcript(session_id: &str) -> Option<Vec<String>> {
    if !is_uuid(session_id) {
        return None;
    }
    let base = std::env::var("HOME").ok()?; // Path.home()
    let base = std::path::PathBuf::from(base)
        .join(".claude")
        .join("projects");
    if !base.is_dir() {
        return None;
    }
    for project_dir in std::fs::read_dir(&base).ok()? {
        let project_dir = project_dir.ok()?.path();
        if !project_dir.is_dir() {
            continue;
        }
        let candidate = project_dir.join(format!("{session_id}.jsonl"));
        if candidate.exists() {
            return std::fs::read_to_string(&candidate)
                .ok()
                .map(|text| text.lines().map(str::to_string).collect());
        }
    }
    None
}

fn is_uuid(s: &str) -> bool {
    s.len() == 36
        && s.as_bytes().iter().enumerate().all(|(i, b)| match i {
            8 | 13 | 18 | 23 => *b == b'-',
            _ => b.is_ascii_hexdigit(),
        })
}

/// The transcript lines as `&str` slices for the scanners.
pub(crate) fn line_slices(lines: &[String]) -> Vec<&str> {
    lines.iter().map(String::as_str).collect()
}

/// `_PHASE_TO_SKILL_DIR`: the ledger phase and the skill directory are
/// separate vocabularies; old phase rows point at the renamed skill.
const PHASE_TO_SKILL_DIR: [(&str, &str); 2] = [("do", "execute"), ("execute", "execute")];

/// `_skill_commit_history`: all commits touching rel, oldest first. Empty on
/// any git failure - never an error. Process-lifetime cache keyed by
/// (root, rel), the same shape the Python fold cached.
pub(crate) fn skill_commit_history(
    root: &std::path::Path,
    rel: &str,
) -> Vec<(chrono::DateTime<chrono::Local>, String)> {
    use std::sync::Mutex;
    static CACHE: Mutex<
        Option<HashMap<(String, String), Vec<(chrono::DateTime<chrono::Local>, String)>>>,
    > = Mutex::new(None);
    let key = (root.display().to_string(), rel.to_string());
    if let Ok(guard) = CACHE.lock() {
        if let Some(cache) = guard.as_ref() {
            if let Some(hit) = cache.get(&key) {
                return hit.clone();
            }
        }
    }
    let mut history: Vec<(chrono::DateTime<chrono::Local>, String)> = Vec::new();
    if let Ok(out) = std::process::Command::new("git")
        .args(["log", "--follow", "--format=%h %aI", "--", rel])
        .current_dir(root)
        .output()
    {
        if out.status.success() {
            let text = String::from_utf8_lossy(&out.stdout);
            for line in text.lines() {
                let (h, iso) = line.split_once(' ').unwrap_or((line, ""));
                let value = Value::String(iso.to_string());
                if let (false, Some(dt)) = (h.is_empty(), parse_ts(Some(&value))) {
                    history.push((dt, h));
                }
            }
        }
    }
    history.sort_by_key(|(dt, _)| *dt);
    if let Ok(mut guard) = CACHE.lock() {
        guard
            .get_or_insert_with(HashMap::new)
            .insert(key, history.clone());
    }
    history
}

/// `_default_skill_version`: best-effort git hash of the skill's SKILL.md at
/// (or before) ts. Any failure reads "unknown", never an error (AC-EDGE).
pub(crate) fn default_skill_version(skill_id: &str, ts_raw: Option<&Value>) -> String {
    let name = skill_id.split(':').nth(1).unwrap_or(skill_id);
    let name = PHASE_TO_SKILL_DIR
        .iter()
        .find(|(phase, _)| *phase == name)
        .map(|(_, dir)| *dir)
        .unwrap_or(name);
    let rel = format!("skills/{name}/SKILL.md");
    // `resolve_repo_root()` answered from the process cwd on the Python leg.
    let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
    let root = crate::paths::worktree_repo_root(&cwd);
    let root = if root.as_os_str().is_empty() {
        return "unknown".to_string();
    } else {
        root
    };
    if !root.join(&rel).exists() {
        return "unknown".to_string();
    }
    let now_local = chrono::Local::now();
    let until = parse_ts(ts_raw).unwrap_or(now_local);
    let mut best: Option<String> = None;
    for (dt, h) in skill_commit_history(&root, &rel) {
        if dt > until {
            break;
        }
        best = Some(h);
    }
    best.unwrap_or_else(|| "unknown".to_string())
}

/// `_extract_skill_runs`: (distinct skill ids, method) for one row.
pub(crate) fn extract_skill_runs(row: &Value) -> (Vec<String>, &'static str) {
    let uuid_re_match = |s: &str| is_uuid(s);
    let sessions: Vec<&str> = row
        .get("sessions")
        .and_then(Value::as_array)
        .map(|list| {
            list.iter()
                .filter_map(Value::as_str)
                .filter(|s| uuid_re_match(s))
                .collect()
        })
        .unwrap_or_default();
    let mut found: Vec<String> = Vec::new();
    for sid in sessions {
        if let Some(lines) = read_transcript(sid) {
            found.extend(skills_from_transcript(&line_slices(&lines)));
        }
    }
    if !found.is_empty() {
        found.sort();
        found.dedup();
        return (found, "transcript");
    }
    let phases: BTreeSet<&str> = row
        .get("phases_completed")
        .and_then(Value::as_array)
        .map(|list| {
            list.iter()
                .filter_map(Value::as_str)
                .filter(|p| PHASE_TO_SKILL.iter().any(|(phase, _)| phase == p))
                .collect()
        })
        .unwrap_or_default();
    if !phases.is_empty() {
        let proxy: Vec<String> = phases
            .into_iter()
            .filter_map(|p| {
                PHASE_TO_SKILL
                    .iter()
                    .find(|(phase, _)| *phase == p)
                    .map(|(_, skill)| skill.to_string())
            })
            .collect();
        return (proxy, "phase-proxy");
    }
    (Vec::new(), "unattributed")
}

/// `_new_skill_bucket`.
struct SkillBucket {
    runs: i64,
    shipped: i64,
    shipped_linked: i64,
    reverted: i64,
    cost_total: f64,
    touches_total: i64,
    methods: BTreeSet<&'static str>,
}

pub(crate) fn build_skill_scoreboard(
    inputs: &RunInputs,
    touch_events: &[Value],
    since_days: i64,
    now: chrono::DateTime<chrono::Local>,
) -> Value {
    let cutoff = now - chrono::Duration::days(since_days);
    let windowed: Vec<&Value> = inputs
        .rows
        .iter()
        .filter(|r| in_window(r.get("completed"), cutoff, now))
        .collect();
    let total = windowed.len() as i64;
    if total == 0 {
        return json!({"state": "no_data", "since_days": since_days, "rows": 0});
    }

    let maps = node_maps(&inputs.graph_nodes);
    let touches_by_node = count_touches_by_node(touch_events, cutoff, now);
    let deliveries = &inputs.by_node;

    let mut buckets: BTreeMap<(String, String), SkillBucket> = BTreeMap::new();
    let mut attributed = 0i64;
    let mut ordered_buckets: Vec<((String, String), SkillBucket)> = Vec::new();
    let mut bucket_index: HashMap<(String, String), usize> = HashMap::new();

    for r in &windowed {
        let (skills, method) = extract_skill_runs(r);
        let shipped = row_shipped(r, Some(deliveries));
        let nid = r
            .get("graph_node_id")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty());
        let cost = num(r.get("cost_usd"));
        let touches = nid
            .and_then(|nid| touches_by_node.get(nid))
            .copied()
            .unwrap_or(0);

        if skills.is_empty() {
            let key = ("unattributed".to_string(), "-".to_string());
            let idx = *bucket_index.entry(key.clone()).or_insert_with(|| {
                ordered_buckets.push((
                    key,
                    SkillBucket {
                        runs: 0,
                        shipped: 0,
                        shipped_linked: 0,
                        reverted: 0,
                        cost_total: 0.0,
                        touches_total: 0,
                        methods: BTreeSet::new(),
                    },
                ));
                ordered_buckets.len() - 1
            });
            let b = &mut ordered_buckets[idx].1;
            b.runs += 1;
            b.cost_total += cost;
            b.touches_total += touches;
            continue;
        }
        attributed += 1;

        let judgeable = shipped
            && nid.is_some()
            && maps.w4_available
            && nid.map(|n| maps.by_id.contains_key(n)).unwrap_or(false);
        let outcome = match (judgeable, nid) {
            (true, Some(nid)) if maps.by_id.contains_key(nid) => Some(node_outcome(
                nid,
                parse_ts(r.get("completed")),
                &maps.by_id,
                &maps.fixes,
            )),
            _ => None,
        };

        for skill in &skills {
            let version = default_skill_version(skill, r.get("completed"));
            let key = (skill.clone(), version);
            let idx = *bucket_index.entry(key.clone()).or_insert_with(|| {
                ordered_buckets.push((
                    key,
                    SkillBucket {
                        runs: 0,
                        shipped: 0,
                        shipped_linked: 0,
                        reverted: 0,
                        cost_total: 0.0,
                        touches_total: 0,
                        methods: BTreeSet::new(),
                    },
                ));
                ordered_buckets.len() - 1
            });
            let b = &mut ordered_buckets[idx].1;
            b.runs += 1;
            b.cost_total += cost;
            b.touches_total += touches;
            b.methods.insert(method);
            if shipped {
                b.shipped += 1;
                if judgeable {
                    b.shipped_linked += 1;
                    if outcome == Some("reverted") || outcome == Some("bounced") {
                        b.reverted += 1;
                    }
                }
            }
        }
    }

    let mut out_rows: Vec<Value> = ordered_buckets
        .into_iter()
        .map(|((skill, version), b)| {
            let runs = b.runs;
            json!({
                "skill": skill,
                "version": version,
                "runs": runs,
                "ship_rate_pct": pct(b.shipped, runs),
                // None (not 0) when nothing was judgeable - a bare 0% would
                // read as "zero of N judged rows reverted", a different fact.
                "revert_rate_pct": if b.shipped_linked > 0 {
                    Value::Number(serde_json::Number::from(pct(b.reverted, b.shipped_linked)))
                } else {
                    Value::Null
                },
                "touches_per_run": round2(b.touches_total as f64 / runs as f64),
                "cost_per_run": round2(b.cost_total / runs as f64),
                "method": if b.methods.is_empty() {
                    "unattributed".to_string()
                } else {
                    b.methods.into_iter().collect::<Vec<_>>().join("+")
                },
            })
        })
        .collect();
    out_rows.sort_by(|a, b| {
        let ra = a["runs"].as_i64().unwrap_or(0);
        let rb = b["runs"].as_i64().unwrap_or(0);
        rb.cmp(&ra).then_with(|| {
            a["skill"]
                .as_str()
                .unwrap_or_default()
                .cmp(b["skill"].as_str().unwrap_or_default())
        })
    });

    json!({
        "state": "ok",
        "since_days": since_days,
        "coverage": {"rows": total, "attributed_pct": pct(attributed, total)},
        "rows": out_rows,
    })
}

/// The touches-per-node counter `--by-skill` folds from human_touch events.
fn count_touches_by_node(
    touch_events: &[Value],
    cutoff: chrono::DateTime<chrono::Local>,
    now: chrono::DateTime<chrono::Local>,
) -> HashMap<String, i64> {
    let mut touches: HashMap<String, i64> = HashMap::new();
    for e in touch_events {
        if !e.is_object() {
            continue;
        }
        if !event_in_window(e, cutoff, now) {
            continue;
        }
        let nid = e
            .get("graph_node_id")
            .and_then(Value::as_str)
            .map(String::from)
            .or_else(|| {
                event_data(e)
                    .get("graph_node_id")
                    .and_then(Value::as_str)
                    .map(String::from)
            });
        if let Some(nid) = nid.filter(|s| !s.is_empty()) {
            *touches.entry(nid).or_default() += 1;
        }
    }
    touches
}

// ── lane truth ────────────────────────────────────────────────────────

const LANE_COVERAGE_FLOOR: i64 = 5;

fn lane_axis<'a>(value: Option<&'a str>, fallback: &'a str) -> &'a str {
    value.filter(|v| !v.is_empty()).unwrap_or(fallback)
}

fn lane_key(row: &Value) -> (String, String, String) {
    (
        lane_axis(row.get("provider").and_then(Value::as_str), "unattributed").to_string(),
        lane_axis(row.get("model").and_then(Value::as_str), "unknown").to_string(),
        lane_axis(row.get("effort").and_then(Value::as_str), "unknown").to_string(),
    )
}

pub(crate) fn build_lanes(
    inputs: &RunInputs,
    registry_rows: &[Value],
    rate_limit_events: &[Value],
    caps: &HashMap<String, Option<i64>>,
    since_days: i64,
    now: chrono::DateTime<chrono::Local>,
) -> Value {
    let cutoff = now - chrono::Duration::days(since_days);
    let deliveries = &inputs.by_node;
    let in_window: Vec<&Value> = inputs
        .rows
        .iter()
        .filter(|row| {
            row.get("type").and_then(Value::as_str) == Some("execution")
                && matches!(parse_ts(row.get("completed")), Some(dt) if cutoff <= dt && dt <= now)
        })
        .collect();
    let mut sizes: HashMap<String, String> = HashMap::new();
    for node in &inputs.graph_nodes {
        if let (Some(id), Some(size)) = (
            node.get("id").and_then(Value::as_str),
            node.get("size").and_then(Value::as_str),
        ) {
            let size = if size.is_empty() { "unknown" } else { size };
            sizes.insert(id.to_string(), size.to_string());
        }
    }
    // Bucket insertion order rides sorted (provider, model, effort, size)
    // because Python iterated `sorted(buckets.items())` before rendering.
    let mut buckets: BTreeMap<(String, String, String, String), (i64, i64, f64, i64)> =
        BTreeMap::new();
    let mut coverage = Map::new();
    coverage.insert(
        "provider".to_string(),
        json!(in_window
            .iter()
            .filter(|row| row
                .get("provider")
                .and_then(Value::as_str)
                .map(|s| !s.is_empty())
                .unwrap_or(false))
            .count()),
    );
    coverage.insert(
        "model".to_string(),
        json!(in_window
            .iter()
            .filter(|row| row
                .get("model")
                .and_then(Value::as_str)
                .map(|s| !s.is_empty())
                .unwrap_or(false))
            .count()),
    );
    coverage.insert(
        "effort".to_string(),
        json!(in_window
            .iter()
            .filter(|row| row
                .get("effort")
                .and_then(Value::as_str)
                .map(|s| !s.is_empty())
                .unwrap_or(false))
            .count()),
    );
    coverage.insert("rows".to_string(), json!(in_window.len()));
    for row in &in_window {
        let (provider, model, effort) = lane_key(row);
        let size = row
            .get("graph_node_id")
            .and_then(Value::as_str)
            .and_then(|nid| sizes.get(nid))
            .cloned()
            .unwrap_or_else(|| "unknown".to_string());
        let entry = buckets
            .entry((provider, model, effort, size))
            .or_insert((0, 0, 0.0, 0));
        entry.0 += 1;
        entry.1 += i64::from(row_shipped(row, Some(deliveries)));
        entry.2 += num(row.get("duration_minutes"));
        entry.3 += num(row.get("carveouts_filed").or_else(|| row.get("carveouts"))) as i64;
    }
    let retrospective: Vec<Value> = buckets
        .into_iter()
        .map(|((provider, model, effort, size), (runs, ok, wall_minutes, carveouts))| {
            json!({
                "provider": provider,
                "model": model,
                "effort": effort,
                "size": size,
                "runs": runs,
                "ok_pct": pct(ok, runs),
                "wall_minutes": round1(wall_minutes),
                "carveouts_filed": carveouts,
                "sample_state": if runs >= LANE_COVERAGE_FLOOR { "ok" } else { "insufficient_sample" },
            })
        })
        .collect();

    let active_statuses = ["live", "busy", "ready", "idle", "spawning"];
    let mut occupancy: BTreeMap<(String, String, String), i64> = BTreeMap::new();
    for row in registry_rows {
        let status = row
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if active_statuses.contains(&status) {
            *occupancy.entry(lane_key(row)).or_default() += 1;
        }
    }
    let live: Vec<Value> = occupancy
        .iter()
        .map(|((provider, model, effort), count)| {
            let cap = caps.get(provider).copied().flatten();
            json!({
                "provider": provider,
                "model": model,
                "effort": effort,
                "occupancy": count,
                "cap": cap,
                "headroom": cap.map(|c| c - count),
            })
        })
        .collect();

    let mut rate_limited = 0i64;
    for event in rate_limit_events {
        let data = event_data(event);
        let raw = event.get("ts").or_else(|| data.get("ts"));
        match parse_ts(raw) {
            None => rate_limited += 1,
            Some(dt) => {
                if cutoff <= dt && dt <= now {
                    rate_limited += 1;
                }
            }
        }
    }
    json!({
        "state": if !in_window.is_empty() || !live.is_empty() { "ok" } else { "no_data" },
        "since_days": since_days,
        "coverage": Value::Object(coverage),
        "retrospective": retrospective,
        "live": live,
        "rate_limited": rate_limited,
        "coverage_floor": LANE_COVERAGE_FLOOR,
    })
}

/// Python `round(x, 1)`.
pub(crate) fn round1(value: f64) -> f64 {
    format!("{value:.1}").parse().unwrap_or(value)
}

// ── session-efficiency fold ───────────────────────────────────────────

/// Recognized `ci` shapes: `FAILURE:*` is red; the rest are recognized
/// benign states the emitter really produces. Anything else is drift.
const CI_RED_PREFIX: &str = "FAILURE:";
const CI_RECOGNIZED_NONRED: [&str; 5] = ["SUCCESS", "PENDING", "none", "unknown", "skipped"];

/// `_percentile`: nearest-rank over a sorted copy; None on empty.
pub(crate) fn percentile(values: &mut Vec<f64>, p: i64) -> Option<f64> {
    if values.is_empty() {
        return None;
    }
    values.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let n = values.len() as i64;
    let k = std::cmp::max(1, super::readers::py_round(p as f64 / 100.0 * n as f64));
    values.get((k - 1) as usize).copied()
}

/// `_row_session_ids`: the `sessions` list plus the scalar `session_id`.
pub(crate) fn row_session_ids(row: &Value) -> BTreeSet<String> {
    let mut ids = BTreeSet::new();
    if let Some(list) = row.get("sessions").and_then(Value::as_array) {
        for s in list {
            if let Some(s) = s.as_str().filter(|s| !s.is_empty()) {
                ids.insert(s.to_string());
            }
        }
    }
    if let Some(scalar) = row.get("session_id").and_then(Value::as_str) {
        if !scalar.is_empty() {
            ids.insert(scalar.to_string());
        }
    }
    ids
}

/// `_ci_reds_from_fires`: distinct red EPISODES across ts-ordered ci values;
/// (None, unparsed) when any fire carried an unrecognized shape.
pub(crate) fn ci_reds_from_fires(ci_ordered: &[Option<&str>]) -> (Option<i64>, i64) {
    let mut unparsed = 0i64;
    for ci in ci_ordered {
        let recognized = ci
            .map(|c| c.starts_with(CI_RED_PREFIX) || CI_RECOGNIZED_NONRED.contains(&c))
            .unwrap_or(false);
        if !recognized {
            unparsed += 1;
        }
    }
    if unparsed > 0 {
        return (None, unparsed);
    }
    let mut episodes = 0i64;
    let mut prev_red = false;
    for ci in ci_ordered {
        let is_red = ci.map(|c| c.starts_with(CI_RED_PREFIX)).unwrap_or(false);
        if is_red && !prev_red {
            episodes += 1;
        }
        prev_red = is_red;
    }
    (Some(episodes), 0)
}

/// `_transcript_counts`: best-effort (turns, toolcalls); both None when no
/// transcript resolves, never 0.
fn transcript_counts(session_ids: &BTreeSet<String>) -> (Option<i64>, Option<i64>) {
    let mut turns = 0i64;
    let mut toolcalls = 0i64;
    let mut found_any = false;
    for sid in session_ids {
        let Some(lines) = read_transcript(sid) else {
            continue;
        };
        found_any = true;
        for line in &lines {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            let Ok(obj) = serde_json::from_str::<Value>(line) else {
                continue;
            };
            if !obj.is_object() {
                continue;
            }
            if obj.get("message").map(|m| !m.is_null()).unwrap_or(false)
                || matches!(
                    obj.get("type").and_then(Value::as_str),
                    Some("user") | Some("assistant")
                )
            {
                turns += 1;
            }
            if let Some(content) = obj
                .get("message")
                .and_then(|m| m.get("content"))
                .and_then(Value::as_array)
            {
                toolcalls += content
                    .iter()
                    .filter(|b| b.get("type").and_then(Value::as_str) == Some("tool_use"))
                    .count() as i64;
            }
        }
    }
    if !found_any {
        return (None, None);
    }
    (Some(turns), Some(toolcalls))
}

/// `_dist`: median/p90/n over the non-None values only.
fn dist(values: Vec<Option<f64>>) -> Value {
    let present: Vec<f64> = values.into_iter().flatten().collect();
    let n = present.len() as i64;
    let mut vals = present.clone();
    let median = percentile(&mut vals, 50);
    let mut vals = present;
    let p90 = percentile(&mut vals, 90);
    json!({
        "median": median,
        "p90": p90,
        "n": n,
    })
}

pub(crate) fn build_efficiency(
    inputs: &RunInputs,
    loop_events: &[Value],
    since_days: i64,
    now: chrono::DateTime<chrono::Local>,
) -> Value {
    let cutoff = now - chrono::Duration::days(since_days);
    let windowed: Vec<&Value> = inputs
        .rows
        .iter()
        .filter(|r| in_window(r.get("completed"), cutoff, now))
        .collect();
    let total = windowed.len() as i64;
    if total == 0 {
        return json!({"state": "no_data", "since_days": since_days, "rows": 0});
    }
    let deliveries = &inputs.by_node;

    // Index loop_check fires by session id: (ts_sort, ci), undated last.
    let mut fires_by_session: HashMap<
        String,
        Vec<(chrono::DateTime<chrono::Local>, Option<String>)>,
    > = HashMap::new();
    for e in loop_events {
        if !e.is_object() {
            continue;
        }
        let data = event_data(e);
        let Some(sid) = data.get("session_id").and_then(Value::as_str) else {
            continue;
        };
        if sid.is_empty() {
            continue;
        }
        // Python sorted fires on `(dt or datetime.max)`: undated LAST.
        let dt = parse_ts(e.get("ts"))
            .or_else(|| parse_ts(data.get("ts")))
            .unwrap_or_else(super::readers::datetime_max);
        fires_by_session.entry(sid.to_string()).or_default().push((
            dt,
            data.get("ci").and_then(Value::as_str).map(str::to_string),
        ));
    }

    let maps = node_maps(&inputs.graph_nodes);
    let mut joined_rows = 0i64;
    let mut transcript_rows = 0i64;
    let mut node_linked = 0i64;
    let mut ci_unparsed_total = 0i64;
    let mut outcome_tracked = 0i64;
    let mut shipped_rows = 0i64;
    let mut plan_cost_by_node: Vec<(String, f64)> = Vec::new();
    let mut build_cost_by_node: HashMap<String, f64> = HashMap::new();

    let mut buckets: Vec<(String, Bucket)> = Vec::new();
    let mut bucket_index: HashMap<String, usize> = HashMap::new();
    let mut per_row_fires: Vec<Option<f64>> = Vec::new();
    let mut per_row_reds: Vec<Option<f64>> = Vec::new();
    let mut per_row_tokens: Vec<Option<f64>> = Vec::new();
    let mut per_row_duration: Vec<Option<f64>> = Vec::new();

    for r in &windowed {
        let sids = row_session_ids(r);
        let mut fires: Vec<(chrono::DateTime<chrono::Local>, Option<String>)> = Vec::new();
        for sid in &sids {
            if let Some(list) = fires_by_session.get(sid) {
                fires.extend(list.iter().cloned());
            }
        }
        fires.sort_by_key(|(dt, _)| *dt);
        let ci_ordered: Vec<Option<&str>> = fires.iter().map(|(_, ci)| ci.as_deref()).collect();

        let loop_fires: Option<i64> = if fires.is_empty() {
            None
        } else {
            Some(fires.len() as i64)
        };
        let mut ci_reds: Option<i64> = None;
        if let Some(n) = loop_fires {
            if n > 0 {
                joined_rows += 1;
                let (reds, unparsed) = ci_reds_from_fires(&ci_ordered);
                ci_unparsed_total += unparsed;
                ci_reds = reds;
            }
        }

        let (turns, _toolcalls) = transcript_counts(&sids);
        if turns.is_some() {
            transcript_rows += 1;
        }

        let nid = r
            .get("graph_node_id")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty());
        if nid.is_some() {
            node_linked += 1;
        }

        let shipped = row_shipped(r, Some(deliveries));
        let class: &str;
        if shipped {
            shipped_rows += 1;
            if maps.w4_available && nid.map(|n| maps.by_id.contains_key(n)).unwrap_or(false) {
                let outcome = node_outcome(
                    nid.unwrap(),
                    parse_ts(r.get("completed")),
                    &maps.by_id,
                    &maps.fixes,
                );
                outcome_tracked += 1;
                class = outcome;
            } else {
                class = "shipped_untracked";
            }
            if let Some(nid) = nid {
                *build_cost_by_node.entry(nid.to_string()).or_default() += num(r.get("cost_usd"));
            }
        } else if is_planned_row(r) {
            class = "planned";
            if let Some(nid) = nid {
                upsert_plan_cost(&mut plan_cost_by_node, nid, num(r.get("cost_usd")));
            }
        } else if r.get("termination_reason").and_then(Value::as_str) == Some("delegated") {
            // A handed-off thread is not waste; keep it out of `unshipped`.
            class = "delegated";
        } else {
            class = "unshipped";
        }

        let tokens = num_opt(r.get("tokens_total"));
        let duration = num_opt(r.get("duration_minutes"));
        let idx = *bucket_index.entry(class.to_string()).or_insert_with(|| {
            buckets.push((
                class.to_string(),
                Bucket {
                    n: 0,
                    spend_usd: 0.0,
                    tokens: Vec::new(),
                    fires: Vec::new(),
                    duration: Vec::new(),
                },
            ));
            buckets.len() - 1
        });
        let b = &mut buckets[idx].1;
        b.n += 1;
        b.spend_usd += num(r.get("cost_usd"));
        if let Some(t) = tokens {
            b.tokens.push(t);
        }
        if let Some(d) = duration {
            b.duration.push(d);
        }
        if let Some(f) = loop_fires {
            b.fires.push(f as f64);
        }

        if loop_fires.is_some() {
            per_row_fires.push(loop_fires.map(|f| f as f64));
            per_row_reds.push(ci_reds.map(|r| r as f64));
            per_row_tokens.push(tokens);
            per_row_duration.push(duration);
        }
    }

    // Python rendered per_outcome_class as a dict comprehension over the
    // insertion-ordered buckets, then the TEXT renderer sorted by (-n,
    // class). The JSON view freezes insertion order.
    let per_outcome_class: Vec<Value> = buckets
        .iter()
        .map(|(cls, b)| {
            let mut tokens = b.tokens.clone();
            let mut fires = b.fires.clone();
            let mut duration = b.duration.clone();
            json!({
                "cls": cls,
                "n": b.n,
                "spend_usd": round2(b.spend_usd),
                "median_tokens": percentile(&mut tokens, 50),
                "median_fires": percentile(&mut fires, 50),
                "median_duration_min": percentile(&mut duration, 50),
            })
        })
        .collect();
    // Reassemble into a key-ordered object matching the fold's dict shape.
    let mut per_outcome_map = Map::new();
    for row in per_outcome_class {
        let obj = row.as_object().cloned().unwrap_or_default();
        let mut entry = Map::new();
        for key in [
            "n",
            "spend_usd",
            "median_tokens",
            "median_fires",
            "median_duration_min",
        ] {
            if let Some(v) = obj.get(key) {
                entry.insert(key.to_string(), v.clone());
            }
        }
        per_outcome_map.insert(
            obj.get("cls")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            Value::Object(entry),
        );
    }

    let mut plan_vs_build = Map::new();
    for (nid, plan_usd) in &plan_cost_by_node {
        plan_vs_build.insert(
            nid.clone(),
            json!({
                "plan_usd": round2(*plan_usd),
                "build_usd": round2(build_cost_by_node.get(nid).copied().unwrap_or(0.0)),
            }),
        );
    }

    json!({
        "state": "ok",
        "since_days": since_days,
        "coverage": {
            "rows": total,
            "loop_join_pct": pct(joined_rows, total),
            "transcript_pct": pct(transcript_rows, total),
            "node_linkage_pct": pct(node_linked, total),
            "outcome_tracked_pct": pct(outcome_tracked, shipped_rows),
            "ci_unparsed": ci_unparsed_total,
        },
        "per_outcome_class": Value::Object(per_outcome_map),
        "plan_vs_build_cost": Value::Object(plan_vs_build),
        "distribution": {
            "loop_fires": dist(per_row_fires),
            "ci_reds": dist(per_row_reds),
            "tokens_total": dist(per_row_tokens),
            "duration_minutes": dist(per_row_duration),
        },
    })
}

fn upsert_plan_cost(list: &mut Vec<(String, f64)>, nid: &str, cost: f64) {
    if let Some(entry) = list.iter_mut().find(|(existing, _)| existing == nid) {
        entry.1 += cost;
    } else {
        list.push((nid.to_string(), cost));
    }
}

struct Bucket {
    n: i64,
    spend_usd: f64,
    tokens: Vec<f64>,
    fires: Vec<f64>,
    duration: Vec<f64>,
}
