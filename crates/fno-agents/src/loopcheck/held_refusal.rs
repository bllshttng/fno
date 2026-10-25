//! The undispatched-node hold: a promise the spawn gate refused.
//!
//! A run that cannot dispatch (lead share, provider cap) and cannot do the
//! work itself sits in a deadlock: the stop gate blocks on the node reading
//! undispatched (no PR) while every dispatch attempt is refused, so the run
//! can neither act nor stop. The refusal event the gate already writes
//! (`agent_spawn_refused`, carrying the node and its exit code) is the held
//! state. When the freshest cap refusal for this node is inside the window,
//! the promise block classifies as HELD: the first fire blocks once naming
//! the hold and its remedies, and the second fire on the same hold
//! terminates `HeldOnQuestion` with the held help, so a run at its share
//! stops instead of re-waking into the same refusal until budget.
//!
//! The rule is deliberately tight on both edges: only a refusal NAMED FOR
//! THIS NODE can hold it (rows predating the node field match nothing), and
//! only a fresh one (an unrelated old refusal never holds a run that moved
//! on). The block-once marker rides the fire's own `loop_check` row, so the
//! held state is the journal itself, never the fingerprint.

use serde_json::Value;
use std::path::PathBuf;

/// How far back a refusal still reads as the cause of the current block.
/// One dispatch attempt writes one row, so a live deadlock keeps a fresh row
/// on every retry; a hold the session has ridden out past this window
/// without a newer refusal is no longer the live blocker.
pub(crate) const HOLD_WINDOW_SECS: i64 = 1800;

/// The exit codes the spawn gate refuses a dispatch with on a share or
/// provider cap: EXIT_PROVIDER_CAP and EXIT_LEAD_SHARE. Both are
/// refusal-specific, so the code plus the axis word in the verdict line is
/// the whole match.
const CAP_EXIT_CODES: [i64; 2] = [
    crate::spawn_gate::EXIT_PROVIDER_CAP as i64,
    crate::spawn_gate::EXIT_LEAD_SHARE as i64,
];

/// One fresh cap refusal for a node.
pub(crate) struct FreshRefusal {
    /// The axis the verdict named: "lead_share" or "provider_cap".
    pub(crate) axis: String,
    /// Seconds old at this fire.
    pub(crate) age_secs: i64,
}

/// The refusal rows this gate reads: cap refusals only, and only ones the
/// gate addressed to this node. The axis word rides the verdict line the
/// writer stamped; the exit code pins it, so a reworded prose line cannot
/// manufacture a hold and a renamed axis cannot hide one.
fn cap_refusal_for(line: &Value, node_id: &str) -> Option<String> {
    if line.pointer("/data/node").and_then(Value::as_str) != Some(node_id) {
        return None;
    }
    let exit = line.pointer("/data/exit_code").and_then(Value::as_i64)?;
    if !CAP_EXIT_CODES.contains(&exit) {
        return None;
    }
    let reason = line.pointer("/data/reason").and_then(Value::as_str)?;
    for axis in ["lead_share", "provider_cap"] {
        if reason.contains(axis) {
            return Some(axis.to_string());
        }
    }
    None
}

