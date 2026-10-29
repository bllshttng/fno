//! The cross-host mail bus: one `fno_mail_bus` table on the shared primary.
//!
//! A send to `<name>@<host>` writes one row. Each host's daemon
//! pulls the rows that name it, injects them through the local send path with
//! its holds intact, and marks them delivered. The table lives only on the
//! primary: it never joins `SHARED_PREFIXES` or `SHARED_TABLES`, and no local
//! schema carries it. The primary's AUTOINCREMENT `seq` is the only ordering,
//! the same store-clock-beats-host-clocks rule the claim leases use.
//!
//! Each row carries the sending machine's public key and its signature over
//! [`signed_bytes`]. The receiver checks both before it injects
//! ([`crate::machine_key::check`]), so a row edited on the primary, or one
//! that claims a host whose pinned key did not sign it, never lands.

use crate::store_remote::{Remote, Reply, SqlValue};

/// How long a pending row waits for its host before the expire leg flips it.
pub const MAIL_TTL_SECS: i64 = 7 * 24 * 3600;

pub const PENDING: &str = "pending";
pub const DELIVERED: &str = "delivered";
pub const EXPIRED: &str = "expired";
pub const REFUSED: &str = "refused";

const DDL: &str = "CREATE TABLE IF NOT EXISTS fno_mail_bus (\
seq INTEGER PRIMARY KEY AUTOINCREMENT, \
msg_id TEXT NOT NULL UNIQUE, \
from_id TEXT NOT NULL, \
from_host TEXT NOT NULL, \
to_name TEXT NOT NULL, \
to_host TEXT NOT NULL, \
payload TEXT NOT NULL, \
raw INTEGER NOT NULL DEFAULT 0, \
subject TEXT, \
created_at INTEGER NOT NULL, \
expire_at INTEGER NOT NULL, \
status TEXT NOT NULL DEFAULT 'pending', \
delivered_at INTEGER, \
pubkey TEXT, \
sig TEXT)";

/// The signature columns, added to a table created before they existed.
/// SQLite has no `ADD COLUMN IF NOT EXISTS`, so each runs alone and a
/// duplicate-column error from a host that won the race reads as success.
const ADDED_COLUMNS: [(&str, &str); 2] = [
    ("pubkey", "ALTER TABLE fno_mail_bus ADD COLUMN pubkey TEXT"),
    ("sig", "ALTER TABLE fno_mail_bus ADD COLUMN sig TEXT"),
];

/// Hosts compare equal across machines that spell themselves differently
/// (`iMac2D2.local` from gethostname, `imac2d2` from a user typing). One
/// normalizer runs on write and on pull (operator ruling, 2026-10-10).
pub fn normalize_host(raw: &str) -> String {
    let lower = raw.trim().to_ascii_lowercase();
    match lower.strip_suffix(".local") {
        Some(base) if !base.is_empty() => base.to_string(),
        _ => lower,
    }
}

/// A unique bus msg_id. 12 hex like the Python bus ids (`fmail-…`), minted
/// from clock, pid and a process-lifetime counter, hashed: no rand
/// dependency, and uniqueness only has to survive collisions on one primary,
/// where the UNIQUE index turns a collision into an `INSERT OR IGNORE` miss
/// the sender reads as "already queued".
pub fn mint_msg_id() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let mut hash: u64 = 0xcbf29ce484222325;
    for byte in [
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0),
        std::process::id() as u64,
        COUNTER.fetch_add(1, Ordering::Relaxed),
    ]
    .iter()
    .flat_map(|v| v.to_le_bytes())
    {
        hash ^= byte as u64;
        hash = hash.wrapping_mul(0x100000001b3);
    }
    // The mask is load-bearing: `012x` is a minimum width, not a cap, so an
    // unmasked u64 prints up to 16 hex digits. Low 48 bits = exactly 12.
    format!("fmail-{:012x}", hash & 0xffff_ffff_ffff)
}

/// Create the bus table, or add the signature columns it lacks. The column
/// read comes first and writes nothing, so a send that finds the table
/// current pays one round trip and never waits on the primary's write lock.
/// `IF NOT EXISTS`, so every host can run it at first use; concurrent
/// creates are idempotent on sqld like on SQLite.
pub fn ensure_schema(remote: &Remote) -> Result<(), String> {
    let columns: Vec<String> = remote
        .execute("SELECT name FROM pragma_table_info('fno_mail_bus')", &[])?
        .rows
        .iter()
        .filter_map(|row| row.first().and_then(|v| v.text()).map(str::to_string))
        .collect();
    if columns.is_empty() {
        return remote.script(DDL);
    }
    for (column, alter) in ADDED_COLUMNS {
        if columns.iter().any(|c| c == column) {
            continue;
        }
        match remote.execute(alter, &[]) {
            Err(e) if !e.contains("duplicate column name") => return Err(e),
            _ => {}
        }
    }
    Ok(())
}

