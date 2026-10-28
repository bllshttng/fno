//! The context-to-outcome trace, ported from `cli/src/fno/scoreboard/fold.py`
//! (`build_context_outcome_trace` and its helpers). Joins canonical
//! observations without persisting a second authority.

use super::readers::{canonical_signature, event_data, event_kind, num_opt, parse_ts};
use serde_json::{json, Value};
use std::collections::BTreeSet;

/// `CONTEXT_TRACE_EVENT_KINDS`: the kinds the trace reads from the journals.
pub(crate) const CONTEXT_TRACE_EVENT_KINDS: [&str; 23] = [
    "claim_acquired",
    "claim_force_overridden",
    "claim_idempotent_reacquired",
    "claim_refreshed",
    "claim_released",
    "claim_stale_reclaimed",
    "context_comparison_declared",
    "context_snapshot",
    "delegated",
    "handoff_failed",
    "handoff_probe_unreadable",
    "loop_check",
    "recovery_capped",
    "recovery_close_notify",
    "recovery_nudge",
    "recovery_skipped",
    "review_attestation",
    "review_finding",
    "review_finding_resolved",
    "run_summary",
    "session_satisfied",
    "termination",
    "transcript_audit_failed",
];

/// `normalize_event` (fno.events.log): project canonical and legacy
/// envelopes into one read-only join shape; `raw` rides along.
pub(crate) fn normalize_event(event: &Value) -> Value {
    let canonical = event.get("data").filter(|d| d.is_object());
    let legacy = event.get("payload").filter(|p| p.is_object());
    let data = canonical.or(legacy).cloned().unwrap_or_else(|| json!({}));
    let data_str = |key: &str| data.get(key).and_then(Value::as_str).map(str::to_string);
    let session_id = event
        .get("session_id")
        .and_then(Value::as_str)
        .map(str::to_string)
        .or_else(|| data_str("session_id"));
    let holder = data.get("holder").and_then(Value::as_str);
    let holder_session_id = holder.and_then(|h| {
        h.strip_prefix("target-session:")
            .map(|tail| tail.to_string())
    });
    let mut node_id = ["node_id", "graph_node_id"]
        .iter()
        .find_map(|k| event.get(k).and_then(Value::as_str))
        .map(str::to_string)
        .or_else(|| data_str("node_id"))
        .or_else(|| data_str("graph_node_id"))
        .or_else(|| data_str("node"));
    if node_id.is_none() {
        node_id = data
            .get("key")
            .and_then(Value::as_str)
            .and_then(|k| k.strip_prefix("node:"))
            .map(str::to_string);
    }
    let pr_number = event
        .get("pr_number")
        .or_else(|| data.get("pr_number"))
        .and_then(pr_number_as_i64);
    json!({
        "type": event.get("type").or_else(|| event.get("kind")),
        "ts": event.get("ts").or_else(|| event.get("timestamp")),
        "source": event.get("source"),
        "session_id": session_id,
        "holder_session_id": holder_session_id,
        "node_id": node_id,
        "pr_number": pr_number,
        "short_id": event
            .get("short_id")
            .or_else(|| data.get("short_id")),
        "head_sha": event.get("head_sha").or_else(|| data.get("head_sha")),
        "data": data,
        "raw": event,
    })
}

/// Python `int()` semantics for a `pr_number` field: digits-only strings
/// convert, everything else stays.
fn pr_number_as_i64(value: &Value) -> Option<i64> {
    match value {
        Value::Number(n) => n.as_i64(),
        Value::String(s) if !s.is_empty() && s.chars().all(|c| c.is_ascii_digit()) => {
            s.parse().ok()
        }
        _ => None,
    }
}

/// `_event_sort_key` over the normalized shape: (dated, ts or min,
/// complete-context, raw signature).
pub(crate) fn normalized_sort_key(event: &Value) -> (bool, i64, bool, String) {
    let parsed = parse_ts(event.get("ts"));
    let signature_source = event.get("raw").filter(|r| r.is_object()).unwrap_or(event);
    let signature = canonical_signature(signature_source);
    let complete_context = event_kind(event) == Some("context_snapshot")
        && event
            .get("data")
            .and_then(|d| d.get("measurement_complete"))
            == Some(&Value::Bool(true));
    let ts = parsed.map(|dt| dt.timestamp_millis()).unwrap_or(i64::MIN);
    (parsed.is_some(), ts, complete_context, signature)
}

