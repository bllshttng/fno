//! The daemon's question sweep: an open question closes when its subject
//! resolves - a terminal node rung, a merged PR the subject names, or a
//! newer ask in the same head family.
//!
//! Every question the fold settles gets one `operator_question_closed` row
//! with an empty answer and the outcome in `reason` (`node-closed: {node}
//! {status}`, `moot: ...` for a user-only ask, `pr-merged: PR {n} merged`,
//! `superseded: {id} re-asks at head {head}`), and one `question_sweep`
//! event logs the count. Graph unreadable closes nothing.
//!
//! A closed node does not settle a question that names a live PR. The sweep
//! moves that question to the node's lead once and leaves it open.
use crate::paths::AgentsHome;
use serde_json::json;
use std::path::{Path, PathBuf};

/// Sweep cadence: hourly, so a question whose subject resolved does not
/// outlive it by much. The interval bounds discovery lag, not freshness.
const QUESTION_SWEEP_INTERVAL_SECS: i64 = 3_600;

static QUESTION_SWEEP_IN_FLIGHT: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

struct Gate<'a>(&'a std::sync::atomic::AtomicBool);
impl Drop for Gate<'_> {
    fn drop(&mut self) {
        self.0.store(false, std::sync::atomic::Ordering::SeqCst);
    }
}

/// The daemon's tick arm: one sweep in flight, off the select loop.
pub fn daemon_tick(home: &AgentsHome, now: i64) {
    use std::sync::atomic::Ordering;
    if !QUESTION_SWEEP_IN_FLIGHT.swap(true, Ordering::SeqCst) {
        let home = home.clone();
        let emitter = crate::events::EventEmitter::new(home.events_jsonl(), "daemon");
        tokio::task::spawn_blocking(move || {
            let _gate = Gate(&QUESTION_SWEEP_IN_FLIGHT);
            question_sweep(&home, &emitter, now);
        });
    }
}

/// Sweep the question journals against the graph on a fixed cadence.
pub fn question_sweep(home: &AgentsHome, emitter: &crate::events::EventEmitter, now: i64) -> usize {
    question_sweep_in(home, emitter, now, &read_facts_with_prs, &LIVE_LADDER)
}

/// The ladder doors the sweep uses, so a test never wakes a live lead.
pub(crate) struct LadderDoors {
    pub on: fn(&Path) -> bool,
    pub place: fn(&Path, Option<&str>, Option<&str>) -> Option<serde_json::Value>,
    pub wake: fn(&serde_json::Value, &str, Option<&str>, &str),
}

const LIVE_LADDER: LadderDoors = LadderDoors {
    on: crate::question_ladder::ladder_on,
    place: crate::question_ladder::place,
    wake: crate::question_ladder::wake_holder,
};

/// A head-scoped subject names one commit: its last token, split at the
/// last space or `@`, is 7 to 40 lowercase hex characters with at least one
/// digit. Returns `(family, head)`; the family is the subject without the
/// head. `visual approval PR 7 at 05ea5bcc` yields the family and the sha;
/// `lead-hold-cap` and `visual approval PR 12 at defaced` (hex letters, no
/// digit) yield `None`.
pub(crate) fn head_scoped(subject: &str) -> Option<(&str, &str)> {
    let subject = subject.trim();
    let split = subject.rfind([' ', '@'])?;
    // A separator at the front leaves no family text: `@abcdef1` names a
    // head and nothing else, and an empty family would collide every such
    // subject on a node into one family.
    if split == 0 {
        return None;
    }
    let (family, head) = (&subject[..split], &subject[split + 1..]);
    let lowercase_hex = |b: u8| b.is_ascii_digit() || (b'a'..=b'f').contains(&b);
    let ok = (7..=40).contains(&head.len())
        && head.bytes().all(lowercase_hex)
        && head.bytes().any(|b| b.is_ascii_digit());
    ok.then_some((family, head))
}

/// Two heads name the same commit when one is a prefix of the other (a
/// short sha and its long form).
pub(crate) fn same_head(a: &str, b: &str) -> bool {
    a.starts_with(b) || b.starts_with(a)
}

