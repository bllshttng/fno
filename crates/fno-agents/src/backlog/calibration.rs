//! Calibration: joins a shipped node's filed estimate (`difficulty_history`,
//! `size`) against what it actually cost (`cost_sessions`) and how much
//! friction it hit (the `friction` event, already tagged with `node_id`).
//! Both inputs are written today and read by nothing - this module is the
//! first reader.
//!
//! `smooth` is derived entirely from facts a node cannot assert about
//! itself: zero `friction` rows against it, and a `difficulty_history` that
//! was never revised past its filed entry. No agent writes this field, so
//! no agent can flatter it.

use serde::Serialize;
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::Path;

/// One shipped node's planned-vs-actual read.
#[derive(Debug, Clone, Serialize)]
pub struct NodeCalibration {
    pub id: String,
    pub title: String,
    pub domain: Option<String>,
    /// `difficulty_history[0].value` - the difficulty filed at intake.
    pub planned_difficulty: Option<String>,
    /// `difficulty_history.len() > 1` - the estimate was revised after filing.
    pub re_estimated: bool,
    /// The flat `size` field; fno tracks no history for it.
    pub planned_size: Option<String>,
    /// Sum of `cost_sessions[].cost_usd`. Never includes an unpriced session
    /// - `cost_sessions` only ever holds priced rows (see
    /// `docs/architecture/cost-accuracy.md`'s "Unpriced policy").
    pub actual_cost_usd: f64,
    pub session_count: usize,
    /// `completed_at - created_at`, when both parse as RFC3339.
    pub duration_hours: Option<f64>,
    /// Friction origin -> row count (`help`, `result_blocked`, `gate_refusal`,
    /// `review_round`, `ci_failure`). Absent entirely when the node hit none.
    pub friction: BTreeMap<String, u32>,
    /// `friction.is_empty() && !re_estimated` - no self-report possible.
    pub smooth: bool,
}

/// One `(domain, planned_difficulty)` rollup: which plans/domains run clean.
#[derive(Debug, Clone, Serialize)]
pub struct AggregateRow {
    pub domain: Option<String>,
    pub planned_difficulty: Option<String>,
    pub node_count: usize,
    pub smooth_count: usize,
    pub avg_cost_usd: f64,
    pub avg_duration_hours: Option<f64>,
}

fn parse_rfc3339_hours(a: &str, b: &str) -> Option<f64> {
    let start = chrono::DateTime::parse_from_rfc3339(a).ok()?;
    let end = chrono::DateTime::parse_from_rfc3339(b).ok()?;
    Some((end - start).num_seconds() as f64 / 3600.0)
}

/// Computes one [`NodeCalibration`] per done node that carries at least one
/// `cost_sessions` row. `friction_by_node` is the caller's batched read (see
/// [`friction_counts_by_node`]); a node absent from it reads as zero
/// friction, never as "unknown" or "skipped".
pub fn compute(
    nodes: &[Value],
    friction_by_node: &BTreeMap<String, BTreeMap<String, u32>>,
) -> Vec<NodeCalibration> {
    let mut out = Vec::new();
    for node in nodes {
        let Some(id) = node.get("id").and_then(Value::as_str) else {
            continue;
        };
        if node.get("status").and_then(Value::as_str) != Some("done") {
            continue;
        }
        let cost_sessions = node
            .get("cost_sessions")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        if cost_sessions.is_empty() {
            continue;
        }
        let difficulty_history = node
            .get("difficulty_history")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let planned_difficulty = difficulty_history
            .first()
            .and_then(|e| e.get("value").and_then(Value::as_str))
            .map(str::to_string);
        let re_estimated = difficulty_history.len() > 1;
        let planned_size = node.get("size").and_then(Value::as_str).map(str::to_string);
        let actual_cost_usd: f64 = cost_sessions
            .iter()
            .filter_map(|s| s.get("cost_usd").and_then(Value::as_f64))
            .sum();
        let session_count = cost_sessions.len();
        let duration_hours = match (
            node.get("created_at").and_then(Value::as_str),
            node.get("completed_at").and_then(Value::as_str),
        ) {
            (Some(a), Some(b)) => parse_rfc3339_hours(a, b),
            _ => None,
        };
        let friction = friction_by_node.get(id).cloned().unwrap_or_default();
        let smooth = friction.is_empty() && !re_estimated;
        out.push(NodeCalibration {
            id: id.to_string(),
            title: node
                .get("title")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            domain: node
                .get("domain")
                .and_then(Value::as_str)
                .map(str::to_string),
            planned_difficulty,
            re_estimated,
            planned_size,
            actual_cost_usd,
            session_count,
            duration_hours,
            friction,
            smooth,
        });
    }
    out
}

