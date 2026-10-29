//! Calibration: joins a shipped node's filed estimate (`difficulty_history`,
//! `size`) against what it actually cost (`cost_sessions`) and how much
//! friction it hit (the `friction` event, already tagged with `node_id`).
//! Both inputs are written today and read by nothing - this module is the
//! first reader.
//!
//! `smooth` is derived entirely from facts a node cannot assert about
//! itself: zero `friction` rows against it, and a `difficulty_history` that
//! was never revised past its filed entry. No agent writes this field, so
//! no agent can flatter it. A node created before the journal's first
//! friction row had no recorder watching it, so its `smooth` reads null:
//! no row there is missing data, not a clean run.
//!
//! `revisions` answers where a filed band ended up, over every done node
//! with a history, cost or no cost - the read a planner consults before it
//! picks a band.

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
    /// A later `difficulty_history` entry whose `value` differs from the
    /// filed one - a confirmation (same value, new `source`) does not count.
    pub re_estimated: bool,
    /// The flat `size` field; fno tracks no history for it.
    pub planned_size: Option<String>,
    /// Sum of `cost_sessions[].cost_usd`. Every writer of a `cost_sessions`
    /// row coerces an unpriced session's cost to `0.0` rather than omitting
    /// it or writing `null` (`cli/src/fno/cost/__init__.py`'s `_as_cost`,
    /// `cli/src/fno/done/cli.py`'s rollup), so this sum is never silently
    /// short a row `session_count` still counts.
    pub actual_cost_usd: f64,
    pub session_count: usize,
    /// `completed_at - created_at`, when both parse as RFC3339.
    pub duration_hours: Option<f64>,
    /// Friction origin -> row count (`help`, `result_blocked`, `gate_refusal`,
    /// `review_round`, `ci_failure`). Absent entirely when the node hit none.
    pub friction: BTreeMap<String, u32>,
    /// `Some(false)` when re-estimated or any friction row exists; otherwise
    /// `Some(true)` when a recorder covered the node, and `None` when none did.
    pub smooth: Option<bool>,
}

/// One `(domain, planned_difficulty, planned_size)` rollup: which plans run
/// clean. `smooth_count` counts only measured nodes, out of `measured_count`.
#[derive(Debug, Clone, Serialize)]
pub struct AggregateRow {
    pub domain: Option<String>,
    pub planned_difficulty: Option<String>,
    pub planned_size: Option<String>,
    pub node_count: usize,
    pub measured_count: usize,
    pub smooth_count: usize,
    pub avg_cost_usd: f64,
    pub avg_duration_hours: Option<f64>,
}

/// One `(domain, filed band)` row: how many done nodes were filed at that
/// band, and how many ended above or below it.
#[derive(Debug, Clone, Serialize)]
pub struct RevisionRow {
    pub domain: Option<String>,
    pub filed: Option<String>,
    pub nodes: usize,
    pub revised_up: usize,
    pub revised_down: usize,
}

/// The batched friction read: counts per node, whether the store read at
/// all, and the timestamp of its first friction row (the recorder's start).
pub struct FrictionRead {
    pub by_node: BTreeMap<String, BTreeMap<String, u32>>,
    pub readable: bool,
    pub first_ms: Option<i64>,
}

/// Reuses the event store's own RFC3339 parser rather than a second hand-
/// rolled one. Negative (completed_at before created_at - clock skew, a
/// backfilled created_at) reads as unmeasured, never as a negative average.
fn parse_rfc3339_hours(a: &str, b: &str) -> Option<f64> {
    let start = crate::event_store::parse_rfc3339_ms(a)?;
    let end = crate::event_store::parse_rfc3339_ms(b)?;
    let hours = (end - start) as f64 / 3_600_000.0;
    (hours >= 0.0).then_some(hours)
}

fn history_value(entry: &Value) -> Option<&str> {
    entry.get("value").and_then(Value::as_str)
}

