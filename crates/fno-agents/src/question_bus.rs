//! The question bus: open questions shared across the machines of one store.
//!
//! One `fleet_question` table on the shared primary, in the mail bus's shape:
//! one row per (machine_id, question_id), written only by the machine that
//! asked, holding the raw `operator_question` record and its close. Every
//! other machine pulls those rows and appends a copy to its own
//! `questions.jsonl`, stamped `data.origin_machine` and `data.host`, so every
//! local reader (the list, the Waiting on you block, the mux tab) sees it with
//! no change. The table never joins `SHARED_TABLES` or `SHARED_PREFIXES`.

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

const NOW: &str = "(SELECT strftime('%s','now'))";

pub fn ensure_schema(remote: &Remote) -> Result<(), String> {
    remote.script(DDL)
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

/// The local fold: the first asked record and the first close, by id.
struct Fold {
    asked: HashMap<String, Value>,
    closed: HashMap<String, Value>,
}

fn fold(index: &Path) -> Fold {
    let mut out = Fold {
        asked: HashMap::new(),
        closed: HashMap::new(),
    };
    let text =
        crate::event_store::journal_text(index, &["operator_question", "operator_question_closed"]);
    for event in text
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
    {
        let Some(qid) = event["data"]["question_id"].as_str().map(str::to_string) else {
            continue;
        };
        let map = match event["type"].as_str() {
            Some("operator_question") => &mut out.asked,
            Some("operator_question_closed") => &mut out.closed,
            _ => continue,
        };
        map.entry(qid).or_insert(event);
    }
    out
}

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
    pub peer_hosts: Vec<String>,
}

/// One beat: publish this machine's questions and closes, pull the peers',
/// then drop rows closed past the retention.
pub(crate) fn beat(remote: &Remote, me: &Host) -> Result<Beat, String> {
    if me.machine.is_empty() {
        return Err("no machine id; the question bus needs one".into());
    }
    let published = publish(remote, me)?;
    let (copied, closed, peer_hosts) = pull(remote, me)?;
    remote.execute(
        &format!("DELETE FROM fleet_question WHERE closed_at < {NOW} - ?"),
        &[SqlValue::Integer(RETAIN_SECS)],
    )?;
    Ok(Beat {
        published,
        copied,
        closed,
        peer_hosts,
    })
}

/// One read of the primary's open rows for this machine decides the writes,
/// so a quiet beat sends one statement; the writes ride one transaction.
fn publish(remote: &Remote, me: &Host) -> Result<usize, String> {
    let local = fold(&me.index());
    let reply = remote.execute(
        "SELECT question_id FROM fleet_question WHERE machine_id = ? AND closed_at IS NULL",
        &[SqlValue::Text(me.machine.clone())],
    )?;
    let on_primary: HashSet<&str> = reply
        .rows
        .iter()
        .filter_map(|r| r.first()?.text())
        .collect();
    let mine = SqlValue::Text(me.machine.clone());
    let mut steps: Vec<(String, Vec<SqlValue>)> = Vec::new();
    for (qid, event) in local.asked.iter().filter(|(_, e)| is_own(e, &me.machine)) {
        let id = SqlValue::Text(qid.clone());
        match (local.closed.get(qid), on_primary.contains(qid.as_str())) {
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
        "SELECT machine_id, question_id, host, closed_at, close_reason \
FROM fleet_question WHERE machine_id != ?",
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
        if open && !local.asked.contains_key(&id) {
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
        "source": "question_bus",
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
}
