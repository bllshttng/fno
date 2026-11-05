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
//! An answer the origin cannot take comes back the same way: the origin
//! stamps `refused` with its reason, and the answering machine's beat tells
//! the user once and stamps `reported_at`.

use crate::store_remote::{Remote, SqlValue};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

/// A closed row leaves the primary this long after its close.
pub const RETAIN_SECS: i64 = 7 * 24 * 3600;

/// How long a carried answer may keep failing its write on the asking machine
/// before the user hears. A busy answer lock clears in seconds; this does not.
const APPLY_PATIENCE_SECS: i64 = 600;

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
refused TEXT, \
reported_at INTEGER, \
PRIMARY KEY (machine_id, question_id))";

const ANSWER_ADDED_COLUMNS: [(&str, &str); 2] = [
    (
        "refused",
        "ALTER TABLE fleet_question_answer ADD COLUMN refused TEXT",
    ),
    (
        "reported_at",
        "ALTER TABLE fleet_question_answer ADD COLUMN reported_at INTEGER",
    ),
];

const IMAGE_DDL: &str = "CREATE TABLE IF NOT EXISTS fleet_question_image (\
machine_id TEXT NOT NULL, \
question_id TEXT NOT NULL, \
seq INTEGER NOT NULL, \
name TEXT NOT NULL, \
bytes BLOB NOT NULL, \
PRIMARY KEY (machine_id, question_id, seq))";

const NOW: &str = "(SELECT strftime('%s','now'))";

