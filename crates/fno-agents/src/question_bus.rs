//! The question bus: open questions shared across the machines of one store.
//!
//! One `fleet_question` table on the shared primary, in the mail bus's shape:
//! one row per (machine_id, question_id), written only by the machine that
//! asked, holding the raw `operator_question` record and its close. Every
//! other machine pulls those rows and appends a copy to its own
//! `questions.jsonl`, stamped `data.origin_machine` and `data.host`, so every
//! local reader (the list, the Waiting on you block, the mux tab) sees it with
//! no change. The table never joins `SHARED_TABLES` or `SHARED_PREFIXES`.
//!
//! Answers to copies ride the same primary on a second carrier,
//! `fleet_question_answer`: one row per (answering machine, copy question
//! id), written only by the machine that answered a copy. The asking
//! machine's beat applies each answer through the shared `append_answer_row`
//! seam (clear, mail, resume) and stamps `applied_at`, so one writer owns
//! each fact: the answer belongs to the answerer, the close to the origin.

use crate::store_remote::{Remote, SqlValue};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

/// A closed row leaves the primary this long after its close.
pub const RETAIN_SECS: i64 = 7 * 24 * 3600;

const DDL: &str = "CREATE TABLE IF NOT EXISTS fleet_question (\
machine_id TEXT NOT NULL, \
question_id TEXT NOT NULL, \
host TEXT NOT NULL, \
record TEXT NOT NULL, \
opened_at INTEGER NOT NULL, \
closed_at INTEGER, \
close_reason TEXT, \
PRIMARY KEY (machine_id, question_id))";

const ANSWER_DDL: &str = "CREATE TABLE IF NOT EXISTS fleet_question_answer (\
machine_id TEXT NOT NULL, \
question_id TEXT NOT NULL, \
origin_machine TEXT NOT NULL, \
answer_host TEXT NOT NULL, \
record TEXT NOT NULL, \
created_at INTEGER NOT NULL, \
applied_at INTEGER, \
PRIMARY KEY (machine_id, question_id))";

const NOW: &str = "(SELECT strftime('%s','now'))";

pub fn ensure_schema(remote: &Remote) -> Result<(), String> {
    remote.script(&format!("{DDL}; {ANSWER_DDL}"))
}

/// The origin machine of a copied question, or `None` for a question this
/// machine asked.
pub(crate) fn peer_origin(question: &Value) -> Option<&str> {
    question
        .get("data")
        .and_then(|d| d.get("origin_machine"))
        .and_then(Value::as_str)
        .filter(|m| !m.is_empty() && *m != crate::claims::machine_id())
}

/// Who this machine is and where its `questions.jsonl` lives (the state
/// root). Tests build one per fake machine.
pub(crate) struct Host {
    pub machine: String,
    pub host: String,
    pub root: PathBuf,
}

impl Host {
    fn live() -> Self {
        let index = crate::provider_cap::questions_path(&crate::paths::AgentsHome::from_env());
        Host {
            machine: crate::claims::machine_id(),
            host: crate::mail_bus::normalize_host(&crate::claims::hostname()),
            root: index
                .parent()
                .map(Path::to_path_buf)
                .unwrap_or_else(|| PathBuf::from(".fno")),
        }
    }

    fn index(&self) -> PathBuf {
        self.root.join("questions.jsonl")
    }
}

/// The local fold: the first asked record and the first close, by id, plus
/// the unsuperseded answer rows, by item id.
struct Fold {
    asked: HashMap<String, Value>,
    closed: HashMap<String, Value>,
    answers: HashMap<String, Value>,
}

