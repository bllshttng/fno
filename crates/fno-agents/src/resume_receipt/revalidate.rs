//! Event canonicalization + the revalidate gate.
//!
//! The canonicalization contract is the deleted Python leg's own words: no
//! resume database. Observations dedup by signature (the raw dict if the
//! event carries one, else the whole event, JSON with sorted keys, compact
//! separators) and sort by (has-parsed-ts, parsed ts on the naive-LOCAL
//! timeline, complete-context flag, signature) - the scoreboard fold's
//! `_event_sort_key`, replicated so the receipt's view of "what happened for
//! this node" is identical to the reducer's. Duplicate observations across
//! journals fold to one latest-by-timestamp value, so stale authority cannot
//! revive.

use serde_json::{Map, Value};

use super::model::{ResumeReceipt, SUPERSEDED_KINDS};
use chrono::{DateTime, Local};

#[derive(Debug, Clone, PartialEq)]
pub struct RevalidationResult {
    pub ok: bool,
    /// Empty on ok; a machine-readable fail-closed code otherwise.
    pub reason: String,
    /// The live values compared, for observability; never empty, so a green
    /// verdict is distinguishable from a no-op.
    pub checked: Map<String, Value>,
}

/// Parse an ISO timestamp onto the naive-LOCAL timeline - the one timeline
/// the fold uses. The dominant source writes naive-local, and `now` is
/// naive-local, so those compare apples-to-apples; aware timestamps convert
/// to local BEFORE their tzinfo is stripped, so an offset like `+02:00` is
/// not silently mis-read.
pub(super) fn parse_ts_local(raw: Option<&str>) -> Option<chrono::NaiveDateTime> {
    let raw = raw?;
    if raw.is_empty() {
        return None;
    }
    let dt = DateTime::parse_from_rfc3339(raw)
        .ok()
        .map(|dt| dt.with_timezone(&Local).naive_local());
    if dt.is_some() {
        return dt;
    }
    // Naive shapes: the ledger's `completed` is `datetime.now().isoformat()`
    // (no offset). Python's fromisoformat reads `YYYY-MM-DDTHH:MM:SS[.ffffff]`
    // and space-separated forms; read the two shapes the journals carry.
    let formats = [
        "%Y-%m-%dT%H:%M:%S%.f",
        "%Y-%m-%d %H:%M:%S%.f",
        "%Y-%m-%dT%H:%M:%S",
        "%Y-%m-%d %H:%M:%S",
    ];
    formats
        .iter()
        .find_map(|fmt| chrono::NaiveDateTime::parse_from_str(raw, fmt).ok())
}

/// One event's signature: the raw dict if it carries one, else the whole
/// event, JSON with sorted keys, compact separators.
pub(super) fn event_signature(event: &Value) -> String {
    let source = event.get("raw").filter(|r| r.is_object()).unwrap_or(event);
    compact_sorted_json(source)
}

/// serde_json with preserve-order keeps insertion order, so the canonical
/// form sorts keys itself.
pub(super) fn compact_sorted_json(value: &Value) -> String {
    match value {
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            let body: Vec<String> = keys
                .iter()
                .map(|k| {
                    format!(
                        "{}:{}",
                        serde_json::to_string(k).unwrap_or_default(),
                        compact_sorted_json(&map[k.as_str()])
                    )
                })
                .collect();
            format!("{{{}}}", body.join(","))
        }
        Value::Array(items) => {
            let body: Vec<String> = items.iter().map(compact_sorted_json).collect();
            format!("[{}]", body.join(","))
        }
        other => other.to_string(),
    }
}

/// One event's kind: `type` first, then `kind` - the fold's order in the
/// sort key, the supersession scan, the duplicate-generation scan, and
/// latest_observation.
pub(super) fn event_kind(event: &Value) -> Option<&str> {
    event
        .get("type")
        .and_then(Value::as_str)
        .or_else(|| event.get("kind").and_then(Value::as_str))
}

