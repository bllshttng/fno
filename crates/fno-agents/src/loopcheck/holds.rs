//! The question-family folds for the stop gate: which open questions hold
//! this node or this session, and which decided questions lack a decision
//! record.
//!
//! A node HELD on an open question must stop once, name the question, and
//! never re-ask on a later fire: the first fire emits a
//! `loop_check_question_gate` row with `gate: held_on_question`, and the
//! second fire (the same question still open) terminates `HeldOnQuestion`
//! with a digest naming the user-side clear. Only the user closes the
//! question, so neither text names the decide verb. The held state is the
//! journal itself, never the fingerprint.

use serde_json::Value;
use std::path::PathBuf;

/// The event types the stop gate's question family reads. `event_lines_of`
/// narrows the journal to this list, so a family type missing here is
/// silently skipped: the folds never see its rows and the gate cannot fire
/// on them. One list serves both folds; each `match` handles only the kinds
/// it folds and skips the rest. The guard test below fails when the schema
/// gains a question, decision or hold type this list lacks.
pub(crate) const QUESTION_FAMILY_TYPES: &[&str] = &[
    "operator_question",
    "operator_question_closed",
    "operator_decision",
    "loop_check_question_gate",
];

/// A question THIS session asked, that was closed WITH an answer, and for
/// which no `operator_decision` event exists on any reachable journal. The
/// stop gate holds the session until the decision is recorded, because a
/// ruling that dies with the transcript is the failure the decision record
/// exists to prevent.
pub(crate) struct UnrecordedDecision {
    pub(crate) question_id: String,
    pub(crate) question: String,
}

/// Fold the question/decision family across a UNION of journals.
///
/// A question can be asked and closed in one journal while the decision lands
/// in another (the operator verbs write to the canonical root's journal; a
/// worktree stop gate reads its own cwd's), so membership is only decidable
/// after every journal is folded - checking per-file would hold a session
/// whose record sits one path away.
///
/// An unreadable or absent journal contributes nothing (fail open): this gate
/// scans for an OBLIGATION contracted elsewhere, and a missing journal means
/// no obligation is visible, not that one was breached. The substring
/// prefilter mirrors the Python reader: the journals are shared, append-only,
/// and never rotated, so parsing every line costs more than the scan.
pub(crate) fn scan_unrecorded_decisions(
    journals: &[PathBuf],
    session_id: &str,
) -> Vec<UnrecordedDecision> {
    let mut asked: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    let mut closed_with_answer: std::collections::HashSet<String> =
        std::collections::HashSet::new();
    let mut recorded: std::collections::HashSet<String> = std::collections::HashSet::new();
    for path in journals {
        let Ok(content) =
            crate::loopcheck::event_lines_of(path, QUESTION_FAMILY_TYPES).map(|l| l.join("\n"))
        else {
            continue;
        }; // committed rows, commit order
        for line in content.lines() {
            if !(line.contains("operator_question") || line.contains("operator_decision")) {
                continue;
            }
            let Ok(val) = serde_json::from_str::<Value>(line) else {
                continue;
            };
            let kind = val.get("type").and_then(|v| v.as_str()).unwrap_or("");
            let data = val
                .get("data")
                .cloned()
                .unwrap_or_else(|| serde_json::json!({}));
            match kind {
                "operator_question" => {
                    if data.get("session_id").and_then(|v| v.as_str()) == Some(session_id) {
                        if let Some(qid) = data.get("question_id").and_then(|v| v.as_str()) {
                            asked.insert(
                                qid.to_string(),
                                data.get("question")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("")
                                    .chars()
                                    .take(80)
                                    .collect(),
                            );
                        }
                    }
                }
                "operator_question_closed" => {
                    let answered = data
                        .get("answer")
                        .and_then(|v| v.as_str())
                        .map(|a| !a.trim().is_empty())
                        .unwrap_or(false);
                    if answered {
                        if let Some(qid) = data.get("question_id").and_then(|v| v.as_str()) {
                            closed_with_answer.insert(qid.to_string());
                        }
                    }
                }
                "operator_decision" => {
                    if let Some(qid) = data.get("question_id").and_then(|v| v.as_str()) {
                        recorded.insert(qid.to_string());
                    }
                }
                _ => {}
            }
        }
    }
    let mut out: Vec<UnrecordedDecision> = asked
        .into_iter()
        .filter(|(qid, _)| closed_with_answer.contains(qid) && !recorded.contains(qid))
        .map(|(question_id, question)| UnrecordedDecision {
            question_id,
            question,
        })
        .collect();
    out.sort_by(|a, b| a.question_id.cmp(&b.question_id));
    out
}