/// The freshest cap refusal naming `node_id`, inside the window. The
/// journals fold as a union: the gate writes its row into the home journal,
/// which a stop gate reads as its global log only when no explicit
/// `--global-events` named something else, so one journal alone can miss
/// the row. An unreadable or absent journal contributes nothing (fail
/// open): the gate holds a run only on a row it can actually read.
pub(crate) fn fresh_refusal(
    journals: &[PathBuf],
    node_id: &str,
    now: chrono::DateTime<chrono::Utc>,
) -> Option<FreshRefusal> {
    let mut seen: Vec<PathBuf> = Vec::new();
    let mut best: Option<FreshRefusal> = None;
    for path in journals {
        if seen.contains(path) {
            continue;
        }
        seen.push(path.clone());
        let Ok(lines) = crate::loopcheck::event_lines_of(path, &["agent_spawn_refused"]) else {
            continue;
        };
        for line in lines {
            let Ok(v) = serde_json::from_str::<Value>(&line) else {
                continue;
            };
            let Some(axis) = cap_refusal_for(&v, node_id) else {
                continue;
            };
            let Some(ts) = v
                .get("ts")
                .and_then(Value::as_str)
                .and_then(|s| s.parse::<chrono::DateTime<chrono::Utc>>().ok())
            else {
                continue;
            };
            let age_secs = (now - ts).num_seconds();
            if age_secs < 0 || age_secs > HOLD_WINDOW_SECS {
                continue;
            }
            match &best {
                Some(prev) if prev.age_secs <= age_secs => {}
                _ => {
                    best = Some(FreshRefusal { axis, age_secs });
                }
            }
        }
    }
    best
}

/// Whether this session already wrote its one held block for this node,
/// read off the loop_check rows the fire already loaded (no second store
/// read). The marker is the block row's own `held_refusal` field, so the
/// held state is the journal itself, never the fingerprint.
pub(crate) fn prior_held_block(rows: Option<&str>, session_id: &str, node_id: &str) -> bool {
    let Some(content) = rows else {
        return false;
    };
    content
        .lines()
        .rev()
        .filter(|l| l.contains(session_id))
        .any(|line| {
            let Ok(v) = serde_json::from_str::<Value>(line) else {
                return false;
            };
            v.get("type").and_then(Value::as_str) == Some("loop_check")
                && v.pointer("/data/session_id").and_then(Value::as_str) == Some(session_id)
                && v.pointer("/data/decision").and_then(Value::as_str) == Some("block")
                && v.pointer("/data/held_refusal/node").and_then(Value::as_str) == Some(node_id)
        })
}

/// What the held-refusal gate decided about this fire.
pub(crate) enum HeldRefusalStop {
    /// No hold: fall through to the ordinary block.
    None,
    /// First fire on this hold: block once, naming the refusal and the
    /// clears. `marker` rides the fire's loop_check row for the next fire.
    Block { reason: String, marker: Value },
    /// The hold already blocked once and still binds: the loop ends.
    Terminate { message: String },
}