fn fold(index: &Path) -> Fold {
    let mut out = Fold {
        asked: HashMap::new(),
        closed: HashMap::new(),
        answers: HashMap::new(),
    };
    let text = crate::event_store::journal_text(
        index,
        &[
            "operator_question",
            "operator_question_closed",
            "attention_answer",
        ],
    );
    for event in text
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
    {
        let data = &event["data"];
        if event["type"].as_str() == Some("attention_answer") {
            // A superseded row never answers (the seam wrote it as a marker).
            if data["superseded"].as_bool() != Some(true) {
                if let Some(id) = data["item_id"].as_str() {
                    out.answers.insert(id.to_string(), event);
                }
            }
            continue;
        }
        let Some(qid) = data["question_id"].as_str().map(str::to_string) else {
            continue;
        };
        let map = match event["type"].as_str() {
            Some("operator_question") => &mut out.asked,
            Some("operator_question_closed") => &mut out.closed,
            _ => continue,
        };
        // The newest row wins: a ladder climb re-appends its question.
        map.insert(qid, event);
    }
    out
}

/// How many rungs a question row climbed; a newer count is a climb the
/// other side has not seen.
fn climbs(event: &Value) -> usize {
    event["data"]["via"].as_array().map_or(0, Vec::len)
}

const CLIMBS_SQL: &str = "CAST(COALESCE(json_array_length(record, '$.data.via'), 0) AS TEXT)";

fn is_own(event: &Value, machine: &str) -> bool {
    event["data"]["origin_machine"]
        .as_str()
        .is_none_or(|m| m.is_empty() || m == machine)
}

fn close_reason(close: &Value) -> String {
    let data = &close["data"];
    match (
        data["reason"].as_str().filter(|r| !r.is_empty()),
        data["answer"].as_str(),
    ) {
        (Some(reason), _) => reason.to_string(),
        (None, Some(answer)) => format!("answered: {answer}"),
        (None, None) => "closed".to_string(),
    }
}

/// What one beat did: the status file's body.
#[derive(Debug, Default, PartialEq)]
pub(crate) struct Beat {
    pub published: usize,
    pub copied: usize,
    pub closed: usize,
    pub answers_published: usize,
    pub answers_applied: usize,
    pub peer_hosts: Vec<String>,
}

/// One beat: publish this machine's questions and closes, pull the peers',
/// publish answers this machine recorded for copies, apply answers recorded
/// for this machine's questions, then drop rows past the retention.
pub(crate) fn beat(remote: &Remote, me: &Host) -> Result<Beat, String> {
    if me.machine.is_empty() {
        return Err("no machine id; the question bus needs one".into());
    }
    let published = publish(remote, me)?;
    let (copied, closed, peer_hosts) = pull(remote, me)?;
    let answers_published = publish_answers(remote, me)?;
    let answers_applied = apply_answers(remote, me)?;
    remote.execute(
        &format!("DELETE FROM fleet_question WHERE closed_at < {NOW} - ?"),
        &[SqlValue::Integer(RETAIN_SECS)],
    )?;
    remote.execute(
        &format!(
            "DELETE FROM fleet_question_answer WHERE COALESCE(applied_at, created_at) < {NOW} - ?"
        ),
        &[SqlValue::Integer(RETAIN_SECS)],
    )?;
    Ok(Beat {
        published,
        copied,
        closed,
        answers_published,
        answers_applied,
        peer_hosts,
    })
}