/// One open question holding this node or this session.
pub(crate) struct OpenHold {
    pub(crate) question_id: String,
    /// First line of the question, capped at 80 chars.
    pub(crate) question: String,
    pub(crate) options: Vec<String>,
    /// A prior fire of THIS session already emitted its one held_on_question
    /// block for this question: the next fire terminates instead of blocking.
    pub(crate) already_blocked: bool,
}

/// The open questions that hold this run: a question holds when it names
/// this node in `blocks`, or when this session asked it with a non-empty
/// `blocks`. A blocked node that `node_done` reads as finished no longer
/// holds anything: the question is mooted, so it is skipped rather than
/// left to wedge the asking session. Everything else a question row may
/// carry is out of scope here.
pub(crate) fn scan_open_holds(
    journals: &[PathBuf],
    session_id: &str,
    node_id: &str,
    node_done: &dyn Fn(&str) -> bool,
) -> Vec<OpenHold> {
    let mut asked: std::collections::HashMap<String, Value> = std::collections::HashMap::new();
    let mut closed: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut blocked_once: std::collections::HashSet<String> = std::collections::HashSet::new();
    for path in journals {
        let Ok(content) =
            crate::loopcheck::event_lines_of(path, QUESTION_FAMILY_TYPES).map(|l| l.join("\n"))
        else {
            continue;
        }; // committed rows, commit order
        for line in content.lines() {
            if !(line.contains("operator_question") || line.contains("held_on_question")) {
                continue;
            }
            let Ok(val) = serde_json::from_str::<Value>(line) else {
                continue;
            };
            let kind = val.get("type").and_then(|v| v.as_str()).unwrap_or("");
            let data = val
                .get("data")
                .cloned()
                .unwrap_or_else(|| serde_json::json!({}));
            match kind {
                "operator_question" => {
                    if let Some(qid) = data.get("question_id").and_then(|v| v.as_str()) {
                        asked.insert(qid.to_string(), data);
                    }
                }
                "operator_question_closed" => {
                    if let Some(qid) = data.get("question_id").and_then(|v| v.as_str()) {
                        closed.insert(qid.to_string());
                    }
                }
                // The first fire's own block row: gate held_on_question,
                // session-scoped, naming the questions it blocked on.
                "loop_check_question_gate" => {
                    if data.get("gate").and_then(|v| v.as_str()) == Some("held_on_question")
                        && data.get("session_id").and_then(|v| v.as_str()) == Some(session_id)
                    {
                        if let Some(held) = data.get("held").and_then(|v| v.as_array()) {
                            for qid in held {
                                if let Some(qid) = qid.as_str() {
                                    blocked_once.insert(qid.to_string());
                                }
                            }
                        }
                    }
                }
                _ => {}
            }
        }
    }
    let mut holds: Vec<OpenHold> = asked
        .into_iter()
        .filter(|(qid, data)| {
            if closed.contains(qid) {
                return false;
            }
            let blocks: Vec<String> = data
                .get("blocks")
                .and_then(|v| v.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(Value::as_str)
                        .map(str::to_string)
                        .collect()
                })
                .unwrap_or_default();
            let asked_by_session =
                data.get("session_id").and_then(|v| v.as_str()) == Some(session_id);
            (node_in(node_id, &blocks) && !node_done(node_id))
                || (asked_by_session && blocks.iter().any(|b| !node_done(b)))
        })
        .map(|(qid, data)| {
            let question = data
                .get("question")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .lines()
                .next()
                .unwrap_or("")
                .chars()
                .take(80)
                .collect();
            let options = data
                .get("options")
                .and_then(|v| v.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(Value::as_str)
                        .map(str::to_string)
                        .collect()
                })
                .unwrap_or_default();
            OpenHold {
                question_id: qid.clone(),
                question,
                options,
                already_blocked: blocked_once.contains(&qid),
            }
        })
        .collect();
    holds.sort_by(|a, b| a.question_id.cmp(&b.question_id));
    holds
}

