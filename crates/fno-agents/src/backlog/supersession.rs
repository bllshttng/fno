//! Supersession verification and blocked_by edge settlement summaries: the
//! pure halves of the reconcile sweep's supersession leg, ported from
//! graph/_reconcile.py. The store-side settlement lives in graph_store
//! (`settle_blocked_by_edges`); this module owns the cause-surface proof a
//! merged PR's changed-file set owes its predecessors, and the receipt
//! rendering both verbs print.

use serde_json::{json, Value};
use std::collections::BTreeMap;

/// One spelling for a repo-relative path on both sides of the match.
///
/// `strip_prefix` and not a trim of the `"./"` character set: a set-trim
/// eats the leading dot of every dotfile path and lets a declared
/// `github/ci.yml` falsely match a changed `.github/ci.yml`.
pub(crate) fn normalize_surface(path: &str) -> String {
    let replaced = path.trim().replace('\\', "/");
    replaced.strip_prefix("./").unwrap_or(&replaced).to_string()
}

/// Verify predecessor cause surfaces against one merged PR's file set.
///
/// Full contract: docs/architecture/backlog-graph-verb-contracts.md
pub(crate) fn verify_pending_supersessions(
    entries: &mut [Value],
    successor: &str,
    changed_files: &[String],
    evidence_pr: i64,
    verified_at: Option<&str>,
    evidence_complete: bool,
) -> Vec<Value> {
    let changed: std::collections::BTreeSet<String> = changed_files
        .iter()
        .filter(|p| !p.trim().is_empty())
        .map(|p| normalize_surface(p))
        .collect();
    let stamp = verified_at
        .map(str::to_string)
        .unwrap_or_else(|| chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Micros, true));
    let mut receipts: Vec<Value> = Vec::new();
    for entry in entries.iter_mut() {
        let Some(id) = entry.get("id").and_then(Value::as_str) else {
            continue;
        };
        if entry.get("superseded_by").and_then(Value::as_str) != Some(successor) {
            continue;
        }
        // A verified record never re-verifies.
        if entry
            .get("supersession")
            .map(|r| r.get("verified_at").is_some())
            .unwrap_or(false)
        {
            continue;
        }
        let surfaces: Vec<String> = entry
            .get("supersession")
            .and_then(|r| r.get("surfaces"))
            .and_then(Value::as_array)
            .map(|rows| {
                rows.iter()
                    .filter_map(Value::as_str)
                    .filter(|s| !s.trim().is_empty())
                    .map(normalize_surface)
                    .collect()
            })
            .unwrap_or_default();
        let matched: Vec<&String> = surfaces.iter().filter(|s| changed.contains(*s)).collect();
        let uncovered: Vec<&String> = surfaces.iter().filter(|s| !changed.contains(*s)).collect();
        if uncovered.is_empty() {
            if let Some(record) = entry.get_mut("supersession").and_then(Value::as_object_mut) {
                record.insert("verified_at".into(), json!(stamp));
                record.insert("evidence_pr".into(), json!(evidence_pr));
                record.insert(
                    "matched_surfaces".into(),
                    json!(matched.into_iter().cloned().collect::<Vec<String>>()),
                );
            }
            continue;
        }
        // Uncovered surfaces: held. Truncated evidence names the truncation;
        // complete evidence names the real gap.
        let kind = if evidence_complete {
            "supersession_unverified"
        } else {
            "supersession_evidence_truncated"
        };
        let cause = entry
            .get("supersession")
            .and_then(|r| r.get("cause"))
            .cloned()
            .unwrap_or(Value::Null);
        receipts.push(json!({
            "kind": kind,
            "predecessor": id,
            "successor": successor,
            "cause": cause,
            "uncovered_surfaces": uncovered.into_iter().cloned().collect::<Vec<String>>(),
            "evidence_pr": evidence_pr,
        }));
    }
    receipts
}

/// Phrases a successor's own text uses to scope work out of itself. A
/// successor that says one of these covers only part of what it replaces,
/// whatever its `--covers` text claims.
const SCOPE_OUT_MARKERS: [&str; 6] = [
    "not this node",
    "not in this node",
    "outside this node",
    "out of scope",
    "scoped out",
    "left for a follow-up",
];