/// One read of the primary's open rows for this machine decides the writes,
/// so a quiet beat sends one statement; the writes ride one transaction.
fn publish(remote: &Remote, me: &Host) -> Result<usize, String> {
    let local = fold(&me.index());
    let reply = remote.execute(
        &format!(
            "SELECT question_id, {CLIMBS_SQL} FROM fleet_question \
WHERE machine_id = ? AND closed_at IS NULL"
        ),
        &[SqlValue::Text(me.machine.clone())],
    )?;
    let on_primary: HashMap<&str, usize> = reply
        .rows
        .iter()
        .filter_map(|r| {
            let climbs = r
                .get(1)
                .and_then(SqlValue::text)
                .and_then(|n| n.parse().ok());
            Some((r.first()?.text()?, climbs.unwrap_or(0)))
        })
        .collect();
    let mine = SqlValue::Text(me.machine.clone());
    let mut steps: Vec<(String, Vec<SqlValue>)> = Vec::new();
    for (qid, event) in local.asked.iter().filter(|(_, e)| is_own(e, &me.machine)) {
        let id = SqlValue::Text(qid.clone());
        let primary_climbs = on_primary.get(qid.as_str()).copied();
        if local.closed.get(qid).is_none() && primary_climbs.is_some_and(|n| n < climbs(event)) {
            steps.push((
                "UPDATE fleet_question SET record = ? \
WHERE machine_id = ? AND question_id = ? AND closed_at IS NULL"
                    .to_string(),
                vec![SqlValue::Text(event.to_string()), mine.clone(), id.clone()],
            ));
            continue;
        }
        match (local.closed.get(qid), primary_climbs.is_some()) {
            (None, false) => steps.push((
                format!(
                    "INSERT OR IGNORE INTO fleet_question \
(machine_id, question_id, host, record, opened_at) VALUES (?, ?, ?, ?, {NOW})"
                ),
                vec![
                    mine.clone(),
                    id,
                    SqlValue::Text(me.host.clone()),
                    SqlValue::Text(event.to_string()),
                ],
            )),
            (Some(close), true) => steps.push((
                format!(
                    "UPDATE fleet_question SET closed_at = {NOW}, close_reason = ? \
WHERE machine_id = ? AND question_id = ? AND closed_at IS NULL"
                ),
                vec![SqlValue::Text(close_reason(close)), mine.clone(), id],
            )),
            _ => {}
        }
    }
    if !steps.is_empty() {
        remote
            .transaction(&steps)
            .map_err(|(_, error)| format!("publish: {error}"))?;
    }
    Ok(steps.len())
}

/// Publish answers this machine recorded on copies of peer questions: one
/// `fleet_question_answer` row per copy answer, written only by the machine
/// that answered. One metadata read decides the writes, so a quiet beat
/// sends nothing.
fn publish_answers(remote: &Remote, me: &Host) -> Result<usize, String> {
    let local = fold(&me.index());
    let reply = remote.execute(
        "SELECT question_id FROM fleet_question_answer WHERE machine_id = ?",
        &[SqlValue::Text(me.machine.clone())],
    )?;
    let on_primary: HashSet<&str> = reply
        .rows
        .iter()
        .filter_map(|r| r.first()?.text())
        .collect();
    let mine = SqlValue::Text(me.machine.clone());
    let mut steps: Vec<(String, Vec<SqlValue>)> = Vec::new();
    for (id, event) in &local.answers {
        // Only answers on copies publish: the origin applies an answer to
        // its own question locally, through the door, and never publishes.
        let Some(asked) = local.asked.get(id) else {
            continue;
        };
        let Some(origin_machine) = peer_origin(asked) else {
            continue;
        };
        if on_primary.contains(id.as_str()) {
            continue;
        }
        // The apply half reads these two from the record: the copy's origin
        // id (stamped on the question beside origin_machine at pull time),
        // and this machine as the answering host.
        let Some(origin_id) = asked["data"]["origin_question_id"].as_str() else {
            continue;
        };
        if !event.get("data").map(|d| d.is_object()).unwrap_or(false) {
            continue;
        }
        let mut record = event.clone();
        record["data"]["origin_question_id"] = json!(origin_id);
        record["data"]["host"] = json!(me.host.clone());
        steps.push((
            format!(
                "INSERT OR IGNORE INTO fleet_question_answer \
(machine_id, question_id, origin_machine, answer_host, record, created_at) \
VALUES (?, ?, ?, ?, ?, {NOW})"
            ),
            vec![
                mine.clone(),
                SqlValue::Text(id.clone()),
                SqlValue::Text(origin_machine.to_string()),
                SqlValue::Text(me.host.clone()),
                SqlValue::Text(record.to_string()),
            ],
        ));
    }
    if !steps.is_empty() {
        remote
            .transaction(&steps)
            .map_err(|(_, error)| format!("publish_answers: {error}"))?;
    }
    Ok(steps.len())
}