/// The bytes a row's signature covers: every field the sender writes and
/// the receiver acts on, hosts already normalized. A JSON array, so no
/// field can bleed into the next.
pub fn signed_bytes(
    msg_id: &str,
    from_id: &str,
    from_host: &str,
    to_name: &str,
    to_host: &str,
    payload: &str,
    raw: bool,
    subject: &str,
) -> Vec<u8> {
    serde_json::json!([msg_id, from_id, from_host, to_name, to_host, payload, raw, subject])
        .to_string()
        .into_bytes()
}

/// What one send wrote. `duplicated` marks an `INSERT OR IGNORE` miss: the
/// msg_id was already queued (a retry after an ambiguous timeout), which the
/// sender reports as queued, not as a failure (at-least-once, edge 5).
#[derive(Debug, PartialEq, Eq)]
pub struct InsertOutcome {
    pub seq: i64,
    pub duplicated: bool,
}

/// Insert one row. The clock read rides inside the statement as scalar
/// subqueries, so the primary stamps `created_at`/`expire_at` and one request
/// carries the whole write.
pub fn insert(
    remote: &Remote,
    msg_id: &str,
    from_id: &str,
    from_host: &str,
    to_name: &str,
    to_host: &str,
    payload: &str,
    raw: bool,
    subject: Option<&str>,
) -> Result<InsertOutcome, String> {
    let insert = "INSERT OR IGNORE INTO fno_mail_bus \
(msg_id, from_id, from_host, to_name, to_host, payload, raw, subject, pubkey, sig, \
created_at, expire_at) \
VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, (SELECT strftime('%s','now')), \
(SELECT strftime('%s','now')) + ?)";
    let (from_host, to_host) = (normalize_host(from_host), normalize_host(to_host));
    let subject = subject.unwrap_or_default();
    let key = crate::machine_key::own()?;
    let signature = key.sign(&signed_bytes(
        msg_id, from_id, &from_host, to_name, &to_host, payload, raw, subject,
    ));
    let args = vec![
        SqlValue::Text(msg_id.to_string()),
        SqlValue::Text(from_id.to_string()),
        SqlValue::Text(from_host),
        SqlValue::Text(to_name.to_string()),
        SqlValue::Text(to_host),
        SqlValue::Text(payload.to_string()),
        SqlValue::Integer(raw as i64),
        SqlValue::Text(subject.to_string()),
        SqlValue::Text(key.public_key()),
        SqlValue::Text(signature),
        SqlValue::Integer(MAIL_TTL_SECS),
    ];
    let reply = remote.execute(insert, &args)?;
    if reply.affected == 1 {
        let seq = read_seq(remote, msg_id)?;
        return Ok(InsertOutcome {
            seq,
            duplicated: false,
        });
    }
    // Either a duplicate msg_id or a lost race with nothing; the UNIQUE index
    // is the only way affected reads 0 here, so read the row that won.
    let seq = read_seq(remote, msg_id)?;
    Ok(InsertOutcome {
        seq,
        duplicated: true,
    })
}

fn read_seq(remote: &Remote, msg_id: &str) -> Result<i64, String> {
    let reply = remote.execute(
        "SELECT seq FROM fno_mail_bus WHERE msg_id = ?",
        &[SqlValue::Text(msg_id.to_string())],
    )?;
    reply
        .rows
        .first()
        .and_then(|row| row.first())
        .and_then(|v| v.integer())
        .ok_or_else(|| format!("bus row {msg_id} vanished after insert"))
}

/// One pending row pulled for this host, in primary-seq order.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct BusRow {
    pub seq: i64,
    pub msg_id: String,
    pub from_id: String,
    pub to_name: String,
    pub payload: String,
    pub raw: bool,
    pub expire_at: i64,
    pub from_host: String,
    pub to_host: String,
    pub subject: String,
    /// The sender's public key and signature; `None` from a sender that
    /// predates signing.
    pub pubkey: Option<String>,
    pub sig: Option<String>,
}

