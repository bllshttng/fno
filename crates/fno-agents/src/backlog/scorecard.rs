//! The by-lane scorecard: one record per node whose last execute session
//! ended in the window, folded into cells keyed
//! `(kind, harness, provider, model)`. Computation only; `calibration_cli`
//! owns argv, the reads and the render, matching the calibration split.
//!
//! Lane axes are never inferred from one another: harness comes from the
//! session row, model from the row's `observed_model` or the node-tagged
//! ledger row, provider only from the ledger row. A missing axis reads as
//! its own visible fallback (`unattributed` harness, `unrecorded` provider,
//! `unknown` model) rather than a dropped record, and the coverage block
//! reports what share of records carry each axis.

use crate::context_window::normalize_model;
use crate::scoreboard_provider::{node_outcome, num_opt, parse_local, pct, percentile, round2};
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};

pub(crate) const UNATTRIBUTED: &str = "unattributed";
pub(crate) const UNRECORDED: &str = "unrecorded";
pub(crate) const UNKNOWN: &str = "unknown";

/// The verb part of `kind`: every execute/target session ran the delivery
/// pipeline, so `target/<difficulty>` is the only slice-1 kind.
const TARGET_VERB: &str = "target";

/// One batched ledger read, indexed by session id and by graph node id: the
/// two joins the lane resolution and the cost fallback need. Execution rows
/// only, the same filter the by-provider view applies.
#[derive(Default)]
pub struct LedgerIndex {
    by_session: HashMap<String, Vec<Value>>,
    by_node: HashMap<String, Vec<Value>>,
}

impl LedgerIndex {
    pub fn build(rows: &[Value]) -> LedgerIndex {
        let mut out = LedgerIndex::default();
        for row in rows {
            if row.get("type").and_then(Value::as_str) != Some("execution") {
                continue;
            }
            for sid in row
                .get("sessions")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                if let Some(sid) = sid.as_str() {
                    out.by_session
                        .entry(sid.to_string())
                        .or_default()
                        .push(row.clone());
                }
            }
            if let Some(nid) = row
                .get("graph_node_id")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
            {
                out.by_node
                    .entry(nid.to_string())
                    .or_default()
                    .push(row.clone());
            }
        }
        out
    }

    /// The ledger beside the graph store, at the same resolve the escalation
    /// view uses (`<agents home>/../ledger.json`); a missing file is an empty
    /// index, so every lane axis falls back honestly instead of erroring.
    pub fn load() -> LedgerIndex {
        let home = crate::paths::AgentsHome::from_env();
        let fno_dir = home
            .root()
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from(".fno"));
        let raw = std::fs::read_to_string(fno_dir.join("ledger.json")).unwrap_or_default();
        let rows: Vec<Value> = serde_json::from_str::<Value>(&raw)
            .ok()
            .and_then(|v| v.get("entries").cloned().or_else(|| Some(v)))
            .and_then(|e| e.as_array().cloned())
            .unwrap_or_default();
        LedgerIndex::build(&rows)
    }

    fn first_by_session(&self, sid: &str) -> Option<&Value> {
        self.by_session.get(sid).and_then(|rows| rows.first())
    }

    fn rows_by_node(&self, nid: &str) -> &[Value] {
        self.by_node.get(nid).map(Vec::as_slice).unwrap_or(&[])
    }
}

/// The session's observed model id: an `{"kind": "observed", "model": ...}`
/// object or a bare string; anything else is unrecorded.
fn observed_model(session: &Value) -> Option<&str> {
    let v = session.get("observed_model")?;
    match v {
        Value::String(s) if !s.is_empty() => Some(s.as_str()),
        Value::Object(_) => {
            if v.get("kind").and_then(Value::as_str) == Some("observed") {
                v.get("model")
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())
            } else {
                None
            }
        }
        _ => None,
    }
}

