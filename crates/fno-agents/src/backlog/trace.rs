//! `fno backlog get <id> --trace`: one node's history as JSON.
//!
//! The fold writes nothing. Phases come from the graph `sessions` rows, and
//! the review span from review_invocation and review_attestation rows whose
//! branch names the node. A value no source answered reads null with a
//! reason beside it, never 0.

use serde_json::{json, Value};

const PHASES: [&str; 5] = ["think", "blueprint", "execute", "review", "ship"];

/// Everything the fold reads, gathered first so the fold itself is pure.
#[derive(Default)]
pub(crate) struct Inputs {
    /// The review rows whose branch names the node.
    pub(crate) reviews: Vec<Value>,
    pub(crate) cost_usd: Option<f64>,
}

fn ms_of(ts: &str) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(ts)
        .ok()
        .map(|t| t.timestamp_millis())
}

fn stamp(ms: i64) -> String {
    chrono::DateTime::from_timestamp_millis(ms)
        .map(|t| t.format("%Y-%m-%dT%H:%M:%SZ").to_string())
        .unwrap_or_default()
}

fn str_of<'a>(v: &'a Value, key: &str) -> Option<&'a str> {
    v.get(key).and_then(Value::as_str).filter(|s| !s.is_empty())
}

/// True when `branch` names `node` as a whole token (`feature/x-aaaa-...`).
pub(crate) fn branch_names(branch: &str, node: &str) -> bool {
    branch.match_indices(node).any(|(at, _)| {
        let before = branch[..at].chars().next_back();
        let after = branch[at + node.len()..].chars().next();
        !before.is_some_and(|c| c.is_ascii_alphanumeric())
            && !after.is_some_and(|c| c.is_ascii_alphanumeric())
    })
}

/// The trace for one node from gathered inputs.
pub(crate) fn fold(node: &Value, inp: &Inputs) -> Value {
    let id = str_of(node, "id").unwrap_or_default();
    let sessions: Vec<&Value> = node
        .get("sessions")
        .and_then(Value::as_array)
        .map(|a| a.iter().collect())
        .unwrap_or_default();
    let phases: Vec<Value> = PHASES
        .iter()
        .map(|phase| phase_row(phase, &sessions, inp))
        .collect();

    let started: Vec<i64> = phases
        .iter()
        .filter_map(|p| p["started_at"].as_str().and_then(ms_of))
        .collect();
    // A phase still open leaves the node open: no end, no duration.
    let ended: Vec<i64> = if phases.iter().any(|p| p["open"] == true) {
        Vec::new()
    } else {
        phases
            .iter()
            .filter_map(|p| p["ended_at"].as_str().and_then(ms_of))
            .collect()
    };
    let mut header = json!({
        "started_at": started.iter().min().map(|m| stamp(*m)),
        "ended_at": ended.iter().max().map(|m| stamp(*m)),
        "duration_s": started.iter().min().zip(ended.iter().max()).map(|(s, e)| (e - s).max(0) / 1000),
        "cost_usd": inp.cost_usd,
        "sessions": sessions.len(),
    });
    if inp.cost_usd.is_none() {
        header["cost_reason"] = json!("no cost recorded");
    }
    json!({
        "node": id,
        "title": str_of(node, "title"),
        "status": str_of(node, "status"),
        "header": header,
        "phases": phases,
    })
}

/// One phase: its span over its session rows and its sessions. Review adds
/// the span of the node's review rows.
fn phase_row(phase: &str, sessions: &[&Value], inp: &Inputs) -> Value {
    let rows: Vec<&&Value> = sessions
        .iter()
        .filter(|s| str_of(s, "phase") == Some(phase))
        .collect();
    let mut starts: Vec<i64> = rows
        .iter()
        .filter_map(|s| str_of(s, "started_at").and_then(ms_of))
        .collect();
    let mut ends: Vec<i64> = rows
        .iter()
        .filter_map(|s| str_of(s, "ended_at").and_then(ms_of))
        .collect();
    let open = rows.iter().any(|s| str_of(s, "ended_at").is_none());
    let listed: Vec<Value> = rows
        .iter()
        .map(|s| {
            json!({
                "session_id": str_of(s, "session_id"),
                "harness": str_of(s, "harness"),
                "model": s.pointer("/observed_model/model").or_else(|| s.get("observed_model").filter(|m| m.is_string())),
                "started_at": str_of(s, "started_at"),
                "ended_at": str_of(s, "ended_at"),
            })
        })
        .collect();
    let derived = phase == "review" && !inp.reviews.is_empty();
    if derived {
        let at = |t: &str| {
            inp.reviews
                .iter()
                .filter(|r| str_of(r, "type") == Some(t))
                .filter_map(|r| str_of(r, "ts").and_then(ms_of))
                .collect::<Vec<i64>>()
        };
        starts.extend(at("review_invocation"));
        let attested = at("review_attestation");
        ends.extend(attested.iter().copied());
        if starts.is_empty() {
            starts.extend(attested.iter().min());
        }
    }
    let mut row = json!({
        "phase": phase,
        "started_at": starts.iter().min().map(|m| stamp(*m)),
        "ended_at": if open && !derived { None } else { ends.iter().max().map(|m| stamp(*m)) },
        "sessions": listed,
    });
    if row["started_at"].is_null() {
        row["start_reason"] = json!(if rows.is_empty() && !derived {
            "no session recorded"
        } else {
            "no start recorded"
        });
    }
    if open && !derived {
        row["open"] = json!(true);
    }
    if derived {
        row["derived_from"] = json!("review events");
    }
    row
}