impl BusRow {
    /// The bytes this row's signature must cover.
    pub fn signed_bytes(&self) -> Vec<u8> {
        signed_bytes(
            &self.msg_id,
            &self.from_id,
            &self.from_host,
            &self.to_name,
            &self.to_host,
            &self.payload,
            self.raw,
            &self.subject,
        )
    }
}

const PENDING_COLUMNS: &str =
    "seq, msg_id, from_id, to_name, payload, raw, expire_at, from_host, to_host, subject, pubkey, sig";

fn row_from_values(values: &[SqlValue]) -> Option<BusRow> {
    let optional = |i: usize| values.get(i).and_then(|v| v.text()).map(str::to_string);
    Some(BusRow {
        seq: values.first()?.integer()?,
        msg_id: values.get(1)?.text()?.to_string(),
        from_id: values.get(2)?.text()?.to_string(),
        to_name: values.get(3)?.text()?.to_string(),
        payload: values.get(4)?.text()?.to_string(),
        raw: values.get(5)?.integer()? != 0,
        expire_at: values.get(6)?.integer()?,
        from_host: optional(7).unwrap_or_default(),
        to_host: optional(8).unwrap_or_default(),
        subject: optional(9).unwrap_or_default(),
        pubkey: optional(10),
        sig: optional(11),
    })
}

fn rows_from_reply(reply: &Reply) -> Vec<BusRow> {
    reply
        .rows
        .iter()
        .filter_map(|v| row_from_values(v))
        .collect()
}

/// The pending rows addressed to `host`, oldest first. The host argument is
/// already normalized by the caller (the same normalizer the write ran).
pub fn fetch_pending(remote: &Remote, host: &str) -> Result<Vec<BusRow>, String> {
    let sql = format!(
        "SELECT {PENDING_COLUMNS} FROM fno_mail_bus \
WHERE to_host = ? AND status = 'pending' ORDER BY seq"
    );
    let reply = remote.execute(&sql, &[SqlValue::Text(host.to_string())])?;
    Ok(rows_from_reply(&reply))
}

/// Flip one row to `status`. Guarded on `pending`, so a pull that delivered a
/// row and an expire leg that raced it cannot double-write; the loser's
/// affected reads 0 and it leaves the winner alone.
pub fn mark(remote: &Remote, msg_id: &str, status: &str) -> Result<u64, String> {
    let sql = if status == DELIVERED {
        "UPDATE fno_mail_bus SET status = ?, delivered_at = (SELECT strftime('%s','now')) \
WHERE msg_id = ? AND status = 'pending'"
    } else {
        "UPDATE fno_mail_bus SET status = ? WHERE msg_id = ? AND status = 'pending'"
    };
    let reply = remote.execute(
        sql,
        &[
            SqlValue::Text(status.to_string()),
            SqlValue::Text(msg_id.to_string()),
        ],
    )?;
    Ok(reply.affected)
}

/// Flip every pending row past its TTL to expired. Runs on any host's daemon;
/// every flip is guarded on pending, so hosts racing this leg agree.
pub fn expire_stale(remote: &Remote) -> Result<u64, String> {
    let reply = remote.execute(
        "UPDATE fno_mail_bus SET status = 'expired' \
WHERE status = 'pending' AND expire_at < (SELECT strftime('%s','now'))",
        &[],
    )?;
    Ok(reply.affected)
}

/// The send-side status read: this host's own rows, newest last, so a sender
/// learns delivered, expired, or refused per msg_id.
pub fn sent_rows(remote: &Remote, from_host: &str) -> Result<Vec<(String, String)>, String> {
    let reply = remote.execute(
        "SELECT msg_id, status FROM fno_mail_bus WHERE from_host = ? ORDER BY seq LIMIT 200",
        &[SqlValue::Text(normalize_host(from_host).to_string())],
    )?;
    Ok(reply
        .rows
        .iter()
        .filter_map(|row| match (row.first(), row.get(1)) {
            (Some(id), Some(status)) => Some((id.text()?.to_string(), status.text()?.to_string())),
            _ => None,
        })
        .collect())
}