/// The coverage reader's filter order: `kind` first, then `type`.
pub(super) fn event_kind_reader(event: &Value) -> Option<&str> {
    event
        .get("kind")
        .and_then(Value::as_str)
        .or_else(|| event.get("type").and_then(Value::as_str))
}

/// Dedup + sort the node's observations the way the scoreboard does: dup by
/// signature, then (has-ts, ts, complete-context, signature) ascending.
pub fn canonicalize_node_events(events: &[Value]) -> Vec<Value> {
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut out: Vec<Value> = Vec::new();
    for e in events {
        if !e.is_object() {
            continue;
        }
        let signature = event_signature(e);
        if seen.contains(&signature) {
            continue;
        }
        seen.insert(signature);
        out.push(e.clone());
    }
    out.sort_by(|a, b| event_sort_key(a).cmp(&event_sort_key(b)));
    out
}

/// The fold's sort key: (parsed is None, parsed or MIN, complete_context,
/// signature). Python tuples compare elementwise ascending; the Rust mirror
/// orders None-timestamps first (False < True reads `parsed is not None` as
/// its own key element: unparsed sorts FIRST in Python because False < True).
pub(super) fn event_sort_key(event: &Value) -> (bool, chrono::NaiveDateTime, bool, String) {
    let parsed = parse_ts_local(
        event
            .get("ts")
            .and_then(Value::as_str)
            .or_else(|| event.get("timestamp").and_then(Value::as_str)),
    );
    let signature = event_signature(event);
    let complete_context = event_kind(event) == Some("context_snapshot") && {
        event
            .get("data")
            .and_then(Value::as_object)
            .and_then(|d| d.get("measurement_complete"))
            .and_then(Value::as_bool)
            == Some(true)
    };
    let ts = parsed.unwrap_or_else(|| {
        chrono::NaiveDate::from_ymd_opt(0, 1, 1)
            .unwrap()
            .and_hms_opt(0, 0, 0)
            .unwrap()
    });
    (parsed.is_none(), ts, complete_context, signature)
}

/// The latest event (by parsed timestamp), optionally kind-filtered. After
/// canonicalization the last element is the latest by timestamp with a
/// deterministic tiebreak.
pub fn latest_observation(events: &[Value], kinds: Option<&[&str]>) -> Option<Value> {
    let mut canon = canonicalize_node_events(events);
    if let Some(kinds) = kinds {
        canon.retain(|e| event_kind(e).map(|k| kinds.contains(&k)).unwrap_or(false));
    }
    canon.pop()
}

/// True if a `delegated` event already minted this generation off-session.
/// Mirrors handoff.sh's (node, harness)-scoped generation count: a delegated
/// event with this generation whose from_session is not the receipt's own
/// session means a second lineage is claiming the same generation slot - the
/// receipt must fail closed as a duplicate rather than re-mint the name.
pub fn detect_duplicate_generation(
    events: &[Value],
    node: &str,
    harness: Option<&str>,
    generation: i64,
    own_session: &str,
) -> bool {
    for e in canonicalize_node_events(events) {
        if event_kind(&e) != Some("delegated") {
            continue;
        }
        let data = e.get("data").and_then(Value::as_object);
        let Some(data) = data else { continue };
        if data.get("node_id").and_then(Value::as_str) != Some(node) {
            continue;
        }
        if let Some(harness) = harness {
            if data.get("harness").and_then(Value::as_str) != Some(harness) {
                continue;
            }
        }
        let Some(raw_gen) = data.get("generation") else {
            continue;
        };
        let Some(gen) = raw_gen.as_i64() else {
            continue;
        };
        if gen == generation {
            let from = data
                .get("from_session")
                .and_then(Value::as_str)
                .unwrap_or_default();
            if from != own_session {
                return true;
            }
        }
    }
    false
}