fn scope_out_marker(successor: &Value) -> Option<&'static str> {
    let text = ["title", "details"]
        .iter()
        .filter_map(|key| successor.get(*key).and_then(Value::as_str))
        .flat_map(str::split_whitespace)
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase();
    SCOPE_OUT_MARKERS.into_iter().find(|m| text.contains(m))
}

fn is_set(row: &Value, key: &str) -> bool {
    row.get(key).is_some_and(|v| match v {
        Value::Null | Value::Bool(false) => false,
        Value::String(s) => !s.is_empty(),
        _ => true,
    })
}

/// The coverage clause a supersede owes, appended to its stored reason: how
/// the successor's done condition covers the old node's (`covers`), or the
/// filed live node holding the scope the successor leaves out (`leftover`).
/// A successor whose own text scopes work out of itself owes a leftover.
pub(crate) fn coverage_clause(
    successor: &Value,
    new_id: &str,
    old_id: &str,
    covers: Option<&str>,
    leftover: Option<&Value>,
) -> Result<String, String> {
    let covers = covers.map(str::trim).filter(|c| !c.is_empty());
    let file_it = format!(
        "File the scope first, then re-run with --leftover <its id>:\n  \
         fno backlog idea \"<the part of {old_id} that {new_id} leaves out>\""
    );
    let Some(leftover) = leftover else {
        if let Some(marker) = scope_out_marker(successor) {
            return Err(format!(
                "Error: {new_id} covers only part of {old_id}: its own text says \
                 \"{marker}\". That is a split, not a supersede.\n{file_it}"
            ));
        }
        let Some(covers) = covers else {
            return Err(format!(
                "Error: name how {new_id} covers the done condition of {old_id} \
                 (--covers \"...\"), or name the filed node that holds what {new_id} \
                 leaves out (--leftover <id>). A successor that covers only part of \
                 the work is a split, not a supersede.\n{file_it}"
            ));
        };
        return Ok(format!("covers: {covers}"));
    };
    let id = leftover.get("id").and_then(Value::as_str).unwrap_or("");
    if id == new_id || id == old_id {
        return Err(format!(
            "Error: --leftover must name a third node, not {id}. {file_it}"
        ));
    }
    let dead = [
        ("completed_at", "done"),
        ("superseded_by", "superseded"),
        ("deferred_at", "deferred"),
    ]
    .into_iter()
    .find(|(key, _)| is_set(leftover, key));
    if let Some((_, state)) = dead {
        return Err(format!(
            "Error: --leftover {id} is {state}; the scope {new_id} leaves out must \
             stay live work. {file_it}"
        ));
    }
    Ok(match covers {
        Some(covers) => format!("covers: {covers}; leftover: {id}"),
        None => format!("leftover: {id}"),
    })
}

/// Superseded candidates whose successor (looked up in `all`) shipped with a
/// PR while coverage stays unproven: no record at all, so nothing was ever
/// written about why, or a record whose surfaces that PR never touched.
pub(crate) fn shipped_successor_unproven(candidates: &[Value], all: &[Value]) -> Vec<Value> {
    let by_id: BTreeMap<&str, &Value> = all
        .iter()
        .filter_map(|e| e.get("id").and_then(Value::as_str).map(|id| (id, e)))
        .collect();
    let mut rows = Vec::new();
    for entry in candidates {
        let Some(successor_id) = entry
            .get("superseded_by")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
        else {
            continue;
        };
        let Some(successor) = by_id.get(successor_id) else {
            continue;
        };
        let record = entry.get("supersession").filter(|r| r.is_object());
        if is_set(entry, "completed_at")
            || !is_set(successor, "completed_at")
            || successor.get("pr_number").and_then(Value::as_i64).is_none()
            || record.is_some_and(|r| is_set(r, "verified_at"))
        {
            continue;
        }
        let list = |key: &str| -> Vec<Value> {
            record
                .and_then(|r| r.get(key))
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default()
        };
        let matched = list("matched_surfaces");
        let uncovered: Vec<Value> = list("surfaces")
            .into_iter()
            .filter(|s| !matched.contains(s))
            .collect();
        rows.push(json!({
            "predecessor": entry.get("id"),
            "successor": successor_id,
            "evidence_pr": successor.get("pr_number"),
            "uncovered_surfaces": uncovered,
            "reason": record.and_then(|r| r.get("reason")),
            "recorded": record.is_some(),
        }));
    }
    rows
}

