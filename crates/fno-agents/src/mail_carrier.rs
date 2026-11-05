//! The mail carrier: machine-local agent mail on the shared primary.
//!
//! A same-host send writes no bus row (`mail_remote`), so mail between two
//! agents on one machine lives only in that machine's chats store. Each
//! daemon publishes its own machine-local rows of the last 7 days to
//! `fleet_chat` on the primary: one row per (machine_id, msg_id), written
//! only by the machine that holds the mail, signed with its machine key, and
//! deleted by that machine past 7 days. `mail-threads` on every other machine
//! reads them and folds them as `name@host` threads.
//!
//! A row this machine pulled from the bus, or sent to it, stays off the
//! carrier: the bus fold already shows it. The beat also writes a small
//! stamp beside the chats store when the other machines' mail moves, so the
//! Messages tab's stat poll sees a change it cannot read over the network.

use crate::store_remote::{Remote, SqlValue};
use serde_json::{json, Value};
use std::collections::HashSet;
use std::path::{Path, PathBuf};

/// A carried row leaves the primary this long after it was written.
pub const RETAIN_SECS: i64 = 7 * 24 * 3600;

/// The most carrier rows one projection reads.
pub const READ_ROWS: i64 = 4000;

/// Rows per insert transaction: a first beat carries a week of mail.
const CHUNK: usize = 200;

const DDL: &str = "CREATE TABLE IF NOT EXISTS fleet_chat (\
machine_id TEXT NOT NULL, \
msg_id TEXT NOT NULL, \
host TEXT NOT NULL, \
owner TEXT NOT NULL, \
from_name TEXT NOT NULL, \
to_name TEXT NOT NULL, \
ts TEXT NOT NULL, \
created_at INTEGER NOT NULL, \
subject TEXT NOT NULL, \
body TEXT NOT NULL, \
pubkey TEXT NOT NULL, \
sig TEXT NOT NULL, \
PRIMARY KEY (machine_id, msg_id)); \
CREATE INDEX IF NOT EXISTS fleet_chat_created ON fleet_chat (created_at)";

pub fn ensure_schema(remote: &Remote) -> Result<(), String> {
    remote.script(DDL)
}

/// One carried message, as the primary holds it.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct CarrierRow {
    pub machine_id: String,
    pub msg_id: String,
    pub host: String,
    pub owner: String,
    pub from_name: String,
    pub to_name: String,
    pub ts: String,
    pub created_at: i64,
    pub subject: String,
    pub body: String,
    pub pubkey: String,
    pub sig: String,
}

impl CarrierRow {
    /// The bytes the signature covers: every field a reader paints. A JSON
    /// array, so no field can bleed into the next.
    pub fn signed_bytes(&self) -> Vec<u8> {
        json!([
            self.msg_id,
            self.machine_id,
            self.host,
            self.owner,
            self.from_name,
            self.to_name,
            self.ts,
            self.subject,
            self.body
        ])
        .to_string()
        .into_bytes()
    }
}

/// Who this machine is, and where its mail lives. Tests build one per fake
/// machine.
pub(crate) struct Me {
    pub machine: String,
    pub host: String,
    pub owner: String,
    pub chats: PathBuf,
    /// Bus ids the pull arm delivered here.
    pub seen: HashSet<String>,
    pub registry: Vec<Value>,
}

impl Me {
    fn live() -> Self {
        Me {
            machine: crate::claims::machine_id(),
            host: crate::name_ledger::local_bus_host(),
            owner: crate::identity::person(),
            chats: crate::chats::chats_dir(),
            seen: crate::mail_arm::local_bus_facts().1,
            registry: crate::mail_threads::registry_rows(),
        }
    }
}

/// Is this store row mail between two agents of this machine? A pulled row
/// keeps its remote `user:host/name` sender, and a send to another machine
/// names `name@host`; both ride the bus. Broadcasts, system arms and the
/// hold digest are not pair mail.
fn machine_local(v: &Value, seen: &HashSet<String>) -> bool {
    let field = |f: &str| v.get(f).and_then(Value::as_str).unwrap_or("");
    let (from, to) = (field("from"), field("to"));
    v.get("type").and_then(Value::as_str) == Some("message")
        && field("kind") != "announce"
        && !from.is_empty()
        && !to.is_empty()
        && !from.contains([':', '@', '/'])
        && !to.contains('@')
        && !field("to_key").contains('@')
        && !to.starts_with("fleet:")
        && !crate::system_sender::is_system_sender(from)
        && !crate::system_sender::is_system_sender(to)
        && !seen.contains(field("id"))
}