pub(crate) fn normalized_events(events: &[Value]) -> Vec<Value> {
    let mut out: Vec<Value> = events
        .iter()
        .filter(|e| e.is_object())
        .map(normalize_event)
        .collect();
    out.sort_by(|a, b| normalized_sort_key(a).cmp(&normalized_sort_key(b)));
    out
}

/// `_row_session_ids` on a delivery row.
fn delivery_session_ids(delivery: &Value) -> BTreeSet<String> {
    let mut ids = BTreeSet::new();
    if let Some(list) = delivery.get("sessions").and_then(Value::as_array) {
        for s in list {
            if let Some(s) = s.as_str().filter(|s| !s.is_empty()) {
                ids.insert(s.to_string());
            }
        }
    }
    if let Some(scalar) = delivery.get("session_id").and_then(Value::as_str) {
        if !scalar.is_empty() {
            ids.insert(scalar.to_string());
        }
    }
    ids
}

/// `_event_matches`: the normalized event joins the delivery on any one
/// identity, with the guards that keep a half-matching event out.
pub(crate) fn event_matches(delivery: &Value, event: &Value, commit: Option<&str>) -> bool {
    let session_ids = delivery_session_ids(delivery);
    let node_id = delivery.get("graph_node_id").and_then(Value::as_str);
    let pr_number = delivery.get("pr_number");
    let mut matched = false;
    if let Some(sid) = event.get("session_id").and_then(Value::as_str) {
        if !session_ids.is_empty() && !session_ids.contains(sid) {
            return false;
        }
        matched |= session_ids.contains(sid);
    }
    if let Some(holder) = event.get("holder_session_id").and_then(Value::as_str) {
        if !session_ids.is_empty() && !session_ids.contains(holder) {
            return false;
        }
        matched |= session_ids.contains(holder);
    }
    if let Some(short) = event.get("short_id").and_then(Value::as_str) {
        let short = short.to_string();
        let short_matches = session_ids
            .iter()
            .any(|session| session == &short || session.starts_with(&short));
        if !session_ids.is_empty() && !short_matches {
            return false;
        }
        matched |= short_matches;
    }
    // normalize_event always materializes these keys (null when absent);
    // Python truthiness reads a null or empty value the same as an absent
    // key, so the guards below must filter both or every node-less event
    // would fail the delivery's node guard.
    if let Some(event_node) = event
        .get("node_id")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
    {
        if node_id.map(|n| !n.is_empty()).unwrap_or(false) && Some(event_node) != node_id {
            return false;
        }
        matched |= node_id
            .filter(|n| !n.is_empty())
            .is_some_and(|n| n == event_node);
    }
    if !event.get("pr_number").map(Value::is_null).unwrap_or(true) {
        let pr_matches = match (pr_number, event.get("pr_number").and_then(Value::as_i64)) {
            (Some(p), Some(event_pr)) => p.as_i64() == Some(event_pr),
            _ => false,
        };
        if pr_number.map(|p| !p.is_null()).unwrap_or(false) && !pr_matches {
            return false;
        }
        matched |= pr_matches;
    }
    if let Some(head) = event
        .get("head_sha")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
    {
        let head_matches = commit.map(|c| !c.is_empty() && head == c).unwrap_or(false);
        if commit.map(|c| !c.is_empty()).unwrap_or(false) && !head_matches {
            return false;
        }
        matched |= head_matches;
    }
    matched
}

