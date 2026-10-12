//! The cross-host mail bus: one `fno_mail_bus` table on the shared primary.
//!
//! A send to `<name>@<host>` writes one row. Each host's daemon
//! pulls the rows that name it, injects them through the local send path with
//! its holds intact, and marks them delivered. The table lives only on the
//! primary: it never joins `SHARED_PREFIXES` or `SHARED_TABLES`, and no local
//! schema carries it. The primary's AUTOINCREMENT `seq` is the only ordering,
//! the same store-clock-beats-host-clocks rule the claim leases use.

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
delivered_at INTEGER)";

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

/// Create the bus table. `IF NOT EXISTS`, so every host can run it at first
/// use; concurrent creates are idempotent on sqld like on SQLite.
pub fn ensure_schema(remote: &Remote) -> Result<(), String> {
    remote.script(DDL)
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
(msg_id, from_id, from_host, to_name, to_host, payload, raw, subject, created_at, expire_at) \
VALUES (?, ?, ?, ?, ?, ?, ?, ?, (SELECT strftime('%s','now')), \
(SELECT strftime('%s','now')) + ?)";
    let args = vec![
        SqlValue::Text(msg_id.to_string()),
        SqlValue::Text(from_id.to_string()),
        SqlValue::Text(normalize_host(from_host)),
        SqlValue::Text(to_name.to_string()),
        SqlValue::Text(normalize_host(to_host)),
        SqlValue::Text(payload.to_string()),
        SqlValue::Integer(raw as i64),
        SqlValue::Text(subject.unwrap_or_default().to_string()),
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
#[derive(Debug, PartialEq, Eq)]
pub struct BusRow {
    pub seq: i64,
    pub msg_id: String,
    pub from_id: String,
    pub to_name: String,
    pub payload: String,
    pub raw: bool,
    pub expire_at: i64,
}

const PENDING_COLUMNS: &str = "seq, msg_id, from_id, to_name, payload, raw, expire_at";

fn row_from_values(values: &[SqlValue]) -> Option<BusRow> {
    Some(BusRow {
        seq: values.first()?.integer()?,
        msg_id: values.get(1)?.text()?.to_string(),
        from_id: values.get(2)?.text()?.to_string(),
        to_name: values.get(3)?.text()?.to_string(),
        payload: values.get(4)?.text()?.to_string(),
        raw: values.get(5)?.integer()? != 0,
        expire_at: values.get(6)?.integer()?,
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
}
