//! Run-row reader: fold `run_recorded` carrier rows (and `run_imported`
//! history rows) into one live row per run key. The newest carrier row wins;
//! an imported history row only surfaces when no carrier row covers its key
//! (the pre-carrier backfill), so live rows are never shadowed by the import.

use std::collections::HashMap;
use std::path::Path;

use serde_json::Value;

use super::{query_events, EventQuery, EventRow};

/// Typed filters for [`query_runs`]; every field is ANDed, absent fields
/// are unfiltered.
#[derive(Debug, Default, Clone)]
pub struct RunFilter {
    pub session_id: Option<String>,
    pub node_id: Option<String>,
    pub pr_number: Option<i64>,
    pub since_ms: Option<i64>,
}

/// The run key both writers and the import share: `fno_id`, else
/// `session_id`, else the backstop's node and PR for rows that predate
/// scalar ids.
pub(crate) fn run_key(run: &Value) -> Option<String> {
    for field in ["fno_id", "session_id"] {
        if let Some(id) = run.get(field).and_then(Value::as_str) {
            if !id.is_empty() {
                return Some(id.to_string());
            }
        }
    }
    let node = ["graph_node_id", "node"]
        .iter()
        .find_map(|f| run.get(*f).and_then(Value::as_str))
        .unwrap_or_default();
    let pr = run
        .get("pr_number")
        .map(|v| v.to_string())
        .unwrap_or_default();
    if node.is_empty() && pr.is_empty() {
        return None;
    }
    Some(format!("{node}\u{1f}{pr}"))
}

fn run_matches(run: &Value, filter: &RunFilter) -> bool {
    if let Some(session) = &filter.session_id {
        let hit = ["fno_id", "session_id"]
            .iter()
            .any(|f| run.get(*f).and_then(Value::as_str) == Some(session.as_str()));
        if !hit {
            return false;
        }
    }
    if let Some(node) = &filter.node_id {
        let hit = ["graph_node_id", "node"]
            .iter()
            .any(|f| run.get(*f).and_then(Value::as_str) == Some(node.as_str()));
        if !hit {
            return false;
        }
    }
    if let Some(pr) = filter.pr_number {
        if run.get("pr_number").and_then(Value::as_i64) != Some(pr) {
            return false;
        }
    }
    true
}

/// One live row per run key: two carrier rows for one session fold to the
/// newest, and a `run_imported` row covers only a key no carrier row covers.
/// Rows read in commit order; the answer keeps that order.
pub fn query_runs(journal: &Path, filter: &RunFilter) -> Result<Vec<Value>, String> {
    let mut q = EventQuery::of_types(&["run_recorded", "run_imported"]);
    q.since_ms = filter.since_ms;
    let rows = query_events(journal, &q)?;
    fold_rows(&rows, filter)
}

/// The fold over already-read rows, split out so the semantics are pinnable
/// without a store.
fn fold_rows(rows: &[EventRow], filter: &RunFilter) -> Result<Vec<Value>, String> {
    let mut carrier_seq: HashMap<String, i64> = HashMap::new();
    let mut carrier: HashMap<String, Value> = HashMap::new();
    let mut order: Vec<String> = Vec::new();
    let mut imported: Vec<(String, i64, Value)> = Vec::new();
    for row in rows {
        let Ok(line) = serde_json::from_str::<Value>(&row.line) else {
            continue;
        };
        let Some(run) = line.pointer("/data/run").filter(|v| v.is_object()) else {
            continue;
        };
        let Some(key) = run_key(run) else {
            continue;
        };
        if row.r#type == "run_recorded" {
            if !carrier_seq.contains_key(&key) {
                order.push(key.clone());
            }
            carrier_seq.insert(key.clone(), row.seq);
            carrier.insert(key, run.clone());
        } else if row.r#type == "run_imported" {
            imported.push((key, row.seq, run.clone()));
        }
    }
    let mut out: Vec<(i64, Value)> = Vec::new();
    for key in &order {
        let run = &carrier[key];
        if run_matches(run, filter) {
            out.push((carrier_seq[key], run.clone()));
        }
    }
    for (key, seq, run) in imported {
        if carrier_seq.contains_key(&key) {
            continue; // a live carrier row covers the imported history
        }
        if run_matches(&run, filter) {
            out.push((seq, run));
        }
    }
    out.sort_by_key(|(seq, _)| *seq);
    Ok(out.into_iter().map(|(_, run)| run).collect())
}