/// A node whose graph row reads done or superseded. An unreadable store, a
/// missing row or any other status is NOT done, so a failed read leaves the
/// hold standing instead of silently releasing a session.
pub(crate) fn node_is_done(id: &str) -> bool {
    use crate::backlog::model::Status;
    let store = crate::backlog::api::Store::new(&crate::graph_get::default_graph_path());
    matches!(
        crate::backlog::api::node(&store, id),
        Ok(Some(n)) if matches!(n.status, Status::Done | Status::Superseded)
    )
}

fn node_in(node_id: &str, blocks: &[String]) -> bool {
    blocks.iter().any(|b| b == node_id)
}

/// What the question gates decided about this fire: fall through, block
/// once, or terminate the loop.
pub(crate) enum QuestionGateStop {
    None,
    Block {
        reason: String,
    },
    Terminate {
        reason: super::TerminationReason,
        message: String,
    },
}

/// The stop gate's question family, in order: a decided question left
/// without a decision record blocks (step 3b), then a node held on an open
/// question blocks once and terminates on the second fire (step 3c). The
/// journals fold as a UNION (operator verbs write to the canonical root's
/// journal; a worktree stop gate reads its own cwd's), and the gate's own
/// loop_check_question_gate/termination rows land through `emit`, so the
/// held state the second fire reads is the journal itself, never the
/// fingerprint.
pub(crate) fn question_gates(
    project_events: &std::path::Path,
    global_events: &std::path::Path,
    cwd: &std::path::Path,
    session_id: &str,
    node_id: &str,
    node_done: &dyn Fn(&str) -> bool,
    emit: &dyn Fn(&str, serde_json::Value),
) -> QuestionGateStop {
    let mut journals = vec![project_events.to_path_buf(), global_events.to_path_buf()];
    if let Some(canon) = crate::paths::canonical_repo_root(cwd) {
        let canonical_journal = crate::paths::events_path(&canon);
        if !journals.contains(&canonical_journal) {
            journals.push(canonical_journal);
        }
    }
    // Step 3b: a session that closed one of ITS OWN operator questions WITH
    // an answer but emitted no matching operator_decision event is held, and
    // the hold names the question. Scopes to questions this session asked so
    // a foreign session's unfinished business cannot wedge an unrelated loop.
    let unrecorded = scan_unrecorded_decisions(&journals, session_id);
    if !unrecorded.is_empty() {
        let names = unrecorded
            .iter()
            .map(|u| format!("{} '{}'", u.question_id, u.question))
            .collect::<Vec<_>>()
            .join(", ");
        emit(
            "loop_check_question_gate",
            serde_json::json!({
                "session_id": session_id,
                "gate": "unrecorded_decision",
                "unrecorded": unrecorded.iter().map(|u| u.question_id.clone()).collect::<Vec<_>>()
            }),
        );
        return QuestionGateStop::Block {
            reason: format!(
                "a decided question has no decision record ({names}); record it with \
                 `fno backlog decide <node> \"...\" --question-id <id>` \
                 (the gate matches on the question id; a re-run of the clear is a \
                 no-op once the question is closed) \
                 so the decision survives this session"
            ),
        };
    }
    // Step 3c: the first fire on a held node blocks ONCE, naming the question
    // id and the user-side remedies; the second fire on the same still-open
    // question terminates, so a held node stops once and is never re-asked.
    let held = scan_open_holds(&journals, session_id, node_id, node_done);
    if !held.is_empty() {
        let qids = held
            .iter()
            .map(|h| h.question_id.clone())
            .collect::<Vec<_>>();
        let first_fire = held.iter().any(|h| !h.already_blocked);
        let h = &held[0];
        if first_fire {
            emit(
                "loop_check_question_gate",
                serde_json::json!({
                    "session_id": session_id,
                    "gate": "held_on_question",
                    "held": qids,
                }),
            );
            return QuestionGateStop::Block {
                reason: format!(
                    "held on open question {qid}: '{}'. Only the user closes it: on the \
                     question board, or at their own terminal with \
                     `fno inbox outstanding clear {qid} --answer \"<answer>\"`. This \
                     session cannot answer it, and a decide record does not close it. If \
                     this session asked it and no longer needs it, withdraw it with \
                     `fno inbox outstanding clear {qid}`. Otherwise end the turn. The next \
                     stop on this question ends the loop.",
                    h.question,
                    qid = h.question_id
                ),
            };
        }
        let options_line = if h.options.is_empty() {
            String::new()
        } else {
            format!(" options: {};", h.options.join(" | "))
        };
        let digest = format!(
            "still held on open question {qid}: '{}'.{} The loop ends here. The user \
             answers on the question board, or with \
             `fno inbox outstanding clear {qid} --answer \"<answer>\"`.",
            h.question,
            options_line,
            qid = h.question_id
        );
        emit(
            "termination",
            serde_json::json!({
                "session_id": session_id,
                "reason": "HeldOnQuestion",
                "question_id": h.question_id,
                "message": digest,
            }),
        );
        return QuestionGateStop::Terminate {
            reason: super::TerminationReason::HeldOnQuestion,
            message: digest,
        };
    }
    QuestionGateStop::None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn qrow(qid: &str, session: &str, blocks: &[&str]) -> String {
        let blocks = blocks
            .iter()
            .map(|b| format!("\"{b}\""))
            .collect::<Vec<_>>()
            .join(",");
        format!(
            r#"{{"ts":"2026-09-17T10:00:00Z","type":"operator_question","source":"target","data":{{"question_id":"{qid}","question":"merge now or hold?","session_id":"{session}","blocks":[{blocks}]}}}}"#
        )
    }

    fn crow(qid: &str) -> String {
        format!(
            r#"{{"ts":"2026-09-17T11:00:00Z","type":"operator_question_closed","source":"target","data":{{"question_id":"{qid}","answer":"decided"}}}}"#
        )
    }

    #[test]
    fn open_question_blocks_once_then_terminates_through_the_real_writer() {
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().join("project-events.jsonl");
        let global = dir.path().join("global-events.jsonl");
        // Outside any git repo, so canonical_repo_root adds no real journal.
        let cwd = dir.path().join("cwd");
        std::fs::create_dir_all(&cwd).unwrap();
        crate::event_store::append_envelope(&project, &qrow("q-1", "sess-a", &["x-n"]), None)
            .unwrap();
        let emit = |t: &str, d: Value| crate::loopcheck::emit_to_both(&project, &global, t, d);

        let QuestionGateStop::Block { reason } =
            question_gates(&project, &global, &cwd, "sess-a", "x-n", &|_| false, &emit)
        else {
            panic!("first fire must block");
        };
        assert!(
            reason.contains("fno inbox outstanding clear q-1"),
            "{reason}"
        );
        assert!(!reason.contains("backlog decide"), "{reason}");

        let QuestionGateStop::Terminate { reason, message } =
            question_gates(&project, &global, &cwd, "sess-a", "x-n", &|_| false, &emit)
        else {
            panic!("second fire must terminate");
        };
        assert!(matches!(
            reason,
            super::super::TerminationReason::HeldOnQuestion
        ));
        assert!(message.contains("q-1"), "{message}");
        assert!(
            message.contains("fno inbox outstanding clear q-1 --answer"),
            "{message}"
        );
        assert!(!message.contains("backlog decide"), "{message}");

        let count = |ty: &str| {
            crate::events::committed_journal_text(&project)
                .lines()
                .filter_map(|l| serde_json::from_str::<Value>(l).ok())
                .filter(|v| v.get("type").and_then(Value::as_str) == Some(ty))
                .count()
        };
        assert_eq!(count("loop_check_question_gate"), 1, "one marker row lands");
        assert_eq!(count("termination"), 1, "one termination row lands");

        // A marker that never lands leaves every fire a first fire: the gate
        // blocks again and never terminates without it.
        let lost = dir.path().join("lost-events.jsonl");
        crate::event_store::append_envelope(&lost, &qrow("q-2", "sess-b", &["x-n"]), None).unwrap();
        let drop_rows = |_: &str, _: Value| {};
        for _ in 0..2 {
            assert!(matches!(
                question_gates(&lost, &lost, &cwd, "sess-b", "x-n", &|_| false, &drop_rows),
                QuestionGateStop::Block { .. }
            ));
        }
    }

    #[test]
    fn gated_types_cover_every_schema_question_family_type() {
        let schema: Value = serde_json::from_str(include_str!("../event_store/events_schema.json"))
            .expect("events_schema.json must parse (generated by build.rs)");
        let names = schema
            .get("event_types")
            .and_then(|v| v.as_object())
            .expect("event_types must be an object")
            .keys();
        let missing: Vec<String> = names
            .filter(|n| {
                n.starts_with("operator_question")
                    || n.starts_with("operator_decision")
                    || n.as_str() == "loop_check_question_gate"
            })
            .filter(|n| !QUESTION_FAMILY_TYPES.contains(&n.as_str()))
            .cloned()
            .collect();
        assert!(
            missing.is_empty(),
            "question/decision/hold types the schema emits but QUESTION_FAMILY_TYPES \
             lacks: {missing:?}; event_lines_of drops their rows before the fold, so \
             the hold gate never sees them"
        );
    }

    #[test]
    fn closed_question_and_foreign_blocks_hold_nothing() {
        let raw = [
            qrow("q-3", "sess-c", &["x-n"]),
            crow("q-3"),
            qrow("q-4", "sess-c", &["x-n"]),
        ]
        .join("\n");
        let path = std::env::temp_dir().join(format!(
            "fno-holds-test-3-{}-{}.jsonl",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::write(&path, format!("{raw}\n")).unwrap();
        let journals = vec![path.clone()];
        let mine = scan_open_holds(&journals, "sess-c", "x-n", &|_| false);
        assert_eq!(mine.len(), 1, "only the open one holds");
        assert_eq!(mine[0].question_id, "q-4");
        // A node the blocks never names, and a session that did not ask:
        let other = scan_open_holds(&journals, "sess-z", "x-other", &|_| false);
        assert!(other.is_empty());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn question_on_a_done_node_holds_nothing_but_a_live_sibling_still_does() {
        let raw = [
            qrow("q-5", "sess-d", &["x-merged"]),
            qrow("q-6", "sess-d", &["x-merged", "x-live"]),
            qrow("q-7", "sess-e", &["x-merged"]),
        ]
        .join("\n");
        let path = std::env::temp_dir().join(format!(
            "fno-holds-test-4-{}-{}.jsonl",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::write(&path, format!("{raw}\n")).unwrap();
        let journals = vec![path.clone()];
        let done = |id: &str| id == "x-merged";
        // The asking session: q-5 is mooted, q-6 still blocks x-live.
        let asker = scan_open_holds(&journals, "sess-d", "x-merged", &done);
        assert_eq!(
            asker.len(),
            1,
            "{:?}",
            asker.iter().map(|h| &h.question_id).collect::<Vec<_>>()
        );
        assert_eq!(asker[0].question_id, "q-6");
        // A session whose own node merged is not held by a question naming it.
        assert!(scan_open_holds(&journals, "sess-z", "x-merged", &done).is_empty());
        let _ = std::fs::remove_file(&path);
    }
}