/// The node an event belongs to: `node_id`/`graph_node_id` at top level, then
/// inside `data`.
pub(super) fn event_node(event: &Value) -> Option<&str> {
    for key in ["node_id", "graph_node_id"] {
        if let Some(v) = event.get(key).and_then(Value::as_str) {
            if !v.is_empty() {
                return Some(v);
            }
        }
    }
    if let Some(data) = event.get("data").and_then(Value::as_object) {
        for key in ["node_id", "graph_node_id"] {
            if let Some(v) = data.get(key).and_then(Value::as_str) {
                if !v.is_empty() {
                    return Some(v);
                }
            }
        }
    }
    None
}

/// The session an event belongs to. `delegated` carries from_session (the
/// delegating predecessor); other kinds carry session_id. Used to exclude a
/// receipt's OWN continuation events (its own later delegation/loop_check)
/// from supersession - those are the receipt's own succession, not a foreign
/// later authority.
pub(super) fn event_session(event: &Value) -> Option<&str> {
    if let Some(data) = event.get("data").and_then(Value::as_object) {
        for key in ["from_session", "session_id"] {
            if let Some(v) = data.get(key).and_then(Value::as_str) {
                if !v.is_empty() {
                    return Some(v);
                }
            }
        }
    }
    for key in ["from_session", "session_id"] {
        if let Some(v) = event.get(key).and_then(Value::as_str) {
            if !v.is_empty() {
                return Some(v);
            }
        }
    }
    None
}

/// A node event strictly after the receipt's write means stale authority.
/// Parsed-timestamp comparison on the fold's one timeline. An undated late
/// event is conservatively treated as non-superseding so a missing ts never
/// false-positives a live receipt into failure.
pub(super) fn superseded_by_later_event(
    events: &[Value],
    node: &str,
    receipt_written_at: &str,
    own_session: Option<&str>,
) -> Option<String> {
    let written = parse_ts_local(Some(receipt_written_at))?;
    for e in canonicalize_node_events(events) {
        let Some(kind) = event_kind(&e) else {
            continue;
        };
        if !SUPERSEDED_KINDS.contains(&kind) {
            continue;
        }
        if event_node(&e) != Some(node) {
            continue;
        }
        if let Some(own) = own_session {
            if event_session(&e) == Some(own) {
                continue;
            }
        }
        let at = parse_ts_local(
            e.get("ts")
                .and_then(Value::as_str)
                .or_else(|| e.get("timestamp").and_then(Value::as_str)),
        );
        if let Some(at) = at {
            if at > written {
                return Some(format!("superseded_by_later_event:{kind}"));
            }
        }
    }
    None
}