/// `_context_snapshot_rows`: the LAST valid context_snapshot event's row,
/// keyed exactly as the golden froze (harness, entry_state, bytes,
/// estimated_tokens, context_hash, source_hashes, measurement_complete,
/// measurement_errors).
pub(crate) fn context_snapshot_rows(events: &[&Value]) -> Option<Value> {
    for event in events.iter().rev() {
        if event_kind(event) != Some("context_snapshot") {
            continue;
        }
        let data = event_data(event);
        let compiled = data
            .get("compiled")
            .filter(|c| c.is_object())
            .cloned()
            .unwrap_or_default();
        let packet = compiled
            .get("packet")
            .filter(|p| p.is_object())
            .cloned()
            .unwrap_or_default();
        let manifest: Vec<Value> = data
            .get("source_manifest")
            .filter(|m| m.is_array())
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let source_hashes: Vec<String> = match data.get("source_hashes").filter(|h| h.is_array()) {
            Some(h) => h
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(Value::as_str)
                        .map(str::to_string)
                        .collect()
                })
                .unwrap_or_default(),
            None => manifest
                .iter()
                .filter_map(|item| item.get("content_hash").and_then(Value::as_str))
                .map(str::to_string)
                .collect(),
        };
        let context_bytes = data
            .get("context_bytes")
            .filter(|v| !v.is_null())
            .or_else(|| packet.get("bytes").filter(|v| !v.is_null()));
        let mut estimated = data
            .get("estimated_tokens")
            .filter(|v| !v.is_null())
            .or_else(|| packet.get("estimated_tokens").filter(|v| !v.is_null()))
            .cloned();
        if estimated.is_none() {
            if let Some(cb) = context_bytes.and_then(Value::as_f64) {
                estimated = Some(json!(((cb as i64) + 3) / 4));
            }
        }
        let mut context_hash = data.get("context_hash").filter(|v| !v.is_null()).cloned();
        if context_hash.is_none() {
            context_hash = compiled
                .get("kernel")
                .and_then(|k| k.get("content_hash"))
                .cloned();
        }
        let measurement_errors: Vec<String> =
            match data.get("measurement_errors").filter(|e| e.is_array()) {
                Some(e) => e
                    .as_array()
                    .map(|a| {
                        a.iter()
                            .filter_map(Value::as_str)
                            .map(str::to_string)
                            .collect::<Vec<String>>()
                    })
                    .unwrap_or_default(),
                None => manifest
                    .iter()
                    .filter_map(|item| {
                        let bad = item
                            .get("status")
                            .and_then(Value::as_str)
                            .map(|s| s != "observed" && s != "reachable")
                            .unwrap_or(false);
                        let source_id = item
                            .get("source_id")
                            .and_then(Value::as_str)
                            .unwrap_or("unknown");
                        bad.then(|| source_id.to_string())
                    })
                    .collect(),
            };
        let measurement_complete = data
            .get("measurement_complete")
            .and_then(Value::as_bool)
            .unwrap_or_else(|| measurement_errors.is_empty());
        return Some(json!({
            "harness": data.get("harness"),
            "entry_state": data.get("entry_state"),
            "bytes": context_bytes,
            "estimated_tokens": estimated,
            "context_hash": context_hash,
            "source_hashes": source_hashes,
            "measurement_complete": measurement_complete,
            "measurement_errors": measurement_errors,
        }));
    }
    None
}

/// `_commit_from_loop_check`: the latest loop_check's fingerprint prefix.
pub(crate) fn commit_from_loop_check(events: &[&Value]) -> Option<String> {
    for event in events.iter().rev() {
        if event_kind(event) != Some("loop_check") {
            continue;
        }
        let fingerprint = event
            .get("data")
            .and_then(|d| d.get("fingerprint"))
            .and_then(Value::as_str);
        if let Some(fingerprint) = fingerprint {
            let commit = fingerprint.split('|').next().unwrap_or_default().trim();
            if !commit.is_empty() {
                return Some(commit.to_string());
            }
        }
    }
    None
}

fn str_field<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value
        .get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
}

