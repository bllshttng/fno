//! The hourly outcome pass. The store is append-only, so a settled correction
//! gets its own `correction_outcome` row, keyed by `correction_id`.
//!
//! `held` is weak evidence: a merge shows nothing broke, never that the
//! corrector was right. The report says so in its header.

use crate::event_store::{append_envelope, query_events, EventQuery};
use crate::provenance::BusIndex;
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashSet};
use std::path::Path;

/// A row with no merge past this age reads `unresolved`.
const UNRESOLVED_AFTER_S: i64 = 14 * 86_400;
/// A later user correction in this window overrules an agent or machine row.
const OVERRULE_WINDOW_S: i64 = 30 * 60;
/// Every row settles by 14 days, so the pass never reads further back.
const READ_WINDOW_S: i64 = 30 * 86_400;
const MAX_ROWS_PER_PASS: usize = 200;

/// One node's merge fact, from the graph.
#[derive(Debug, Clone, Default)]
pub(crate) struct NodeMerge {
    pub(crate) merged_at: Option<i64>,
}

struct Correction {
    id: String,
    by: String,
    kind: String,
    session: String,
    ts: i64,
    node: Option<String>,
    corrector: Option<String>,
}

fn epoch(text: Option<&str>) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(text?)
        .ok()
        .map(|d| d.timestamp())
}

fn data_rows(journal: &Path, kind: &str, since_ms: i64) -> Vec<Value> {
    let query = EventQuery {
        types: vec![kind.to_string()],
        since_ms: Some(since_ms),
        ..Default::default()
    };
    query_events(journal, &query)
        .unwrap_or_default()
        .iter()
        .filter_map(|r| serde_json::from_str::<Value>(&r.line).ok())
        .map(|mut v| v["data"].take())
        .collect()
}

fn parse(row: &Value) -> Option<Correction> {
    let text = |k: &str| row.get(k).and_then(Value::as_str).map(str::to_string);
    Some(Correction {
        id: text("correction_id")?,
        by: text("corrected_by")?,
        kind: text("kind")?,
        session: text("session_id")?,
        ts: epoch(row.get("ts_turn").and_then(Value::as_str))?,
        node: row
            .pointer("/node/value")
            .and_then(Value::as_str)
            .map(str::to_string),
        corrector: text("corrected_by_session"),
    })
}

/// The settled outcome of one row, or `None` while it stays open.
fn settle(
    row: &Correction,
    later_user: &BTreeMap<String, Vec<i64>>,
    decision_named: &HashSet<String>,
    retracted: &dyn Fn(&Correction) -> bool,
    merges: &BTreeMap<String, NodeMerge>,
    now: i64,
) -> Option<(&'static str, &'static str)> {
    if decision_named.contains(&row.id) {
        return Some(("overruled", "decision_span"));
    }
    // A person's word overrules an agent's or a gate's, never their own.
    if row.by != "user"
        && later_user.get(&row.session).is_some_and(|times| {
            times
                .iter()
                .any(|t| *t > row.ts && *t - row.ts <= OVERRULE_WINDOW_S)
        })
    {
        return Some(("overruled", "later_user_correction"));
    }
    if row.by == "agent" && retracted(row) {
        return Some(("overruled", "retraction"));
    }
    let merged = row
        .node
        .as_deref()
        .and_then(|n| merges.get(n))
        .and_then(|m| m.merged_at);
    if merged.is_some_and(|m| m > row.ts) {
        return Some(("held", "merged"));
    }
    (now - row.ts > UNRESOLVED_AFTER_S).then_some(("unresolved", "age"))
}