/// The origin's half: apply each peer answer to its own question through the
/// same seam a local mux answer takes, then stamp `applied_at` so the row is
/// applied exactly once. A row whose question closed or vanished before the
/// answer arrived stays unstamped and ages out with the retention; a lost
/// race against a local door answer is a no-op and retries next beat.
fn apply_answers(remote: &Remote, me: &Host) -> Result<usize, String> {
    let reply = remote.execute(
        "SELECT machine_id, question_id FROM fleet_question_answer \
WHERE origin_machine = ? AND applied_at IS NULL",
        &[SqlValue::Text(me.machine.clone())],
    )?;
    if reply.rows.is_empty() {
        return Ok(0);
    }
    let rows: Vec<(String, String)> = reply
        .rows
        .iter()
        .filter_map(|r| {
            Some((
                r.first()?.text()?.to_string(),
                r.get(1)?.text()?.to_string(),
            ))
        })
        .collect();
    let local = fold(&me.index());
    let mut applied: Vec<(String, String, Vec<SqlValue>)> = Vec::new();
    let mut count = 0usize;
    for (answerer, copy_id) in &rows {
        // The record rides a second read, one row at a time: a full table of
        // records in one reply crosses the primary's reply limit.
        let Some(record) = fetch_answer_record(remote, answerer, copy_id)? else {
            continue;
        };
        // The origin's own id rides the copy's data, stamped at copy time.
        let Some(origin_id) = record["data"]["origin_question_id"]
            .as_str()
            .map(str::to_string)
        else {
            continue;
        };
        let Some(event) = local.asked.get(&origin_id) else {
            continue;
        };
        if local.closed.contains_key(&origin_id) || !is_own(event, &me.machine) {
            continue;
        }
        let answer = record_to_answer(&record);
        let answerer_host = record["data"]["host"].as_str().unwrap_or(answerer);
        match crate::attention_arm::append_answer_row(
            &origin_id,
            &format!("peer:{answerer_host}"),
            &answer,
            "sink",
            "question_bus",
            answerer,
        ) {
            Ok(_) => {
                count += 1;
                applied.push((
                    answerer.clone(),
                    copy_id.clone(),
                    vec![
                        SqlValue::Text(me.machine.clone()),
                        SqlValue::Text(copy_id.clone()),
                    ],
                ));
            }
            Err(e) => {
                // A lost race against a local door answer is a no-op: the
                // first answer wins, and the row retries next beat.
                eprintln!("question-bus: apply {copy_id}: {e}");
            }
        }
    }
    if !applied.is_empty() {
        let steps: Vec<(String, Vec<SqlValue>)> = applied
            .iter()
            .map(|(_, _, params)| {
                (
                    format!(
                        "UPDATE fleet_question_answer SET applied_at = {NOW} \
WHERE machine_id = ? AND question_id = ?"
                    ),
                    params.clone(),
                )
            })
            .collect();
        remote
            .transaction(&steps)
            .map_err(|(_, error)| format!("apply_answers: {error}"))?;
    }
    Ok(count)
}

/// The answer row's record, one row at a time under the reply limit.
fn fetch_answer_record(
    remote: &Remote,
    answerer: &str,
    copy_id: &str,
) -> Result<Option<Value>, String> {
    let reply = remote.execute(
        "SELECT record FROM fleet_question_answer WHERE machine_id = ? AND question_id = ?",
        &[
            SqlValue::Text(answerer.into()),
            SqlValue::Text(copy_id.into()),
        ],
    )?;
    let text = reply.rows.first().and_then(|r| r.first()?.text());
    Ok(text.and_then(|t| serde_json::from_str(t).ok()))
}

/// Map a copy answer record back to the answer kind the seam writes.
fn record_to_answer(record: &Value) -> crate::attention_file::FileAnswer {
    let data = &record["data"];
    if data["delegate"].as_bool() == Some(true) {
        return crate::attention_file::FileAnswer::Delegate;
    }
    if data["done"].as_bool() == Some(true) {
        return crate::attention_file::FileAnswer::Done;
    }
    if let Some(n) = data["option"].as_u64() {
        return crate::attention_file::FileAnswer::Option(n as u32);
    }
    crate::attention_file::FileAnswer::Words(data["words"].as_str().unwrap_or_default().to_string())
}