/// An address's display name: its registry row's name, else the address.
fn name_of(raw: &str, registry: &[Value]) -> String {
    let key = crate::mail_threads::resolve_key(raw, registry);
    crate::mail_threads::registry_lookup(registry, &key)
        .and_then(|row| row.get("name").and_then(Value::as_str))
        .filter(|n| !n.is_empty())
        .unwrap_or(raw)
        .to_string()
}

fn epoch_of(ts: &str) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(ts)
        .ok()
        .map(|t| t.timestamp())
}

/// The machine-local rows of chats files written at or after `mtime_floor`,
/// sent at or after `since`, that `published` does not hold yet. Unsigned.
pub(crate) fn local_rows(
    me: &Me,
    since: i64,
    mtime_floor: u64,
    published: &HashSet<String>,
) -> Vec<CarrierRow> {
    let Ok(rd) = std::fs::read_dir(&me.chats) else {
        return Vec::new();
    };
    let mut rows = Vec::new();
    for entry in rd.flatten() {
        let path = entry.path().join("messages.jsonl");
        let fresh = path
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .is_some_and(|d| d.as_secs() >= mtime_floor && d.as_secs() as i64 >= since);
        if !fresh {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        for line in text.lines() {
            let Ok(v) = serde_json::from_str::<Value>(line) else {
                continue;
            };
            if !machine_local(&v, &me.seen) {
                continue;
            }
            let field = |f: &str| v.get(f).and_then(Value::as_str).unwrap_or("").to_string();
            let (id, ts) = (field("id"), field("ts"));
            let Some(created_at) = epoch_of(&ts).filter(|t| *t >= since) else {
                continue;
            };
            if id.is_empty() || published.contains(&id) {
                continue;
            }
            let from_session = field("from_session");
            let sender = if from_session.is_empty() {
                field("from")
            } else {
                from_session
            };
            let from_name = match name_of(&sender, &me.registry) {
                n if n == sender => field("from"),
                n => n,
            };
            rows.push(CarrierRow {
                machine_id: me.machine.clone(),
                msg_id: id,
                host: me.host.clone(),
                owner: me.owner.clone(),
                from_name,
                to_name: name_of(&field("to"), &me.registry),
                ts,
                created_at,
                subject: field("subject"),
                body: field("body"),
                ..Default::default()
            });
        }
    }
    rows.sort_by(|a, b| a.ts.cmp(&b.ts));
    rows
}

/// What this process remembers between beats: the ids already on the
/// primary, read once, and the newest file time the last walk covered.
#[derive(Default)]
pub(crate) struct Cursor {
    published: Option<HashSet<String>>,
    walked_at: u64,
}

/// What one beat did.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Beat {
    pub published: usize,
    pub expired: u64,
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// One beat: expire this machine's old rows, publish its new machine-local
/// rows, then refresh the stamp. Only this machine's rows are deleted.
pub(crate) fn beat(remote: &Remote, me: &Me, cursor: &mut Cursor) -> Result<Beat, String> {
    let now = now_secs();
    let since = now as i64 - RETAIN_SECS;
    let mine = SqlValue::Text(me.machine.clone());
    let expired = remote
        .execute(
            "DELETE FROM fleet_chat WHERE machine_id = ? AND created_at < ?",
            &[mine.clone(), SqlValue::Integer(since)],
        )?
        .affected;
    if cursor.published.is_none() {
        let reply = remote.execute(
            "SELECT msg_id FROM fleet_chat WHERE machine_id = ?",
            &[mine],
        )?;
        let ids = reply
            .rows
            .iter()
            .filter_map(|r| Some(r.first()?.text()?.to_string()))
            .collect();
        cursor.published = Some(ids);
        cursor.walked_at = 0;
    }
    let published = cursor.published.get_or_insert_with(HashSet::new);
    // A file written during the last walk may hold rows that walk missed.
    let rows = local_rows(me, since, cursor.walked_at.saturating_sub(60), published);
    let key = crate::machine_key::own()?;
    let mut count = 0;
    for chunk in rows.chunks(CHUNK) {
        let steps: Vec<(String, Vec<SqlValue>)> = chunk
            .iter()
            .map(|r| {
                let sig = key.sign(&r.signed_bytes());
                (
                    "INSERT OR IGNORE INTO fleet_chat (machine_id, msg_id, host, owner, \
from_name, to_name, ts, created_at, subject, body, pubkey, sig) \
VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)"
                        .to_string(),
                    vec![
                        SqlValue::Text(r.machine_id.clone()),
                        SqlValue::Text(r.msg_id.clone()),
                        SqlValue::Text(r.host.clone()),
                        SqlValue::Text(r.owner.clone()),
                        SqlValue::Text(r.from_name.clone()),
                        SqlValue::Text(r.to_name.clone()),
                        SqlValue::Text(r.ts.clone()),
                        SqlValue::Integer(r.created_at),
                        SqlValue::Text(r.subject.clone()),
                        SqlValue::Text(r.body.clone()),
                        SqlValue::Text(key.public_key()),
                        SqlValue::Text(sig),
                    ],
                )
            })
            .collect();
        remote
            .transaction(&steps)
            .map_err(|(_, e)| format!("publish: {e}"))?;
        published.extend(chunk.iter().map(|r| r.msg_id.clone()));
        count += chunk.len();
    }
    cursor.walked_at = now;
    write_stamp(remote, me)?;
    Ok(Beat {
        published: count,
        expired,
    })
}

/// The change signal the Messages tab's stat poll reads: `db/mail/carrier/
/// stamp`, beside the chats store, so the client crate finds it through the
/// same `chats_dir` ladder.
pub fn stamp_path(chats: &Path) -> PathBuf {
    chats
        .parent()
        .unwrap_or(chats)
        .join("carrier")
        .join("stamp")
}

/// Rewrite the stamp only when the other machines' carried mail, or the
/// bus, moved: the file's mtime is the signal.
fn write_stamp(remote: &Remote, me: &Me) -> Result<(), String> {
    let reply = remote.execute(
        "SELECT (SELECT COUNT(*) FROM fleet_chat WHERE machine_id != ?), \
(SELECT COALESCE(MAX(created_at), 0) FROM fleet_chat WHERE machine_id != ?), \
(SELECT COALESCE(MAX(seq), 0) FROM fno_mail_bus)",
        &[
            SqlValue::Text(me.machine.clone()),
            SqlValue::Text(me.machine.clone()),
        ],
    )?;
    let facts: Vec<String> = reply
        .rows
        .first()
        .into_iter()
        .flatten()
        .map(|v| v.integer().unwrap_or(0).to_string())
        .collect();
    let body = facts.join(" ");
    let path = stamp_path(&me.chats);
    if std::fs::read_to_string(&path).ok().as_deref() == Some(body.as_str()) {
        return Ok(());
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("stamp: {e}"))?;
    }
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, &body)
        .and_then(|()| std::fs::rename(&tmp, &path))
        .map_err(|e| format!("stamp: {e}"))
}