/// Groups [`NodeCalibration`] rows by `(domain, planned_difficulty)`,
/// averaging cost and duration and counting how many came in smooth - the
/// "which domains/sizes run clean vs rough" read.
pub fn aggregate(rows: &[NodeCalibration]) -> Vec<AggregateRow> {
    let mut groups: BTreeMap<(Option<String>, Option<String>), Vec<&NodeCalibration>> =
        BTreeMap::new();
    for row in rows {
        groups
            .entry((row.domain.clone(), row.planned_difficulty.clone()))
            .or_default()
            .push(row);
    }
    groups
        .into_iter()
        .map(|((domain, planned_difficulty), members)| {
            let node_count = members.len();
            let smooth_count = members.iter().filter(|m| m.smooth).count();
            let avg_cost_usd =
                members.iter().map(|m| m.actual_cost_usd).sum::<f64>() / node_count.max(1) as f64;
            let durations: Vec<f64> = members.iter().filter_map(|m| m.duration_hours).collect();
            let avg_duration_hours = if durations.is_empty() {
                None
            } else {
                Some(durations.iter().sum::<f64>() / durations.len() as f64)
            };
            AggregateRow {
                domain,
                planned_difficulty,
                node_count,
                smooth_count,
                avg_cost_usd,
                avg_duration_hours,
            }
        })
        .collect()
}

/// One batched read of every `friction` row in the journal, grouped
/// client-side by `data.node_id` - never one query per node. A row with no
/// `node_id` (friction with no node correlation) is skipped: it cannot
/// contribute to any node's calibration.
pub fn friction_counts_by_node(journal: &Path) -> BTreeMap<String, BTreeMap<String, u32>> {
    let mut out: BTreeMap<String, BTreeMap<String, u32>> = BTreeMap::new();
    let query = crate::event_store::EventQuery {
        types: vec!["friction".to_string()],
        ..Default::default()
    };
    let rows = match crate::event_store::query_events(journal, &query) {
        Ok(rows) => rows,
        Err(_) => return out,
    };
    for row in rows {
        let Ok(env) = serde_json::from_str::<Value>(&row.line) else {
            continue;
        };
        let Some(data) = env.get("data") else {
            continue;
        };
        let Some(node_id) = data.get("node_id").and_then(Value::as_str) else {
            continue;
        };
        let origin = data
            .get("origin")
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        *out.entry(node_id.to_string())
            .or_default()
            .entry(origin.to_string())
            .or_insert(0) += 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn done_node(id: &str, domain: &str, difficulty: &str, cost: f64) -> Value {
        json!({
            "id": id,
            "title": format!("node {id}"),
            "domain": domain,
            "status": "done",
            "difficulty_history": [{"value": difficulty, "source": "filed", "ts": "2026-01-01T00:00:00Z"}],
            "cost_sessions": [{"session_id": "s1", "cost_usd": cost, "timestamp": "2026-01-01T00:00:00Z"}],
            "created_at": "2026-01-01T00:00:00Z",
            "completed_at": "2026-01-01T02:00:00Z",
        })
    }

    #[test]
    fn smooth_excludes_in_progress_and_costless_nodes() {
        let mut in_progress = done_node("x-dddd", "code", "medium", 5.0);
        in_progress["status"] = json!("in_progress");
        let mut costless = done_node("x-eeee", "code", "medium", 0.0);
        costless["cost_sessions"] = json!([]);
        let clean = done_node("x-aaaa", "code", "medium", 5.0);
        let rows = compute(&[in_progress, costless, clean], &BTreeMap::new());
        assert_eq!(rows.len(), 1);
        assert!(rows[0].smooth);
        assert_eq!(rows[0].duration_hours, Some(2.0));
    }

    #[test]
    fn ci_failure_friction_or_a_re_estimate_makes_it_rough() {
        let friction = BTreeMap::from([(
            "x-bbbb".to_string(),
            BTreeMap::from([("ci_failure".to_string(), 1)]),
        )]);
        let rows = compute(&[done_node("x-bbbb", "code", "medium", 5.0)], &friction);
        assert!(!rows[0].smooth);

        let mut revised = done_node("x-cccc", "code", "medium", 5.0);
        revised["difficulty_history"] = json!([
            {"value": "medium", "source": "filed", "ts": "2026-01-01T00:00:00Z"},
            {"value": "high", "source": "revised", "ts": "2026-01-02T00:00:00Z"}
        ]);
        let rows = compute(&[revised], &BTreeMap::new());
        assert!(rows[0].re_estimated && !rows[0].smooth);
    }

    #[test]
    fn aggregate_groups_by_domain_and_planned_difficulty() {
        let nodes = [
            done_node("x-f001", "code", "medium", 4.0),
            done_node("x-f002", "code", "medium", 6.0),
            done_node("x-f003", "docs", "low", 1.0),
        ];
        let agg = aggregate(&compute(&nodes, &BTreeMap::new()));
        let code_medium = agg
            .iter()
            .find(|a| {
                a.domain.as_deref() == Some("code")
                    && a.planned_difficulty.as_deref() == Some("medium")
            })
            .expect("code/medium group present");
        assert_eq!(code_medium.node_count, 2);
        assert_eq!(code_medium.smooth_count, 2);
        assert!((code_medium.avg_cost_usd - 5.0).abs() < 1e-9);
    }
}