/// Compare a receipt against live state. Read-only; never mutates claims.
///
/// Returns ok=true only when every check passes. On any mismatch returns
/// ok=false with a machine-readable reason; the caller parks WITHOUT touching
/// the predecessor's claim or state (predecessor state preserved by
/// construction).
///
/// Checks (all fail-closed):
///   stale_head            - receipt.head != live HEAD (the candidate moved)
///   stale_branch          - the line of work changed
///   dead_worktree         - the recorded worktree no longer exists
///   foreign_claim         - node held by a session outside the receipt's lineage
///   duplicate_generation  - a delegated event already minted this generation
///                           off the receipt's own session
///   superseded            - a later terminal/delegated/loop_check for the node
///                           postdates the receipt (stale authority revived)
#[allow(clippy::too_many_arguments)]
pub fn revalidate(
    receipt: &ResumeReceipt,
    live_head: &str,
    live_branch: &str,
    worktree_exists: bool,
    live_claim_holder: Option<&str>,
    own_session: Option<&str>,
    node_events: &[Value],
    harness: Option<&str>,
) -> RevalidationResult {
    let ident = &receipt.identity;
    let own = own_session
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| ident.session.clone());
    let checked = serde_json::json!({
        "node": ident.node,
        "receipt_head": receipt.head,
        "live_head": live_head,
        "receipt_branch": receipt.branch,
        "live_branch": live_branch,
        "worktree_exists": worktree_exists,
        "live_claim_holder": live_claim_holder,
        "own_session": own,
    });
    let checked = checked.as_object().cloned().unwrap_or_default();

    // stale HEAD: the candidate must still be the live HEAD. A successor
    // starts at the predecessor's HEAD; if it moved (a commit landed, or the
    // branch was rewound), the receipt's next_action is no longer
    // trustworthy.
    if !receipt.head.is_empty() && !live_head.is_empty() && receipt.head != live_head {
        return RevalidationResult {
            ok: false,
            reason: "stale_head".into(),
            checked,
        };
    }
    // branch drift is the same class of failure (the line of work changed).
    if !receipt.branch.is_empty() && !live_branch.is_empty() && receipt.branch != live_branch {
        return RevalidationResult {
            ok: false,
            reason: "stale_branch".into(),
            checked,
        };
    }
    if !worktree_exists {
        return RevalidationResult {
            ok: false,
            reason: "dead_worktree".into(),
            checked,
        };
    }
    // foreign claim: the node may be held by the receipt's own session (ok,
    // idempotent re-acquire), a free claim (ok, the successor acquires), or a
    // FOREIGN session (fail - someone else owns the node). SUSPECT counts as
    // a live foreign holder (a respawned worker's protected slot).
    if let Some(holder) = live_claim_holder {
        if holder != own {
            return RevalidationResult {
                ok: false,
                reason: "foreign_claim".into(),
                checked,
            };
        }
    }
    if detect_duplicate_generation(node_events, &ident.node, harness, ident.generation, &own) {
        return RevalidationResult {
            ok: false,
            reason: "duplicate_generation".into(),
            checked,
        };
    }
    if let Some(late) =
        superseded_by_later_event(node_events, &ident.node, &receipt.written_at, Some(&own))
    {
        return RevalidationResult {
            ok: false,
            reason: late,
            checked,
        };
    }
    RevalidationResult {
        ok: true,
        reason: String::new(),
        checked,
    }
}