/// `build_context_outcome_trace`: join canonical observations for one
/// delivery row without persisting a second authority. Key order mirrors
/// the Python literal, which the golden froze.
pub(crate) fn build_context_outcome_trace(
    delivery: &Value,
    graph_node: Option<&Value>,
    events: &[Value],
) -> Value {
    let empty_node = json!({});
    let graph_node = graph_node.unwrap_or(&empty_node);
    let normalized = normalized_events(events);
    let identity_matching: Vec<&Value> = normalized
        .iter()
        .filter(|e| event_matches(delivery, e, None))
        .collect();
    let delivered_commit: Option<String> = ["commit_sha", "head_sha", "sha"]
        .iter()
        .find_map(|key| str_field(delivery, key))
        .map(str::to_string)
        .or_else(|| commit_from_loop_check(&identity_matching));
    let matching: Vec<&Value> = normalized
        .iter()
        .filter(|e| event_matches(delivery, e, delivered_commit.as_deref()))
        .collect();
    let completed_at = parse_ts(
        delivery
            .get("completed")
            .filter(|v| !v.is_null())
            .or_else(|| delivery.get("completed_at").filter(|v| !v.is_null())),
    );
    let context_events: Vec<&Value> = matching
        .iter()
        .filter(|e| {
            event_kind(e) == Some("context_snapshot")
                && completed_at.map_or(true, |done| {
                    parse_ts(e.get("ts").or_else(|| e.get("timestamp")))
                        .map(|at| at <= done)
                        .unwrap_or(false)
                })
        })
        .copied()
        .collect();
    let context = context_snapshot_rows(&context_events);
    let loop_checks: Vec<&Value> = matching
        .iter()
        .filter(|e| event_kind(e) == Some("loop_check"))
        .copied()
        .collect();
    let latest_loop = loop_checks.last();
    let loop_data = latest_loop.copied().map(event_data).unwrap_or_default();
    let ci_state = loop_data.get("ci").cloned().unwrap_or(Value::Null);
    // Python: `ci_state not in (None, "", "none", "unknown")`.
    let ci_observed = !match ci_state {
        Value::Null => true,
        Value::String(ref s) => s.is_empty() || s == "none" || s == "unknown",
        _ => false,
    };
    let attestations = matching
        .iter()
        .filter(|e| event_kind(e) == Some("review_attestation"))
        .count() as i64;
    let findings = matching
        .iter()
        .filter(|e| event_kind(e) == Some("review_finding"))
        .count() as i64;
    let mut reviewed: Option<bool> = loop_data.get("reviewed").and_then(Value::as_bool);
    let review_observed = reviewed.is_some() || attestations > 0 || findings > 0;
    if reviewed.is_none() && attestations > 0 {
        reviewed = Some(matching.iter().any(|e| {
            event_kind(e) == Some("review_attestation")
                && event_data(e).get("verdict").and_then(Value::as_str) == Some("pass")
        }));
    }
    let merge_events: Vec<&Value> = matching
        .iter()
        .filter(|e| {
            (event_kind(e) == Some("session_satisfied")
                && event_data(e).get("source").and_then(Value::as_str) == Some("pr_merge"))
                || matches!(event_kind(e), Some("pr_merged") | Some("merged"))
        })
        .copied()
        .collect();
    let merged_at_node = graph_node.get("merged_at").cloned().unwrap_or(Value::Null);
    let mut merged_at = merged_at_node.clone();
    let merged = loop_data.get("pr_state").and_then(Value::as_str) == Some("MERGED")
        || !merge_events.is_empty()
        || !merged_at.is_null()
        || graph_node.get("merge_status").and_then(Value::as_str) == Some("merged");
    let merge_status = graph_node
        .get("merge_status")
        .cloned()
        .unwrap_or(Value::Null);
    let pr_state = loop_data.get("pr_state").and_then(Value::as_str);
    let merge_state: Value = if merged {
        json!("merged")
    } else if merge_status.is_string() && !merge_status.as_str().unwrap_or_default().is_empty() {
        merge_status.clone()
    } else if matches!(pr_state, Some("OPEN") | Some("CLOSED") | Some("MERGED")) {
        json!(pr_state.unwrap().to_lowercase())
    } else {
        Value::Null
    };
    let merge_observed = matches!(pr_state, Some("OPEN") | Some("CLOSED") | Some("MERGED"))
        || !merge_events.is_empty()
        || !merged_at.is_null()
        || graph_node.get("merge_status").and_then(Value::as_str) == Some("merged");
    if merged_at.is_null() && !merge_events.is_empty() {
        merged_at = merge_events
            .last()
            .and_then(|e| e.get("ts"))
            .cloned()
            .unwrap_or(Value::Null);
    }
    let revert_events: Vec<&Value> = matching
        .iter()
        .filter(|e| event_kind(e).map(|k| k.contains("revert")).unwrap_or(false))
        .copied()
        .collect();
    let reverted_at_node = graph_node
        .get("reverted_at")
        .cloned()
        .unwrap_or(Value::Null);
    let mut reverted_at = reverted_at_node.clone();
    if reverted_at.is_null() && !revert_events.is_empty() {
        reverted_at = revert_events
            .last()
            .and_then(|e| e.get("ts"))
            .cloned()
            .unwrap_or(Value::Null);
    }
    let reverted_value = graph_node.get("reverted").cloned().unwrap_or(Value::Null);
    let revert_observed =
        !revert_events.is_empty() || !reverted_at.is_null() || reverted_value.is_boolean();
    let reverted =
        !revert_events.is_empty() || reverted_value == Value::Bool(true) || !reverted_at.is_null();
    let recovery: Vec<Value> = matching
        .iter()
        .filter(|e| {
            event_kind(e)
                .map(|k| {
                    k.starts_with("recovery_") || k.starts_with("handoff_") || k == "delegated"
                })
                .unwrap_or(false)
        })
        .map(|e| {
            json!({
                "type": event_kind(e),
                "at": e.get("ts"),
                "reason": event_data(e).get("reason"),
            })
        })
        .collect();
    let claim_events: Vec<&Value> = matching
        .iter()
        .filter(|e| {
            event_kind(e)
                .map(|k| k.starts_with("claim_"))
                .unwrap_or(false)
        })
        .copied()
        .collect();
    let claim = json!({
        "observed": !claim_events.is_empty(),
        "events": claim_events.iter().map(|e| {
            json!({
                "type": event_kind(e),
                "at": e.get("ts"),
                "key": event_data(e).get("key"),
                "holder": event_data(e).get("holder"),
            })
        }).collect::<Vec<_>>(),
    });
    let latency = num_opt(delivery.get("duration_minutes"));
    let spend = num_opt(delivery.get("cost_usd"));
    let falsifiable = ci_observed
        || review_observed
        || merge_observed
        || revert_observed
        || !recovery.is_empty()
        || latency.is_some()
        || spend.is_some();
    let mut missing: Vec<&str> = Vec::new();
    if context.is_none() {
        missing.push("context_snapshot");
    }
    if !falsifiable {
        missing.push("downstream_outcome");
    }
    let receipt = [
        "evidence_receipt",
        "verification_receipt",
        "preflight_receipt",
    ]
    .iter()
    .find_map(|key| delivery.get(*key).filter(|v| !v.is_null()))
    .cloned()
    .unwrap_or(Value::Null);
    let pr_number_raw = delivery.get("pr_number").cloned().unwrap_or(Value::Null);
    let pr_number = match pr_number_raw {
        Value::String(ref s) if !s.is_empty() && s.chars().all(|c| c.is_ascii_digit()) => {
            json!(s.parse::<i64>().unwrap_or(0))
        }
        other => other,
    };
    let mut provenance: Vec<&str> = vec!["ledger"];
    if !graph_node.as_object().map(|o| o.is_empty()).unwrap_or(true) {
        provenance.push("graph");
    }
    if !matching.is_empty() {
        provenance.push("events");
    }
    json!({
        "objective": graph_node.get("title")
            .or_else(|| delivery.get("objective"))
            .or_else(|| delivery.get("title")),
        "plan_node": delivery.get("graph_node_id").or_else(|| graph_node.get("id")),
        "plan_path": delivery.get("plan_path"),
        "worker_session": delivery.get("session_id"),
        "claim": claim,
        "commit": delivered_commit,
        "evidence_receipt": receipt,
        "pr": {"number": pr_number, "url": delivery.get("pr_url")},
        "context": context,
        "outcomes": {
            "ci": {
                "state": ci_state,
                "observed_at": if ci_observed { latest_loop.and_then(|e| e.get("ts")) } else { None },
            },
            "review": {
                "reviewed": reviewed,
                "attestations": attestations,
                "findings": findings,
                "observed": review_observed,
            },
            "merge": {
                "observed": merge_observed,
                "state": merge_state,
                "merged": if merge_observed { json!(merged) } else { Value::Null },
                "at": merged_at,
            },
            "revert": {
                "observed": revert_observed,
                "reverted": if revert_observed { json!(reverted) } else { Value::Null },
                "at": reverted_at,
            },
            "recovery": recovery,
            "latency_minutes": latency,
            "spend_usd": spend,
        },
        "completed_at": delivery.get("completed"),
        "falsifiable": falsifiable,
        "missing": missing,
        "provenance": provenance,
    })
}
