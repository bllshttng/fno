//! The autonomy-debt summary: `gate_escape` events ranked by reason, so the
//! roadmap sees which reliability fix pays first ("dead-bot: 7, spawn-cap:
//! 4"). A missing log is a clean zero, not an error. Logged emit failures
//! are surfaced, because a broken counter reads low.

use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::classify::py_splitlines;

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct GateEscapeSummary {
    pub total: i64,
    /// `(reason, count)`, most frequent first, ties by reason name.
    pub by_reason: Vec<(String, i64)>,
    pub prs_by_reason: BTreeMap<String, Vec<i64>>,
    pub nodes_by_reason: BTreeMap<String, Vec<String>>,
    pub emit_failures: i64,
}

/// Fold `gate_escape` rows out of journal lines. Other rows, and any line
/// that is not a JSON object of that type, are ignored.
pub fn summarize_lines(lines: &[&str], failure_log: &str) -> GateEscapeSummary {
    let mut counts: BTreeMap<String, i64> = BTreeMap::new();
    let mut prs: BTreeMap<String, Vec<i64>> = BTreeMap::new();
    let mut nodes: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for line in lines {
        if !line.contains("\"gate_escape\"") {
            continue;
        }
        let Ok(ev) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if ev.get("type").and_then(Value::as_str) != Some("gate_escape") {
            continue;
        }
        let data = ev.get("data").cloned().unwrap_or(Value::Null);
        let reason = data
            .get("reason")
            .filter(|r| super::truthy(Some(r)))
            .map_or_else(|| "other".to_string(), |r| super::py_str(Some(r)));
        *counts.entry(reason.clone()).or_default() += 1;
        if let Some(pr) = data.get("pr").and_then(Value::as_i64).filter(|n| *n > 0) {
            let list = prs.entry(reason.clone()).or_default();
            if !list.contains(&pr) {
                list.push(pr);
            }
        }
        if let Some(node) = data
            .get("graph_node_id")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
        {
            let list = nodes.entry(reason.clone()).or_default();
            if !list.iter().any(|n| n == node) {
                list.push(node.to_string());
            }
        }
    }
    let mut ranked: Vec<(String, i64)> = counts.into_iter().collect();
    ranked.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    GateEscapeSummary {
        total: ranked.iter().map(|(_, n)| n).sum(),
        prs_by_reason: ranked
            .iter()
            .map(|(r, _)| (r.clone(), prs.get(r).cloned().unwrap_or_default()))
            .collect(),
        nodes_by_reason: ranked
            .iter()
            .map(|(r, _)| (r.clone(), nodes.get(r).cloned().unwrap_or_default()))
            .collect(),
        by_reason: ranked,
        emit_failures: py_splitlines(failure_log)
            .iter()
            .filter(|l| !l.trim().is_empty())
            .count() as i64,
    }
}

/// Summarize the committed `gate_escape` rows of `events_path`, plus the
/// emit-failure log beside it.
pub fn summarize(events_path: &Path) -> GateEscapeSummary {
    let text = crate::event_store::journal_text(events_path, &["gate_escape"]);
    let failure_path = events_path.with_file_name("gate_escape_emit_failures.jsonl");
    let failures = std::fs::read_to_string(failure_path).unwrap_or_default();
    summarize_lines(&py_splitlines(&text), &failures)
}

/// Human-readable lines: `0 by reason` on an empty log, and a may-under-
/// report warning when emit failures were logged.
pub fn render(summary: &GateEscapeSummary) -> Vec<String> {
    let mut lines = Vec::new();
    if summary.total == 0 {
        lines.push("gate_escapes: 0 by reason".to_string());
    } else {
        let parts: Vec<String> = summary
            .by_reason
            .iter()
            .map(|(reason, n)| {
                let prs = summary
                    .prs_by_reason
                    .get(reason)
                    .cloned()
                    .unwrap_or_default();
                let attribution = if prs.is_empty() {
                    String::new()
                } else {
                    format!(
                        " ({})",
                        prs.iter()
                            .map(|p| format!("PR #{p}"))
                            .collect::<Vec<_>>()
                            .join(", ")
                    )
                };
                format!("{reason}={n}{attribution}")
            })
            .collect();
        lines.push(format!(
            "gate_escapes ({} total, most-frequent first): {}",
            summary.total,
            parts.join(" ")
        ));
    }
    if summary.emit_failures > 0 {
        lines.push(format!(
            "WARN gate_escape metric may under-report: {} emit failure(s) logged (fail-open); a broken counter reads LOW",
            summary.emit_failures
        ));
    }
    lines
}