/// One beat inside the mail arm's off-loop task, so it shares that arm's
/// cadence, its one-in-flight gate and its `store.remote_url` check.
pub fn tick(remote: &Remote) {
    static SCHEMA: std::sync::OnceLock<Result<(), String>> = std::sync::OnceLock::new();
    static CURSOR: std::sync::LazyLock<std::sync::Mutex<Cursor>> =
        std::sync::LazyLock::new(Default::default);
    if let Err(e) = SCHEMA.get_or_init(|| ensure_schema(remote)) {
        eprintln!("mail-carrier: {e}");
        return;
    }
    let mut cursor = CURSOR.lock().unwrap_or_else(|e| e.into_inner());
    match beat(remote, &Me::live(), &mut cursor) {
        Ok(b) if b.published as u64 + b.expired > 0 => {
            eprintln!(
                "mail-carrier: published {} expired {}",
                b.published, b.expired
            )
        }
        Ok(_) => {}
        // A down primary fails every beat; say each distinct error once.
        Err(e) => {
            static LAST: std::sync::Mutex<String> = std::sync::Mutex::new(String::new());
            let mut last = LAST.lock().unwrap_or_else(|e| e.into_inner());
            if *last != e {
                eprintln!("mail-carrier: {e}");
                *last = e;
            }
        }
    }
}