/// One record per node: the lane of its last execute session plus the
/// outcome columns. `shared` marks a node whose execute sessions ran on more
/// than one (harness, model) lane, mirroring the by-provider view's shared
/// read.
pub fn build_records(
    nodes: &[Value],
    ledger: &LedgerIndex,
    friction_by_node: &BTreeMap<String, BTreeMap<String, u32>>,
    since_days: i64,
    now: chrono::NaiveDateTime,
    domain: Option<&str>,
) -> Vec<Value> {
    // node_outcome context: every node by id, plus fix nodes keyed by the
    // origin they fix - the same two maps the by-provider view builds.
    let mut by_id: HashMap<&str, &Value> = HashMap::new();
    let mut fixes: HashMap<&str, Vec<&Value>> = HashMap::new();
    for n in nodes {
        if let Some(id) = n.get("id").and_then(Value::as_str) {
            by_id.insert(id, n);
            if let Some(origin) = n.get("caused_by").and_then(Value::as_str) {
                if !origin.is_empty() {
                    fixes.entry(origin).or_default().push(n);
                }
            }
        }
    }
    let cutoff = now - chrono::Duration::days(since_days);

    let mut out = Vec::new();
    for node in nodes {
        // Archived rows read through the key's presence, null included; a
        // live row never carries the key.
        if node.get("archived_at").is_some() {
            continue;
        }
        let Some(id) = node.get("id").and_then(Value::as_str) else {
            continue;
        };
        let sessions: &[Value] = node
            .get("sessions")
            .and_then(Value::as_array)
            .map(Vec::as_slice)
            .unwrap_or(&[]);
        let execs: Vec<&Value> = sessions
            .iter()
            .filter(|s| {
                matches!(
                    s.get("phase").and_then(Value::as_str),
                    Some("execute") | Some("target")
                )
            })
            .collect();
        // The last execute session decides the window: a node whose newest
        // ended_at (started_at as the un-ended fallback) sits outside it is
        // not this window's work.
        let last = execs
            .iter()
            .max_by_key(|s| {
                s.get("ended_at")
                    .and_then(Value::as_str)
                    .or_else(|| s.get("started_at").and_then(Value::as_str))
                    .unwrap_or_default()
            })
            .copied();
        let Some(last) = last else { continue };
        let end_raw = last
            .get("ended_at")
            .and_then(Value::as_str)
            .or_else(|| last.get("started_at").and_then(Value::as_str))
            .unwrap_or_default();
        let Some(ended) = parse_local(end_raw) else {
            continue;
        };
        if ended < cutoff || ended > now {
            continue;
        }

        let harness = last
            .get("harness")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| UNATTRIBUTED.to_string());
        let sid = last
            .get("session_id")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let ledger_row = ledger.first_by_session(sid);
        let model = observed_model(last)
            .or_else(|| {
                ledger_row
                    .and_then(|r| r.get("model"))
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())
            })
            .map(normalize_model)
            .unwrap_or_else(|| UNKNOWN.to_string());
        // Never infer provider from harness or model: only a ledger row's
        // own provider field fills the axis.
        let provider = ledger_row
            .and_then(|r| r.get("provider"))
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| UNRECORDED.to_string());
        let effort = last
            .get("effort")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();

        // Distinct (harness, model) lanes over ALL execute sessions decide
        // `shared`; the by-provider view counts the same way per node.
        let mut lanes: HashSet<(String, String)> = HashSet::new();
        let mut work_hours = 0.0;
        for s in &execs {
            lanes.insert((
                s.get("harness")
                    .and_then(Value::as_str)
                    .filter(|h| !h.is_empty())
                    .unwrap_or(UNATTRIBUTED)
                    .to_string(),
                observed_model(s)
                    .map(normalize_model)
                    .unwrap_or_else(|| UNKNOWN.to_string()),
            ));
            let hours = match (
                s.get("started_at").and_then(Value::as_str),
                s.get("ended_at").and_then(Value::as_str),
            ) {
                (Some(a), Some(b)) => parse_local(a)
                    .zip(parse_local(b))
                    .map(|(x, y)| (y - x).num_seconds() as f64 / 3_600.0),
                _ => None,
            };
            if let Some(h) = hours.filter(|h| *h >= 0.0) {
                work_hours += h;
            }
        }

        // Cost: the node's own cost_sessions coerce unpriced rows to 0.0, so
        // a non-empty list is a measured sum even when it reads zero. The
        // ledger is the fallback and only counts when a cost parses.
        let cost_sessions: &[Value] = node
            .get("cost_sessions")
            .and_then(Value::as_array)
            .map(Vec::as_slice)
            .unwrap_or(&[]);
        let cost_usd = if !cost_sessions.is_empty() {
            Some(
                cost_sessions
                    .iter()
                    .filter_map(|s| s.get("cost_usd").and_then(Value::as_f64))
                    .sum::<f64>(),
            )
        } else {
            let sums: Vec<f64> = ledger
                .rows_by_node(id)
                .iter()
                .filter_map(|r| num_opt(r.get("cost_usd")))
                .collect();
            (!sums.is_empty()).then(|| sums.iter().sum())
        };

        let merged = node.get("merge_status").and_then(Value::as_str) == Some("merged");
        let shipped_at = node
            .get("merged_at")
            .and_then(Value::as_str)
            .and_then(parse_local)
            .or_else(|| {
                node.get("completed_at")
                    .and_then(Value::as_str)
                    .and_then(parse_local)
            });
        let bounce = node_outcome(id, shipped_at, &by_id, &fixes);
        let friction = friction_by_node.get(id);

        if let Some(d) = domain {
            if node.get("domain").and_then(Value::as_str) != Some(d) {
                continue;
            }
        }

        out.push(json!({
            "id": id,
            "kind": format!("{TARGET_VERB}/{}",
                node.get("difficulty").and_then(Value::as_str).unwrap_or("unknown")),
            "harness": harness,
            "provider": provider,
            "model": model,
            "effort": effort,
            "merged": merged,
            "cost_usd": cost_usd,
            "work_hours": if work_hours > 0.0 { Some(round2(work_hours)) } else { None },
            "bounce": bounce,
            "ci_failure": friction.map(|f| f.get("ci_failure").copied().unwrap_or(0)).unwrap_or(0),
            "review_round": friction.map(|f| f.get("review_round").copied().unwrap_or(0)).unwrap_or(0),
            "shared": lanes.len() > 1,
        }));
    }
    out
}