pub fn ensure_schema(remote: &Remote) -> Result<(), String> {
    remote.script(&format!("{DDL}; {ANSWER_DDL}; {IMAGE_DDL}"))?;
    let reply = remote.execute(
        "SELECT name FROM pragma_table_info('fleet_question_answer')",
        &[],
    )?;
    let present: HashSet<&str> = reply
        .rows
        .iter()
        .filter_map(|r| r.first()?.text())
        .collect();
    for (column, alter) in ANSWER_ADDED_COLUMNS {
        if !present.contains(column) {
            match remote.execute(alter, &[]) {
                Err(e) if !e.contains("duplicate column name") => return Err(e),
                _ => {}
            }
        }
    }
    Ok(())
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
/// root). The home is the agents home the store belongs to: the apply half
/// records an applied peer answer through it, so the row lands in this
/// machine's journal and the answer lock stays beside it. Tests build one
/// per fake machine. `notify` tells the user, and returns whether it landed.
pub(crate) struct Host {
    pub machine: String,
    pub host: String,
    pub root: PathBuf,
    pub home: crate::paths::AgentsHome,
    pub notify: Box<dyn Fn(&str, &str) -> bool>,
}

impl Host {
    fn live() -> Self {
        let home = crate::paths::AgentsHome::from_env();
        let index = crate::provider_cap::questions_path(&home);
        Host {
            machine: crate::claims::machine_id(),
            host: crate::mail::address::normalize_host(&crate::claims::hostname()),
            root: index
                .parent()
                .map(Path::to_path_buf)
                .unwrap_or_else(|| PathBuf::from(".fno")),
            home,
            notify: Box::new(|title, body| {
                crate::operator_notice::notify_operator_confirmed(title, body, None)
            }),
        }
    }

    fn index(&self) -> PathBuf {
        self.root.join("questions.jsonl")
    }
}

/// The local fold: the first asked record and the first close, by id, plus
/// the unsuperseded answer rows, by item id, and the evidence of each ladder
/// that gave up, by item id.
struct Fold {
    asked: HashMap<String, Value>,
    closed: HashMap<String, Value>,
    answers: HashMap<String, Value>,
    failed: HashMap<String, String>,
}

fn fold(index: &Path) -> Fold {
    let mut out = Fold {
        asked: HashMap::new(),
        closed: HashMap::new(),
        answers: HashMap::new(),
        failed: HashMap::new(),
    };
    let text = crate::event_store::journal_text(
        index,
        &[
            "operator_question",
            "operator_question_closed",
            "attention_answer",
            "attention_delivery",
        ],
    );
    for event in text
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
    {
        let data = &event["data"];
        if event["type"].as_str() == Some("attention_delivery") {
            if let (Some("failed"), Some(id)) = (data["outcome"].as_str(), data["item_id"].as_str())
            {
                let evidence = data["evidence"].as_str().unwrap_or("its ladder gave up");
                out.failed.insert(id.to_string(), evidence.to_string());
            }
            continue;
        }
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
    pub answers_refused: usize,
    pub refusals_reported: usize,
    /// Shots this beat shipped to the primary, and ask-time copies that no
    /// longer read from this machine's disk. A missing shot is counted,
    /// never silent.
    pub images_published: usize,
    pub images_missing: usize,
    /// Shots a copied question landed on this machine's disk.
    pub images_materialized: usize,
    pub peer_hosts: Vec<String>,
}

/// One beat: publish this machine's questions and closes, pull the peers',
/// publish answers this machine recorded for copies, apply answers recorded
/// for this machine's questions, tell the user which of this machine's answers
/// the asking machine refused, then drop rows past the retention.
pub(crate) fn beat(remote: &Remote, me: &Host) -> Result<Beat, String> {
    if me.machine.is_empty() {
        return Err("no machine id; the question bus needs one".into());
    }
    let published = publish(remote, me)?;
    let (images_published, images_missing) = publish_images(remote, me)?;
    let (copied, closed, images_materialized, peer_hosts) = pull(remote, me)?;
    let answers_published = publish_answers(remote, me)?;
    let (answers_applied, refused_now) = apply_answers(remote, me)?;
    let answers_refused = refused_now + refuse_failed_doors(remote, me)?;
    let refusals_reported = report_refusals(remote, me)?;
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
    remote.execute(
        "DELETE FROM fleet_question_image WHERE NOT EXISTS (\
SELECT 1 FROM fleet_question f WHERE f.machine_id = fleet_question_image.machine_id \
AND f.question_id = fleet_question_image.question_id)",
        &[],
    )?;
    Ok(Beat {
        published,
        copied,
        closed,
        answers_published,
        answers_applied,
        answers_refused,
        refusals_reported,
        images_published,
        images_missing,
        images_materialized,
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

/// Publish the shots of this machine's own open questions: the ask-time
/// copies live only on this machine's disk, so a peer can never judge the
/// paint until the primary holds the bytes. One metadata read decides the
/// writes, so a quiet beat sends nothing. Returns (shipped, missing).
fn publish_images(remote: &Remote, me: &Host) -> Result<(usize, usize), String> {
    let local = fold(&me.index());
    let reply = remote.execute(
        "SELECT question_id, seq FROM fleet_question_image WHERE machine_id = ?",
        &[SqlValue::Text(me.machine.clone())],
    )?;
    let shipped: HashSet<(String, i64)> = reply
        .rows
        .iter()
        .filter_map(|r| {
            let qid = r.first()?.text()?;
            match r.get(1)? {
                SqlValue::Integer(seq) => Some((qid.to_string(), *seq)),
                _ => None,
            }
        })
        .collect();
    let mine = SqlValue::Text(me.machine.clone());
    let mut steps: Vec<(String, Vec<SqlValue>)> = Vec::new();
    let mut missing = 0;
    for (qid, event) in local.asked.iter().filter(|(_, e)| is_own(e, &me.machine)) {
        if local.closed.contains_key(qid) {
            continue;
        }
        let Some(images) = event["data"]["images"].as_array() else {
            continue;
        };
        for (i, img) in images.iter().enumerate() {
            let Some(path) = img.as_str() else { continue };
            let seq = i as i64;
            if shipped.contains(&(qid.clone(), seq)) {
                continue;
            }
            let name = Path::new(path)
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            match std::fs::read(path) {
                Ok(bytes) => steps.push((
                    "INSERT OR IGNORE INTO fleet_question_image \
(machine_id, question_id, seq, name, bytes) VALUES (?, ?, ?, ?, ?)"
                        .to_string(),
                    vec![
                        mine.clone(),
                        SqlValue::Text(qid.clone()),
                        SqlValue::Integer(seq),
                        SqlValue::Text(name),
                        SqlValue::Blob(bytes),
                    ],
                )),
                Err(_) => missing += 1,
            }
        }
    }
    if !steps.is_empty() {
        remote
            .transaction(&steps)
            .map_err(|(_, error)| format!("publish_images: {error}"))?;
    }
    Ok((steps.len(), missing))
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

/// What the asking machine did with one carried answer.
enum Applied {
    /// The row is in this machine's journal, now or from a beat whose stamp
    /// never reached the primary.
    Done,
    /// The answer can never apply; the answering machine hears why.
    Refused(String),
    /// The write failed; the row retries next beat.
    Retry(String),
}

fn apply_one(me: &Host, local: &Fold, answerer: &str, record: &Value) -> Applied {
    const EARLIER: &str = "an earlier answer won";
    let Some(origin_id) = record["data"]["origin_question_id"].as_str() else {
        return Applied::Refused("the answer row names no question".into());
    };
    let own = local.asked.get(origin_id);
    if !own.is_some_and(|event| is_own(event, &me.machine)) {
        return Applied::Refused("this machine no longer holds the question".into());
    }
    if let Some(won) = local.answers.get(origin_id) {
        return if won["data"]["attested_by"].as_str() == Some(answerer) {
            Applied::Done
        } else {
            Applied::Refused(EARLIER.into())
        };
    }
    if let Some(close) = local.closed.get(origin_id) {
        return Applied::Refused(format!("it closed first: {}", close_reason(close)));
    }
    let answer = record_to_answer(record);
    let answerer_host = record["data"]["host"].as_str().unwrap_or(answerer);
    match crate::attention_arm::append_answer_row(
        &me.home,
        origin_id,
        &format!("peer:{answerer_host}"),
        &answer,
        "sink",
        "question_bus",
        answerer,
    ) {
        Ok((_, false)) => Applied::Done,
        Ok((_, true)) => Applied::Refused(EARLIER.into()),
        Err(e) => Applied::Retry(e),
    }
}

/// The origin's half: apply each peer answer to its own question through the
/// same seam a local mux answer takes, then stamp `applied_at` so the row is
/// applied exactly once. An answer that can never apply stamps `refused`
/// instead, so the answering machine can tell the user. A failed write retries
/// until `APPLY_PATIENCE_SECS`, then refuses. Returns (applied, refused).
fn apply_answers(remote: &Remote, me: &Host) -> Result<(usize, usize), String> {
    let reply = remote.execute(
        &format!(
            "SELECT machine_id, question_id, CAST({NOW} - created_at AS TEXT) \
FROM fleet_question_answer \
WHERE origin_machine = ? AND applied_at IS NULL AND refused IS NULL"
        ),
        &[SqlValue::Text(me.machine.clone())],
    )?;
    if reply.rows.is_empty() {
        return Ok((0, 0));
    }
    let local = fold(&me.index());
    // A row addressed here means this machine asked, so an empty fold is a
    // failed read (the journal read degrades to empty), never a lost question.
    if local.asked.is_empty() {
        return Ok((0, 0));
    }
    let mut steps: Vec<(String, Vec<SqlValue>)> = Vec::new();
    let (mut applied, mut refused) = (0usize, 0usize);
    for row in &reply.rows {
        let text = |i: usize| row.get(i).and_then(SqlValue::text);
        let (Some(answerer), Some(copy_id)) = (text(0), text(1)) else {
            continue;
        };
        // The record rides a second read, one row at a time: a full table of
        // records in one reply crosses the primary's reply limit.
        let Some(record) = fetch_answer_record(remote, answerer, copy_id)? else {
            continue;
        };
        // The carrier row's machine_id is the answerer, not the origin: a
        // stamp must match the insert keys.
        let keys = vec![
            SqlValue::Text(answerer.to_string()),
            SqlValue::Text(copy_id.to_string()),
        ];
        let why = match apply_one(me, &local, answerer, &record) {
            Applied::Done => {
                applied += 1;
                steps.push((
                    format!(
                        "UPDATE fleet_question_answer SET applied_at = {NOW} \
WHERE machine_id = ? AND question_id = ?"
                    ),
                    keys,
                ));
                continue;
            }
            Applied::Refused(why) => why,
            Applied::Retry(error) => {
                let age: i64 = text(2).and_then(|t| t.parse().ok()).unwrap_or(0);
                if age < APPLY_PATIENCE_SECS {
                    eprintln!("question-bus: apply {copy_id}: {error}");
                    continue;
                }
                format!("it could not record the answer: {error}")
            }
        };
        refused += 1;
        steps.push(refusal_step(why, keys));
    }
    if !steps.is_empty() {
        remote
            .transaction(&steps)
            .map_err(|(_, error)| format!("apply_answers: {error}"))?;
    }
    Ok((applied, refused))
}

fn refusal_step(why: String, keys: Vec<SqlValue>) -> (String, Vec<SqlValue>) {
    let mut params = vec![SqlValue::Text(why)];
    params.extend(keys);
    (
        "UPDATE fleet_question_answer SET refused = ? WHERE machine_id = ? AND question_id = ?"
            .to_string(),
        params,
    )
}

/// An applied answer can still lose at this machine's door: the attention
/// arm's clear refuses it until its ladder gives up, and the question stays
/// open. That failed delivery row is the refusal the answering machine never
/// sees on its own. A failure on a closed question is a failed wake, and the
/// answer stands. Reads the primary only when such a ladder exists.
fn refuse_failed_doors(remote: &Remote, me: &Host) -> Result<usize, String> {
    let local = fold(&me.index());
    if local.failed.keys().all(|id| local.closed.contains_key(id)) {
        return Ok(0);
    }
    let reply = remote.execute(
        "SELECT machine_id, question_id FROM fleet_question_answer \
WHERE origin_machine = ? AND applied_at IS NOT NULL AND refused IS NULL",
        &[SqlValue::Text(me.machine.clone())],
    )?;
    let mut steps: Vec<(String, Vec<SqlValue>)> = Vec::new();
    for row in &reply.rows {
        let text = |i: usize| row.get(i).and_then(SqlValue::text);
        let (Some(answerer), Some(copy)) = (text(0), text(1)) else {
            continue;
        };
        let origin_id = copy.rsplit_once('@').map_or(copy, |(id, _)| id);
        let Some(evidence) = local.failed.get(origin_id) else {
            continue;
        };
        if !local.closed.contains_key(origin_id) {
            let keys = vec![
                SqlValue::Text(answerer.to_string()),
                SqlValue::Text(copy.to_string()),
            ];
            steps.push(refusal_step(evidence.clone(), keys));
        }
    }
    if !steps.is_empty() {
        remote
            .transaction(&steps)
            .map_err(|(_, error)| format!("refuse_failed_doors: {error}"))?;
    }
    Ok(steps.len())
}

/// The answering machine's half: one notice per refused answer, stamped only
/// after it lands, so a failed notice retries and a landed one never repeats.
fn report_refusals(remote: &Remote, me: &Host) -> Result<usize, String> {
    let mine = SqlValue::Text(me.machine.clone());
    let reply = remote.execute(
        "SELECT question_id, refused FROM fleet_question_answer \
WHERE machine_id = ? AND refused IS NOT NULL AND reported_at IS NULL",
        std::slice::from_ref(&mine),
    )?;
    let mut reported = 0;
    for row in &reply.rows {
        let text = |i: usize| row.get(i).and_then(SqlValue::text);
        let (Some(copy), Some(why)) = (text(0), text(1)) else {
            continue;
        };
        let (origin_id, host) = copy
            .rsplit_once('@')
            .unwrap_or((copy, "the asking machine"));
        let title = format!("Your answer to {copy} did not apply");
        let body = format!(
            "{host} did not take your answer to {copy}: {why}. If it is still open, answer \
{origin_id} on {host}."
        );
        // A failed notice can hold its 15 s bound, so the rest wait a beat.
        if !(me.notify)(&title, &body) {
            break;
        }
        remote.execute(
            &format!(
                "UPDATE fleet_question_answer SET reported_at = {NOW} \
WHERE machine_id = ? AND question_id = ?"
            ),
            &[mine.clone(), SqlValue::Text(copy.to_string())],
        )?;
        reported += 1;
    }
    Ok(reported)
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
/// materialized shots, peer hosts seen).
fn pull(remote: &Remote, me: &Host) -> Result<(usize, usize, usize, Vec<String>), String> {
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
    let mut images_materialized = 0;
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
            images_materialized += materialize_images(remote, me, machine, qid, &id, &mut record);
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
            drop_copy_files(me, &id);
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
        drop_copy_files(me, id);
        closed += 1;
    }
    hosts.sort();
    Ok((copied, closed, images_materialized, hosts))
}

fn fetch_record(remote: &Remote, machine: &str, qid: &str) -> Result<Option<Value>, String> {
    let reply = remote.execute(
        "SELECT record FROM fleet_question WHERE machine_id = ? AND question_id = ?",
        &[SqlValue::Text(machine.into()), SqlValue::Text(qid.into())],
    )?;
    let text = reply.rows.first().and_then(|r| r.first()?.text());
    Ok(text.and_then(|t| serde_json::from_str(t).ok()))
}

/// Write a copied question's shots onto this machine's disk under
/// copy-id-prefixed names, and point the copy's `images` at them, so every
/// local reader (the projection, the viewer, the board row) reads files
/// that exist here. A shot whose bytes the primary lacks keeps its origin
/// path: the vault re-root may still rescue it. Returns how many landed.
fn materialize_images(
    remote: &Remote,
    me: &Host,
    origin_machine: &str,
    qid: &str,
    copy: &str,
    record: &mut Value,
) -> usize {
    let Some(images) = record["data"]["images"].as_array() else {
        return 0;
    };
    if images.is_empty() {
        return 0;
    }
    let screens = me.root.join("screens");
    if std::fs::create_dir_all(&screens).is_err() {
        return 0;
    }
    let origin: Vec<Value> = images.clone();
    let mut landed = 0;
    let mut remapped: Vec<Value> = Vec::with_capacity(images.len());
    // One read per shot: a question's bytes together can cross the
    // primary's reply limit, the way fetch_record reads one row at a time.
    for (i, img) in images.iter().enumerate() {
        let fallback = img.as_str().unwrap_or_default().to_string();
        let fetched = remote.execute(
            "SELECT name, bytes FROM fleet_question_image \
WHERE machine_id = ? AND question_id = ? AND seq = ?",
            &[
                SqlValue::Text(origin_machine.into()),
                SqlValue::Text(qid.into()),
                SqlValue::Integer(i as i64),
            ],
        );
        let local = fetched.ok().and_then(|reply| {
            let row = reply.rows.first()?;
            let name = row.first()?.text()?;
            let SqlValue::Blob(bytes) = row.get(1)? else {
                return None;
            };
            let dest = screens.join(format!("{copy}-{}-{name}", i + 1));
            std::fs::write(&dest, bytes).ok()?;
            let abs = std::fs::canonicalize(&dest).unwrap_or(dest);
            Some(abs.to_string_lossy().into_owned())
        });
        match local {
            Some(path) => {
                landed += 1;
                remapped.push(json!(path));
            }
            None => remapped.push(json!(fallback)),
        }
    }
    if let Some(data) = record.get_mut("data").and_then(Value::as_object_mut) {
        data.insert("origin_images".into(), json!(origin));
        data.insert("images".into(), Value::Array(remapped));
    }
    landed
}

/// Delete a copy's materialized screenshots: the copy closed, so nothing
/// here reads its files any more.
fn drop_copy_files(me: &Host, copy: &str) {
    let Ok(entries) = std::fs::read_dir(me.root.join("screens")) else {
        return;
    };
    let prefix = format!("{copy}-");
    for entry in entries.flatten() {
        if entry.file_name().to_string_lossy().starts_with(&prefix) {
            let _ = std::fs::remove_file(entry.path());
        }
    }
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
        Ok(b) => json!({"at": at, "published": b.published, "images_missing": b.images_missing,
            "peer_hosts": b.peer_hosts,
            "answers_refused": b.answers_refused, "refusals_reported": b.refusals_reported,
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
    if let Ok(b) = &outcome {
        if b.answers_refused + b.refusals_reported > 0 {
            eprintln!(
                "question-bus: refused {} reported {}",
                b.answers_refused, b.refusals_reported
            );
        }
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

    type Notices = std::rc::Rc<std::cell::RefCell<Vec<String>>>;

    fn machine(id: &str, host: &str, dir: &tempfile::TempDir, notices: &Notices) -> Host {
        let notices = notices.clone();
        Host {
            machine: id.into(),
            host: host.into(),
            root: dir.path().into(),
            home: crate::paths::AgentsHome::at(dir.path().join("agents")),
            notify: Box::new(move |_, body| {
                notices.borrow_mut().push(body.to_string());
                true
            }),
        }
    }

    /// The row the door writes for an answer on a copy.
    fn answer_copy(me: &Host, copy_id: &str) {
        let answer = json!({"ts": "2026-10-27T00:01:00Z", "type": "attention_answer",
            "source": "test", "data": {"item_id": copy_id, "sink": "mux", "option": 2,
            "words": "", "done": false, "answered_at": "2026-10-27T00:01:00Z",
            "authority": "sink", "attested_by": "mux", "mapped_by": "needs_door",
            "superseded": false, "delegate": false, "decision_id": null}});
        crate::provider_cap::append_questions_row(&me.index(), &answer).unwrap();
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
        let notices = Notices::default();
        let a = machine("m-A", "hosta", &ta, &notices);
        let b = machine("m-B", "hostb", &tb, &notices);

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
        let notices = Notices::default();
        let mut a = machine("m-A", "hosta", &ta, &notices);
        let b = machine("m-B", "hostb", &tb, &notices);

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
        answer_copy(&b, "q-1@hosta");
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

        // A lost stamp replays: the row in A's journal is this answer's own,
        // so A stamps it again and refuses nothing.
        let unstamp = |id: &str| {
            let db = primary.db.lock().unwrap();
            db.execute(
                "UPDATE fleet_question_answer SET applied_at = NULL WHERE question_id = ?1",
                [id],
            )
            .unwrap();
        };
        unstamp("q-1@hosta");
        let replay = beat(remote, &a).unwrap();
        assert_eq!((replay.answers_applied, replay.answers_refused), (1, 0));
        assert!(notices.borrow().is_empty());

        // Carry one more answer to A: asked, copied, answered, published.
        let carry = |qid: &str, a: &Host| {
            ask(a, qid);
            beat(remote, a).unwrap();
            beat(remote, &b).unwrap();
            answer_copy(&b, &format!("{qid}@hosta"));
            assert_eq!(beat(remote, &b).unwrap().answers_published, 1);
        };
        let close = |me: &Host, qid: &str| {
            let close = json!({"ts": "2026-10-27T00:02:00Z", "type": "operator_question_closed",
                "source": "test", "data": {"question_id": qid, "answer": "local"}});
            crate::provider_cap::append_questions_row(&me.index(), &close).unwrap();
        };

        // The question closed before the answer arrived: B hears once, naming
        // the question and the reason, and a later beat stays quiet.
        carry("q-2", &a);
        close(&a, "q-2");
        assert_eq!(beat(remote, &a).unwrap().answers_refused, 1);
        assert_eq!(beat(remote, &b).unwrap().refusals_reported, 1);
        assert_eq!(beat(remote, &b).unwrap().refusals_reported, 0);
        assert_eq!(
            notices.borrow().as_slice(),
            [
                "hosta did not take your answer to q-2@hosta: it closed first: answered: local. \
If it is still open, answer q-2 on hosta."
            ]
        );

        // Applied, then A's own door refused it until the ladder gave up: the
        // failed delivery comes back. A failed wake on a closed question does
        // not.
        let gave_up = |qid: &str| {
            json!({"ts": "2026-10-27T00:03:00Z", "type": "attention_delivery", "source": "test",
                "data": {"item_id": qid, "rung": "none", "outcome": "failed",
                "evidence": "the clear kept failing: clear exited 3: outstanding: refused: no"}})
        };
        carry("q-3", &a);
        carry("q-4", &a);
        // The second carry's own beat already applied the first answer.
        assert_eq!(beat(remote, &a).unwrap().answers_applied, 1);
        for qid in ["q-3", "q-4"] {
            crate::provider_cap::append_questions_row(&a.index(), &gave_up(qid)).unwrap();
        }
        close(&a, "q-4");
        assert_eq!(beat(remote, &a).unwrap().answers_refused, 1);
        beat(remote, &b).unwrap();
        assert_eq!(notices.borrow().len(), 2);
        assert!(notices.borrow()[1].contains("q-3@hosta: the clear kept failing"));

        // A failed write keeps the row for the next beat, until the patience
        // runs out and the answer comes back refused. The home sits under a
        // plain file, so the answer lock cannot open.
        carry("q-5", &a);
        let plain = ta.path().join("plain");
        std::fs::write(&plain, "").unwrap();
        let good_home = std::mem::replace(
            &mut a.home,
            crate::paths::AgentsHome::at(plain.join("agents")),
        );
        let held = beat(remote, &a).unwrap();
        assert_eq!((held.answers_applied, held.answers_refused), (0, 0));
        {
            let db = primary.db.lock().unwrap();
            db.execute(
                "UPDATE fleet_question_answer SET created_at = created_at - 3600 \
WHERE question_id = 'q-5@hosta'",
                [],
            )
            .unwrap();
        }
        assert_eq!(beat(remote, &a).unwrap().answers_refused, 1);
        a.home = good_home;

        // A notice that fails stays owed, and the next beat sends it.
        let silent = Host {
            notify: Box::new(|_, _| false),
            ..machine("m-B", "hostb", &tb, &notices)
        };
        assert_eq!(beat(remote, &silent).unwrap().refusals_reported, 0);
        assert_eq!(beat(remote, &b).unwrap().refusals_reported, 1);

        // A dead primary keeps the answer in the journal; the first beat after
        // it answers publishes the row.
        ask(&a, "q-6");
        beat(remote, &a).unwrap();
        beat(remote, &b).unwrap();
        answer_copy(&b, "q-6@hosta");
        assert!(beat(&test_primary::dead(), &b).is_err());
        assert_eq!(beat(remote, &b).unwrap().answers_published, 1);

        // An unreadable journal reads as empty: the row waits, unrefused.
        let tc = tempfile::tempdir().unwrap();
        let unread = machine("m-A", "hosta", &tc, &notices);
        assert_eq!(beat(remote, &unread).unwrap().answers_refused, 0);
        assert_eq!(beat(remote, &a).unwrap().answers_applied, 1);
    }

    // A primary from before the refusal columns gains them on the next start.
    #[test]
    fn ensure_schema_adds_the_refusal_columns_to_an_old_answer_table() {
        let primary = test_primary::start();
        primary
            .db
            .lock()
            .unwrap()
            .execute_batch(
                "CREATE TABLE fleet_question_answer (machine_id TEXT NOT NULL, \
question_id TEXT NOT NULL, origin_machine TEXT NOT NULL, answer_host TEXT NOT NULL, \
record TEXT NOT NULL, created_at INTEGER NOT NULL, applied_at INTEGER, \
PRIMARY KEY (machine_id, question_id))",
            )
            .unwrap();
        ensure_schema(&primary.remote).unwrap();
        ensure_schema(&primary.remote).unwrap();
        let reply = primary
            .remote
            .execute(
                "SELECT COUNT(*) FROM pragma_table_info('fleet_question_answer') \
WHERE name IN ('refused', 'reported_at')",
                &[],
            )
            .unwrap();
        assert_eq!(reply.rows[0][0], SqlValue::Integer(2));
    }

    // One declaration for the image-carrier contract: the origin's beat
    // ships the ask's shots, a copy materializes them under local names,
    // and the bytes die with the row.
    #[test]
    fn a_question_screenshots_cross_machines_and_die_with_the_row() {
        let primary = test_primary::start();
        let remote = &primary.remote;
        ensure_schema(remote).unwrap();
        let (ta, tb) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let notices = Notices::default();
        let a = machine("m-A", "hosta", &ta, &notices);
        let b = machine("m-B", "hostb", &tb, &notices);

        // A asks with two real shots and one path that never existed.
        let screens = ta.path().join("screens");
        std::fs::create_dir_all(&screens).unwrap();
        let one = screens.join("q-9-1-shot.png");
        let two = screens.join("q-9-2-shot.png");
        std::fs::write(&one, b"png-one").unwrap();
        std::fs::write(&two, b"png-two").unwrap();
        let event = json!({"ts": "2026-10-27T00:00:00Z", "type": "operator_question",
            "source": "test", "data": {"question_id": "q-9", "question": "judge this paint",
            "asker": "w1", "images": [one, two, "/gone/q-9-3-shot.png"]}});
        crate::provider_cap::append_questions_row(&a.index(), &event).unwrap();

        // A's beat ships the record and the readable bytes; the missing
        // shot is counted, never silent.
        let shipped = beat(remote, &a).unwrap();
        assert_eq!(shipped.published, 1);
        assert_eq!(shipped.images_published, 2);
        assert_eq!(shipped.images_missing, 1);
        let rows: Vec<(i64, String, Vec<u8>)> = {
            let db = primary.db.lock().unwrap();
            let mut stmt = db
                .prepare(
                    "SELECT seq, name, bytes FROM fleet_question_image \
                 WHERE machine_id = 'm-A' AND question_id = 'q-9' ORDER BY seq",
                )
                .unwrap();
            stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
                .unwrap()
                .collect::<Result<Vec<_>, _>>()
                .unwrap()
        };
        assert_eq!(
            rows,
            vec![
                (0, "q-9-1-shot.png".into(), b"png-one".to_vec()),
                (1, "q-9-2-shot.png".into(), b"png-two".to_vec()),
            ]
        );

        // B's beat copies the question and materializes the shots under
        // copy-id names; the copy row names the local files and keeps the
        // origin's paths for provenance.
        let pulled = beat(remote, &b).unwrap();
        assert_eq!(pulled.copied, 1);
        assert_eq!(pulled.images_materialized, 2);
        let copy = fold(&b.index()).asked["q-9@hosta"].clone();
        let images: Vec<String> = copy["data"]["images"]
            .as_array()
            .unwrap()
            .iter()
            .map(|p| p.as_str().unwrap().to_string())
            .collect();
        assert_eq!(images.len(), 3);
        assert_eq!(std::fs::read(&images[0]).unwrap(), b"png-one".to_vec());
        assert_eq!(std::fs::read(&images[1]).unwrap(), b"png-two".to_vec());
        assert!(images[0].contains("q-9@hosta-1-"));
        assert!(images[1].contains("q-9@hosta-2-"));
        // The unreadable shot keeps its origin path: the vault re-root may
        // still rescue it, and the provenance array holds every origin path.
        assert_eq!(images[2], "/gone/q-9-3-shot.png");
        assert_eq!(
            copy["data"]["origin_images"][2].as_str(),
            Some("/gone/q-9-3-shot.png")
        );

        // A closes; B's copy follows and the materialized files go.
        let close = json!({"ts": "2026-10-27T00:01:00Z", "type": "operator_question_closed",
            "source": "test", "data": {"question_id": "q-9", "answer": "yes"}});
        crate::provider_cap::append_questions_row(&a.index(), &close).unwrap();
        assert_eq!(beat(remote, &a).unwrap().published, 1);
        assert_eq!(beat(remote, &b).unwrap().closed, 1);
        let left: Vec<String> = std::fs::read_dir(tb.path().join("screens"))
            .unwrap()
            .filter_map(|e| e.unwrap().file_name().into_string().ok())
            .filter(|n| n.starts_with("q-9@hosta-"))
            .collect();
        assert!(left.is_empty(), "{left:?}");

        // The primary's bytes die with the question row.
        {
            let db = primary.db.lock().unwrap();
            db.execute(
                "DELETE FROM fleet_question WHERE machine_id = 'm-A' AND question_id = 'q-9'",
                [],
            )
            .unwrap();
        }
        beat(remote, &b).unwrap();
        let left_rows: i64 = {
            let db = primary.db.lock().unwrap();
            db.query_row("SELECT COUNT(*) FROM fleet_question_image", [], |r| {
                r.get(0)
            })
            .unwrap()
        };
        assert_eq!(left_rows, 0);
    }
}