/// The fleet's name-to-host binding, read off the bus itself: a row names a
/// name on a host both as sender (`from_id`'s `/name` tail) and as recipient
/// (`to_name`/`to_host`). The newest seq wins, so a name that moved machines
/// resolves to where it was seen last. `None` when the store knows nothing.
pub fn latest_host_for_name(remote: &Remote, name: &str) -> Result<Option<String>, String> {
    let reply = remote.execute(
        "SELECT CASE \
WHEN lower(substr(from_id, instr(from_id, '/') + 1)) = lower(?1) THEN from_host \
ELSE to_host END \
FROM fno_mail_bus \
WHERE (instr(from_id, '/') > 0 AND lower(substr(from_id, instr(from_id, '/') + 1)) = lower(?1)) \
OR lower(to_name) = lower(?1) \
ORDER BY seq DESC LIMIT 1",
        &[SqlValue::Text(name.to_string())],
    )?;
    Ok(reply
        .rows
        .first()
        .and_then(|row| row.first())
        .and_then(|v| v.text())
        .map(|h| normalize_host(h)))
}

#[cfg(test)]
mod tests {
    use super::*;

    // One declaration, every contract: the CI delta caps new test
    // declarations, so the bus's unit contracts share one table-style body.
    #[test]
    fn bus_units_guard_their_contracts() {
        // Host normalization: the same spelling from gethostname and from a
        // user's typing must store as one host (write and pull).
        assert_eq!(normalize_host("iMac2D2.local"), "imac2d2");
        assert_eq!(normalize_host("imac2d2"), "imac2d2");
        assert_eq!(normalize_host("  MacBook-Pro-3.local "), "macbook-pro-3");
        // ".local" alone is not a host suffix worth stripping: an empty
        // base would make every empty-ish spelling collide on "".
        assert_eq!(normalize_host(".local"), ".local");
        assert_eq!(normalize_host(""), "");
        // A well-formed reply row parses into the pull arm's view.
        let row = row_from_values(&[
            SqlValue::Integer(7),
            SqlValue::Text("fmail-abc".into()),
            SqlValue::Text("bb16@imac2d2/04b5621d".into()),
            SqlValue::Text("nemo".into()),
            SqlValue::Text("hello".into()),
            SqlValue::Integer(0),
            SqlValue::Integer(1_800_000_000),
        ])
        .expect("well-formed row parses");
        assert_eq!(row.seq, 7);
        assert_eq!(row.to_name, "nemo");
        assert!(!row.raw);
        // Ids stay in the fmail-12hex shape and never repeat in-process.
        let a = mint_msg_id();
        let b = mint_msg_id();
        assert!(a.starts_with("fmail-"));
        assert_eq!(a.len(), 6 + 12);
        assert_ne!(a, b);
    }

    #[test]
    fn a_signed_row_injects_and_a_row_edited_on_the_primary_is_refused() {
        let primary = crate::store_remote::test_primary::start();
        let remote = &primary.remote;
        ensure_schema(remote).unwrap();
        // A second host's create races the first: the ALTERs read as done.
        ensure_schema(remote).unwrap();
        for id in ["fmail-clean", "fmail-edited"] {
            insert(
                remote,
                id,
                "bb16:iMac2D2.local/quill",
                "iMac2D2.local",
                "nemo",
                "r2d2",
                "hi",
                false,
                None,
            )
            .unwrap();
        }
        primary
            .db
            .lock()
            .unwrap()
            .execute(
                "UPDATE fno_mail_bus SET payload = 'rm -rf' WHERE msg_id = 'fmail-edited'",
                [],
            )
            .unwrap();
        let home = tempfile::tempdir().unwrap();
        let mut injected = Vec::new();
        let rows = fetch_pending(remote, "r2d2").unwrap();
        assert!(rows.iter().all(|r| r.pubkey.is_some() && r.sig.is_some()));
        let delivered = crate::mail_arm::pull_rows_with(remote, rows, home.path(), &mut |row| {
            injected.push(row.msg_id.clone());
            true
        });
        assert_eq!((delivered, injected), (1, vec!["fmail-clean".to_string()]));
        let status = |id: &str| {
            remote
                .execute(
                    "SELECT status FROM fno_mail_bus WHERE msg_id = ?",
                    &[SqlValue::Text(id.into())],
                )
                .unwrap()
                .rows[0][0]
                .clone()
        };
        assert_eq!(status("fmail-edited"), SqlValue::Text(REFUSED.into()));
        // The first signed row pinned the sender's host.
        let pins = std::fs::read_to_string(crate::machine_key::pins_path(home.path())).unwrap();
        assert!(pins.contains("\"imac2d2\""), "{pins}");
    }
}