/// The gate over one fire: fold the freshest cap refusal for the node, then
/// decide by the marker. Everything is decided here so the caller only
/// renders the verdict it is handed.
pub(crate) fn held_gate(
    journals: &[PathBuf],
    fire_rows: Option<&str>,
    session_id: &str,
    node_id: &str,
    now: chrono::DateTime<chrono::Utc>,
) -> HeldRefusalStop {
    let Some(refusal) = fresh_refusal(journals, node_id, now) else {
        return HeldRefusalStop::None;
    };
    let axis = refusal.axis.as_str();
    let age = refusal.age_secs;
    if prior_held_block(fire_rows, session_id, node_id) {
        return HeldRefusalStop::Terminate {
            message: format!(
                "still held: node {node_id} reads undispatched and every dispatch is refused \
                 on {axis} (refused {age}s ago), so the run can neither act nor stop. \
                 The loop ends here. Free share seats (stop workers this lead holds), \
                 or dispatch with --force if the operator's policy allows it; a later run \
                 ships when a dispatch is admitted."
            ),
        };
    }
    HeldRefusalStop::Block {
        reason: format!(
            "held: the spawn gate refused to dispatch node {node_id} on {axis} \
             (refused {age}s ago), and the node reads undispatched, so this run can \
             neither act nor stop. Free share seats (stop workers this lead holds), \
             or dispatch with --force if the operator's policy allows it. Ending the \
             turn again on this hold ends the loop."
        ),
        marker: serde_json::json!({
            "node": node_id,
            "axis": axis,
            "refused_age_secs": age,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn refusal_row(node: &str, exit: i64, reason: &str, ts: &str) -> String {
        format!(
            r#"{{"ts":"{ts}","type":"agent_spawn_refused","source":"daemon","data":{{"argv":["spawn"],"exit_code":{exit},"node":"{node}","reason":"{reason}"}}}}"#
        )
    }

    fn loop_check_block(node: &str, session: &str) -> String {
        format!(
            r#"{{"ts":"2026-10-24T12:00:00Z","type":"loop_check","source":"hook","data":{{"session_id":"{session}","fingerprint":"fp","fires":1,"consecutive_unchanged":1,"decision":"block","intent":"promise","pr_state":"none","ci":"none","reviewed":false,"held_refusal":{{"node":"{node}","axis":"lead_share","refused_age_secs":5}}}}}}"#
        )
    }

    fn write_rows(path: &std::path::Path, rows: &[String]) {
        for row in rows {
            crate::event_store::append_envelope(path, row, None).unwrap();
        }
    }

    #[test]
    fn a_fresh_cap_refusal_blocks_once_then_terminates_through_the_real_writer() {
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().join("project-events.jsonl");
        let global = dir.path().join("global-events.jsonl");
        write_rows(
            &global,
            &[refusal_row(
                "x-n",
                crate::spawn_gate::EXIT_LEAD_SHARE as i64,
                "spawn-gate: refused on lead_share (lead_share, exit 80): lead=abc12345, held=5",
                "2026-10-24T11:59:30Z",
            )],
        );
        let now: chrono::DateTime<chrono::Utc> = "2026-10-24T12:00:00Z".parse().unwrap();
        let journals = vec![project.clone(), global.clone()];

        let first = held_gate(&journals, None, "sess-a", "x-n", now);
        let HeldRefusalStop::Block { reason, marker } = first else {
            panic!("first fire on a fresh refusal must block once");
        };
        assert!(reason.contains("lead_share"), "{reason}");
        assert!(reason.contains("x-n"), "{reason}");
        assert!(reason.contains("ends the loop"), "{reason}");
        assert_eq!(marker.pointer("/node").and_then(Value::as_str), Some("x-n"));

        // The marker rides the session's own block row, exactly as the
        // production fire_row writes it.
        let rows = loop_check_block("x-n", "sess-a");
        write_rows(&project, &[rows]);

        let second = held_gate(
            &journals,
            Some(&crate::events::committed_journal_text(&project)),
            "sess-a",
            "x-n",
            now,
        );
        let HeldRefusalStop::Terminate { message } = second else {
            panic!("second fire on the same hold must terminate");
        };
        assert!(message.contains("still held"), "{message}");
        assert!(message.contains("lead_share"), "{message}");
    }

    #[test]
    fn a_refusal_holds_inside_the_window_and_not_past_it() {
        // The window is the whole freshness rule: the SAME refusal is the
        // live blocker inside HOLD_WINDOW_SECS and history past it, and a
        // provider_cap refusal classifies with its axis and age the same way
        // a lead_share one does.
        let dir = tempfile::tempdir().unwrap();
        let now: chrono::DateTime<chrono::Utc> = "2026-10-24T12:00:00Z".parse().unwrap();

        let global = dir.path().join("global-events.jsonl");
        write_rows(
            &global,
            &[refusal_row(
                "x-n",
                crate::spawn_gate::EXIT_LEAD_SHARE as i64,
                "spawn-gate: refused on lead_share (lead_share, exit 80): lead=abc12345, held=5",
                "2026-10-24T11:59:30Z",
            )],
        );
        let journals = vec![global.clone()];
        let fresh = fresh_refusal(&journals, "x-n", now).expect("fresh refusal must read back");
        assert_eq!(fresh.axis, "lead_share");
        assert_eq!(fresh.age_secs, 30);
        assert!(matches!(
            held_gate(&journals, None, "sess-a", "x-n", now),
            HeldRefusalStop::Block { .. }
        ));

        // The same refusal older than the window never holds a run that
        // moved on.
        let stale = dir.path().join("stale-events.jsonl");
        write_rows(
            &stale,
            &[refusal_row(
                "x-n",
                crate::spawn_gate::EXIT_LEAD_SHARE as i64,
                "spawn-gate: refused on lead_share (lead_share, exit 80): lead=abc12345, held=5",
                "2026-10-24T10:00:00Z",
            )],
        );
        let stale_journals = vec![stale.clone()];
        assert!(fresh_refusal(&stale_journals, "x-n", now).is_none());
        assert!(matches!(
            held_gate(&stale_journals, None, "sess-a", "x-n", now),
            HeldRefusalStop::None
        ));

        // A provider_cap refusal inside the window classifies with its axis.
        let cap = dir.path().join("cap-events.jsonl");
        write_rows(
            &cap,
            &[refusal_row(
                "x-n",
                crate::spawn_gate::EXIT_PROVIDER_CAP as i64,
                "spawn-gate: refused on provider_cap (provider_cap, exit 78): cap=2, count=2",
                "2026-10-24T11:59:00Z",
            )],
        );
        let cap_journals = vec![cap.clone()];
        let cap_refusal =
            fresh_refusal(&cap_journals, "x-n", now).expect("cap refusal must read back");
        assert_eq!(cap_refusal.axis, "provider_cap");
        assert_eq!(cap_refusal.age_secs, 60);
    }

    #[test]
    fn a_refusal_for_another_node_or_axis_holds_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let global = dir.path().join("global-events.jsonl");
        write_rows(
            &global,
            &[
                // Fresh, but names a different node.
                refusal_row(
                    "x-other",
                    crate::spawn_gate::EXIT_LEAD_SHARE as i64,
                    "spawn-gate: refused on lead_share (lead_share, exit 80): held=5",
                    "2026-10-24T11:59:50Z",
                ),
                // Fresh, and this node, but an axis the hold does not read
                // (the node was simply occupied).
                refusal_row(
                    "x-n",
                    92,
                    "spawn-gate: refused on node_occupied (node_occupied, exit 92)",
                    "2026-10-24T11:59:55Z",
                ),
                // Fresh, and this node, but a plain capacity refusal the
                // session can clear by waiting: never a hold.
                refusal_row(
                    "x-n",
                    crate::spawn_gate::EXIT_NO_WAIT as i64,
                    "spawn-gate: refused on no_wait (no_wait_mutex_held, exit 76)",
                    "2026-10-24T11:59:58Z",
                ),
            ],
        );
        let now: chrono::DateTime<chrono::Utc> = "2026-10-24T12:00:00Z".parse().unwrap();
        let journals = vec![global.clone()];
        assert!(fresh_refusal(&journals, "x-n", now).is_none());
        assert!(matches!(
            held_gate(&journals, None, "sess-a", "x-n", now),
            HeldRefusalStop::None
        ));
    }

    #[test]
    fn an_unreadable_journal_fails_open() {
        let now: chrono::DateTime<chrono::Utc> = "2026-10-24T12:00:00Z".parse().unwrap();
        let missing = std::env::temp_dir().join(format!(
            "held-refusal-missing-{}-{}.jsonl",
            std::process::id(),
            now.timestamp_millis()
        ));
        assert!(fresh_refusal(&[missing], "x-n", now).is_none());
    }

    #[test]
    fn only_a_block_row_marks_the_hold() {
        // The terminate fire writes an ALLOW row carrying the same
        // held_refusal field (telemetry). It must never read as the
        // block-once marker: the marker is the first fire's block, and an
        // allow means the hold already ended the loop.
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().join("project-events.jsonl");
        let allow_row = loop_check_block("x-n", "sess-a")
            .replace(r#""decision":"block""#, r#""decision":"allow""#);
        write_rows(&project, &[allow_row]);
        assert!(!prior_held_block(
            Some(&crate::events::committed_journal_text(&project)),
            "sess-a",
            "x-n"
        ));
    }
}