/// A copy's local id: the origin's id with its host, so a fixed id two
/// machines both use (`provider-cap:<lane>`) stays two questions here.
pub(crate) fn copy_id(qid: &str, host: &str) -> String {
    format!("{qid}@{host}")
}

/// Copy new peer questions in, close copies their origin closed, and close
/// copies whose origin row is gone (closed and aged out while this machine
/// was away: an origin never deletes an open row). Returns (copied, closed,
/// peer hosts seen).
fn pull(remote: &Remote, me: &Host) -> Result<(usize, usize, Vec<String>), String> {
    let reply = remote.execute(
        &format!(
            "SELECT machine_id, question_id, host, closed_at, close_reason, {CLIMBS_SQL} \
FROM fleet_question WHERE machine_id != ?"
        ),
        &[SqlValue::Text(me.machine.clone())],
    )?;
    let index = me.index();
    let local = fold(&index);
    let mut seen: HashSet<String> = HashSet::new();
    let mut hosts: Vec<String> = Vec::new();
    let (mut copied, mut closed) = (0, 0);
    for row in &reply.rows {
        let text = |i: usize| row.get(i).and_then(SqlValue::text).unwrap_or_default();
        let (machine, qid, host) = (text(0), text(1), text(2));
        let open = row.get(3).is_none_or(|v| *v == SqlValue::Null);
        let id = copy_id(qid, host);
        if !hosts.iter().any(|h| h == host) {
            hosts.push(host.to_string());
        }
        let remote_climbs: usize = text(5).parse().unwrap_or(0);
        let stale = local
            .asked
            .get(&id)
            .is_some_and(|copy| climbs(copy) < remote_climbs);
        if open && (stale || !local.asked.contains_key(&id)) {
            // The record rides a second read, one row at a time: a full
            // table of records in one reply crosses the primary's reply limit.
            let Some(mut record) = fetch_record(remote, machine, qid)? else {
                continue;
            };
            if let Some(data) = record.get_mut("data").and_then(Value::as_object_mut) {
                data.insert("question_id".into(), json!(id));
                data.insert("origin_question_id".into(), json!(qid));
                data.insert("origin_machine".into(), json!(machine));
                data.insert("host".into(), json!(host));
            }
            crate::provider_cap::append_questions_row(&index, &record)?;
            copied += 1;
        } else if !open && local.asked.contains_key(&id) && !local.closed.contains_key(&id) {
            let reason = format!("closed on {host}: {}", text(4));
            crate::provider_cap::append_questions_row(&index, &peer_close(&id, &reason, host))?;
            closed += 1;
        }
        seen.insert(id);
    }
    for (id, event) in &local.asked {
        if is_own(event, &me.machine) || local.closed.contains_key(id) || seen.contains(id) {
            continue;
        }
        let host = event["data"]["host"].as_str().unwrap_or("its machine");
        let reason = format!("gone from {host}");
        crate::provider_cap::append_questions_row(&index, &peer_close(id, &reason, host))?;
        closed += 1;
    }
    hosts.sort();
    Ok((copied, closed, hosts))
}

fn fetch_record(remote: &Remote, machine: &str, qid: &str) -> Result<Option<Value>, String> {
    let reply = remote.execute(
        "SELECT record FROM fleet_question WHERE machine_id = ? AND question_id = ?",
        &[SqlValue::Text(machine.into()), SqlValue::Text(qid.into())],
    )?;
    let text = reply.rows.first().and_then(|r| r.first()?.text());
    Ok(text.and_then(|t| serde_json::from_str(t).ok()))
}

fn peer_close(qid: &str, reason: &str, host: &str) -> Value {
    json!({
        "ts": chrono::Utc::now().to_rfc3339(),
        "type": "operator_question_closed",
        "source": "daemon",
        "data": {"question_id": qid, "reason": reason, "closed_by": format!("peer:{host}")},
    })
}