/// Successor id -> successor node (cloned), for pending predecessors
/// already owed proof. Only a CLOSED successor is owed here: an open one
/// still has its ordinary close ahead of it, and that path verifies.
pub(crate) fn successors_owing_verification(entries: &[Value]) -> BTreeMap<String, Value> {
    let by_id: BTreeMap<&str, &Value> = entries
        .iter()
        .filter_map(|e| e.get("id").and_then(Value::as_str).map(|id| (id, e)))
        .collect();
    let mut owed = BTreeMap::new();
    for entry in entries {
        let Some(successor_id) = entry.get("superseded_by").and_then(Value::as_str) else {
            continue;
        };
        let Some(record) = entry.get("supersession") else {
            continue;
        };
        if record.get("verified_at").is_some() {
            continue;
        }
        let Some(successor) = by_id.get(successor_id) else {
            continue;
        };
        let closed = successor
            .get("completed_at")
            .map(|v| !v.is_null())
            .unwrap_or(false);
        let has_pr = successor
            .get("pr_number")
            .map(Value::is_i64)
            .unwrap_or(false);
        if closed && has_pr {
            owed.insert(successor_id.to_string(), (*successor).clone());
        }
    }
    owed
}

/// Apply the settlement's `blocked_by` map to rows in place; receipts out.
/// The pruned-edge trail rides the same receipts onto the LIVE rows (the
/// sweep's own entries are a throwaway copy, so here is where the commit
/// is): without it a reopen finds no trail and restores nothing.
pub(crate) fn apply_edge_settlement(entries: &mut [Value], settlement: &Value) -> Vec<Value> {
    let Some(map) = settlement.get("blocked_by").and_then(Value::as_object) else {
        return Vec::new();
    };
    for node in entries.iter_mut() {
        let Some(id) = node.get("id").and_then(Value::as_str) else {
            continue;
        };
        if let Some(blockers) = map.get(id) {
            if let Some(obj) = node.as_object_mut() {
                obj.insert("blocked_by".into(), blockers.clone());
            }
        }
    }
    let receipts = settlement
        .get("receipts")
        .and_then(Value::as_array)
        .map(|rows| rows.clone())
        .unwrap_or_default();
    crate::graph_store::write_pruned_dependents_trail(entries, &receipts);
    receipts
}