/// The node's own project store, from its `project` root in the work map,
/// else this checkout's store.
fn store_of(node: &Value) -> std::path::PathBuf {
    str_of(node, "project")
        .and_then(super::settings::project_root)
        .map(|root| crate::paths::events_path(std::path::Path::new(&root)))
        .unwrap_or_else(crate::paths::project_events_path)
}

/// Shared reads for one trace call: the store and the ledger, read once.
pub(crate) struct Shared {
    reviews: Vec<Value>,
    ledger: super::scorecard::LedgerIndex,
}

impl Shared {
    pub(crate) fn load(root: &Value) -> Shared {
        let events_path = store_of(root);
        // An unreadable store leaves no review rows; the review phase then
        // reads its sessions alone.
        let _ = crate::event_store::import_all(&events_path);
        let reviews = crate::event_store::query_events(
            &events_path,
            &crate::event_store::EventQuery::of_types(&["review_invocation", "review_attestation"]),
        )
        .map(|rows| {
            rows.iter()
                .filter_map(|r| serde_json::from_str(&r.line).ok())
                .collect()
        })
        .unwrap_or_default();
        Shared {
            reviews,
            ledger: super::scorecard::LedgerIndex::load(),
        }
    }

    /// The inputs for one node.
    pub(crate) fn gather(&self, node: &Value) -> Inputs {
        let id = str_of(node, "id").unwrap_or_default();
        let reviews = self
            .reviews
            .iter()
            .filter(|r| {
                r.pointer("/data/branch")
                    .and_then(Value::as_str)
                    .is_some_and(|b| branch_names(b, id))
            })
            .cloned()
            .collect();
        Inputs {
            reviews,
            cost_usd: super::scorecard::node_cost_usd(node, id, &self.ledger),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session(phase: &str, sid: &str, start: Option<&str>, end: Option<&str>) -> Value {
        json!({"phase": phase, "harness": "claude", "session_id": sid,
               "started_at": start, "ended_at": end})
    }

    #[test]
    fn phase_spans_fold_and_an_open_phase_leaves_the_node_open() {
        let node = json!({"id": "x-1", "sessions": [
            session("think", "a", Some("2026-10-01T00:00:00Z"), Some("2026-10-01T01:00:00Z")),
            session("blueprint", "b", None, Some("2026-10-02T01:00:00Z")),
            session("execute", "c", Some("2026-10-03T00:00:00Z"), None),
        ]});
        let review = json!({"type": "review_attestation", "ts": "2026-10-03T02:00:00Z"});
        let inp = Inputs {
            reviews: vec![review],
            ..Default::default()
        };
        let t = fold(&node, &inp);
        assert_eq!(t["phases"][0]["started_at"], "2026-10-01T00:00:00Z");
        assert_eq!(t["phases"][1]["start_reason"], "no start recorded");
        assert_eq!(t["phases"][2]["open"], true);
        assert_eq!(t["phases"][3]["started_at"], "2026-10-03T02:00:00Z");
        assert_eq!(t["phases"][4]["start_reason"], "no session recorded");
        assert!(t["header"]["ended_at"].is_null());
        assert_eq!(t["header"]["cost_reason"], "no cost recorded");
        assert!(branch_names("feature/x-aaaa-trace", "x-aaaa"));
        assert!(!branch_names("feature/x-aaaa0", "x-aaaa"));
    }
}