/// The other machines' carried rows of the last 7 days, newest `READ_ROWS`,
/// oldest first.
pub fn read_peer_rows(
    remote: &Remote,
    machine: &str,
    since: i64,
) -> Result<Vec<CarrierRow>, String> {
    let reply = remote.execute(
        "SELECT machine_id, msg_id, host, owner, from_name, to_name, ts, created_at, subject, \
body, pubkey, sig FROM fleet_chat WHERE created_at >= ? AND machine_id != ? \
ORDER BY created_at DESC LIMIT ?",
        &[
            SqlValue::Integer(since),
            SqlValue::Text(machine.to_string()),
            SqlValue::Integer(READ_ROWS),
        ],
    )?;
    let mut rows: Vec<CarrierRow> = reply
        .rows
        .iter()
        .filter_map(|v| {
            let text = |i: usize| {
                v.get(i)
                    .and_then(|x| x.text())
                    .unwrap_or_default()
                    .to_string()
            };
            Some(CarrierRow {
                machine_id: text(0),
                msg_id: v.get(1)?.text()?.to_string(),
                host: text(2),
                owner: text(3),
                from_name: text(4),
                to_name: text(5),
                ts: text(6),
                created_at: v.get(7)?.integer()?,
                subject: text(8),
                body: text(9),
                pubkey: text(10),
                sig: text(11),
            })
        })
        .collect();
    rows.reverse();
    Ok(rows)
}