/// What the question fold reads: node statuses, each node's merged PR, and
/// the raw question journals.
#[derive(Default)]
pub(crate) struct RungFacts {
    pub statuses: std::collections::BTreeMap<String, String>,
    /// node id -> title, for the card's node line.
    pub titles: std::collections::BTreeMap<String, String>,
    /// node id -> every merged PR number, primary and additional_prs alike,
    /// only when its merge_status reads merged.
    pub merged_prs: std::collections::BTreeMap<String, Vec<u64>>,
    /// (asker cwd, PR) -> open, for the PRs a closing door read. The hot
    /// attention read leaves this empty: an unread PR only keeps a row open.
    pub pr_open: std::collections::BTreeMap<(String, u64), bool>,
    pub raw: String,
}

/// The real reading: graph statuses plus the question journal family.
/// `pub(crate)`: the bell's clear-settled door reads the same facts through
/// the same reader, so the sweep keeps the one graph+journal read.
pub(crate) fn read_closed_rung_facts(cwd: &Path, home: &AgentsHome) -> RungFacts {
    let graph = crate::org_board::graph_json_path(cwd);
    let store = crate::backlog::api::Store::new(&graph);
    let mut facts = statuses_of(&store);
    facts.raw = journals_raw(&fno_dir_of(home), cwd);
    facts
}

/// The closing doors' reading: [`read_closed_rung_facts`] plus one REST read
/// for each PR the node-closed rule needs. An unreadable PR stays unread.
pub(crate) fn read_facts_with_prs(cwd: &Path, home: &AgentsHome) -> RungFacts {
    let mut facts = read_closed_rung_facts(cwd, home);
    for (asker_cwd, pr) in crate::needs::prs_to_read(&facts.raw, &facts) {
        if let Some(open) = crate::gc_sweep::gh_pr_is_open(pr, &asker_cwd) {
            facts.pr_open.insert((asker_cwd, pr), open);
        }
    }
    facts
}

pub(crate) fn statuses_of(store: &crate::backlog::api::Store) -> RungFacts {
    let mut facts = RungFacts::default();
    if let Ok(rows) = crate::graph_store::read_rows_where(
        &store.graph,
        &crate::backlog::RowQuery {
            fields: Some(
                [
                    "id",
                    "status",
                    "title",
                    "pr_number",
                    "merge_status",
                    "additional_prs",
                ]
                .into_iter()
                .map(str::to_string)
                .collect(),
            ),
            with_blockers: true,
            ..Default::default()
        },
    )
    .map_err(|error| crate::backlog::api::ApiError(error.to_string()))
    {
        for row in rows {
            let (Some(id), Some(status)) = (
                row.get("id").and_then(serde_json::Value::as_str),
                row.get("status").and_then(serde_json::Value::as_str),
            ) else {
                continue;
            };
            facts.statuses.insert(id.to_string(), status.to_string());
            if let Some(title) = row.get("title").and_then(serde_json::Value::as_str) {
                facts.titles.insert(id.to_string(), title.to_string());
            }
            // The stamp is not a rung by itself (a multi-ship node can carry
            // one while work remains); it only feeds the pr-merged rule,
            // which also demands the question name the PR. Later ships ride
            // additional_prs and feed the same rule.
            let mut merged: Vec<u64> = Vec::new();
            if row.get("merge_status").and_then(serde_json::Value::as_str) == Some("merged") {
                if let Some(pr) = row.get("pr_number").and_then(serde_json::Value::as_u64) {
                    merged.push(pr);
                }
            }
            if let Some(serde_json::Value::Array(items)) = row.get("additional_prs") {
                for item in items {
                    if item.get("merge_status").and_then(serde_json::Value::as_str)
                        == Some("merged")
                    {
                        if let Some(pr) = item
                            .get("number")
                            .and_then(serde_json::Value::as_i64)
                            .and_then(|p| u64::try_from(p).ok())
                        {
                            merged.push(pr);
                        }
                    }
                }
            }
            if !merged.is_empty() {
                facts.merged_prs.insert(id.to_string(), merged);
            }
        }
    }
    facts
}