/// The pass. Returns the rows appended; a store error stops it, and the next
/// hour retries, because an outcome already stored is an idempotent hit.
pub(crate) fn outcome_pass(
    journal: &Path,
    bus: &BusIndex,
    merges: &BTreeMap<String, NodeMerge>,
    now: i64,
) -> Result<usize, String> {
    let since_ms = (now - READ_WINDOW_S) * 1000;
    let rows: Vec<Correction> = data_rows(journal, "agent_corrected", since_ms)
        .iter()
        .filter_map(parse)
        .collect();
    let settled: HashSet<String> = data_rows(journal, "correction_outcome", since_ms)
        .iter()
        .filter_map(|r| r.get("correction_id").and_then(Value::as_str))
        .map(str::to_string)
        .collect();
    let decision_named: HashSet<String> = data_rows(journal, "decision_span", since_ms)
        .iter()
        .filter(|r| r.get("span_kind").and_then(Value::as_str) == Some("correction"))
        .filter_map(|r| r.get("correction_id").and_then(Value::as_str))
        .map(str::to_string)
        .collect();
    let mut later_user: BTreeMap<String, Vec<i64>> = BTreeMap::new();
    for row in rows
        .iter()
        .filter(|r| r.by == "user" && matches!(r.kind.as_str(), "correct" | "reject"))
    {
        later_user
            .entry(row.session.clone())
            .or_default()
            .push(row.ts);
    }
    let retracted = |row: &Correction| {
        let Some(sender) = row.corrector.as_deref() else {
            return false;
        };
        bus.rows().iter().any(|m| {
            m.from_session.as_deref() == Some(sender)
                && m.to_session.as_deref() == Some(row.session.as_str())
                && epoch(Some(&m.ts)).is_some_and(|t| t > row.ts)
                && crate::correction_meter::retraction_re().is_match(&m.body)
        })
    };
    let stamp = chrono::DateTime::from_timestamp(now, 0)
        .map(|d| d.to_rfc3339_opts(chrono::SecondsFormat::Secs, true))
        .unwrap_or_default();
    let mut appended = 0;
    for row in rows.iter().filter(|r| !settled.contains(&r.id)) {
        if appended >= MAX_ROWS_PER_PASS {
            break;
        }
        let Some((outcome, basis)) =
            settle(row, &later_user, &decision_named, &retracted, merges, now)
        else {
            continue;
        };
        let envelope = json!({"ts": stamp, "type": "correction_outcome", "source": "daemon",
            "session_id": row.session,
            "data": {"correction_id": row.id, "outcome": outcome, "basis": basis}});
        let id = format!("corrout:{}", row.id);
        append_envelope(journal, &envelope.to_string(), Some(&id))?;
        appended += 1;
    }
    Ok(appended)
}

/// Merge facts by node from the graph rows.
pub(crate) fn node_merges(rows: &[Value]) -> BTreeMap<String, NodeMerge> {
    let mut out = BTreeMap::new();
    for row in rows {
        let Some(id) = row.get("id").and_then(Value::as_str) else {
            continue;
        };
        if row.get("merge_status").and_then(Value::as_str) != Some("merged") {
            continue;
        }
        let merged_at = epoch(row.get("completed_at").and_then(Value::as_str));
        out.insert(id.to_string(), NodeMerge { merged_at });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(journal: &Path, id: &str, by: &str, session: &str, node: &str, ts: i64) {
        let stamp = chrono::DateTime::from_timestamp(ts, 0)
            .unwrap()
            .to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
        let envelope = json!({"ts": stamp, "type": "agent_corrected", "source": "daemon",
            "data": {"correction_id": id, "corrected_by": by, "kind": "correct",
                "session_id": session, "rule_version": "v1", "ts_turn": stamp,
                "node": {"value": node, "basis": "registry"}}});
        append_envelope(journal, &envelope.to_string(), Some(id)).unwrap();
    }

    #[test]
    fn ac10_a_merge_holds_a_later_user_word_overrules_and_age_leaves_it_unresolved() {
        let dir = tempfile::tempdir().unwrap();
        let journal = dir.path().join("events.jsonl");
        let now = 1_793_000_000;
        let day = 86_400;
        row(&journal, "merged", "agent", "s1", "x-aaaa", now - 2 * day);
        row(&journal, "over", "agent", "s2", "x-aaaa", now - 2 * day);
        row(
            &journal,
            "over-user",
            "user",
            "s2",
            "x-bbbb",
            now - 2 * day + 60,
        );
        row(&journal, "stale", "agent", "s3", "x-bbbb", now - 15 * day);
        row(&journal, "open", "agent", "s4", "x-bbbb", now - day);
        let mut merges = BTreeMap::new();
        merges.insert(
            "x-aaaa".to_string(),
            NodeMerge {
                merged_at: Some(now - day),
            },
        );
        outcome_pass(&journal, &BusIndex::empty(), &merges, now).unwrap();
        let first = outcome_pass(&journal, &BusIndex::empty(), &merges, now).unwrap();
        assert_eq!(first, 0, "a settled row never settles twice");
        let got: BTreeMap<String, String> = data_rows(&journal, "correction_outcome", 0)
            .iter()
            .map(|r| {
                (
                    r["correction_id"].as_str().unwrap().to_string(),
                    r["outcome"].as_str().unwrap().to_string(),
                )
            })
            .collect();
        assert_eq!(got["merged"], "held");
        assert_eq!(got["over"], "overruled");
        assert_eq!(got["stale"], "unresolved");
        assert!(!got.contains_key("open"), "a young unmerged row stays open");
    }
}