/// Read + dedup the node's events across one or more journals: the
/// coverage-aware reader's event view, kinds-filtered to the claim-lifecycle
/// kinds. Each journal's `.ephemeral` sibling is read too; a missing file
/// reads as empty.
pub fn read_node_events(paths: &[std::path::PathBuf]) -> Vec<Value> {
    let kinds = [
        "delegated",
        "handoff_failed",
        "termination",
        "session_satisfied",
        "loop_check",
        "claim_acquired",
        "claim_idempotent_reacquired",
        "claim_stale_reclaimed",
        "claim_released",
    ];
    let mut journals: Vec<std::path::PathBuf> = Vec::new();
    for p in paths {
        journals.push(p.to_path_buf());
        // journal_and_ephemeral_sibling: <name>.ephemeral beside the journal,
        // where the claim lifecycle kinds live since retention routing.
        let mut os = p.as_os_str().to_os_string();
        os.push(".ephemeral");
        journals.push(std::path::PathBuf::from(os));
    }
    let mut events: Vec<Value> = Vec::new();
    for path in &journals {
        let Ok(text) = std::fs::read_to_string(path) else {
            continue;
        };
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            let Ok(e) = serde_json::from_str::<Value>(line) else {
                continue;
            };
            if !e.is_object() {
                continue;
            }
            if let Some(kind) = event_kind_reader(&e) {
                if kinds.contains(&kind) {
                    events.push(e);
                }
            }
        }
    }
    events
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resume_receipt::model::{load_receipt_bytes, ReceiptError};
    use serde_json::json;

    fn ev(ts: &str, kind: &str, node: &str) -> Value {
        json!({"ts": ts, "type": kind, "node_id": node, "data": {}})
    }

    #[test]
    fn duplicates_across_journals_fold_to_one_observation() {
        let a = vec![ev("2026-10-24T10:00:00Z", "delegated", "x-1")];
        let b = vec![ev("2026-10-24T10:00:00Z", "delegated", "x-1")];
        let mut both = a;
        both.extend(b);
        let canon = canonicalize_node_events(&both);
        assert_eq!(canon.len(), 1);
    }

    #[test]
    fn unparsed_timestamps_sort_last_and_never_supersede() {
        let undated = json!({"type": "termination", "node_id": "x-1"});
        let dated = vec![
            undated.clone(),
            ev("2026-10-24T10:00:00Z", "delegated", "x-1"),
        ];
        let canon = canonicalize_node_events(&dated);
        // The fold's ascending key puts dated observations first; the undated
        // one lands last, which is where "latest" reads from.
        assert_eq!(event_kind(&canon[0]), Some("delegated"));
        assert_eq!(event_kind(&canon[canon.len() - 1]), Some("termination"));
        // And an undated late event never supersedes a dated receipt.
        let receipt_ts = "2026-10-23T00:00:00Z";
        assert_eq!(
            superseded_by_later_event(&[undated], "x-1", receipt_ts, None),
            None
        );
    }

    #[test]
    fn own_session_continuation_never_supersedes() {
        let events = vec![json!({
            "ts": "2026-10-25T10:00:00Z",
            "type": "delegated",
            "node_id": "x-1",
            "data": {"from_session": "s1", "node_id": "x-1", "generation": 1},
        })];
        assert_eq!(
            superseded_by_later_event(&events, "x-1", "2026-10-24T00:00:00Z", Some("s1")),
            None
        );
        assert_eq!(
            superseded_by_later_event(&events, "x-1", "2026-10-24T00:00:00Z", Some("s2")),
            Some("superseded_by_later_event:delegated".to_string())
        );
    }

    #[test]
    fn duplicate_generation_refuses_a_foreign_mint() {
        let events = vec![json!({
            "ts": "2026-10-24T10:00:00Z",
            "type": "delegated",
            "data": {"node_id": "x-1", "harness": "claude", "generation": 2, "from_session": "other"},
        })];
        assert!(detect_duplicate_generation(
            &events,
            "x-1",
            Some("claude"),
            2,
            "mine"
        ));
        // Own-session delegated events do not trip it.
        let own = vec![json!({
            "ts": "2026-10-24T10:00:00Z",
            "type": "delegated",
            "data": {"node_id": "x-1", "harness": "claude", "generation": 2, "from_session": "mine"},
        })];
        assert!(!detect_duplicate_generation(
            &own,
            "x-1",
            Some("claude"),
            2,
            "mine"
        ));
    }

    #[test]
    fn the_gate_fails_closed_on_every_axis() {
        let receipt = super::super::model::build_receipt(
            "x-1",
            "s1",
            "do",
            1,
            "r",
            "/definitely/not/a/worktree",
            "b",
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "cmd",
            None,
            "2026-10-24T00:00:00Z",
            vec![],
            vec![],
            vec![],
            vec![],
            vec![],
            vec![],
            None,
        )
        .unwrap();
        let events: Vec<Value> = Vec::new();
        // dead worktree
        let res = revalidate(&receipt, "", "", false, None, None, &events, None);
        assert_eq!(res.reason, "dead_worktree");
        // foreign claim (worktree exists=false still outranks; test the arm
        // directly with an existing path)
        let res = revalidate(
            &receipt,
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "b",
            true,
            Some("someone-else"),
            None,
            &events,
            None,
        );
        assert_eq!(res.reason, "foreign_claim");
        // stale head
        let res = revalidate(
            &receipt,
            "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            "b",
            true,
            None,
            None,
            &events,
            None,
        );
        assert_eq!(res.reason, "stale_head");
        // stale branch
        let res = revalidate(
            &receipt,
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "other",
            true,
            None,
            None,
            &events,
            None,
        );
        assert_eq!(res.reason, "stale_branch");
        // ok
        let res = revalidate(
            &receipt,
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "b",
            true,
            None,
            None,
            &events,
            None,
        );
        assert!(res.ok);
    }

    #[test]
    fn load_helpers_stay_reachable() {
        // The model's byte-parse answers for a corrupt body even from here.
        let err = load_receipt_bytes("{not json", "<t>").unwrap_err();
        assert!(matches!(err, ReceiptError::Malformed(m) if m.contains("not valid JSON")));
    }
}