/// Append the shared derived-close row for each (question id, reason) pair
/// and return its per-id receipt. The sweep, the clear-settled door and the
/// intake's supersede share this writer: an empty answer, the outcome in
/// `reason`, and `closed_by` naming the writer.
pub(crate) fn close_questions(
    index: &Path,
    settled: &[(String, String)],
    now: i64,
    closed_by: &str,
) -> (Vec<String>, Vec<String>) {
    let mut closed = Vec::new();
    let mut errors = Vec::new();
    for (qid, reason) in settled {
        match crate::provider_cap::append_questions_row(
            index,
            &json!({
                "ts": crate::provider_cap::epoch_to_rfc3339(now),
                "type": "operator_question_closed",
                "source": "daemon",
                "data": {
                    "question_id": qid,
                    "answer": "",
                    "reason": reason,
                    "closed_by": closed_by,
                },
            }),
        ) {
            Ok(()) => closed.push(qid.clone()),
            Err(error) => errors.push(format!("question {qid}: {error}")),
        }
    }
    (closed, errors)
}

fn fno_dir_of(home: &AgentsHome) -> PathBuf {
    home.root()
        .parent()
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(".fno"))
}

fn journals_raw(fno_dir: &Path, cwd: &Path) -> String {
    let mut raw = String::new();
    for path in crate::needs::question_journals(fno_dir, cwd) {
        let content = crate::event_store::journal_text(
            &path,
            &[
                "operator_question",
                "operator_question_closed",
                "question_sweep",
            ],
        );
        raw.push_str(&content);
        raw.push('\n');
    }
    crate::event_store::activity_text(&raw)
}
/// Move each held question to its node's lead and wake that lead. A row whose
/// repo turns the ladder off stays where it sits.
fn move_held_questions(
    held: Vec<crate::needs::HeldQuestion>,
    index: &Path,
    now: i64,
    ladder: &LadderDoors,
) -> (Vec<String>, Vec<String>) {
    let mut moved = Vec::new();
    let mut errors = Vec::new();
    let ts = crate::provider_cap::epoch_to_rfc3339(now);
    for mut q in held {
        let cwd = PathBuf::from(q.data["cwd"].as_str().unwrap_or("").trim());
        if !(ladder.on)(&cwd) {
            continue;
        }
        if let Some(data) = q.data.as_object_mut() {
            data.remove("ts");
        }
        let next = (ladder.place)(&cwd, Some(q.node.as_str()), None);
        let row = crate::question_ladder::moved_for_open_pr(
            &json!({"type": "operator_question", "source": "daemon", "data": q.data}),
            next.clone(),
            &format!(
                "node {} reads {}; PR {} is still open",
                q.node, q.status, q.pr
            ),
            &ts,
        );
        match crate::provider_cap::append_questions_row(index, &row) {
            Ok(()) => {
                if let Some(rung) = &next {
                    let question = row["data"]["question"].as_str().unwrap_or("");
                    (ladder.wake)(rung, &q.id, Some(q.node.as_str()), question);
                }
                moved.push(q.id);
            }
            Err(error) => errors.push(format!("move {}: {error}", q.id)),
        }
    }
    (moved, errors)
}