/// Folds records into one cell per `(kind, harness, provider, model)`.
#[derive(Default)]
struct CellAcc {
    nodes: u64,
    merged: u64,
    bounced: u64,
    spend: f64,
    measured_cost: bool,
    work_hours: Vec<f64>,
    ci_failure: u64,
    review_round: u64,
    shared: u64,
}

pub fn fold_cells(records: &[Value]) -> Vec<Value> {
    let mut cells: BTreeMap<(String, String, String, String), CellAcc> = BTreeMap::new();
    for r in records {
        let key = (
            r["kind"].as_str().unwrap_or_default().to_string(),
            r["harness"].as_str().unwrap_or_default().to_string(),
            r["provider"].as_str().unwrap_or_default().to_string(),
            r["model"].as_str().unwrap_or_default().to_string(),
        );
        let c = cells.entry(key).or_default();
        c.nodes += 1;
        if r["merged"].as_bool() == Some(true) {
            c.merged += 1;
            if matches!(r["bounce"].as_str(), Some("bounced") | Some("reverted")) {
                c.bounced += 1;
            }
        }
        if let Some(cost) = num_opt(r.get("cost_usd")) {
            c.spend += cost;
            c.measured_cost = true;
        }
        if let Some(h) = num_opt(r.get("work_hours")) {
            c.work_hours.push(h);
        }
        c.ci_failure += r["ci_failure"].as_u64().unwrap_or(0);
        c.review_round += r["review_round"].as_u64().unwrap_or(0);
        if r["shared"].as_bool() == Some(true) {
            c.shared += 1;
        }
    }
    let mut out: Vec<Value> = cells
        .into_iter()
        .map(|((kind, harness, provider, model), c)| {
            let merged_pct = if c.nodes > 0 {
                Some(pct(c.merged, c.nodes))
            } else {
                None
            };
            let cost_per_merged = if c.merged > 0 && c.measured_cost {
                Some(round2(c.spend / c.merged as f64))
            } else {
                None
            };
            let bounce_pct = if c.merged > 0 {
                Some(pct(c.bounced, c.merged))
            } else {
                None
            };
            json!({
                "kind": kind,
                "harness": harness,
                "provider": provider,
                "model": model,
                "nodes": c.nodes,
                "merged": c.merged,
                "merged_pct": merged_pct,
                "spend_usd": round2(c.spend),
                "cost_per_merged_usd": cost_per_merged,
                "median_work_hours": percentile(&c.work_hours, 50),
                "bounce_pct": bounce_pct,
                "ci_failure_per_node": round2(c.ci_failure as f64 / c.nodes.max(1) as f64),
                "review_round_per_node": round2(c.review_round as f64 / c.nodes.max(1) as f64),
                "shared": c.shared,
            })
        })
        .collect();
    // Unattributed-harness cells sort last so the readable lanes lead; the
    // rest stays kind/harness/provider/model ordered.
    out.sort_by(|x, y| {
        (x["harness"].as_str() == Some(UNATTRIBUTED))
            .cmp(&(y["harness"].as_str() == Some(UNATTRIBUTED)))
            .then_with(|| x["kind"].as_str().cmp(&y["kind"].as_str()))
            .then_with(|| x["harness"].as_str().cmp(&y["harness"].as_str()))
            .then_with(|| x["provider"].as_str().cmp(&y["provider"].as_str()))
            .then_with(|| x["model"].as_str().cmp(&y["model"].as_str()))
    });
    out
}