/// One line for the sweep report; held edges name why they stay.
pub(crate) fn summarize_edge_settlement(receipts: &[Value]) -> String {
    let count = |kind: &str| {
        receipts
            .iter()
            .filter(|r| r.get("kind").and_then(Value::as_str) == Some(kind))
            .count()
    };
    format!(
        "blocked_by edges settled: {} pruned, {} rewired, {} held (a deferred or missing blocker stays)",
        count("blocked_by_pruned"),
        count("blocked_by_rewired"),
        count("blocked_by_held"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pred(id: &str, surfaces: &[&str], verified: bool) -> Value {
        let mut record = json!({"cause": "dup of the legend work", "surfaces": surfaces});
        if verified {
            record["verified_at"] = json!("2026-09-01T00:00:00Z");
        }
        json!({
            "id": id,
            "superseded_by": "x-succ",
            "supersession": record,
        })
    }

    #[test]
    fn full_surface_match_stamps_the_record() {
        let mut entries = vec![pred("x-pre", &["src/a.rs", "./src/b.rs"], false)];
        let receipts = verify_pending_supersessions(
            &mut entries,
            "x-succ",
            &["src/a.rs".into(), "src/b.rs".into(), "src/c.rs".into()],
            77,
            Some("2026-10-02T00:00:00Z"),
            true,
        );
        assert!(receipts.is_empty());
        let record = &entries[0]["supersession"];
        assert_eq!(record["verified_at"], "2026-10-02T00:00:00Z");
        assert_eq!(record["evidence_pr"], 77);
        assert_eq!(record["matched_surfaces"], json!(["src/a.rs", "src/b.rs"]));
    }

    #[test]
    fn uncovered_surfaces_hold_and_the_kind_tracks_completeness() {
        for (complete, kind) in [
            (true, "supersession_unverified"),
            (false, "supersession_evidence_truncated"),
        ] {
            let mut entries = vec![pred("x-pre", &["src/a.rs", "src/gone.rs"], false)];
            let receipts = verify_pending_supersessions(
                &mut entries,
                "x-succ",
                &["src/a.rs".into()],
                77,
                None,
                complete,
            );
            assert_eq!(receipts.len(), 1, "{kind}");
            assert_eq!(receipts[0]["kind"], kind);
            assert_eq!(receipts[0]["predecessor"], "x-pre");
            assert_eq!(receipts[0]["uncovered_surfaces"], json!(["src/gone.rs"]));
            assert!(entries[0]["supersession"].get("verified_at").is_none());
        }
    }

    #[test]
    fn a_narrower_successor_is_refused_until_the_leftover_is_filed() {
        fn check(covers: Option<&str>, leftover: Option<&Value>) -> Result<String, String> {
            let narrow = json!({"id": "x-new", "details": "Pass 2 (not this\nnode): identifiers."});
            coverage_clause(&narrow, "x-new", "x-old", covers, leftover)
        }
        let refused = check(Some("renames all"), None).unwrap_err();
        assert!(refused.contains("\"not this node\""), "{refused}");
        let stamp = "2026-10-01T00:00:00Z";
        for (row, state) in [
            (json!({"id": "x-rest", "completed_at": stamp}), "done"),
            (json!({"id": "x-rest", "deferred_at": stamp}), "deferred"),
            (json!({"id": "x-old"}), "third node"),
        ] {
            let err = check(None, Some(&row)).unwrap_err();
            assert!(err.contains(state), "{err}");
        }
        let filed = json!({"id": "x-rest"});
        assert_eq!(check(None, Some(&filed)).unwrap(), "leftover: x-rest");
        let whole = json!({"id": "x-new"});
        assert!(coverage_clause(&whole, "x-new", "x-old", Some("  "), None).is_err());
        assert_eq!(
            coverage_clause(&whole, "x-new", "x-old", Some("renames all"), None).unwrap(),
            "covers: renames all"
        );
    }

    #[test]
    fn the_probe_counts_a_recordless_and_an_uncovered_supersession() {
        let mut entries = vec![
            json!({"id": "x-succ", "completed_at": "2026-10-02T00:00:00Z", "pr_number": 9}),
            json!({"id": "x-open", "superseded_by": "x-live"}),
            json!({"id": "x-live"}),
            json!({"id": "x-bare", "superseded_by": "x-succ"}),
            pred("x-pre", &["src/a.rs", "src/b.rs"], false),
            pred("x-ok", &["src/a.rs"], true),
        ];
        entries[4]["supersession"]["matched_surfaces"] = json!(["src/a.rs"]);
        let row = |id: &str, uncovered: Value, recorded: bool| {
            json!({"predecessor": id, "successor": "x-succ", "evidence_pr": 9,
                   "uncovered_surfaces": uncovered, "reason": null, "recorded": recorded})
        };
        assert_eq!(
            shipped_successor_unproven(&entries, &entries),
            vec![
                row("x-bare", json!([]), false),
                row("x-pre", json!(["src/b.rs"]), true)
            ]
        );
    }

    #[test]
    fn settlement_applies_and_summarizes() {
        let mut entries = vec![
            json!({"id": "x-a", "blocked_by": ["x-done"]}),
            json!({"id": "x-b", "blocked_by": ["x-live"]}),
        ];
        let settlement = json!({
            "blocked_by": {"x-a": []},
            "receipts": [
                {"kind": "blocked_by_pruned"},
                {"kind": "blocked_by_rewired"},
                {"kind": "blocked_by_held"},
                {"kind": "blocked_by_held"},
            ],
        });
        let receipts = apply_edge_settlement(&mut entries, &settlement);
        assert_eq!(entries[0]["blocked_by"], json!([]));
        assert_eq!(entries[1]["blocked_by"], json!(["x-live"]));
        assert_eq!(receipts.len(), 4);
        assert_eq!(
            summarize_edge_settlement(&receipts),
            "blocked_by edges settled: 1 pruned, 1 rewired, 2 held (a deferred or missing blocker stays)"
        );
    }
}