/// [`question_sweep`] with the reading injected, so tests drive the stamp,
/// the fold, and the writes against fixtures instead of the live graph.
/// `pub(crate)`: the reading type is crate-private, so the crate's public
/// surface never names it.
pub(crate) fn question_sweep_in(
    home: &AgentsHome,
    emitter: &crate::events::EventEmitter,
    now: i64,
    read: &dyn Fn(&Path, &AgentsHome) -> RungFacts,
    ladder: &LadderDoors,
) -> usize {
    let stamp = home.root().join("question-sweep.stamp");
    let last = std::fs::read_to_string(&stamp)
        .ok()
        .and_then(|s| s.trim().parse::<i64>().ok())
        .unwrap_or(0);
    if now.saturating_sub(last) < QUESTION_SWEEP_INTERVAL_SECS {
        return 0;
    }
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let facts = read(&cwd, home);
    let settled = crate::needs::settled_questions(&facts.raw, &facts);
    let task_ids = crate::fleet_task::node_closed_task_ids(&facts.raw, &facts.statuses);
    let legacy_ids = crate::fleet_task::legacy_question_ids(&facts.raw);
    let mut tasks_closed = Vec::new();
    let mut legacy_moved = Vec::new();
    // The reasons travel with the pairs: the fold owns which rule closed a
    // question and says so; this writer only records it.
    let index = crate::provider_cap::questions_path(home);
    let (closed, mut errors) = close_questions(&index, &settled, now, "question-sweep");
    let held = crate::needs::questions_for_the_lead(&facts.raw, &facts, &settled);
    let (moved, move_errors) = move_held_questions(held, &index, now, ladder);
    errors.extend(move_errors);
    if settled.is_empty()
        && task_ids.is_empty()
        && legacy_ids.is_empty()
        && moved.is_empty()
        && errors.is_empty()
    {
        let _ = emitter.emit(
            "question_sweep",
            &json!({"closed": 0, "tasks_closed": 0, "legacy_moved": 0, "outcome": "none"}),
        );
    } else {
        for tid in &task_ids {
            match crate::provider_cap::append_questions_row(
                &index,
                &json!({
                    "ts": crate::provider_cap::epoch_to_rfc3339(now),
                    "type": "fleet_task_closed",
                    "source": "daemon",
                    "data": {
                        "task_id": tid,
                        "reason": "node-closed",
                        "closed_by": "question-sweep",
                    },
                }),
            ) {
                Ok(()) => tasks_closed.push(tid.clone()),
                Err(error) => errors.push(format!("task {tid}: {error}")),
            }
        }
        for qid in &legacy_ids {
            match crate::provider_cap::append_questions_row(
                &index,
                &json!({
                    "ts": crate::provider_cap::epoch_to_rfc3339(now),
                    "type": "operator_question_closed",
                    "source": "daemon",
                    "data": {
                        "question_id": qid,
                        // Empty answer for the same reason as node-closed
                        // above: nobody made this decision.
                        "answer": "",
                        "reason": "moved-to-fleet-task",
                        "closed_by": "question-sweep",
                    },
                }),
            ) {
                Ok(()) => legacy_moved.push(qid.clone()),
                Err(error) => errors.push(format!("legacy question {qid}: {error}")),
            }
        }
        let _ = emitter.emit(
            "question_sweep",
            &json!({
                "closed": closed.len(),
                "tasks_closed": tasks_closed.len(),
                "legacy_moved": legacy_moved.len(),
                "moved_to_lead": moved,
                "outcome": if errors.is_empty() { "closed" } else { "partial" },
                "ids": closed,
                "task_ids": tasks_closed,
                "legacy_ids": legacy_moved,
                "errors": errors,
            }),
        );
    }
    if errors.is_empty() {
        let _ = std::fs::write(&stamp, now.to_string());
    }
    if closed.is_empty() && tasks_closed.is_empty() && legacy_moved.is_empty() && moved.is_empty() {
        0
    } else {
        1
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    const TEST_LADDER: LadderDoors = LadderDoors {
        on: |_| true,
        place: |_, _, _| {
            Some(json!({"level": "lead", "holder_session": "lead-sid", "holder_name": "muriel"}))
        },
        wake: |_, _, _, _| {},
    };

    fn tmp_home(tag: &str) -> AgentsHome {
        let mut p = std::env::temp_dir();
        p.push(format!(
            "fno-agents-qswp-{}-{}-{}",
            tag,
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        // The question store resolves one level ABOVE the home root, so the
        // home gets an "agents" child: the store then lands beside it, in
        // this test's own unique directory instead of the shared temp root.
        let home = AgentsHome::at(p.join("agents"));
        home.ensure_root().unwrap();
        home
    }

    fn fixture_read(
        statuses: Vec<(String, String)>,
        raw: String,
    ) -> impl Fn(&Path, &AgentsHome) -> RungFacts {
        move |_, _| RungFacts {
            statuses: statuses.iter().cloned().collect(),
            merged_prs: Default::default(),
            raw: raw.clone(),
            ..Default::default()
        }
    }

    fn fixture_read_merged(
        statuses: Vec<(String, String)>,
        merged: Vec<(String, u64)>,
        raw: String,
    ) -> impl Fn(&Path, &AgentsHome) -> RungFacts {
        move |_, _| {
            let mut merged_prs: std::collections::BTreeMap<String, Vec<u64>> = Default::default();
            for (node, pr) in &merged {
                merged_prs.entry(node.clone()).or_default().push(*pr);
            }
            RungFacts {
                statuses: statuses.iter().cloned().collect(),
                merged_prs,
                raw: raw.clone(),
                ..Default::default()
            }
        }
    }

    fn open_row(qid: &str, node: &str) -> String {
        format!(
            r#"{{"ts":"2026-09-17T10:00:00Z","type":"operator_question","source":"target","data":{{"question_id":"{qid}","question":"which way?","node":"{node}","blocks":["{node}"]}}}}"#
        )
    }

    #[test]
    fn failed_index_writes_are_reported_and_retried_without_advancing_the_stamp() {
        let home = tmp_home("partial");
        let emitter = crate::events::EventEmitter::new(home.events_jsonl(), "daemon");
        let index = crate::provider_cap::questions_path(&home);
        let blocked_store = crate::event_store::store_path(&index);
        std::fs::create_dir_all(&blocked_store).unwrap();
        let read = fixture_read(
            vec![("x-done".to_string(), "done".to_string())],
            open_row("q-done", "x-done"),
        );

        assert_eq!(
            question_sweep_in(&home, &emitter, 1_000_000, &read, &TEST_LADDER),
            0
        );
        assert!(!home.root().join("question-sweep.stamp").exists());
        let events = crate::events::committed_journal_text(&home.events_jsonl());
        assert!(events.contains(r#""outcome":"partial""#), "{events}");
        assert!(events.contains("question q-done:"), "{events}");

        std::fs::remove_dir_all(blocked_store).unwrap();
        assert_eq!(
            question_sweep_in(&home, &emitter, 1_000_001, &read, &TEST_LADDER),
            1
        );
        assert_eq!(
            crate::event_store::journal_text(&index, &["operator_question_closed"])
                .matches("q-done")
                .count(),
            1
        );
    }

    #[test]
    fn leaves_an_in_progress_node_s_question_open() {
        let home = tmp_home("inprogress");
        let emitter = crate::events::EventEmitter::new(home.events_jsonl(), "daemon");
        let read = fixture_read(
            vec![("x-live".to_string(), "in_progress".to_string())],
            open_row("q-live", "x-live"),
        );
        assert_eq!(
            question_sweep_in(&home, &emitter, 1_000_000, &read, &TEST_LADDER),
            0
        );
        let questions = crate::event_store::journal_text(
            &crate::provider_cap::questions_path(&home),
            &["operator_question_closed"],
        );
        assert!(!questions.contains("q-live"));
    }

    #[test]
    fn closes_a_user_only_question_as_moot_and_a_machine_one_as_node_closed() {
        // A resolved subject closes all three; the reason records what
        // happened: a user-only ask whose node closed closes moot, a paint
        // ask whose PR merged closes pr-merged (why_user never blocks that
        // rule), a machine question node-closed. Every row keeps an empty
        // answer and names the node.
        let home = tmp_home("user-only");
        let emitter = crate::events::EventEmitter::new(home.events_jsonl(), "daemon");
        let user_row = format!(
            r#"{{"ts":"2026-09-17T10:00:00Z","type":"operator_question","source":"target","data":{{"question_id":"q-user","question":"approve the rules?","node":"x-done","blocks":["x-done"],"why_user":"a product or taste call"}}}}"#
        );
        let paint_row = r#"{"ts":"2026-09-17T10:00:00Z","type":"operator_question","source":"target","data":{"question_id":"q-paint","question":"does the paint look right?","node":"x-done","blocks":["x-done"],"why_user":"taste - whether the paint looks right is the user's call","subject":"visual approval PR 7 at 1111111a"}}"#;
        let read = fixture_read_merged(
            vec![("x-done".to_string(), "done".to_string())],
            vec![("x-done".to_string(), 7)],
            format!("{user_row}\n{paint_row}\n{}", open_row("q-mach", "x-done")),
        );
        assert_eq!(
            question_sweep_in(&home, &emitter, 1_000_000, &read, &TEST_LADDER),
            1
        );
        let questions = crate::event_store::journal_text(
            &crate::provider_cap::questions_path(&home),
            &["operator_question_closed"],
        );
        assert!(
            questions.contains("q-user") && questions.contains(r#""reason":"moot: x-done done""#),
            "{questions}"
        );
        assert!(
            questions.contains("q-paint")
                && questions.contains(r#""reason":"pr-merged: PR 7 merged (node x-done)""#)
                && questions.contains(r#""answer":""#),
            "{questions}"
        );
        assert!(
            questions.contains("q-mach")
                && questions.contains(r#""reason":"node-closed: x-done done""#),
            "{questions}"
        );
    }

    #[test]
    fn a_done_node_with_a_live_pr_moves_its_question_to_the_lead_and_keeps_it_open() {
        let home = tmp_home("open-pr");
        let emitter = crate::events::EventEmitter::new(home.events_jsonl(), "daemon");
        let raw = r#"{"ts":"2026-10-18T21:33:38Z","type":"operator_question","source":"target","data":{"question_id":"q-land","question":"Close PR 3212 (x-gone)?","cwd":"/repo","node":"x-gone"}}"#;
        let read = |_: &Path, _: &AgentsHome| {
            let mut facts = RungFacts {
                raw: raw.to_string(),
                ..Default::default()
            };
            facts.statuses.insert("x-gone".into(), "done".into());
            facts.pr_open.insert(("/repo".into(), 3212), true);
            facts
        };
        assert_eq!(
            question_sweep_in(&home, &emitter, 1_000_000, &read, &TEST_LADDER),
            1
        );
        let index = crate::provider_cap::questions_path(&home);
        assert!(
            !crate::event_store::journal_text(&index, &["operator_question_closed"])
                .contains("q-land")
        );
        let rows = crate::event_store::journal_text(&index, &["operator_question"]);
        assert!(rows.contains(r#""holder_name":"muriel""#), "{rows}");
        assert!(rows.contains(r#""action":"open-pr""#), "{rows}");
        assert!(
            rows.contains("node x-gone reads done; PR 3212 is still open"),
            "{rows}"
        );
    }

    #[test]
    fn retires_legacy_machine_questions_and_leaves_agent_asks_open() {
        // AC12-HP + AC13-EDGE + AC14-EDGE: each legacy-marker row closes
        // with an empty answer, reason moved-to-fleet-task; an agent ask
        // quoting a marker mid-text stays open, as do the markers this plan
        // did not move.
        let home = tmp_home("legacy-retire");
        let emitter = crate::events::EventEmitter::new(home.events_jsonl(), "daemon");
        let raw = [
            r#"{"ts":"t","type":"operator_question","source":"daemon","data":{"question_id":"q-heal","question":"heal: PR 9 rebase conflict. Resolve with x"}}"#,
            r#"{"ts":"t","type":"operator_question","source":"daemon","data":{"question_id":"q-hold","question":"[reap-hold: x] held"}}"#,
            r#"{"ts":"t","type":"operator_question","source":"daemon","data":{"question_id":"q-agent","question":"PR 9 failed and the log quotes heal: PR 9 mid-sentence"}}"#,
            r#"{"ts":"t","type":"operator_question","source":"daemon","data":{"question_id":"q-branch","question":"[session-transition-branch: keep or clean?]"}}"#,
        ]
        .join("\n");
        let read = fixture_read(vec![], raw);
        assert_eq!(
            question_sweep_in(&home, &emitter, 1_000_000, &read, &TEST_LADDER),
            1
        );
        let questions = crate::event_store::journal_text(
            &crate::provider_cap::questions_path(&home),
            &["operator_question_closed"],
        );
        // The store beside the temp home is shared by sibling tests, so the
        // count is scoped per seeded id, never global.
        let moved = |qid: &str| {
            questions
                .lines()
                .filter(|l| l.contains(qid) && l.contains("moved-to-fleet-task"))
                .count()
        };
        assert_eq!(moved("q-heal"), 1, "q-heal retired: {questions}");
        assert_eq!(moved("q-hold"), 1, "q-hold retired: {questions}");
        assert_eq!(
            moved("q-agent"),
            0,
            "the agent ask quoting a marker mid-text stays open: {questions}"
        );
        assert_eq!(
            moved("q-branch"),
            0,
            "the markers this plan did not move stay open: {questions}"
        );
        let events = crate::events::committed_journal_text(&home.events_jsonl());
        assert!(events.contains(r#""legacy_moved":2"#), "{events}");
    }
}