/// The coverage block: what share of records carry each lane axis and a
/// measured cost. The unattributed share is the harness number's gap.
pub fn coverage(records: &[Value]) -> Value {
    let total = records.len() as u64;
    let axis = |k: &str| -> u64 {
        records
            .iter()
            .filter(|r| {
                let v = r[k].as_str().unwrap_or_default();
                !v.is_empty() && v != UNATTRIBUTED && v != UNRECORDED && v != UNKNOWN
            })
            .count() as u64
    };
    json!({
        "nodes": total,
        "harness_pct": pct(axis("harness"), total),
        "provider_pct": pct(axis("provider"), total),
        "model_pct": pct(axis("model"), total),
        "cost_pct": pct(
            records
                .iter()
                .filter(|r| num_opt(r.get("cost_usd")).is_some())
                .count() as u64,
            total,
        ),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// A merged node with one execute session; cost_sessions empty unless a
    /// cost is given, so the ledger fallback can fill it.
    fn node(id: &str, sessions: Value, difficulty: &str, cost: Option<f64>) -> Value {
        let mut n = json!({
            "id": id,
            "status": "done",
            "difficulty": difficulty,
            "merge_status": "merged",
            "completed_at": "2026-10-15T12:00:00Z",
            "sessions": sessions,
        });
        if let Some(c) = cost {
            n["cost_sessions"] = json!([{"session_id": "s1", "cost_usd": c}]);
        }
        n
    }

    /// One execute session ended `ended_at`; harness/model/effort optional.
    fn session(harness: Option<&str>, model: Option<&str>, sid: &str, ended_at: &str) -> Value {
        let mut s = json!({
            "phase": "execute",
            "session_id": sid,
            "started_at": "2026-10-14T10:00:00Z",
            "ended_at": ended_at,
        });
        if let Some(h) = harness {
            s["harness"] = json!(h);
        }
        if let Some(m) = model {
            s["observed_model"] = json!({"kind": "observed", "model": m});
        }
        s
    }

    fn ledger_row(
        sids: &[&str],
        provider: &str,
        model: &str,
        nid: Option<&str>,
        cost: f64,
    ) -> Value {
        let mut r = json!({
            "type": "execution",
            "sessions": sids,
            "provider": provider,
            "model": model,
            "cost_usd": cost,
        });
        if let Some(n) = nid {
            r["graph_node_id"] = json!(n);
        }
        r
    }

    const NOW: &str = "2026-10-20T12:00:00Z";

    fn records(nodes: &[Value], ledger_rows: &[Value]) -> Vec<Value> {
        build_records(
            nodes,
            &LedgerIndex::build(ledger_rows),
            &BTreeMap::new(),
            28,
            parse_local(NOW).unwrap(),
            None,
        )
    }

    #[test]
    fn grouping_and_cost_fallback() {
        // AC1-HP: two merged nodes on the same lane, costs 10 and 20, fold
        // into one cell reading cost_per_merged_usd 15.
        let s1 = "sess-1";
        let s2 = "sess-2";
        let nodes = vec![
            node(
                "x-aaaa",
                json!([session(Some("claude"), None, s1, "2026-10-15T10:00:00Z")]),
                "medium",
                Some(10.0),
            ),
            node(
                "x-bbbb",
                json!([session(Some("claude"), None, s2, "2026-10-16T10:00:00Z")]),
                "medium",
                Some(20.0),
            ),
        ];
        let ledger_rows = vec![
            ledger_row(&[s1], "anthropic", "sonnet", Some("x-aaaa"), 1.0),
            ledger_row(&[s2], "anthropic", "sonnet", Some("x-bbbb"), 2.0),
        ];
        let recs = records(&nodes, &ledger_rows);
        assert_eq!(recs.len(), 2, "{recs}");
        let cells = fold_cells(&recs);
        assert_eq!(cells.len(), 1, "one lane cell: {cells}");
        let c = &cells[0];
        assert_eq!(c["kind"], "target/medium");
        assert_eq!(c["harness"], "claude");
        assert_eq!(c["provider"], "anthropic");
        assert_eq!(c["model"], "sonnet", "ledger model fills the axis: {c}");
        assert_eq!(c["nodes"], 2);
        assert_eq!(c["merged"], 2);
        assert_eq!(c["merged_pct"], 100);
        assert_eq!(c["spend_usd"], 30.0);
        assert_eq!(c["cost_per_merged_usd"], 15.0);

        // Cost fallback: a node with no cost_sessions takes its cost from
        // the ledger row tagged with its graph node id.
        let s3 = "sess-3";
        let nodes = vec![node(
            "x-cccc",
            json!([session(Some("claude"), None, s3, "2026-10-17T10:00:00Z")]),
            "medium",
            None,
        )];
        let ledger_rows = vec![ledger_row(
            &[s3],
            "anthropic",
            "sonnet",
            Some("x-cccc"),
            7.5,
        )];
        let recs = records(&nodes, &ledger_rows);
        assert_eq!(recs[0]["cost_usd"], 7.5, "ledger cost fills in: {recs}");
        let cov = coverage(&recs);
        assert_eq!(cov["nodes"], 1);
        assert_eq!(cov["cost_pct"], 100);
    }

    #[test]
    fn unattributed_never_dropped() {
        // A node whose execute session row carries no harness lands in the
        // visible unattributed bucket and coverage.harness_pct drops.
        let nodes = vec![
            node(
                "x-dddd",
                json!([session(
                    Some("claude"),
                    None,
                    "sess-4",
                    "2026-10-15T10:00:00Z"
                )]),
                "high",
                Some(5.0),
            ),
            node(
                "x-eeee",
                json!([session(None, None, "sess-5", "2026-10-16T10:00:00Z")]),
                "high",
                None,
            ),
        ];
        let recs = records(&nodes, &[]);
        assert_eq!(recs.len(), 2, "{recs}");
        let cells = fold_cells(&recs);
        let unattr = cells
            .iter()
            .find(|c| c["harness"] == UNATTRIBUTED)
            .expect("unattributed bucket kept");
        assert_eq!(unattr["nodes"], 1);
        assert_eq!(
            unattr["provider"], UNRECORDED,
            "no ledger row, no provider: {unattr}"
        );
        assert_eq!(unattr["model"], UNKNOWN);
        let last = cells.last().unwrap();
        assert_eq!(
            last["harness"], UNATTRIBUTED,
            "unattributed sorted last: {cells}"
        );
        let cov = coverage(&recs);
        assert!(cov["harness_pct"].as_i64().unwrap() < 100, "{cov}");
    }

    #[test]
    fn window_keeps_only_recent_last_sessions() {
        // A last execute session 40 days old is outside a 28-day window; a
        // 10-day-old one is inside.
        let old = node(
            "x-ffff",
            json!([session(
                Some("claude"),
                None,
                "sess-6",
                "2026-09-05T10:00:00Z"
            )]),
            "low",
            None,
        );
        let recent = node(
            "x-0000",
            json!([session(
                Some("claude"),
                None,
                "sess-7",
                "2026-10-10T10:00:00Z"
            )]),
            "low",
            None,
        );
        let recs = records(&[old, recent], &[]);
        assert_eq!(recs.len(), 1, "only the in-window node: {recs}");
        assert_eq!(recs[0]["id"], "x-0000");
        assert_eq!(recs[0]["work_hours"], Value::Null, "no span, no hours");
        // A merged node with no measured cost anywhere reads null, not 0.
        assert_eq!(recs[0]["cost_usd"], Value::Null);
    }
}