/// The rows this machine may paint, as bus-shaped rows for the thread fold:
/// another machine's, the same person's, and signed by the key pinned for
/// its host. Each refused host is named once on stderr.
pub fn trusted(
    rows: &[CarrierRow],
    machine: &str,
    owner: &str,
    here: &[String],
    pins: &Path,
) -> Vec<crate::mail_bus::FleetRow> {
    let mut refused: HashSet<String> = HashSet::new();
    rows.iter()
        .filter(|r| r.machine_id != machine && r.owner == owner)
        .filter(|r| !here.contains(&crate::mail::address::normalize_host(&r.host)))
        .filter(|r| {
            let checked = crate::machine_key::check(
                pins,
                &r.host,
                Some((r.pubkey.as_str(), r.sig.as_str())),
                &r.signed_bytes(),
            );
            match checked {
                Ok(_) => true,
                Err(e) => {
                    if refused.insert(r.host.clone()) {
                        eprintln!("mail-threads: carrier rows from {} refused: {e:?}", r.host);
                    }
                    false
                }
            }
        })
        .map(|r| crate::mail_bus::FleetRow {
            msg_id: r.msg_id.clone(),
            from_id: format!("{}:{}/{}", r.owner, r.host, r.from_name),
            from_host: r.host.clone(),
            to_name: r.to_name.clone(),
            to_host: r.host.clone(),
            payload: r.body.clone(),
            subject: r.subject.clone(),
            created_at: r.created_at,
            status: "delivered".into(),
            pubkey: r.pubkey.clone(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store_remote::test_primary;

    fn line(id: &str, from: &str, to: &str, ts: &str) -> String {
        json!({"v": 1, "type": "message", "kind": "send", "id": id, "ts": ts,
            "from": from, "to": to, "to_key": to, "body": format!("body {id}")})
        .to_string()
    }

    fn machine(id: &str, host: &str, dir: &tempfile::TempDir) -> Me {
        Me {
            machine: id.into(),
            host: host.into(),
            owner: "bb16".into(),
            chats: dir.path().join("mail").join("chats"),
            seen: ["pulled".to_string()].into_iter().collect(),
            registry: vec![
                json!({"name": "ivy", "session_id": "aaaa1111-0000-0000-0000-000000000000"}),
            ],
        }
    }

    fn write_chat(me: &Me, lines: &[String]) {
        let dir = me.chats.join("chat-1");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("messages.jsonl"), lines.join("\n")).unwrap();
    }

    /// A same-machine pair crosses to another machine's fold; bus mail,
    /// broadcasts and system rows stay home; a peer reads only another
    /// machine's rows of its own person with a good signature; expiry
    /// deletes only the writer's own rows.
    #[test]
    fn same_machine_mail_crosses_and_bus_mail_stays_home() {
        let primary = test_primary::start();
        let remote = &primary.remote;
        ensure_schema(remote).unwrap();
        crate::mail_bus::ensure_schema(remote).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let a = machine("m-A", "imac2d2", &dir);
        let now = chrono::Utc::now();
        let ts = |ago: i64| {
            (now - chrono::Duration::seconds(ago))
                .format("%Y-%m-%dT%H:%M:%SZ")
                .to_string()
        };
        let mut announce: Value = serde_json::from_str(&line("ann", "ivy", "oak", &ts(5))).unwrap();
        announce["kind"] = json!("announce");
        write_chat(
            &a,
            &[
                line("local", "aaaa1111", "oak", &ts(10)),
                line("to-bus", "ivy", "milo@r2d2", &ts(9)),
                line("from-bus", "bb16:r2d2/milo", "ivy", &ts(8)),
                line("pulled", "milo", "ivy", &ts(7)),
                line("system", "fno/questions", "ivy", &ts(6)),
                announce.to_string(),
                line("old", "ivy", "oak", &ts(RETAIN_SECS + 100)),
            ],
        );
        let mut cursor = Cursor::default();
        let first = beat(remote, &a, &mut cursor).unwrap();
        assert_eq!(first.published, 1, "only the same-machine pair rides");
        assert_eq!(beat(remote, &a, &mut cursor).unwrap().published, 0);
        let stamp = std::fs::read_to_string(stamp_path(&a.chats)).unwrap();
        assert_eq!(stamp, "0 0 0", "no other machine's mail yet");

        let rows = read_peer_rows(remote, "m-B", 0).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(
            (rows[0].from_name.as_str(), rows[0].to_name.as_str()),
            ("ivy", "oak")
        );
        let pins = dir.path().join("pins.json");
        let folded = trusted(&rows, "m-B", "bb16", &["mbp".into()], &pins);
        assert_eq!(folded.len(), 1);
        assert_eq!(folded[0].from_id, "bb16:imac2d2/ivy");
        let mut projection = json!({"participants": [], "threads": []});
        let here = ["mbp".to_string()];
        let fold = crate::mail_threads::fold_fleet_rows;
        fold(
            &mut projection,
            &folded,
            &here,
            &HashSet::new(),
            &[],
            "carrier",
        );
        let thread = &projection["threads"][0];
        assert_eq!(
            thread["participants"],
            json!(["ivy@imac2d2", "oak@imac2d2"])
        );
        assert_eq!(thread["rows"][0]["origin"], "carrier");
        assert!(
            trusted(&rows, "m-A", "bb16", &[], &pins).is_empty(),
            "own rows"
        );
        assert!(
            trusted(&rows, "m-B", "eve", &[], &pins).is_empty(),
            "another person"
        );
        let mut edited = rows.clone();
        edited[0].body = "rm -rf".into();
        assert!(
            trusted(&edited, "m-B", "bb16", &[], &pins).is_empty(),
            "tampered"
        );

        // Machine B sees A's row move its stamp; expiry spares A's rows.
        let b_dir = tempfile::tempdir().unwrap();
        let b = machine("m-B", "mbp", &b_dir);
        primary
            .db
            .lock()
            .unwrap()
            .execute("UPDATE fleet_chat SET created_at = 1", [])
            .unwrap();
        let expired = beat(remote, &b, &mut Cursor::default()).unwrap().expired;
        assert_eq!(expired, 0, "B never deletes A's row");
        assert!(std::fs::read_to_string(stamp_path(&b.chats))
            .unwrap()
            .starts_with("1 1 "));
        assert_eq!(beat(remote, &a, &mut cursor).unwrap().expired, 1);
    }
}