/// `state/question_bus/status.json`: the done probe reads `at` and
/// `peer_hosts`.
fn write_status(me: &Host, outcome: &Result<Beat, String>) {
    let dir = me.root.join("state").join("question_bus");
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    let at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let body = match outcome {
        Ok(b) => json!({"at": at, "published": b.published, "peer_hosts": b.peer_hosts,
            "last_error": null}),
        Err(e) => json!({"at": at, "published": 0, "peer_hosts": [], "last_error": e}),
    };
    let tmp = dir.join("status.json.tmp");
    if std::fs::write(&tmp, body.to_string()).is_ok() {
        let _ = std::fs::rename(&tmp, dir.join("status.json"));
    }
}

/// One beat inside the mail arm's off-loop task, so it shares that arm's
/// cadence (`agents.remote.pull_interval_secs`), its one-in-flight gate and
/// its `store.remote_url` check.
pub fn tick(remote: &Remote) {
    static SCHEMA: std::sync::OnceLock<Result<(), String>> = std::sync::OnceLock::new();
    let me = Host::live();
    let outcome = match SCHEMA.get_or_init(|| ensure_schema(remote)) {
        Err(e) => Err(e.clone()),
        Ok(()) => beat(remote, &me),
    };
    match &outcome {
        Ok(b) if b.copied + b.closed > 0 => {
            eprintln!("question-bus: copied {} closed {}", b.copied, b.closed)
        }
        Ok(_) => {}
        Err(e) => eprintln!("question-bus: {e}"),
    }
    write_status(&me, &outcome);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store_remote::test_primary;

    fn ask(me: &Host, qid: &str) {
        let event = json!({"ts": "2026-10-27T00:00:00Z", "type": "operator_question",
            "source": "test", "data": {"question_id": qid, "question": "ship it?", "asker": "w1"}});
        crate::provider_cap::append_questions_row(&me.index(), &event).unwrap();
    }

    fn reasons(me: &Host) -> Vec<String> {
        let closed = fold(&me.index()).closed.into_values();
        closed
            .map(|c| c["data"]["reason"].as_str().unwrap_or_default().into())
            .collect()
    }

    // One declaration for the bus's two-machine contracts: the CI delta caps
    // new test declarations.
    #[test]
    fn a_question_crosses_machines_and_its_close_follows() {
        let primary = test_primary::start();
        let remote = &primary.remote;
        ensure_schema(remote).unwrap();
        let (ta, tb) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let a = Host {
            machine: "m-A".into(),
            host: "hosta".into(),
            root: ta.path().into(),
        };
        let b = Host {
            machine: "m-B".into(),
            host: "hostb".into(),
            root: tb.path().into(),
        };

        // B asks; A's beat copies it once, stamped with B's host, beside
        // A's own question of the same id.
        ask(&a, "q-1");
        ask(&b, "q-1");
        assert_eq!(beat(remote, &b).unwrap().published, 1);
        let first = beat(remote, &a).unwrap();
        assert_eq!(
            (first.copied, first.peer_hosts),
            (1, vec!["hostb".to_string()])
        );
        assert_eq!(
            beat(remote, &a).unwrap(),
            Beat {
                peer_hosts: vec!["hostb".into()],
                ..Beat::default()
            }
        );
        let copy = fold(&a.index()).asked["q-1@hostb"].clone();
        assert_eq!(
            (copy["data"]["host"].as_str(), peer_origin(&copy)),
            (Some("hostb"), Some("m-B"))
        );

        // B climbs it; the climbed row replaces the primary record and A's
        // copy follows.
        let mut climbed = fold(&b.index()).asked["q-1"].clone();
        climbed["data"]["via"] = json!([{"holder_name": "tomas", "evidence": "read PR 12"}]);
        crate::provider_cap::append_questions_row(&b.index(), &climbed).unwrap();
        assert_eq!(beat(remote, &b).unwrap().published, 1);
        assert_eq!(beat(remote, &a).unwrap().copied, 1);
        assert_eq!(climbs(&fold(&a.index()).asked["q-1@hostb"]), 1);

        // B closes it; the close publishes and A's copy follows.
        let close = json!({"ts": "2026-10-27T00:01:00Z", "type": "operator_question_closed",
            "source": "test", "data": {"question_id": "q-1", "answer": "yes"}});
        crate::provider_cap::append_questions_row(&b.index(), &close).unwrap();
        assert_eq!(beat(remote, &b).unwrap().published, 1);
        assert_eq!(beat(remote, &a).unwrap().closed, 1);
        assert_eq!(
            reasons(&a),
            vec!["closed on hostb: answered: yes".to_string()]
        );

        // A copy whose origin row is gone from a clean pull closes as gone.
        ask(&b, "q-2");
        beat(remote, &b).unwrap();
        assert_eq!(beat(remote, &a).unwrap().copied, 1);
        let db = primary.db.lock().unwrap();
        db.execute("DELETE FROM fleet_question WHERE question_id = 'q-2'", [])
            .unwrap();
        drop(db);
        assert_eq!(beat(remote, &a).unwrap().closed, 1);
        assert!(reasons(&a).contains(&"gone from hostb".to_string()));
    }

    // One declaration for the answer-carrier contract, mirroring the
    // question test above: the answer crosses, the origin applies once,
    // the close follows back.
    #[test]
    fn a_copy_answer_reaches_the_asking_machine_and_its_close_follows() {
        let primary = test_primary::start();
        let remote = &primary.remote;
        ensure_schema(remote).unwrap();
        let (ta, tb) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let a = Host {
            machine: "m-A".into(),
            host: "hosta".into(),
            root: ta.path().into(),
        };
        let b = Host {
            machine: "m-B".into(),
            host: "hostb".into(),
            root: tb.path().into(),
        };

        // A asks; B copies. The copy carries the origin stamps.
        ask(&a, "q-1");
        assert_eq!(beat(remote, &a).unwrap().published, 1);
        assert_eq!(beat(remote, &b).unwrap().copied, 1);
        let copy = fold(&b.index()).asked["q-1@hosta"].clone();
        assert_eq!(
            (copy["data"]["host"].as_str(), peer_origin(&copy)),
            (Some("hosta"), Some("m-A"))
        );

        // B answers its copy the way the door writes the row, then beats:
        // the answer publishes once to the carrier.
        let answer = json!({"ts": "2026-10-27T00:01:00Z", "type": "attention_answer",
            "source": "test", "data": {"item_id": "q-1@hosta", "sink": "mux", "option": 2,
            "words": "", "done": false, "answered_at": "2026-10-27T00:01:00Z",
            "authority": "sink", "attested_by": "mux", "mapped_by": "needs_door",
            "superseded": false, "delegate": false, "decision_id": null}});
        crate::provider_cap::append_questions_row(&b.index(), &answer).unwrap();
        assert_eq!(beat(remote, &b).unwrap().answers_published, 1);
        assert_eq!(
            beat(remote, &b).unwrap(),
            Beat {
                peer_hosts: vec!["hosta".into()],
                ..Beat::default()
            },
            "a second beat republishes nothing"
        );

        // A's beat applies the answer through the shared seam exactly once:
        // the row lands keyed to the origin id, sink names the peer.
        assert_eq!(beat(remote, &a).unwrap().answers_applied, 1);
        let applied_row = fold(&a.index()).answers["q-1"].clone();
        assert_eq!(applied_row["data"]["sink"], json!("peer:hostb"));
        assert_eq!(applied_row["data"]["attested_by"], json!("m-B"));
        assert_eq!(
            beat(remote, &a).unwrap().answers_applied,
            0,
            "a second A beat applies nothing"
        );

        // The close follows: A closes its question, B's copy closes.
        let close = json!({"ts": "2026-10-27T00:02:00Z", "type": "operator_question_closed",
            "source": "test", "data": {"question_id": "q-1", "answer": "option 2"}});
        crate::provider_cap::append_questions_row(&a.index(), &close).unwrap();
        assert_eq!(beat(remote, &a).unwrap().published, 1);
        assert_eq!(beat(remote, &b).unwrap().closed, 1);
        assert_eq!(
            reasons(&b),
            vec!["closed on hosta: answered: option 2".to_string()]
        );
    }
}