/// Computes one [`NodeCalibration`] per done node that carries at least one
/// `cost_sessions` row. `friction_by_node` is the caller's batched read (see
/// [`read_friction`]); a node absent from it reads as zero friction when
/// `first_ms` says a recorder covered it, and as unmeasured otherwise.
pub fn compute(
    nodes: &[Value],
    friction_by_node: &BTreeMap<String, BTreeMap<String, u32>>,
    first_ms: Option<i64>,
) -> Vec<NodeCalibration> {
    let mut out = Vec::new();
    for node in nodes {
        let Some(id) = node.get("id").and_then(Value::as_str) else {
            continue;
        };
        if node.get("status").and_then(Value::as_str) != Some("done") {
            continue;
        }
        let cost_sessions: &[Value] = node
            .get("cost_sessions")
            .and_then(Value::as_array)
            .map(Vec::as_slice)
            .unwrap_or(&[]);
        if cost_sessions.is_empty() {
            continue;
        }
        let difficulty_history: &[Value] = node
            .get("difficulty_history")
            .and_then(Value::as_array)
            .map(Vec::as_slice)
            .unwrap_or(&[]);
        let planned_difficulty = difficulty_history
            .first()
            .and_then(history_value)
            .map(str::to_string);
        // A blueprint confirmation appends a history row too (same value,
        // source "blueprint"), so `len() > 1` alone would flag an
        // unrevised node as re-estimated. Compare values: only a later
        // entry that actually differs from the filed one counts.
        let re_estimated = difficulty_history
            .iter()
            .skip(1)
            .any(|e| history_value(e) != planned_difficulty.as_deref());
        let planned_size = node.get("size").and_then(Value::as_str).map(str::to_string);
        let actual_cost_usd: f64 = cost_sessions
            .iter()
            .filter_map(|s| s.get("cost_usd").and_then(Value::as_f64))
            .sum();
        let session_count = cost_sessions.len();
        let created_at = node.get("created_at").and_then(Value::as_str);
        let duration_hours = match (created_at, node.get("completed_at").and_then(Value::as_str)) {
            (Some(a), Some(b)) => parse_rfc3339_hours(a, b),
            _ => None,
        };
        let measured = match (
            first_ms,
            created_at.and_then(crate::event_store::parse_rfc3339_ms),
        ) {
            (Some(first), Some(created)) => created >= first,
            _ => false,
        };
        let friction = friction_by_node.get(id).cloned().unwrap_or_default();
        // Any friction row proves rough even when the node predates the
        // recorder; only a clean read needs the recorder's coverage.
        let smooth = if re_estimated || !friction.is_empty() {
            Some(false)
        } else if measured {
            Some(friction.is_empty())
        } else {
            None
        };
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

/// Groups [`NodeCalibration`] rows by `(domain, planned_difficulty,
/// planned_size)`, averaging cost and duration and counting how many
/// measured nodes came in smooth.
pub fn aggregate(rows: &[NodeCalibration]) -> Vec<AggregateRow> {
    type Key = (Option<String>, Option<String>, Option<String>);
    let mut groups: BTreeMap<Key, Vec<&NodeCalibration>> = BTreeMap::new();
    for row in rows {
        groups
            .entry((
                row.domain.clone(),
                row.planned_difficulty.clone(),
                row.planned_size.clone(),
            ))
            .or_default()
            .push(row);
    }
    groups
        .into_iter()
        .map(|((domain, planned_difficulty, planned_size), members)| {
            let node_count = members.len();
            let measured_count = members.iter().filter(|m| m.smooth.is_some()).count();
            let smooth_count = members.iter().filter(|m| m.smooth == Some(true)).count();
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
                planned_size,
                node_count,
                measured_count,
                smooth_count,
                avg_cost_usd,
                avg_duration_hours,
            }
        })
        .collect()
}

/// Where each filed band ended, per domain, over every done node with a
/// `difficulty_history` - no `cost_sessions` needed. The last entry is the
/// final band; a band outside the dispatch grid's order counts toward
/// `nodes` and neither direction.
pub fn revisions(nodes: &[Value]) -> Vec<RevisionRow> {
    let mut groups: BTreeMap<(Option<String>, Option<String>), RevisionRow> = BTreeMap::new();
    for node in nodes {
        if node.get("status").and_then(Value::as_str) != Some("done") {
            continue;
        }
        let Some(history) = node
            .get("difficulty_history")
            .and_then(Value::as_array)
            .filter(|h| !h.is_empty())
        else {
            continue;
        };
        let domain = node
            .get("domain")
            .and_then(Value::as_str)
            .map(str::to_string);
        let filed = history.first().and_then(history_value);
        let filed_rank = filed.map_or(-1, crate::route_slot::band_rank);
        let final_rank = history
            .last()
            .and_then(history_value)
            .map_or(-1, crate::route_slot::band_rank);
        let row = groups
            .entry((domain.clone(), filed.map(str::to_string)))
            .or_insert_with(|| RevisionRow {
                domain,
                filed: filed.map(str::to_string),
                nodes: 0,
                revised_up: 0,
                revised_down: 0,
            });
        row.nodes += 1;
        if filed_rank >= 0 && final_rank >= 0 {
            if final_rank > filed_rank {
                row.revised_up += 1;
            } else if final_rank < filed_rank {
                row.revised_down += 1;
            }
        }
    }
    groups.into_values().collect()
}

/// One batched read of every `friction` row in the journal, grouped
/// client-side by `data.node_id` - never one query per node. A row with no
/// `node_id` (friction with no node correlation) is skipped for the counts
/// but still dates the recorder's first row. An unreadable store reports
/// `readable: false`, so a caller never mistakes silence for a clean signal.
pub fn read_friction(journal: &Path) -> FrictionRead {
    let mut out = FrictionRead {
        by_node: BTreeMap::new(),
        readable: true,
        first_ms: None,
    };
    let query = crate::event_store::EventQuery {
        types: vec!["friction".to_string()],
        ..Default::default()
    };
    let rows = match crate::event_store::query_events(journal, &query) {
        Ok(rows) => rows,
        Err(err) => {
            // Unlike a fresh node with no friction ever recorded, this is an
            // unreadable store (locked, corrupt, or a newer schema this
            // build cannot parse). Warn so that silence is never mistaken
            // for a clean signal.
            eprintln!(
                "fno backlog calibration: could not read the friction event store ({err}); \
no node's friction reads as measured."
            );
            out.readable = false;
            return out;
        }
    };
    for row in rows {
        out.first_ms = Some(out.first_ms.map_or(row.ts_ms, |m| m.min(row.ts_ms)));
        // The canonical identity reader (resolves both the "node_id" and
        // "node" key aliases a friction row could carry), not a second
        // hand-rolled parse of the same envelope.
        let Some(node_id) = crate::event_store::extract_identity(&row.line).node_id else {
            continue;
        };
        let origin = serde_json::from_str::<Value>(&row.line)
            .ok()
            .and_then(|env| env.get("data")?.get("origin")?.as_str().map(str::to_string))
            .unwrap_or_else(|| "unknown".to_string());
        *out.by_node
            .entry(node_id)
            .or_default()
            .entry(origin)
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

    fn recorder_from(ts: &str) -> Option<i64> {
        crate::event_store::parse_rfc3339_ms(ts)
    }

    #[test]
    fn smooth_needs_a_recorder_only_for_a_clean_read() {
        let node = done_node("x-dddd", "code", "medium", 5.0);
        let later = recorder_from("2026-02-01T00:00:00Z");
        let rows = compute(&[node.clone()], &BTreeMap::new(), later);
        assert_eq!(rows[0].smooth, None);
        let agg = aggregate(&rows);
        assert_eq!((agg[0].measured_count, agg[0].smooth_count), (0, 0));

        // A friction row is proof of rough even before the recorder's start.
        let friction = BTreeMap::from([(
            "x-dddd".to_string(),
            BTreeMap::from([("ci_failure".to_string(), 1)]),
        )]);
        let rows = compute(&[node.clone()], &friction, later);
        assert_eq!(rows[0].smooth, Some(false));

        let earlier = recorder_from("2025-12-01T00:00:00Z");
        let rows = compute(&[node.clone()], &BTreeMap::new(), earlier);
        assert_eq!(rows[0].smooth, Some(true));

        // A revision reads rough with no recorder at all.
        let mut revised = node;
        revised["difficulty_history"] = json!([
            {"value": "medium", "source": "filed", "ts": "2026-01-01T00:00:00Z"},
            {"value": "high", "source": "revised", "ts": "2026-01-02T00:00:00Z"}
        ]);
        let rows = compute(&[revised], &BTreeMap::new(), None);
        assert!(rows[0].re_estimated);
        assert_eq!(rows[0].smooth, Some(false));
    }

    #[test]
    fn aggregate_groups_by_domain_difficulty_and_size() {
        let mut sized_s = done_node("x-0000", "code", "medium", 4.0);
        sized_s["size"] = json!("S");
        let mut sized_l = done_node("x-1111", "code", "medium", 6.0);
        sized_l["size"] = json!("L");
        let nodes = [
            sized_s,
            sized_l,
            done_node("x-2222", "code", "medium", 5.0),
            done_node("x-3333", "code", "medium", 7.0),
        ];
        let covered = recorder_from("2025-12-01T00:00:00Z");
        let agg = aggregate(&compute(&nodes, &BTreeMap::new(), covered));
        assert_eq!(agg.len(), 3);
        let no_size = agg
            .iter()
            .find(|a| a.planned_size.is_none())
            .expect("code/medium group with no size present");
        assert_eq!(no_size.node_count, 2);
        assert_eq!((no_size.measured_count, no_size.smooth_count), (2, 2));
        assert!((no_size.avg_cost_usd - 6.0).abs() < 1e-9);
    }

    #[test]
    fn revisions_count_direction_and_ignore_confirmation() {
        let mut up = done_node("x-4444", "code", "medium", 1.0);
        up["difficulty_history"] = json!([
            {"value": "medium", "source": "filed"},
            {"value": "high", "source": "blueprint"}
        ]);
        let mut down = done_node("x-5555", "code", "medium", 1.0);
        down["difficulty_history"] = json!([
            {"value": "medium", "source": "filed"},
            {"value": "low", "source": "update"}
        ]);
        let mut confirmed = done_node("x-6666", "code", "medium", 1.0);
        confirmed["difficulty_history"] = json!([
            {"value": "medium", "source": "filed"},
            {"value": "medium", "source": "blueprint"}
        ]);
        // No cost_sessions: revisions still counts it, compute skips it.
        confirmed.as_object_mut().unwrap().remove("cost_sessions");
        let nodes = [up, down, confirmed];
        let rows = revisions(&nodes);
        assert_eq!(rows.len(), 1);
        assert_eq!(
            (rows[0].nodes, rows[0].revised_up, rows[0].revised_down),
            (3, 1, 1)
        );
        assert_eq!(compute(&nodes, &BTreeMap::new(), None).len(), 2);
    }
}
