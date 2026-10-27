//! The shared backlog: with `store.remote_url` and `store.share_backlog` set,
//! the backlog tables live on the shared primary and this machine's graph.db
//! is a replica of them.
//!
//! Write: every backlog write opens through `backlog::open_connection`, which
//! calls [`attach`]. TEMP triggers copy each row change to a shared table into
//! the local `backlog_outbox`, in the writer's own transaction, so a commit
//! stays local and holds no lock across the network. [`flush`] then sends the
//! outbox to the primary in one request, under its own lock file, after the
//! graph lock is gone: one transaction of conditional statements, where each
//! update and delete matches every old column and each insert must not
//! collide. The statements run parent table first, so a parent row exists
//! before the update that names it. A row a peer changed first refuses the
//! whole batch. The take-back covers only the node the refusal named: the
//! primary's copy comes back for that node's rows, every other change in the
//! batch re-queues for the next flush, and an INSERT the primary refuses
//! keeps its local row. The take-back keeps the refused statements in
//! `backlog_refused`, and an operator notice (stderr plus a
//! `backlog_refused_flush` journal event) names the nodes and columns it
//! touched. So the primary decides, and no machine overwrites a peer's
//! change it never saw.
//!
//! Read: reads stay local. One daemon arm per machine runs [`flush`] and then
//! [`sync`], which applies the primary's change log to the replica.
//!
//! Guard: a store that shares carries a persistent trigger on each shared
//! table that calls `fno_backlog_writer()`. Only a build with this module
//! registers that function, so an older build's write to a shared table
//! fails at once instead of landing with no outbox record.
//!
//! Unset, [`attach`] drops any guard a past share left, so an older build
//! can write again, and opens no socket.

use crate::store_remote::{Remote, Reply, SqlValue};
use rusqlite::config::DbConfig;
use rusqlite::functions::FunctionFlags;
use rusqlite::{Connection, OptionalExtension, TransactionBehavior};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// The backlog tables. They live on the primary when sharing is on. A
/// parent comes before every table that references it: the primary enforces
/// foreign keys, so seed and publish write in this order.
pub(crate) const SHARED_TABLES: &[&str] = &[
    "harnesses",
    "models",
    "agent_sessions",
    "nodes",
    "nodes_raw",
    "node_dispatch",
    "node_provenance",
    "supersessions",
    "sessions",
    "comments",
    "encounters",
    "findings",
    "pull_requests",
    "relations",
    "relations_unresolved",
    "node_costs",
    "decisions",
    "node_decisions",
    "edges",
];

/// The position of `table` in `SHARED_TABLES`.
fn rank(table: &str) -> usize {
    SHARED_TABLES
        .iter()
        .position(|t| *t == table)
        .unwrap_or(SHARED_TABLES.len())
}

/// A refused flush's text starts with this.
pub const REFUSED: &str = "backlog write refused by the shared primary";

/// A refusal carrying this names a row a peer changed first: a conflict to
/// retry, not a fault.
pub const CHANGED: &str = "changed on the primary after this machine read it";

/// The SQLite text of the `backlog_cas` row-count CHECK the conditional
/// statements rely on. Kept as data so the operator text can say what it
/// means while the matcher tracks the engine's wording.
const CAS_GUARD: &str = "CHECK constraint failed: n = 1";

/// A stale-row refusal: the primary's CAS guard refused a statement because
/// the row moved under this machine. The take-back and sync already ran, so
/// a resend rebases on fresh rows; this is the one refusal a bounded resend
/// can still land.
pub(crate) fn is_stale_row_refusal(error: &str) -> bool {
    error.contains(CHANGED)
}

const CURSOR: &str = "backlog_share_cursor";
/// The last outbox id of a batch whose result never came back. The next
/// flush resends exactly that batch, so the primary can answer that it
/// already landed.
const SENDING: &str = "backlog_share_sending";
/// The function every guard trigger calls.
const WRITER: &str = "fno_backlog_writer";
const PAGE: i64 = 200;
pub const INTERVAL: Duration = Duration::from_secs(5);
/// How long the primary keeps a change-log row. A replica asleep longer
/// takes a snapshot on its next sync. Six hours is seven times the worst
/// cursor lag measured on the fleet (64 rows, 52 minutes, 2026-10-27); the
/// cursor gate in the prune keeps every row a live replica has not read
/// regardless of the window.
const KEEP: Duration = Duration::from_secs(6 * 3600);
/// One small prune a minute per machine: a big DELETE would hold the
/// primary's write lock as long as the stall it exists to end.
const PRUNE_EVERY: Duration = Duration::from_secs(60);
const PRUNE_ROWS: i64 = 100;
const FLUSH_WAIT: Duration = Duration::from_secs(30);

const OUTBOX_DDL: &str = "CREATE TABLE IF NOT EXISTS main.backlog_outbox (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  change TEXT NOT NULL
);";

/// The dead-letter side of the outbox: a refused batch's statements land
/// here, exactly as [`publish`] sent them, so a take-back never destroys the
/// only record of what this machine tried to write.
const REFUSED_DDL: &str = "CREATE TABLE IF NOT EXISTS main.backlog_refused (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  batch TEXT NOT NULL,
  at INTEGER NOT NULL,
  error TEXT NOT NULL,
  ops TEXT NOT NULL
);";

/// The primary-side bookkeeping. `backlog_cas.n` takes only 1, so a
/// conditional statement that matched no row fails its batch. A batch id
/// lands once, so a resent outbox never applies twice. The seed marker exists
/// only once the seed finished, so a write to a half-seeded primary refuses.
const PRIMARY_DDL: &str = "
CREATE TABLE IF NOT EXISTS backlog_cas (n INTEGER NOT NULL CHECK (n = 1));
CREATE TABLE IF NOT EXISTS backlog_changes (
  seq INTEGER PRIMARY KEY AUTOINCREMENT,
  origin TEXT NOT NULL,
  batch TEXT NOT NULL UNIQUE,
  at INTEGER NOT NULL,
  ops TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS backlog_cursors (
  origin TEXT PRIMARY KEY,
  seq INTEGER NOT NULL,
  at INTEGER NOT NULL
);";

#[derive(Clone, Copy, Debug, PartialEq)]
enum Op {
    Insert,
    Update,
    Delete,
}

impl Op {
    fn code(self) -> &'static str {
        match self {
            Op::Insert => "i",
            Op::Update => "u",
            Op::Delete => "d",
        }
    }

    fn parse(code: &str) -> Option<Self> {
        match code {
            "i" => Some(Op::Insert),
            "u" => Some(Op::Update),
            "d" => Some(Op::Delete),
            _ => None,
        }
    }
}

/// One row change: the table, the column names, and the row before and
/// after. An insert has no old row, a delete no new one.
#[derive(Clone, Debug, PartialEq)]
struct Change {
    op: Op,
    table: String,
    columns: Vec<String>,
    old: Vec<SqlValue>,
    new: Vec<SqlValue>,
}

impl Change {
    fn to_json(&self) -> Value {
        let values = |row: &[SqlValue]| row.iter().map(SqlValue::to_hrana).collect::<Vec<_>>();
        json!({"op": self.op.code(), "t": self.table, "c": self.columns,
               "o": values(&self.old), "n": values(&self.new)})
    }

    fn from_json(row: &Value) -> Result<Self, String> {
        let values = |v: &Value| -> Result<Vec<SqlValue>, String> {
            v.as_array()
                .into_iter()
                .flatten()
                .map(SqlValue::from_hrana)
                .collect()
        };
        Ok(Change {
            op: row["op"]
                .as_str()
                .and_then(Op::parse)
                .ok_or("change without an op")?,
            table: row["t"].as_str().ok_or("change without a table")?.into(),
            columns: row["c"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|c| c.as_str().map(str::to_string))
                .collect(),
            old: values(&row["o"])?,
            new: values(&row["n"])?,
        })
    }
}

#[cfg(test)]
thread_local! {
    static TEST_PRIMARY: std::cell::RefCell<Option<(Remote, std::path::PathBuf)>> =
        const { std::cell::RefCell::new(None) };
}

/// Test seam: share the store at `db` (a graph.db path) with `remote` on
/// this thread.
#[cfg(test)]
pub(crate) fn route_to_primary(route: Option<(Remote, std::path::PathBuf)>) {
    TEST_PRIMARY.with(|p| *p.borrow_mut() = route);
}

/// The primary that holds the backlog behind `graph`. Only this machine's
/// configured graph is shared; a space, a sandbox or a fixture never dials
/// out.
pub(crate) fn primary_for(graph: &Path) -> Result<Option<Remote>, String> {
    let db = crate::backlog::database_path(graph);
    #[cfg(test)]
    if let Some((remote, at)) = TEST_PRIMARY.with(|p| p.borrow().clone()) {
        return Ok((at == db).then_some(remote));
    }
    let Some(remote) = crate::store_remote::share_backlog()? else {
        return Ok(None);
    };
    let configured = crate::backlog::database_path(&crate::backlog::settings::graph_path());
    Ok((configured == db).then_some(remote))
}

/// Column names per shared table, in table order.
fn shared_columns(connection: &Connection) -> Result<BTreeMap<String, Vec<String>>, String> {
    let mut out = BTreeMap::new();
    for table in SHARED_TABLES {
        let columns = column_info(connection, table)?
            .into_iter()
            .map(|(name, _)| name)
            .collect::<Vec<_>>();
        if !columns.is_empty() {
            out.insert(table.to_string(), columns);
        }
    }
    Ok(out)
}

/// `(name, primary-key position)` per column; position 0 is not in the key.
fn column_info(connection: &Connection, table: &str) -> Result<Vec<(String, i64)>, String> {
    let mut statement = connection
        .prepare(&format!(
            "SELECT name, pk FROM pragma_table_info('{table}')"
        ))
        .map_err(|e| e.to_string())?;
    let rows = statement
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        })
        .map_err(|e| e.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string());
    rows
}

/// Mark `connection` as a writer that records its changes. Every store
/// connection this build opens calls it before its first write, key on or
/// off, so the guard triggers pass it.
pub(crate) fn register_writer(connection: &Connection) -> Result<(), String> {
    connection
        .create_scalar_function(WRITER, 0, FunctionFlags::SQLITE_DETERMINISTIC, |_| Ok(1))
        .map_err(|e| e.to_string())
}

/// Install the guard triggers on every shared table this store holds.
fn guard(connection: &Connection, columns: &BTreeMap<String, Vec<String>>) -> Result<(), String> {
    let mut ddl = String::new();
    for table in columns.keys() {
        for (op, event) in [("i", "INSERT"), ("u", "UPDATE"), ("d", "DELETE")] {
            ddl.push_str(&format!(
                "CREATE TRIGGER IF NOT EXISTS main.fno_share_guard_{table}_{op} BEFORE {event} \
                 ON \"{table}\" BEGIN SELECT {WRITER}(); END;\n"
            ));
        }
    }
    connection.execute_batch(&ddl).map_err(|e| e.to_string())
}

/// Drop the guard triggers: with sharing off, no write needs an outbox.
fn unguard(connection: &Connection) -> Result<(), String> {
    let names: Vec<String> = {
        let mut statement = connection
            .prepare(
                "SELECT name FROM sqlite_master WHERE type = 'trigger' \
                 AND name LIKE 'fno\\_share\\_guard\\_%' ESCAPE '\\'",
            )
            .map_err(|e| e.to_string())?;
        let names = statement
            .query_map([], |row| row.get(0))
            .map_err(|e| e.to_string())?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())?;
        names
    };
    let ddl: String = names
        .iter()
        .map(|name| format!("DROP TRIGGER IF EXISTS main.\"{name}\";\n"))
        .collect();
    connection.execute_batch(&ddl).map_err(|e| e.to_string())
}

/// Install the write path on a backlog connection when sharing is on.
///
/// TEMP triggers live in this connection only, so the shared tables keep
/// their schema. They hand each row to a Rust function, which keeps every
/// value's type, and write the result to the outbox in the same transaction.
/// (The preupdate hook and the session extension would capture the same
/// rows, but either one makes the SQLite build need libclang through
/// bindgen, on every machine, key on or off.)
pub(crate) fn attach(connection: &Connection, graph: &Path) -> Result<(), String> {
    if primary_for(graph)?.is_none() {
        // Only the key going off drops the guard. With the key on, a store
        // this process does not share may still be the shared one.
        return match crate::store_remote::share_backlog()? {
            None => unguard(connection),
            Some(_) => Ok(()),
        };
    }
    connection
        .execute_batch(OUTBOX_DDL)
        .map_err(|e| e.to_string())?;
    let columns = shared_columns(connection)?;
    guard(connection, &columns)?;
    let names = columns.clone();
    connection
        .create_scalar_function(
            "fno_backlog_change",
            -1,
            FunctionFlags::SQLITE_UTF8,
            move |ctx| {
                let text = |i: usize| ctx.get_raw(i).as_str().unwrap_or("").to_string();
                let (table, op) = (text(0), text(1));
                let (Some(columns), Some(op)) = (names.get(&table), Op::parse(&op)) else {
                    return Err(rusqlite::Error::UserFunctionError(
                        format!("fno_backlog_change: unknown table {table} or op {op}").into(),
                    ));
                };
                let values: Vec<SqlValue> = (2..ctx.len())
                    .map(|i| SqlValue::from(ctx.get_raw(i)))
                    .collect();
                let (old, new) = match op {
                    Op::Insert => (Vec::new(), values),
                    Op::Delete => (values, Vec::new()),
                    Op::Update => {
                        let (old, new) = values.split_at(columns.len().min(values.len()));
                        (old.to_vec(), new.to_vec())
                    }
                };
                let change = Change {
                    op,
                    table,
                    columns: columns.clone(),
                    old,
                    new,
                };
                Ok(change.to_json().to_string())
            },
        )
        .map_err(|e| e.to_string())?;
    let mut triggers = String::new();
    for (table, columns) in &columns {
        let row = |side: &str| {
            columns
                .iter()
                .map(|c| format!("{side}.\"{c}\""))
                .collect::<Vec<_>>()
                .join(", ")
        };
        for (op, event, values) in [
            ("i", "INSERT", row("NEW")),
            ("u", "UPDATE", format!("{}, {}", row("OLD"), row("NEW"))),
            ("d", "DELETE", row("OLD")),
        ] {
            triggers.push_str(&format!(
                "CREATE TEMP TRIGGER IF NOT EXISTS fno_share_{table}_{op} AFTER {event} ON main.\"{table}\" \
                 BEGIN INSERT INTO main.backlog_outbox (change) \
                 VALUES (fno_backlog_change('{table}', '{op}', {values})); END;\n"
            ));
        }
    }
    connection
        .execute_batch(&triggers)
        .map_err(|e| e.to_string())
}

/// One row's identity for chaining: its table and every value.
fn row_key(table: &str, row: &[SqlValue]) -> String {
    let values = row.iter().map(SqlValue::to_hrana).collect::<Vec<_>>();
    format!("{table}\u{1f}{}", Value::Array(values))
}

/// Fold the outbox into one change per row: the row as the primary last saw
/// it and as it is now. Triggers can record a nested change (a touch
/// trigger's update) before the change that caused it, so the fold chains
/// records by value, each one's new row being the next one's old row,
/// instead of trusting the record order. Deletes go first, child table
/// first. Then the writes go parent table first, and inside one table the
/// updates go before the inserts, so a key freed by one row is free before
/// another row takes it and a parent row exists before the update that
/// names it.
fn net(changes: Vec<Change>) -> Vec<Change> {
    let mut by_old: std::collections::HashMap<String, Vec<usize>> = Default::default();
    let mut news = std::collections::HashSet::new();
    for (i, change) in changes.iter().enumerate() {
        if !change.old.is_empty() {
            by_old
                .entry(row_key(&change.table, &change.old))
                .or_default()
                .push(i);
        }
        if !change.new.is_empty() {
            news.insert(row_key(&change.table, &change.new));
        }
    }
    let mut used = vec![false; changes.len()];
    let is_head = |c: &Change| c.old.is_empty() || !news.contains(&row_key(&c.table, &c.old));
    let order = (0..changes.len())
        .filter(|&i| is_head(&changes[i]))
        .chain(0..changes.len())
        .collect::<Vec<_>>();
    let mut out = Vec::new();
    for head in order {
        if used[head] {
            continue;
        }
        used[head] = true;
        let mut tail = head;
        while !changes[tail].new.is_empty() {
            let key = row_key(&changes[tail].table, &changes[tail].new);
            let next = by_old
                .get(&key)
                .and_then(|ids| ids.iter().copied().find(|&j| !used[j]));
            let Some(next) = next else { break };
            used[next] = true;
            tail = next;
        }
        let (old, new) = (changes[head].old.clone(), changes[tail].new.clone());
        let op = match (old.is_empty(), new.is_empty()) {
            (true, true) => continue,
            (true, false) => Op::Insert,
            (false, true) => Op::Delete,
            (false, false) if old == new => continue,
            (false, false) => Op::Update,
        };
        out.push(Change {
            op,
            old,
            new,
            ..changes[head].clone()
        });
    }
    out.sort_by_key(|c| match c.op {
        Op::Delete => (0, SHARED_TABLES.len() - rank(&c.table), 0),
        Op::Update => (1, rank(&c.table), 0),
        Op::Insert => (1, rank(&c.table), 1),
    });
    out
}

/// `col IS ?n AND ...` over `columns`, numbered from `first`.
fn match_all(columns: &[String], first: usize) -> String {
    columns
        .iter()
        .enumerate()
        .map(|(i, c)| format!("\"{c}\" IS ?{}", first + i))
        .collect::<Vec<_>>()
        .join(" AND ")
}

fn quoted(columns: &[String]) -> String {
    columns
        .iter()
        .map(|c| format!("\"{c}\""))
        .collect::<Vec<_>>()
        .join(", ")
}

fn insert_sql(table: &str, columns: &[String]) -> String {
    let marks = (1..=columns.len())
        .map(|i| format!("?{i}"))
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "INSERT INTO \"{table}\" ({}) VALUES ({marks})",
        quoted(columns)
    )
}

/// The statement that applies `change` only when the row is still as this
/// replica saw it.
fn conditional(change: &Change) -> (String, Vec<SqlValue>) {
    let (table, columns) = (&change.table, &change.columns);
    match change.op {
        Op::Insert => (insert_sql(table, columns), change.new.clone()),
        Op::Delete => (
            format!("DELETE FROM \"{table}\" WHERE {}", match_all(columns, 1)),
            change.old.clone(),
        ),
        Op::Update => {
            let set = columns
                .iter()
                .enumerate()
                .map(|(i, c)| format!("\"{c}\" = ?{}", i + 1))
                .collect::<Vec<_>>()
                .join(", ");
            let mut args = change.new.clone();
            args.extend(change.old.iter().cloned());
            (
                format!(
                    "UPDATE \"{table}\" SET {set} WHERE {}",
                    match_all(columns, columns.len() + 1)
                ),
                args,
            )
        }
    }
}

fn ops_json(changes: &[Change]) -> String {
    Value::Array(changes.iter().map(Change::to_json).collect()).to_string()
}

/// The process that produced this batch: pid plus the first three argv
/// words, so a bad batch on the primary names its writer. The status flip
/// shipped 175 stored-status reversals with no writer field anywhere.
fn writer_stamp() -> (i64, String) {
    let cmd: String = std::env::args_os()
        .take(3)
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(200)
        .collect();
    (i64::from(std::process::id()), cmd)
}

fn parse_ops(text: &str) -> Result<Vec<Change>, String> {
    let rows: Vec<Value> = serde_json::from_str(text).map_err(|e| e.to_string())?;
    rows.iter().map(Change::from_json).collect()
}

/// Why [`publish`] gave up: the operator-facing refusal text, and the index
/// of the change a batch statement refused. The index is `None` when the
/// request itself failed rather than a statement in it.
struct Refusal {
    message: String,
    change: Option<usize>,
}

/// Send one outbox batch: every change as a conditional statement with its
/// row-count check, then the change-log row, in one request.
fn publish(remote: &Remote, origin: &str, batch: &str, changes: &[Change]) -> Result<(), Refusal> {
    let mut steps = vec![(
        "SELECT 1 FROM backlog_seed".to_string(),
        Vec::<SqlValue>::new(),
    )];
    let mut owner = vec![None];
    for (i, change) in changes.iter().enumerate() {
        steps.push(conditional(change));
        owner.push(Some(i));
        steps.push((
            "INSERT INTO backlog_cas (n) SELECT changes()".to_string(),
            Vec::new(),
        ));
        owner.push(Some(i));
    }
    steps.push(("DELETE FROM backlog_cas".to_string(), Vec::new()));
    owner.push(None);
    steps.push((
        "INSERT INTO backlog_changes (origin, batch, at, ops) VALUES (?1, ?2, ?3, ?4)".to_string(),
        vec![
            SqlValue::Text(origin.to_string()),
            SqlValue::Text(batch.to_string()),
            SqlValue::Integer(crate::backlog::now_ms() as i64),
            SqlValue::Text(ops_json(changes)),
        ],
    ));
    owner.push(None);
    // The writer stamp: which process produced this batch. A side table
    // instead of new change-log columns, so a primary from before the stamp
    // gains the table from the first new-binary publish and an old binary
    // never reads it - no schema skew between deploy moments. The upsert
    // keeps a re-sent batch (a lost reply, the same batch id) from failing
    // the stamp instead of answering the data.
    let (pid, cmd) = writer_stamp();
    steps.push((
        "CREATE TABLE IF NOT EXISTS main.backlog_writers (
           batch TEXT PRIMARY KEY, pid INTEGER NOT NULL, cmd TEXT NOT NULL, at INTEGER NOT NULL)"
            .to_string(),
        Vec::new(),
    ));
    owner.push(None);
    steps.push((
        "INSERT INTO backlog_writers (batch, pid, cmd, at) VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT(batch) DO UPDATE SET pid = excluded.pid, cmd = excluded.cmd, at = excluded.at"
            .to_string(),
        vec![
            SqlValue::Text(batch.to_string()),
            SqlValue::Integer(pid),
            SqlValue::Text(cmd),
            SqlValue::Integer(crate::backlog::now_ms() as i64),
        ],
    ));
    owner.push(None);
    remote
        .transaction(&steps)
        .map(drop)
        .map_err(|(step, error)| {
            let change = step.and_then(|s| owner.get(s).copied().flatten());
            Refusal {
                message: refusal_message(&error, changes, change),
                change,
            }
        })
}

/// The operator-facing refusal line: the raw primary error plus a hint,
/// with the CAS guard's opaque SQLite text said in words instead. That text
/// was read as a timestamp or schema CHECK (the fn-0735 bind refusal,
/// 2026-10-27), so a refused row names its table.
fn refusal_message(error: &str, changes: &[Change], change: Option<usize>) -> String {
    let hint = if unseeded(error) {
        " The primary holds no seeded backlog: run `fno agents claim backlog seed` on one machine first.".to_string()
    } else if crate::store_remote::is_unreachable(error) || landed_before(error) {
        String::new()
    } else {
        match change {
            Some(i) if orphaned(error) && changes[i].op == Op::Insert => format!(
                " The {} write names a parent row the primary does not hold. This machine kept the local row; a retry sends the same write, so report this refusal.",
                changes[i].table
            ),
            Some(i) if orphaned(error) && changes[i].op != Op::Delete => format!(
                " The {} write names a parent row the primary does not hold. This machine took the primary's rows back; a retry sends the same write, so report this refusal.",
                changes[i].table
            ),
            Some(i) if changes[i].op == Op::Insert => format!(
                " The {} row already exists on the primary. This machine kept its own row; the refused statements are kept in backlog_refused.",
                changes[i].table
            ),
            Some(i) => format!(
                " The {} row {CHANGED}. This machine took the primary's rows back; retry the write.",
                changes[i].table
            ),
            None => String::new(),
        }
    };
    let said = match change {
        Some(i) if error.contains(CAS_GUARD) => format!(
            "the {} row on the primary no longer matches this machine's copy",
            changes[i].table
        ),
        _ => error.to_string(),
    };
    format!("{REFUSED}: {said}.{hint}")
}

fn unseeded(error: &str) -> bool {
    error.contains("no such table: backlog_seed")
}

/// An insert or update named a parent row the primary lacks. No peer changed
/// anything, so the changed-row hint would send the caller to retry a write
/// that cannot succeed. A delete refused the same way means a peer added a
/// child row first, which is a real conflict.
fn orphaned(error: &str) -> bool {
    error.contains("FOREIGN KEY constraint failed")
}

/// The batch id is already on the primary: an earlier flush landed it and
/// died before it cleared the outbox.
fn landed_before(error: &str) -> bool {
    error.contains("UNIQUE constraint failed: backlog_changes.batch")
}

/// The replica's connection: no TEMP triggers, no main-schema triggers (the
/// change log already carries every row a trigger made), no foreign-key
/// actions. It never takes the graph lock; only a store that does not exist
/// yet goes through the full open once.
fn replica(graph: &Path) -> Result<Connection, String> {
    let db = crate::backlog::database_path(graph);
    if !db.exists() {
        drop(crate::backlog::open(graph)?);
    }
    let connection = crate::store_conn::open_write(&db)?;
    register_writer(&connection)?;
    connection
        .set_db_config(DbConfig::SQLITE_DBCONFIG_ENABLE_TRIGGER, false)
        .map_err(|e| e.to_string())?;
    connection
        .execute_batch(&format!(
            "PRAGMA foreign_keys=OFF;\n{OUTBOX_DDL}\n{REFUSED_DDL}"
        ))
        .map_err(|e| e.to_string())?;
    Ok(connection)
}

/// Send this machine's outbox to the primary. Runs under its own lock file,
/// never the graph lock, so a slow primary holds up no writer. `Ok(n)` is the
/// number of row changes the primary took. A refusal takes the primary's rows
/// back for the node the refusal named, re-queues the batch's other writes
/// for the next flush, keeps the refused statements in `backlog_refused`,
/// names the take-back in an operator notice (stderr plus a
/// `backlog_refused_flush` journal event), clears the batch, and returns the
/// refusal. An unreachable or unseeded primary keeps the outbox for the next
/// flush.
pub fn flush(graph: &Path) -> Result<usize, String> {
    let Some(remote) = primary_for(graph)? else {
        return Ok(0);
    };
    let db = crate::backlog::database_path(graph);
    let _lock = crate::graph_store::BoundedLock::acquire(&db.with_extension("share"), FLUSH_WAIT)
        .map_err(|e| e.to_string())?;
    let mut connection = replica(graph)?;
    // Each renumbered insert costs one more request; a batch with more
    // clashing inserts than this finishes on the next flush.
    for _ in 0..RENUMBER_LIMIT {
        if let Some(sent) = send(&remote, &db, &mut connection)? {
            return Ok(sent);
        }
    }
    Err(format!(
        "backlog-share: {RENUMBER_LIMIT} clashing keys renumbered in one flush."
    ))
}

const RENUMBER_LIMIT: usize = 8;

/// One outbox batch to the primary. `None` means a clashing key was
/// renumbered and the batch re-queued, so the caller sends again.
fn send(remote: &Remote, db: &Path, connection: &mut Connection) -> Result<Option<usize>, String> {
    let mut sending =
        crate::backlog::meta(connection, SENDING)?.and_then(|v| v.parse::<i64>().ok());
    if let Some(mark) = sending {
        let held: bool = connection
            .query_row(
                "SELECT EXISTS (SELECT 1 FROM backlog_outbox WHERE id <= ?1)",
                [mark],
                |r| r.get(0),
            )
            .map_err(|e| e.to_string())?;
        if !held {
            connection
                .execute("DELETE FROM graph_meta WHERE key = ?1", [SENDING])
                .map_err(|e| e.to_string())?;
            sending = None;
        }
    }
    let rows: Vec<(i64, String)> = {
        let mut statement = connection
            .prepare("SELECT id, change FROM backlog_outbox WHERE id <= ?1 ORDER BY id")
            .map_err(|e| e.to_string())?;
        let rows = statement
            .query_map([sending.unwrap_or(i64::MAX)], |row| {
                Ok((row.get(0)?, row.get(1)?))
            })
            .map_err(|e| e.to_string())?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())?;
        rows
    };
    let Some(&(last, _)) = rows.last() else {
        return Ok(Some(0));
    };
    let changes = net(rows
        .iter()
        .map(|(_, text)| {
            serde_json::from_str(text)
                .map_err(|e| e.to_string())
                .and_then(|v| Change::from_json(&v))
        })
        .collect::<Result<Vec<_>, _>>()?);
    let clear = |connection: &Connection| {
        connection
            .execute_batch(&format!(
                "DELETE FROM backlog_outbox WHERE id <= {last};
                 DELETE FROM graph_meta WHERE key = '{SENDING}';"
            ))
            .map_err(|e| e.to_string())
    };
    if changes.is_empty() {
        clear(&*connection)?;
        return Ok(Some(0));
    }
    let origin = crate::claims::machine_id();
    // The content hash keeps a reused outbox id (a rebuilt or restored
    // store) from reading as a batch that already landed.
    let batch = format!(
        "{origin}:{}:{last}:{:016x}",
        db.display(),
        fnv1a(ops_json(&changes).as_bytes())
    );
    crate::backlog::stamp_meta(connection, SENDING, &last.to_string())?;
    match publish(remote, &origin, &batch, &changes) {
        Ok(()) => {
            clear(&*connection)?;
            Ok(Some(changes.len()))
        }
        Err(refusal) if landed_before(&refusal.message) => {
            clear(&*connection)?;
            Ok(Some(changes.len()))
        }
        Err(refusal)
            if crate::store_remote::is_unreachable(&refusal.message)
                || unseeded(&refusal.message) =>
        {
            Err(refusal.message)
        }
        Err(refusal) => {
            let reader: &Connection = connection;
            let clash = refusal
                .change
                .filter(|&i| rowid_clash(reader, &refusal.message, &changes[i]));
            if let Some(i) = clash {
                renumber(remote, connection, &changes, i, last)?;
                return Ok(None);
            }
            repair(
                remote,
                connection,
                &changes,
                refusal.change,
                last,
                &batch,
                &refusal.message,
            )?;
            Err(refusal.message)
        }
    }
}

/// The primary refused `change`, an insert, because its rowid key (an
/// `INTEGER PRIMARY KEY`, such as `decisions.seq`) is taken. Each replica
/// mints that key as its own local max plus one, so two machines that write
/// before they sync mint the same value. The key is a surrogate: no row
/// refers to it, so a new value loses nothing.
fn rowid_clash(connection: &Connection, error: &str, change: &Change) -> bool {
    let Some(rest) = error.split("UNIQUE constraint failed: ").nth(1) else {
        return false;
    };
    let named = rest.split(['.', ',', ' ']).take(2).collect::<Vec<_>>();
    let [table, column] = named.as_slice() else {
        return false;
    };
    let single = rest
        .strip_prefix(&format!("{table}.{column}"))
        .is_some_and(|tail| !tail.starts_with(','));
    let key: Option<String> = connection
        .query_row(
            &format!(
                "SELECT name FROM pragma_table_info('{table}')
                 WHERE pk = 1 AND upper(type) = 'INTEGER'
                   AND (SELECT COUNT(*) FROM pragma_table_info('{table}') WHERE pk > 0) = 1"
            ),
            [],
            |r| r.get(0),
        )
        .optional()
        .ok()
        .flatten();
    single && change.op == Op::Insert && change.table == *table && key.as_deref() == Some(*column)
}

/// Give the refused insert `changes[refused]` a rowid key past the
/// primary's, this replica's and this batch's highest, move the local row
/// to it, and re-queue the whole batch. Nothing is taken back: the
/// primary refused only a key value, never a peer's change.
fn renumber(
    remote: &Remote,
    connection: &mut Connection,
    changes: &[Change],
    refused: usize,
    last: i64,
) -> Result<(), String> {
    let change = &changes[refused];
    let table = &change.table;
    let key = column_info(connection, table)?
        .into_iter()
        .find(|(_, pk)| *pk == 1)
        .map(|(name, _)| name)
        .ok_or_else(|| format!("{table} has no rowid key"))?;
    let at = change
        .columns
        .iter()
        .position(|c| *c == key)
        .ok_or_else(|| format!("the {table} insert carries no {key}"))?;
    let max_sql = format!("SELECT COALESCE(MAX(\"{key}\"), 0) FROM \"{table}\"");
    let on_primary = remote
        .execute(&max_sql, &[])?
        .rows
        .first()
        .and_then(|r| r[0].integer())
        .unwrap_or(0);
    let local: i64 = connection
        .query_row(&max_sql, [], |r| r.get(0))
        .map_err(|e| e.to_string())?;
    let queued = changes
        .iter()
        .filter(|c| c.table == *table && c.op == Op::Insert)
        .filter_map(|c| c.new.get(at).and_then(SqlValue::integer))
        .max()
        .unwrap_or(0);
    let next = on_primary.max(local).max(queued) + 1;
    let mut moved = change.clone();
    moved.new[at] = SqlValue::Integer(next);
    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|e| e.to_string())?;
    // A sync may already have replaced the local row with the peer's row
    // that holds the key; matching every column leaves that row alone.
    remove(
        &transaction,
        table,
        &match_all(&change.columns, 1),
        &change.new,
    )?;
    replace(&transaction, table, &change.columns, &moved.new)?;
    transaction
        .execute_batch(&format!(
            "DELETE FROM backlog_outbox WHERE id <= {last};
             DELETE FROM graph_meta WHERE key = '{SENDING}';"
        ))
        .map_err(|e| e.to_string())?;
    for (i, c) in changes.iter().enumerate() {
        let c = if i == refused { &moved } else { c };
        transaction
            .execute(
                "INSERT INTO backlog_outbox (change) VALUES (?1)",
                [&c.to_json().to_string()],
            )
            .map_err(|e| e.to_string())?;
    }
    stamp_fresh(&transaction, "renumber")?;
    transaction.commit().map_err(|e| e.to_string())?;
    let was = change.new[at].integer().unwrap_or_default();
    eprintln!(
        "backlog-share: a peer holds {table}.{key} {was} on the primary; this machine's row moved to {key} {next} and the batch is sent again."
    );
    Ok(())
}

/// 64-bit FNV-1a: a stable hash, the same in every build.
fn fnv1a(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(0x0100_0000_01b3)
    })
}

/// Take the primary's rows back for the nodes the refusal named, re-queue
/// the batch's other writes for the next flush, keep the refused statements
/// for retry or inspection, and clear the batch from the outbox, in one
/// local transaction. The primary reads run first, outside the transaction.
/// An INSERT the take-back covers keeps its local row whether the primary
/// holds the id (a collision) or lacks it: reverting it would delete a node
/// this machine minted, the loss this repair exists to stop. The same guard
/// covers an UPDATE whose target the primary returns nothing for: a row the
/// primary never received must not die one write later; a real peer delete
/// reaches the local row through sync instead. When the refusal names no
/// node (a global table, or the request itself failed), the take-back covers
/// the whole batch and its INSERTs re-queue instead of dying with it, all
/// but the refused one, which keeps its local row.
/// Newer queued writes to the refused node ride outbox rows past the
/// snapshot; the take-back already reverted their effects locally, so they
/// drop with the batch. After the commit, the operator notice (stderr plus a
/// `backlog_refused_flush` journal event) names what the refusal touched.
fn repair(
    remote: &Remote,
    connection: &mut Connection,
    changes: &[Change],
    refused: Option<usize>,
    last: i64,
    batch: &str,
    error: &str,
) -> Result<(), String> {
    // The take-back set is every change naming a node the refusal named;
    // the rest re-queues.
    let refused_ids: Vec<String> = refused
        .and_then(|i| changes.get(i))
        .map(node_ids_of)
        .unwrap_or_default();
    let taken_back = |change: &Change| {
        refused_ids.is_empty()
            || node_ids_of(change)
                .iter()
                .any(|id| refused_ids.contains(id))
    };
    let mut targets: Vec<(String, Vec<String>, Vec<String>, Vec<SqlValue>, bool)> = Vec::new();
    for change in changes
        .iter()
        .filter(|c| taken_back(c) && c.op != Op::Insert)
    {
        let info = column_info(connection, &change.table)?;
        let mut key: Vec<(usize, i64)> = change
            .columns
            .iter()
            .enumerate()
            .filter_map(|(i, c)| {
                info.iter()
                    .find(|(name, pk)| name == c && *pk > 0)
                    .map(|(_, pk)| (i, *pk))
            })
            .collect();
        key.sort_by_key(|(_, pk)| *pk);
        // A table with no declared key matches on every column.
        let key: Vec<usize> = if key.is_empty() {
            (0..change.columns.len()).collect()
        } else {
            key.into_iter().map(|(i, _)| i).collect()
        };
        let names: Vec<String> = key.iter().map(|&i| change.columns[i].clone()).collect();
        // An update whose target the primary lacks keeps its local row; a
        // delete completed there, so its absence removes the local row.
        let keep_on_empty = change.op == Op::Update;
        for row in [&change.old, &change.new] {
            if row.is_empty() {
                continue;
            }
            let values: Vec<SqlValue> = key.iter().map(|&i| row[i].clone()).collect();
            let target = (
                change.table.clone(),
                change.columns.clone(),
                names.clone(),
                values,
                keep_on_empty,
            );
            if !targets.contains(&target) {
                targets.push(target);
            }
        }
    }
    let mut fetched = Vec::new();
    for (table, columns, names, values, _) in &targets {
        let reply = remote.execute(
            &format!(
                "SELECT {} FROM \"{table}\" WHERE {}",
                quoted(columns),
                match_all(names, 1)
            ),
            values,
        )?;
        fetched.push(reply.rows);
    }
    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|e| e.to_string())?;
    let mut kept_unlanded = 0usize;
    for ((table, columns, names, values, keep_on_empty), rows) in targets.iter().zip(fetched) {
        if rows.is_empty() && *keep_on_empty {
            kept_unlanded += 1;
            continue;
        }
        remove(&transaction, table, &match_all(names, 1), values)?;
        for row in &rows {
            replace(&transaction, table, columns, row)?;
        }
    }
    transaction
        .execute(
            "INSERT INTO backlog_refused (batch, at, error, ops) VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![
                batch,
                crate::backlog::now_ms() as i64,
                error,
                ops_json(changes)
            ],
        )
        .map_err(|e| e.to_string())?;
    transaction
        .execute_batch(&format!(
            "DELETE FROM backlog_outbox WHERE id <= {last};
             DELETE FROM graph_meta WHERE key = '{SENDING}';"
        ))
        .map_err(|e| e.to_string())?;
    // An unnamed refusal takes the whole batch back, so its INSERTs would
    // die with it; they re-queue instead. The insert the primary refused
    // stays out: resent, it refuses again, and every later batch with it.
    let requeued: Vec<&Change> = changes
        .iter()
        .enumerate()
        .filter(|&(i, c)| {
            if refused_ids.is_empty() {
                c.op == Op::Insert && Some(i) != refused
            } else {
                !taken_back(c)
            }
        })
        .map(|(_, c)| c)
        .collect();
    for change in &requeued {
        transaction
            .execute(
                "INSERT INTO backlog_outbox (change) VALUES (?1)",
                [&change.to_json().to_string()],
            )
            .map_err(|e| e.to_string())?;
    }
    let mut dropped_newer = 0usize;
    if !refused_ids.is_empty() {
        let stale: Vec<i64> = {
            let mut statement = transaction
                .prepare("SELECT id, change FROM backlog_outbox WHERE id > ?1")
                .map_err(|e| e.to_string())?;
            let rows = statement
                .query_map([last], |row| Ok((row.get(0)?, row.get(1)?)))
                .map_err(|e| e.to_string())?
                .collect::<Result<Vec<(i64, String)>, _>>()
                .map_err(|e| e.to_string())?;
            rows.into_iter()
                .filter(|(_, text)| {
                    serde_json::from_str::<Value>(text)
                        .ok()
                        .and_then(|v| Change::from_json(&v).ok())
                        .map(|c| node_ids_of(&c).iter().any(|id| refused_ids.contains(id)))
                        .unwrap_or(false)
                })
                .map(|(id, _)| id)
                .collect()
        };
        for id in stale {
            transaction
                .execute("DELETE FROM backlog_outbox WHERE id = ?1", [id])
                .map_err(|e| e.to_string())?;
            dropped_newer += 1;
        }
    }
    stamp_fresh(&transaction, "repair")?;
    transaction.commit().map_err(|e| e.to_string())?;
    let taken: Vec<Change> = changes.iter().filter(|c| taken_back(c)).cloned().collect();
    notice_refusal(
        &taken,
        requeued.len(),
        dropped_newer,
        kept_unlanded,
        batch,
        error,
    );
    Ok(())
}

/// The refused flush's operator notice: stderr, then a best-effort
/// `backlog_refused_flush` journal event in the space this process resolves.
/// Both name the nodes and columns the take-back covered, the writes that
/// re-queued, the newer queued writes dropped with the batch, and the local
/// rows kept because the primary never received them. A notice failure never
/// fails the repair that already committed.
fn notice_refusal(
    taken: &[Change],
    requeued: usize,
    dropped_newer: usize,
    kept_unlanded: usize,
    batch: &str,
    error: &str,
) {
    eprintln!(
        "backlog-share: {}",
        refusal_notice(taken, requeued, dropped_newer, kept_unlanded)
    );
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let Some(space) = crate::paths::space_dir_opt(&cwd) else {
        return;
    };
    let path = space.join("events.jsonl");
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    let emitter = crate::events::EventEmitter::new(path, "backlog");
    let _ = emitter.emit(
        "backlog_refused_flush",
        &refused_flush_data(taken, batch, error),
    );
}

/// The operator notice for one refused flush: the nodes and columns the
/// take-back covered, the writes that re-queued, the newer queued writes
/// dropped with the batch, and the rows kept for a primary that never held
/// them, then the other tables' row counts.
fn refusal_notice(
    taken: &[Change],
    requeued: usize,
    dropped_newer: usize,
    kept_unlanded: usize,
) -> String {
    let requeue_part = if requeued > 0 {
        format!(", re-queued {requeued} write(s) the refusal did not name for the next flush")
    } else {
        String::new()
    };
    let dropped_part = if dropped_newer > 0 {
        format!(", dropped {dropped_newer} newer queued write(s) to the refused node")
    } else {
        String::new()
    };
    let kept_part = if kept_unlanded > 0 {
        format!(", kept {kept_unlanded} local row(s) the primary never received")
    } else {
        String::new()
    };
    let mut nodes: Vec<String> = Vec::new();
    let mut others: BTreeMap<&str, usize> = BTreeMap::new();
    for change in taken {
        if change.table == "nodes" {
            let id = id_of(change);
            if let Some(id) = id {
                let line = format!("node {id} ({})", change.columns.join(", "));
                if !nodes.contains(&line) {
                    nodes.push(line);
                }
            }
        } else {
            *others.entry(change.table.as_str()).or_insert(0) += 1;
        }
    }
    let others_part = if others.is_empty() {
        String::new()
    } else {
        let pairs: Vec<String> = others
            .iter()
            .map(|(table, count)| format!("{count} row(s) in {table}"))
            .collect();
        format!(", plus {}", pairs.join(", "))
    };
    if nodes.is_empty() {
        format!(
            "the primary refused a batch and took the named rows back{requeue_part}{dropped_part}{kept_part}{}. The refused statements are kept in backlog_refused for retry or inspection.",
            others_part
        )
    } else {
        format!(
            "the primary refused a batch and took the named rows back{requeue_part}{dropped_part}{kept_part}: {}{}. The refused statements are kept in backlog_refused for retry or inspection.",
            nodes.join("; "),
            others_part
        )
    }
}

/// The `backlog_refused_flush` event body: the facts the notice names, structured for audit.
fn refused_flush_data(changes: &[Change], batch: &str, error: &str) -> Value {
    let mut nodes: Vec<Value> = Vec::new();
    for change in changes {
        if change.table != "nodes" {
            continue;
        }
        if let Some(id) = id_of(change) {
            nodes.push(json!({
                "id": id,
                "columns": change.columns,
            }));
        }
    }
    let mut tables: BTreeMap<&str, usize> = BTreeMap::new();
    for change in changes {
        if change.table != "nodes" {
            *tables.entry(change.table.as_str()).or_insert(0) += 1;
        }
    }
    json!({
        "batch": batch,
        "error": error,
        "nodes": nodes,
        "tables": tables
            .iter()
            .map(|(table, rows)| json!({"table": table, "rows": rows}))
            .collect::<Vec<Value>>(),
    })
}

/// The node id a change names: the `id` column's old value, else its new
/// one. `None` when the change carries no id column value.
fn id_of(change: &Change) -> Option<String> {
    let i = change.columns.iter().position(|c| c == "id")?;
    change
        .old
        .get(i)
        .or_else(|| change.new.get(i))
        .and_then(SqlValue::text)
        .map(str::to_string)
}

/// Every node id a change names: the `id` column, plus the node-scoped
/// columns the child tables key on (`node_id`, and relations'
/// `related_node_id` and `listed_on`). `id_of` alone missed every child row,
/// so a refused node's provenance or dispatch row re-queued instead of
/// taking back with it, and a re-queued stale child row refused the next
/// flush again.
fn node_ids_of(change: &Change) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for name in ["id", "node_id", "related_node_id", "listed_on"] {
        let Some(i) = change.columns.iter().position(|c| c == name) else {
            continue;
        };
        let Some(id) = change
            .old
            .get(i)
            .or_else(|| change.new.get(i))
            .and_then(SqlValue::text)
        else {
            continue;
        };
        let id = id.to_string();
        if !out.contains(&id) {
            out.push(id);
        }
    }
    out
}

/// After a write through `mutate_rows`, once the graph lock is gone: flush
/// the outbox. Only a refusal took the write back, so only a refusal syncs
/// the replica and returns, and the caller retries on fresh rows. Every
/// other failure leaves the write in the outbox for the next flush, and a
/// retry would apply it twice.
pub(crate) fn publish_after_write(graph: &Path) -> Result<(), String> {
    match flush(graph) {
        Ok(_) => Ok(()),
        Err(error)
            if error.starts_with(REFUSED)
                && !crate::store_remote::is_unreachable(&error)
                && !unseeded(&error) =>
        {
            if let Err(sync_error) = sync(graph) {
                eprintln!("backlog-share: sync after a refusal: {sync_error}");
            }
            Err(error)
        }
        Err(error) => {
            eprintln!("{error} The write is kept and sent on the next flush.");
            Ok(())
        }
    }
}

/// What one sync did.
#[derive(Debug, Default, PartialEq, serde::Serialize)]
pub struct Receipt {
    pub shared: bool,
    pub snapshot: bool,
    pub applied: usize,
    pub cursor: i64,
}

/// Bring the replica up to the primary. A replica that never synced takes
/// a full snapshot first.
pub fn sync(graph: &Path) -> Result<Receipt, String> {
    let Some(remote) = primary_for(graph)? else {
        return Ok(Receipt::default());
    };
    let mut connection = replica(graph)?;
    let cursor = crate::backlog::meta(&connection, CURSOR)?.and_then(|v| v.parse::<i64>().ok());
    let Some(cursor) = cursor else {
        return snapshot(&remote, graph, &mut connection);
    };
    match catch_up(&remote, &mut connection, cursor) {
        Err(error) if error == PRUNED_PAST => snapshot(&remote, graph, &mut connection),
        outcome => outcome,
    }
}

/// The change log no longer holds the rows right after this replica's
/// cursor: [`prune`] took them. Only a fresh snapshot brings it level.
const PRUNED_PAST: &str = "the change log was pruned past this replica's cursor";

/// Tell the primary how far this replica has read. A primary seeded
/// before the cursor table existed gets it here.
fn report_cursor(remote: &Remote, origin: &str, seq: i64, at_ms: i64) -> Result<(), String> {
    remote
        .transaction(&[
            (
                "CREATE TABLE IF NOT EXISTS backlog_cursors (
                   origin TEXT PRIMARY KEY, seq INTEGER NOT NULL, at INTEGER NOT NULL)"
                    .to_string(),
                Vec::new(),
            ),
            (
                "INSERT INTO backlog_cursors (origin, seq, at) VALUES (?1, ?2, ?3)
                 ON CONFLICT(origin) DO UPDATE SET seq = excluded.seq, at = excluded.at"
                    .to_string(),
                vec![
                    SqlValue::Text(origin.to_string()),
                    SqlValue::Integer(seq),
                    SqlValue::Integer(at_ms),
                ],
            ),
        ])
        .map(drop)
        .map_err(|(_, error)| error)
}

/// Delete up to `rows` of the oldest change-log rows written before
/// `cutoff_ms` that every live replica has read. A replica is live when it
/// reported its cursor since `cutoff_ms`; with none live, nothing goes. The
/// newest row always stays, so a replica past a pruned stretch sees a seq
/// gap, never an empty log. Without this the log kept every batch ever
/// sent: on the shared primary it grew to 97 percent of the store, and
/// every write paid for the size.
fn prune(remote: &Remote, cutoff_ms: i64, rows: i64) -> Result<u64, String> {
    let changes = remote
        .execute(
            "DELETE FROM backlog_changes WHERE at < ?1
             AND seq <= (SELECT COALESCE(MIN(seq), 0) FROM backlog_cursors WHERE at >= ?1)
             AND seq IN (
               SELECT seq FROM backlog_changes
               WHERE seq < (SELECT MAX(seq) FROM backlog_changes)
               ORDER BY seq LIMIT ?2)",
            &[SqlValue::Integer(cutoff_ms), SqlValue::Integer(rows)],
        )?
        .affected;
    // The writer stamp rides the batch's lifetime: the stamp row goes when
    // the change-log row goes. A primary from before the table has none, so
    // its absence prunes nothing instead of failing the run.
    let writers = match remote.execute(
        "DELETE FROM backlog_writers WHERE at < ?1
         AND batch NOT IN (SELECT batch FROM backlog_changes)",
        &[SqlValue::Integer(cutoff_ms)],
    ) {
        Ok(reply) => reply.affected,
        Err(error) if error.contains("no such table: backlog_writers") => 0,
        Err(error) => return Err(error),
    };
    Ok(changes + writers)
}

/// Apply the change log past `cursor`, one page per local transaction. Each
/// page is read from the primary before its transaction opens.
fn catch_up(
    remote: &Remote,
    connection: &mut Connection,
    mut cursor: i64,
) -> Result<Receipt, String> {
    let mut receipt = Receipt {
        shared: true,
        cursor,
        ..Receipt::default()
    };
    let mut limit = PAGE;
    loop {
        let page = read_page(
            remote,
            "SELECT seq, ops FROM backlog_changes WHERE seq > ?1 ORDER BY seq LIMIT ?2",
            cursor,
            &mut limit,
        )?;
        limit = (limit * 2).min(PAGE);
        if page.rows.is_empty() {
            return Ok(receipt);
        }
        let mut batches = Vec::new();
        for row in &page.rows {
            let seq = row[0].integer().ok_or("change row without a seq")?;
            // Seqs come from one AUTOINCREMENT on a single writer, so they
            // run without holes until a prune cuts some. A prune keys on
            // each writer's own clock, so a skewed row can survive with
            // younger pruned rows behind it: check every row, not the first.
            if seq != batches.last().map_or(cursor, |(last, _)| *last) + 1 {
                return Err(PRUNED_PAST.to_string());
            }
            batches.push((seq, parse_ops(row[1].text().unwrap_or("[]"))?));
        }
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|e| e.to_string())?;
        for (seq, changes) in &batches {
            for change in changes {
                if apply(&transaction, change)? {
                    receipt.applied += 1;
                }
            }
            cursor = *seq;
        }
        crate::backlog::stamp_meta(&transaction, CURSOR, &cursor.to_string())?;
        stamp_fresh(&transaction, &cursor.to_string())?;
        transaction.commit().map_err(|e| e.to_string())?;
        receipt.cursor = cursor;
    }
}

/// Read one page of `sql`, whose `?1` is the key to read past and `?2` the
/// row limit. sqld refuses a reply over its size limit, so a refused page
/// halves `limit` until it fits; a single row over the limit still refuses.
/// The caller keeps `limit` across pages, so a run of large rows does not
/// pay the refusals again on every page.
fn read_page(remote: &Remote, sql: &str, after: i64, limit: &mut i64) -> Result<Reply, String> {
    loop {
        match remote.execute(sql, &[SqlValue::Integer(after), SqlValue::Integer(*limit)]) {
            Err(e) if *limit > 1 && e.contains("Response is too large") => *limit /= 2,
            read => return read,
        }
    }
}

fn row_exists(connection: &Connection, change: &Change, row: &[SqlValue]) -> Result<bool, String> {
    connection
        .query_row(
            &format!(
                "SELECT 1 FROM \"{}\" WHERE {} LIMIT 1",
                change.table,
                match_all(&change.columns, 1)
            ),
            rusqlite::params_from_iter(row),
            |_| Ok(()),
        )
        .optional()
        .map(|found| found.is_some())
        .map_err(|e| format!("{}: {e}", change.table))
}

/// Apply one logged change. The primary already decided it, and the log is
/// in order, so the replica takes the new row whatever it held: the old row
/// goes, and the new one replaces any row it collides with. `false` means
/// the replica already held the change, so a replay is harmless.
fn apply(connection: &Connection, change: &Change) -> Result<bool, String> {
    if !SHARED_TABLES.contains(&change.table.as_str()) {
        return Err(format!(
            "the change log names a local table {}",
            change.table
        ));
    }
    let present = |row: &[SqlValue]| row_exists(connection, change, row);
    match change.op {
        Op::Insert | Op::Update if present(&change.new)? => return Ok(false),
        Op::Delete if !present(&change.old)? => return Ok(false),
        Op::Insert => {}
        Op::Update | Op::Delete => remove(
            connection,
            &change.table,
            &match_all(&change.columns, 1),
            &change.old,
        )?,
    }
    if change.op != Op::Delete {
        replace(connection, &change.table, &change.columns, &change.new)?;
    }
    Ok(true)
}

/// Delete the rows of `table` that `filter` selects. The replica runs with
/// the triggers off, so a nodes row leaves the search index by hand first.
/// A full index rebuild per write held the write lock long enough to stall
/// every graph writer.
fn remove(
    connection: &Connection,
    table: &str,
    filter: &str,
    args: &[SqlValue],
) -> Result<(), String> {
    if table == "nodes" {
        crate::backlog::search::unindex(connection, filter, rusqlite::params_from_iter(args))
            .map_err(|e| format!("{table}: {e}"))?;
    }
    connection
        .execute(
            &format!("DELETE FROM \"{table}\" WHERE {filter}"),
            rusqlite::params_from_iter(args),
        )
        .map_err(|e| format!("{table}: {e}"))?;
    Ok(())
}

/// Write `row`, replacing any row it collides with. INSERT OR REPLACE drops
/// a colliding row without a trigger, so a nodes row that shares the key or
/// a UNIQUE column with `row` leaves the search index first, and `row` joins
/// it after.
fn replace(
    connection: &Connection,
    table: &str,
    columns: &[String],
    row: &[SqlValue],
) -> Result<(), String> {
    let nodes = table == "nodes";
    if nodes {
        let (filter, args) = collisions(connection, table, columns, row)?;
        if !filter.is_empty() {
            crate::backlog::search::unindex(connection, &filter, rusqlite::params_from_iter(args))
                .map_err(|e| format!("{table}: {e}"))?;
        }
    }
    connection
        .execute(
            &insert_sql(table, columns).replacen("INSERT", "INSERT OR REPLACE", 1),
            rusqlite::params_from_iter(row),
        )
        .map_err(|e| format!("{table}: {e}"))?;
    if nodes {
        crate::backlog::search::index(connection, connection.last_insert_rowid())
            .map_err(|e| format!("{table}: {e}"))?;
    }
    Ok(())
}

/// The rows of `table` that `row` collides with: one `col = ?n` group per
/// key or UNIQUE index whose columns `columns` carries, joined with OR, and
/// the values from `row` they bind. `=` matches UNIQUE, where NULLs never
/// collide.
fn collisions(
    connection: &Connection,
    table: &str,
    columns: &[String],
    row: &[SqlValue],
) -> Result<(String, Vec<SqlValue>), String> {
    let column_names = |sql: &str, arg: &str| -> Result<Vec<Option<String>>, String> {
        let mut statement = connection.prepare_cached(sql).map_err(|e| e.to_string())?;
        let names = statement
            .query_map([arg], |r| r.get(0))
            .map_err(|e| e.to_string())?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string());
        names
    };
    let mut groups = Vec::new();
    let mut args = Vec::new();
    let indexes = column_names(
        "SELECT name FROM pragma_index_list(?1) WHERE \"unique\" AND NOT partial",
        table,
    )?;
    for index in indexes.into_iter().flatten() {
        let key = column_names(
            "SELECT name FROM pragma_index_info(?1) ORDER BY seqno",
            &index,
        )?;
        let positions: Option<Vec<usize>> = key
            .iter()
            .map(|name| columns.iter().position(|c| Some(c) == name.as_ref()))
            .collect();
        let Some(positions) = positions.filter(|p| !p.is_empty()) else {
            continue;
        };
        let group = positions
            .iter()
            .map(|&i| {
                args.push(row[i].clone());
                format!("\"{}\" = ?{}", columns[i], args.len())
            })
            .collect::<Vec<_>>()
            .join(" AND ");
        groups.push(format!("({group})"));
    }
    Ok((groups.join(" OR "), args))
}

/// A fresh content version, so a locked mutate that read before this write
/// sees a conflict and re-reads.
fn stamp_fresh(connection: &Connection, why: &str) -> Result<(), String> {
    crate::backlog::stamp_version(
        connection,
        &format!("sqlite:share-{why}-{}", crate::backlog::now_ms()),
    )
}

/// Every shared table's rows on the primary, read page by page.
fn primary_rows(
    remote: &Remote,
    columns: &BTreeMap<String, Vec<String>>,
) -> Result<Vec<(String, Vec<Vec<SqlValue>>)>, String> {
    let mut out = Vec::new();
    for (table, columns) in columns {
        let mut rows = Vec::new();
        let (mut after, mut limit) = (0, PAGE);
        loop {
            let page = read_page(
                remote,
                &format!(
                    "SELECT rowid, {} FROM \"{table}\" WHERE rowid > ?1 ORDER BY rowid LIMIT ?2",
                    quoted(columns)
                ),
                after,
                &mut limit,
            )?;
            let full = page.rows.len() as i64 == limit;
            limit = (limit * 2).min(PAGE);
            for mut row in page.rows {
                after = row[0].integer().unwrap_or(after);
                row.remove(0);
                rows.push(row);
            }
            if !full {
                break;
            }
        }
        out.push((table.clone(), rows));
    }
    Ok(out)
}

/// Replace every shared table with the primary's rows. All primary reads run
/// before the local transaction opens. The first snapshot keeps a backup
/// copy of the replica beside it, because this machine's own backlog rows
/// are replaced.
fn snapshot(remote: &Remote, graph: &Path, connection: &mut Connection) -> Result<Receipt, String> {
    let top = remote
        .execute("SELECT COALESCE(MAX(seq), 0) FROM backlog_changes", &[])?
        .rows
        .first()
        .and_then(|r| r[0].integer())
        .unwrap_or(0);
    let columns = shared_columns(connection)?;
    let tables = primary_rows(remote, &columns)?;
    if crate::backlog::meta(connection, CURSOR)?.is_none() {
        let db = crate::backlog::database_path(graph);
        let backup = db.with_extension(format!("pre-share-{}.db", crate::backlog::now_ms()));
        let mut copy = Connection::open(&backup).map_err(|e| e.to_string())?;
        rusqlite::backup::Backup::new(connection, &mut copy)
            .and_then(|b| b.run_to_completion(256, Duration::ZERO, None))
            .map_err(|e| format!("backup {}: {e}", backup.display()))?;
    }
    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|e| e.to_string())?;
    let mut applied = 0;
    for (table, rows) in &tables {
        transaction
            .execute(&format!("DELETE FROM \"{table}\""), [])
            .map_err(|e| format!("{table}: {e}"))?;
        let insert = insert_sql(table, &columns[table]);
        for row in rows {
            transaction
                .execute(&insert, rusqlite::params_from_iter(row))
                .map_err(|e| format!("{table}: {e}"))?;
            applied += 1;
        }
    }
    crate::backlog::stamp_meta(&transaction, CURSOR, &top.to_string())?;
    guard(&transaction, &columns)?;
    // Every nodes row was replaced with the triggers off: one rebuild here.
    crate::backlog::search::rebuild(&transaction)?;
    stamp_fresh(&transaction, "snapshot")?;
    transaction.commit().map_err(|e| e.to_string())?;
    // Rows written after `top` replay as present or apply.
    let caught = catch_up(remote, connection, top)?;
    Ok(Receipt {
        shared: true,
        snapshot: true,
        applied: applied + caught.applied,
        cursor: caught.cursor.max(top),
    })
}

/// Copy this replica's backlog into an empty primary. Refuses a primary
/// that already holds a seeded backlog.
pub fn seed(graph: &Path) -> Result<Value, String> {
    let remote = primary_for(graph)?
        .ok_or("set store.remote_url and store.share_backlog = true in the global config first")?;
    let seeded = remote.execute(
        "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'backlog_seed'",
        &[],
    )?;
    if !seeded.rows.is_empty() {
        return Err(format!(
            "the primary {} already holds a seeded backlog; run `fno agents claim backlog sync` instead",
            remote.url()
        ));
    }
    let db = crate::backlog::database_path(graph);
    let _lock = crate::graph_store::BoundedLock::acquire(&db.with_extension("share"), FLUSH_WAIT)
        .map_err(|e| e.to_string())?;
    let connection = replica(graph)?;
    // A write after this point may miss the copy, so its record stays.
    let carried: i64 = connection
        .query_row("SELECT COALESCE(MAX(id), 0) FROM backlog_outbox", [], |r| {
            r.get(0)
        })
        .map_err(|e| e.to_string())?;
    let mut ddl = String::from(PRIMARY_DDL);
    {
        let mut statement = connection
            .prepare(
                "SELECT sql FROM sqlite_master WHERE sql IS NOT NULL
                 AND type IN ('table', 'index') AND tbl_name = ?1
                 ORDER BY CASE type WHEN 'table' THEN 0 ELSE 1 END",
            )
            .map_err(|e| e.to_string())?;
        for table in SHARED_TABLES {
            for sql in statement
                .query_map([table], |row| row.get::<_, String>(0))
                .map_err(|e| e.to_string())?
            {
                let sql = sql
                    .map_err(|e| e.to_string())?
                    .replacen("CREATE TABLE ", "CREATE TABLE IF NOT EXISTS ", 1)
                    .replacen("CREATE INDEX ", "CREATE INDEX IF NOT EXISTS ", 1)
                    .replacen(
                        "CREATE UNIQUE INDEX ",
                        "CREATE UNIQUE INDEX IF NOT EXISTS ",
                        1,
                    )
                    .replace("IF NOT EXISTS IF NOT EXISTS", "IF NOT EXISTS");
                ddl.push_str(&sql);
                ddl.push_str(";\n");
            }
        }
    }
    remote.script(&ddl)?;
    let columns = shared_columns(&connection)?;
    let mut counts = serde_json::Map::new();
    for table in SHARED_TABLES {
        let Some(columns) = columns.get(*table) else {
            continue;
        };
        let insert = insert_sql(table, columns);
        let mut steps = vec![(format!("DELETE FROM \"{table}\""), Vec::new())];
        let mut statement = connection
            .prepare(&format!("SELECT {} FROM \"{table}\"", quoted(columns)))
            .map_err(|e| e.to_string())?;
        let mut rows = statement.query([]).map_err(|e| e.to_string())?;
        let mut count = 0;
        while let Some(row) = rows.next().map_err(|e| e.to_string())? {
            let values = (0..columns.len())
                .map(|i| row.get_ref(i).map(SqlValue::from))
                .collect::<Result<Vec<_>, _>>()
                .map_err(|e| e.to_string())?;
            steps.push((insert.clone(), values));
            count += 1;
            if steps.len() as i64 >= PAGE {
                remote
                    .transaction(&std::mem::take(&mut steps))
                    .map_err(|(_, e)| format!("{table}: {e}"))?;
            }
        }
        if !steps.is_empty() {
            remote
                .transaction(&steps)
                .map_err(|(_, e)| format!("{table}: {e}"))?;
        }
        counts.insert(table.to_string(), json!(count));
    }
    remote.script(
        "CREATE TABLE backlog_seed (at INTEGER NOT NULL); INSERT INTO backlog_seed (at) VALUES (0);",
    )?;
    let top = remote
        .execute("SELECT COALESCE(MAX(seq), 0) FROM backlog_changes", &[])?
        .rows
        .first()
        .and_then(|r| r[0].integer())
        .unwrap_or(0);
    crate::backlog::stamp_meta(&connection, CURSOR, &top.to_string())?;
    guard(&connection, &columns)?;
    // The seed carried every row these records name. A later record is sent
    // by the next flush; a primary that already holds its row refuses it,
    // and the repair takes back the row the seed copied.
    connection
        .execute_batch(&format!(
            "DELETE FROM backlog_outbox WHERE id <= {carried};
             DELETE FROM graph_meta WHERE key = '{SENDING}';"
        ))
        .map_err(|e| e.to_string())?;
    Ok(json!({"primary": remote.url(), "rows": counts, "cursor": top}))
}

/// Report this replica's cursor, then prune what every live replica read.
fn report_and_prune(graph: &Path) -> Result<(), String> {
    let Some(remote) = primary_for(graph)? else {
        return Ok(());
    };
    let cursor = crate::backlog::meta(&replica(graph)?, CURSOR)?.and_then(|v| v.parse().ok());
    let now = crate::backlog::now_ms() as i64;
    if let Some(cursor) = cursor {
        report_cursor(&remote, &crate::claims::machine_id(), cursor, now)?;
    }
    prune(&remote, now - KEEP.as_millis() as i64, PRUNE_ROWS).map(drop)
}

/// Time one real read and one write-lock round trip on the primary. Plain
/// HTTP answering says nothing about the write path: a primary stuck behind
/// a held write lock still answers GET at once.
pub fn probe(remote: &Remote) -> Result<Value, String> {
    let started = Instant::now();
    let tables = remote
        .execute("SELECT COUNT(*) FROM sqlite_master", &[])?
        .rows
        .first()
        .and_then(|r| r[0].integer());
    let read_ms = started.elapsed().as_millis();
    let started = Instant::now();
    remote.transaction(&[]).map_err(|(_, error)| {
        format!(
            "{error} (the write lock took {} ms before failing)",
            started.elapsed().as_millis()
        )
    })?;
    let write_ms = started.elapsed().as_millis();
    let log = remote
        .execute(
            "SELECT MIN(seq), MAX(seq), (SELECT at FROM backlog_changes ORDER BY seq LIMIT 1),
                    (SELECT AVG(LENGTH(ops)) FROM
                       (SELECT ops FROM backlog_changes ORDER BY seq DESC LIMIT 100))
             FROM backlog_changes",
            &[],
        )
        .ok()
        .and_then(|reply| reply.rows.into_iter().next())
        .map(|row| {
            let n = |i: usize| row.get(i).and_then(SqlValue::integer);
            let f = |i: usize| row.get(i).and_then(SqlValue::real);
            json!({
                "first_seq": n(0), "last_seq": n(1), "oldest_at_ms": n(2),
                // The reading the stall work rides on: the newest 100
                // batches' average size. Flip-bearing batches ran 87,688
                // bytes against a 20,000-byte healthy line (2026-10-27).
                "recent_avg_batch_bytes": f(3).map(|v| v as i64).or_else(|| n(3)),
            })
        });
    Ok(json!({
        "primary": remote.url(),
        "tables": tables,
        "read_ms": read_ms,
        "write_ms": write_ms,
        "change_log": log,
    }))
}

/// `fno agents claim backlog seed|sync|probe`.
pub fn run(args: &[String]) -> i32 {
    let graph = crate::backlog::settings::graph_path();
    let outcome = match args.first().map(String::as_str) {
        Some("seed") => seed(&graph),
        Some("probe") => crate::store_remote::configured().and_then(|remote| {
            probe(&remote.ok_or("set store.remote_url in the global config first")?)
        }),
        Some("sync") => flush(&graph).and_then(|sent| {
            let receipt = sync(&graph)?;
            let plans = crate::plan_carry::sync(&graph)?;
            Ok(json!({"sent": sent, "sync": receipt, "plans": plans.receipt()}))
        }),
        _ => {
            eprintln!(
                "usage: fno agents claim backlog seed|sync|probe\n  seed   copy this machine's backlog into an empty shared primary\n  sync   send this machine's outbox, bring its replica up to the primary, then carry open nodes' plan bodies\n  probe  time one read and one write-lock round trip on the primary"
            );
            return 2;
        }
    };
    match outcome {
        Ok(receipt) => {
            println!("{receipt}");
            0
        }
        Err(error) => {
            eprintln!("claim backlog: {error}");
            1
        }
    }
}

/// The resident syncer: one flush and one sync every [`INTERVAL`], and one
/// change-log prune every [`PRUNE_EVERY`], one in flight, off the daemon's
/// own loop.
#[derive(Default)]
pub struct Arm {
    last_tick: Mutex<Option<Instant>>,
    last_prune: Mutex<Option<Instant>>,
    in_flight: Arc<AtomicBool>,
}

pub fn maybe_tick(arm: &Arm) {
    match crate::store_remote::share_backlog() {
        Ok(Some(_)) => {}
        Ok(None) => return,
        Err(error) => {
            static WARNED: std::sync::Once = std::sync::Once::new();
            WARNED.call_once(|| eprintln!("backlog-share: {error}"));
            return;
        }
    }
    {
        let mut last = arm.last_tick.lock().unwrap_or_else(|e| e.into_inner());
        if last.is_some_and(|tick| tick.elapsed() < INTERVAL)
            || arm.in_flight.swap(true, Ordering::SeqCst)
        {
            return;
        }
        *last = Some(Instant::now());
    }
    let prune_due = {
        let mut last = arm.last_prune.lock().unwrap_or_else(|e| e.into_inner());
        let due = last.is_none_or(|tick| tick.elapsed() >= PRUNE_EVERY);
        if due {
            *last = Some(Instant::now());
        }
        due
    };
    let flag = Arc::clone(&arm.in_flight);
    std::thread::spawn(move || {
        let graph = crate::backlog::settings::graph_path();
        if let Err(error) = flush(&graph) {
            eprintln!("backlog-share: flush: {error}");
        }
        if let Err(error) = sync(&graph) {
            eprintln!("backlog-share: sync: {error}");
        }
        if let Err(error) = crate::plan_carry::sync(&graph) {
            eprintln!("backlog-share: plans: {error}");
        }
        if prune_due {
            if let Err(error) = report_and_prune(&graph) {
                eprintln!("backlog-share: prune: {error}");
            }
        }
        flag.store(false, Ordering::SeqCst);
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store_remote::test_primary;
    use rusqlite::params;
    use std::path::PathBuf;

    /// The fixtures write rows the way any writer does, through the open
    /// seam; the store module owns the table, so its name rides a constant.
    const NODES: &str = "nodes";

    struct Machine {
        _dir: tempfile::TempDir,
        graph: PathBuf,
    }

    fn machine() -> Machine {
        let dir = tempfile::tempdir().unwrap();
        let graph = dir.path().join("graph.json");
        drop(crate::backlog::open(&graph).unwrap());
        Machine { _dir: dir, graph }
    }

    fn on(primary: &test_primary::Primary, m: &Machine) {
        route_to_primary(Some((
            primary.remote.clone(),
            crate::backlog::database_path(&m.graph),
        )));
    }

    fn title(m: &Machine, id: &str) -> Option<String> {
        crate::backlog::open(&m.graph)
            .unwrap()
            .query_row("SELECT title FROM nodes WHERE id = ?1", [id], |r| r.get(0))
            .optional()
            .unwrap()
    }

    fn set_title(connection: &Connection, id: &str, title: &str) {
        connection
            .execute(
                &format!("UPDATE {NODES} SET title = ?2 WHERE id = ?1"),
                params![id, title],
            )
            .unwrap();
    }

    fn retitle(m: &Machine, id: &str, title: &str) {
        set_title(&crate::backlog::open(&m.graph).unwrap(), id, title);
    }

    fn outbox(m: &Machine) -> i64 {
        crate::backlog::open(&m.graph)
            .unwrap()
            .query_row("SELECT COUNT(*) FROM backlog_outbox", [], |r| r.get(0))
            .unwrap()
    }

    /// The rows the search index finds for `word`, once FTS5 confirms the
    /// index matches the nodes it mirrors.
    fn found(m: &Machine, word: &str) -> i64 {
        let connection = crate::backlog::open(&m.graph).unwrap();
        crate::backlog::search::check(&connection).unwrap();
        connection
            .query_row(
                "SELECT COUNT(*) FROM nodes_fts WHERE nodes_fts MATCH ?1",
                [word],
                |r| r.get(0),
            )
            .unwrap()
    }

    fn add_node(m: &Machine, id: &str, title: &str) {
        let mut connection = crate::backlog::open(&m.graph).unwrap();
        let transaction = connection.transaction().unwrap();
        transaction
            .execute(
                &format!(
                    "INSERT INTO {NODES} (id, ordinal, slug, title, kind, status, priority)
                     VALUES (?1, 1, ?1, 'draft', 'task', 'idea', 'p2')"
                ),
                params![id],
            )
            .unwrap();
        set_title(&transaction, id, title);
        transaction.commit().unwrap();
    }

    /// A node-scoped child row, the shape `id_of` missed: provenance keys on
    /// `node_id`, so a refused node's take-back set must reach it. Written
    /// through the owning module's save, like any writer.
    fn add_provenance(m: &Machine, node_id: &str) {
        let connection = crate::backlog::open(&m.graph).unwrap();
        let mut node = crate::backlog::nodes::load(&connection, node_id)
            .unwrap()
            .expect("fixture node exists");
        node.provenance.source = Some("fixture".into());
        crate::backlog::nodes::save(&connection, &node).unwrap();
    }

    /// A node_dispatch row for the orphan test, through the owning module.
    fn add_dispatch(m: &Machine, node_id: &str) {
        let connection = crate::backlog::open(&m.graph).unwrap();
        let mut node = crate::backlog::nodes::load(&connection, node_id)
            .unwrap()
            .expect("fixture node exists");
        node.dispatch.brief = Some("fixture brief".into());
        crate::backlog::nodes::save(&connection, &node).unwrap();
    }

    #[test]
    fn a_flushed_write_lands_on_the_primary_and_a_peer_syncs_it() {
        let primary = test_primary::start();
        let (a, b) = (machine(), machine());
        on(&primary, &a);
        seed(&a.graph).unwrap();
        add_node(&a, "x-1", "shared");
        assert_eq!(
            flush(&a.graph).unwrap(),
            1,
            "insert then update fold to one row"
        );
        assert_eq!(outbox(&a), 0);
        let (on_primary, log): (String, i64) = primary
            .db
            .lock()
            .unwrap()
            .query_row(
                "SELECT (SELECT title FROM nodes WHERE id = 'x-1'), (SELECT COUNT(*) FROM backlog_changes)",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!((on_primary.as_str(), log), ("shared", 1));

        on(&primary, &b);
        assert!(sync(&b.graph).unwrap().snapshot);
        assert_eq!(title(&b, "x-1").as_deref(), Some("shared"));
        assert_eq!(
            found(&b, "shared"),
            1,
            "the replica's search index sees the synced row"
        );

        // A's own write replays on A and leaves the row as it was.
        on(&primary, &a);
        sync(&a.graph).unwrap();
        assert_eq!(title(&a, "x-1").as_deref(), Some("shared"));

        // A build without this module registers no writer function, so the
        // guard refuses its write on both the seeding and the synced store.
        for m in [&a, &b] {
            let old_build = Connection::open(crate::backlog::database_path(&m.graph)).unwrap();
            let refused = old_build
                .execute(
                    &format!("UPDATE {NODES} SET title = 'old build' WHERE id = 'x-1'"),
                    [],
                )
                .unwrap_err()
                .to_string();
            assert!(refused.contains(WRITER), "{refused}");
        }
        assert_eq!(title(&a, "x-1").as_deref(), Some("shared"));

        // A retitle reaches the peer through the change log, not a snapshot:
        // the index drops the old title and finds the new one.
        retitle(&a, "x-1", "renamed");
        flush(&a.graph).unwrap();
        on(&primary, &b);
        assert!(!sync(&b.graph).unwrap().snapshot);
        assert_eq!((found(&b, "renamed"), found(&b, "shared")), (1, 0));

        // Sharing off: the next open drops the guard, so an older build
        // writes again.
        route_to_primary(None);
        drop(crate::backlog::open(&a.graph).unwrap());
        Connection::open(crate::backlog::database_path(&a.graph))
            .unwrap()
            .execute(
                &format!("UPDATE {NODES} SET title = 'old build' WHERE id = 'x-1'"),
                [],
            )
            .unwrap();
    }

    /// Cap the primary's replies at twice the widest one-row reply of `sql`
    /// over the keys `keys_sql` lists, so one row fits and a full page of
    /// six does not.
    fn cap_at_two_rows(primary: &test_primary::Primary, sql: &str, keys_sql: &str) {
        let keys: Vec<i64> = {
            let db = primary.db.lock().unwrap();
            let mut statement = db.prepare(keys_sql).unwrap();
            let keys = statement.query_map([], |r| r.get(0)).unwrap();
            keys.map(Result::unwrap).collect()
        };
        let one =
            |key: i64| primary.reply_len(sql, &[SqlValue::Integer(key - 1), SqlValue::Integer(1)]);
        let cap = 2 * keys.iter().map(|&key| one(key)).max().unwrap();
        let page = primary.reply_len(sql, &[SqlValue::Integer(0), SqlValue::Integer(PAGE)]);
        assert!(
            page > cap,
            "a full page ({page} bytes) must exceed the cap ({cap})"
        );
        primary.set_cap(cap);
    }

    #[test]
    fn a_page_over_the_primary_reply_limit_halves_until_it_fits() {
        let primary = test_primary::start();
        let (a, b) = (machine(), machine());
        let long = |word: &str| format!("{word} {}", "x".repeat(1_500));
        on(&primary, &a);
        seed(&a.graph).unwrap();
        for i in 0..6 {
            add_node(&a, &format!("x-{i}"), &long("first"));
            flush(&a.graph).unwrap();
        }
        let columns = shared_columns(&crate::backlog::open(&a.graph).unwrap()).unwrap();
        let nodes = format!(
            "SELECT rowid, {} FROM \"{NODES}\" WHERE rowid > ?1 ORDER BY rowid LIMIT ?2",
            quoted(&columns[NODES])
        );
        cap_at_two_rows(&primary, &nodes, &format!("SELECT rowid FROM {NODES}"));
        on(&primary, &b);
        assert!(sync(&b.graph).unwrap().snapshot);
        assert_eq!(found(&b, "first"), 6, "the snapshot read every row");

        primary.set_cap(usize::MAX);
        on(&primary, &a);
        for i in 0..6 {
            retitle(&a, &format!("x-{i}"), &long("second"));
            flush(&a.graph).unwrap();
        }
        let after = crate::backlog::meta(&crate::backlog::open(&b.graph).unwrap(), CURSOR)
            .unwrap()
            .unwrap();
        cap_at_two_rows(
            &primary,
            "SELECT seq, ops FROM backlog_changes WHERE seq > ?1 ORDER BY seq LIMIT ?2",
            &format!("SELECT seq FROM backlog_changes WHERE seq > {after}"),
        );
        on(&primary, &b);
        let receipt = sync(&b.graph).unwrap();
        assert!(!receipt.snapshot);
        assert_eq!(found(&b, "second"), 6, "the catch-up read every change");
        let top: i64 = primary
            .db
            .lock()
            .unwrap()
            .query_row("SELECT MAX(seq) FROM backlog_changes", [], |r| r.get(0))
            .unwrap();
        assert_eq!(receipt.cursor, top, "the cursor reached the log's end");
        route_to_primary(None);
    }

    #[test]
    fn a_stale_write_is_refused_and_an_unreachable_one_waits() {
        let primary = test_primary::start();
        let (a, b) = (machine(), machine());
        on(&primary, &a);
        seed(&a.graph).unwrap();
        add_node(&a, "x-1", "first");
        flush(&a.graph).unwrap();
        on(&primary, &b);
        sync(&b.graph).unwrap();
        on(&primary, &a);
        retitle(&a, "x-1", "from a");
        flush(&a.graph).unwrap();

        on(&primary, &b);
        retitle(&b, "x-1", "from b");
        let refusal = flush(&b.graph).unwrap_err();
        assert!(
            refusal.starts_with(REFUSED) && refusal.contains("nodes row changed"),
            "{refusal}"
        );
        assert_eq!(title(&b, "x-1").as_deref(), Some("from a"));
        assert_eq!(
            (found(&b, "a"), found(&b, "b")),
            (1, 0),
            "the take-back moves the index with the row"
        );
        assert_eq!(outbox(&b), 0);

        // A second seed refuses; an unreachable primary keeps the outbox.
        on(&primary, &a);
        assert!(seed(&a.graph).unwrap_err().contains("already holds"));
        route_to_primary(Some((
            test_primary::dead(),
            crate::backlog::database_path(&a.graph),
        )));
        add_node(&a, "x-2", "waits");
        assert!(flush(&a.graph).unwrap_err().contains("is unreachable"));
        assert!(outbox(&a) > 0);
        route_to_primary(None);
    }

    /// Two machines record a decision before either syncs, so both mint the
    /// same `decisions.seq`. A decision names no node, so the old take-back
    /// covered the whole batch: every flush after that reverted the
    /// machine's other writes and re-queued the clashing insert forever.
    #[test]
    fn two_writers_minting_one_decision_seq_both_land() {
        fn decide(m: &Machine, id: &str) {
            let event = json!({"type": "operator_decision", "source": "test",
                               "data": {"decision_id": id, "decision": id}});
            let connection = crate::backlog::open(&m.graph).unwrap();
            crate::backlog::decisions::record(&connection, &event).unwrap();
        }
        fn seq_of(connection: &Connection, id: &str) -> i64 {
            connection
                .query_row("SELECT seq FROM decisions WHERE event_id = ?1", [id], |r| {
                    r.get(0)
                })
                .unwrap()
        }
        let primary = test_primary::start();
        let (a, b) = (machine(), machine());
        on(&primary, &a);
        seed(&a.graph).unwrap();
        add_node(&a, "x-1", "first");
        flush(&a.graph).unwrap();
        on(&primary, &b);
        sync(&b.graph).unwrap();

        on(&primary, &a);
        decide(&a, "d-a");
        flush(&a.graph).unwrap();
        on(&primary, &b);
        decide(&b, "d-b");
        retitle(&b, "x-1", "from b");
        flush(&b.graph).unwrap();

        assert_eq!(outbox(&b), 0);
        let held = primary.db.lock().unwrap();
        let (on_a, on_b) = (seq_of(&*held, "d-a"), seq_of(&*held, "d-b"));
        let retitled: String = held
            .query_row("SELECT title FROM nodes WHERE id = 'x-1'", [], |r| r.get(0))
            .unwrap();
        drop(held);
        assert!(on_b > on_a, "d-b moved past d-a: {on_a} {on_b}");
        assert_eq!(retitled, "from b", "the batch's other write landed");
        let local = crate::backlog::open(&b.graph).unwrap();
        assert_eq!(seq_of(&local, "d-b"), on_b, "the replica row moved with it");
        drop(local);

        // The next local decision mints past the moved key.
        decide(&b, "d-b2");
        flush(&b.graph).unwrap();
        assert_eq!(outbox(&b), 0);
        on(&primary, &a);
        sync(&a.graph).unwrap();
        let synced = crate::backlog::open(&a.graph).unwrap();
        assert_eq!(seq_of(&synced, "d-b"), on_b);
        route_to_primary(None);
    }

    /// The silent take-back sequence: a landed add, a peer's change on the
    /// primary, a local update while the primary is unreachable, then a
    /// flush the primary refuses. The take-back must keep the refused
    /// statements for retry or inspection, and the notice must name what it
    /// reverted.
    #[test]
    fn a_refused_flush_keeps_the_statements_and_the_notice_names_the_revert() {
        let primary = test_primary::start();
        let a = machine();
        on(&primary, &a);
        seed(&a.graph).unwrap();
        add_node(&a, "fb-4908", "shared");
        flush(&a.graph).unwrap();

        // A peer moved the row on the primary behind this machine's back.
        primary
            .db
            .lock()
            .unwrap()
            .execute(
                &format!("UPDATE {NODES} SET title = 'peer' WHERE id = 'fb-4908'"),
                [],
            )
            .unwrap();

        // The primary goes unreachable; the write lands in the outbox only.
        route_to_primary(Some((
            test_primary::dead(),
            crate::backlog::database_path(&a.graph),
        )));
        crate::backlog::open(&a.graph)
            .unwrap()
            .execute(
                &format!(
                    "UPDATE {NODES} SET plan_path = 'p.md', status = 'ready'
                     WHERE id = 'fb-4908'"
                ),
                [],
            )
            .unwrap();
        assert!(flush(&a.graph).unwrap_err().contains("is unreachable"));
        assert!(outbox(&a) > 0);

        // The primary is back. Its refusal takes the row back to the peer's.
        on(&primary, &a);
        let refusal = flush(&a.graph).unwrap_err();
        route_to_primary(None);
        assert!(refusal.starts_with(REFUSED), "{refusal}");
        let connection = crate::backlog::open(&a.graph).unwrap();
        let (plan_path, status): (Option<String>, String) = connection
            .query_row(
                &format!("SELECT plan_path, status FROM {NODES} WHERE id = 'fb-4908'"),
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(
            (plan_path.as_deref(), status.as_str()),
            (None, "idea"),
            "the update was taken back, so the writer must be told"
        );
        assert_eq!(outbox(&a), 0);

        // The refused statements are kept, exactly as publish sent them.
        let (kept, error, ops): (i64, String, String) = connection
            .query_row(
                "SELECT COUNT(*), COALESCE(MAX(error), ''), COALESCE(MAX(ops), '')
                 FROM backlog_refused",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(kept, 1, "the refused batch is kept");
        assert!(error.starts_with(REFUSED), "{error}");
        let parsed = parse_ops(&ops).unwrap();
        assert!(
            parsed
                .iter()
                .any(|c| c.table == NODES && c.columns.iter().any(|name| name == "plan_path")),
            "{ops}"
        );
        assert!(
            refusal_notice(&parsed, 0, 0, 0).contains("fb-4908"),
            "{ops}"
        );
    }

    /// The take-back a refusal must perform: the refused update takes the
    /// primary's copy back, and a node minted alongside it in the same batch
    /// survives with its insert re-queued for the next flush. A second case
    /// covers the id clash: the insert itself refused, the local mint
    /// survives, and the refused batch names it.
    #[test]
    fn a_refused_flush_keeps_rows_the_refusal_did_not_name() {
        let primary = test_primary::start();
        let a = machine();
        on(&primary, &a);
        seed(&a.graph).unwrap();
        add_node(&a, "x-1", "shared");
        flush(&a.graph).unwrap();

        // A peer moved x-1 on the primary behind this machine's back.
        primary
            .db
            .lock()
            .unwrap()
            .execute(
                &format!("UPDATE {NODES} SET title = 'peer' WHERE id = 'x-1'"),
                [],
            )
            .unwrap();

        // One batch: the stale x-1 update and a fresh node's rows (the
        // nodes insert plus its node-scoped provenance row).
        retitle(&a, "x-1", "from a");
        add_node(&a, "x-new", "brand new");
        add_provenance(&a, "x-new");
        let refusal = flush(&a.graph).unwrap_err();
        route_to_primary(None);
        assert!(refusal.starts_with(REFUSED), "{refusal}");

        // The refused update took the primary's copy back.
        assert_eq!(title(&a, "x-1").as_deref(), Some("peer"));
        // The rows the refusal did not name survive, still queued: the
        // provenance row takes back with its node, not alone.
        assert_eq!(title(&a, "x-new").as_deref(), Some("brand new"));
        assert_eq!(outbox(&a), 2, "the new node's rows re-queue");

        // The next flush sends them, and the primary holds both.
        on(&primary, &a);
        flush(&a.graph).unwrap();
        assert_eq!(outbox(&a), 0);
        let (landed, prov): (String, i64) = primary
            .db
            .lock()
            .unwrap()
            .query_row(
                "SELECT (SELECT title FROM nodes WHERE id = 'x-new'),
                        (SELECT COUNT(*) FROM node_provenance WHERE node_id = 'x-new')",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(landed, "brand new");
        assert_eq!(prov, 1, "the provenance row landed with its node");

        // A second case: the refused change is itself the insert, and the
        // id already lives on the primary. The local mint and its child
        // rows survive, nothing re-queues (a retry would refuse forever),
        // and the refused batch names the node.
        primary
            .db
            .lock()
            .unwrap()
            .execute(
                &format!(
                    "INSERT INTO {NODES} (id, ordinal, slug, title, status, priority)
                     VALUES ('x-clash', 2, 'x-clash', 'peer mint', 'idea', 'p2')"
                ),
                [],
            )
            .unwrap();
        add_node(&a, "x-clash", "mine");
        add_provenance(&a, "x-clash");
        let refusal = flush(&a.graph).unwrap_err();
        route_to_primary(None);
        assert!(refusal.contains("kept its own row"), "{refusal}");
        assert_eq!(title(&a, "x-clash").as_deref(), Some("mine"));
        assert_eq!(outbox(&a), 0, "the refused insert does not re-queue");
        let connection = crate::backlog::open(&a.graph).unwrap();
        let (ops, prov_kept): (String, i64) = connection
            .query_row(
                "SELECT (SELECT ops FROM backlog_refused ORDER BY id DESC LIMIT 1),
                        (SELECT COUNT(*) FROM node_provenance WHERE node_id = 'x-clash')",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert!(ops.contains("x-clash"), "{ops}");
        assert_eq!(
            prov_kept, 1,
            "the refused node's child row stays local, unattached to the primary's row"
        );
    }

    /// The chain that lost the lost nodes a second way: a child-row insert
    /// the primary orphans (the peer deleted the node the row names) stays
    /// local and unsent, then the node's next write refuses with the primary
    /// holding no row. The take-back must keep the local row it cannot
    /// replace, or the node dies one write later.
    #[test]
    fn an_update_the_primary_has_no_row_for_keeps_the_local_row() {
        let primary = test_primary::start();
        let a = machine();
        on(&primary, &a);
        seed(&a.graph).unwrap();
        add_node(&a, "x-del", "doomed");
        flush(&a.graph).unwrap();

        // The peer deletes the node on the primary; this machine has not
        // synced the delete. A child-row write naming that node then
        // refuses: node_dispatch.node_id references nodes(id). Whether the
        // refusal lands on the child insert or the base row's rewritten
        // stamp, the batch rolls back whole and the take-back keeps every
        // local row of the node.
        primary
            .db
            .lock()
            .unwrap()
            .execute(&format!("DELETE FROM {NODES} WHERE id = 'x-del'"), [])
            .unwrap();
        add_dispatch(&a, "x-del");
        let refusal = flush(&a.graph).unwrap_err();
        route_to_primary(None);
        assert!(refusal.starts_with(REFUSED), "{refusal}");
        assert_eq!(title(&a, "x-del").as_deref(), Some("doomed"));
        assert_eq!(outbox(&a), 0, "the refused batch does not re-queue");

        // The node's next write refuses with the primary holding no row.
        // The take-back fetches nothing and must keep the local row.
        on(&primary, &a);
        retitle(&a, "x-del", "still here");
        let refusal = flush(&a.graph).unwrap_err();
        route_to_primary(None);
        assert!(refusal.starts_with(REFUSED), "{refusal}");
        assert_eq!(title(&a, "x-del").as_deref(), Some("still here"));
        assert_eq!(outbox(&a), 0);
    }

    #[test]
    fn a_pruned_change_log_keeps_its_newest_row_and_a_lagging_replica_resnapshots() {
        let primary = test_primary::start();
        let (a, b) = (machine(), machine());
        on(&primary, &a);
        seed(&a.graph).unwrap();
        add_node(&a, "x-1", "first");
        flush(&a.graph).unwrap();
        on(&primary, &b);
        sync(&b.graph).unwrap();

        // B sleeps while A writes twice and the log is pruned behind it.
        on(&primary, &a);
        retitle(&a, "x-1", "second");
        flush(&a.graph).unwrap();
        retitle(&a, "x-1", "third");
        flush(&a.graph).unwrap();
        let cutoff = crate::backlog::now_ms() as i64 + 1;
        let live = cutoff + 60_000;
        assert_eq!(
            prune(&primary.remote, cutoff, PAGE).unwrap(),
            0,
            "no replica reported, so nothing goes"
        );
        report_cursor(&primary.remote, "a", 3, live).unwrap();
        report_cursor(&primary.remote, "b", 1, live).unwrap();
        assert_eq!(
            prune(&primary.remote, 0, PAGE).unwrap(),
            0,
            "a young row stays"
        );
        assert_eq!(prune(&primary.remote, cutoff, PAGE).unwrap(), 1);
        assert_eq!(
            prune(&primary.remote, cutoff, PAGE).unwrap(),
            0,
            "rows a live replica has not read stay"
        );
        // B stops reporting: past the cutoff it no longer holds the log.
        report_cursor(&primary.remote, "b", 1, 0).unwrap();
        assert_eq!(
            prune(&primary.remote, cutoff, PAGE).unwrap(),
            1,
            "a dead replica's cursor holds nothing"
        );
        assert_eq!(
            prune(&primary.remote, cutoff, PAGE).unwrap(),
            0,
            "the newest row stays"
        );

        on(&primary, &b);
        let receipt = sync(&b.graph).unwrap();
        assert!(receipt.snapshot, "{receipt:?}");
        assert_eq!(title(&b, "x-1").as_deref(), Some("third"));
        assert!(
            !sync(&b.graph).unwrap().snapshot,
            "level again, the log serves it"
        );

        // A skewed clock can leave a hole behind a surviving row: the
        // replica must not read past it.
        on(&primary, &a);
        retitle(&a, "x-1", "fourth");
        flush(&a.graph).unwrap();
        retitle(&a, "x-1", "fifth");
        flush(&a.graph).unwrap();
        retitle(&a, "x-1", "sixth");
        flush(&a.graph).unwrap();
        primary
            .db
            .lock()
            .unwrap()
            .execute("DELETE FROM backlog_changes WHERE seq = 5", [])
            .unwrap();
        on(&primary, &b);
        assert!(sync(&b.graph).unwrap().snapshot);
        assert_eq!(title(&b, "x-1").as_deref(), Some("sixth"));
        route_to_primary(None);

        let probed = probe(&primary.remote).unwrap();
        assert_eq!(
            (
                probed["change_log"]["first_seq"].as_i64(),
                probed["change_log"]["last_seq"].as_i64()
            ),
            (Some(3), Some(6)),
            "{probed}"
        );
        assert!(probed["write_ms"].is_u64(), "{probed}");
    }

    #[test]
    fn a_whole_graph_write_stores_no_readiness_status() {
        // The flip defect: the writer read ran the readiness overlay, so a
        // whole-graph write stored `blocked` for an open blocker and the
        // next writer with a different blocker view stored the rung status
        // back. The write path must ship the statuses it read.
        let primary = test_primary::start();
        let a = machine();
        on(&primary, &a);
        let rows = vec![
            serde_json::json!({"id": "x-a", "title": "dependent work", "type": "feature",
                               "status": "ready", "priority": "p2", "domain": "code",
                               "blocked_by": ["x-b"]}),
            serde_json::json!({"id": "x-b", "title": "the blocker", "type": "feature",
                               "status": "idea", "priority": "p2", "domain": "code"}),
        ];
        crate::graph_store::seed_rows(&a.graph, &rows).unwrap();
        seed(&a.graph).unwrap();
        let retitle_once = |graph: &PathBuf| {
            crate::graph_store::mutate_rows(
                graph,
                std::time::Duration::from_secs(5),
                None,
                None,
                |working| {
                    for row in working.iter_mut() {
                        if row.get("id").and_then(Value::as_str) == Some("x-a") {
                            if let Some(obj) = row.as_object_mut() {
                                obj.insert("title".into(), Value::String("renamed".into()));
                            }
                        }
                    }
                    Ok(true)
                },
            )
            .unwrap()
        };
        assert!(retitle_once(&a.graph).is_some(), "the write lands");
        let connection = crate::backlog::open(&a.graph).unwrap();
        let stored: String = connection
            .query_row("SELECT status FROM nodes WHERE id = 'x-a'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(
            stored, "ready",
            "the overlay's blocked never reaches the stored row"
        );
        let ops: String = primary
            .db
            .lock()
            .unwrap()
            .query_row(
                "SELECT ops FROM backlog_changes ORDER BY seq DESC LIMIT 1",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!(
            !ops.contains("blocked"),
            "the shipped batch names no overlay answer: {ops}"
        );
        let changes: Vec<Value> = serde_json::from_str(&ops).unwrap();
        assert!(
            changes
                .iter()
                .any(|c| c.get("t").and_then(Value::as_str) == Some("nodes")
                    && c.get("n")
                        .map(|n| n.to_string().contains("renamed"))
                        .unwrap_or(false)),
            "the batch ships the title change: {changes:?}"
        );
        // The same write again: nothing differs, so nothing ships.
        let before: i64 = primary
            .db
            .lock()
            .unwrap()
            .query_row("SELECT COUNT(*) FROM backlog_changes", [], |r| r.get(0))
            .unwrap();
        assert!(retitle_once(&a.graph).is_some(), "the repeat write lands");
        let after: i64 = primary
            .db
            .lock()
            .unwrap()
            .query_row("SELECT COUNT(*) FROM backlog_changes", [], |r| r.get(0))
            .unwrap();
        assert_eq!(after, before, "the identical write adds no change at all");
        route_to_primary(None);
    }

    #[test]
    fn a_landed_batch_stamps_its_writer() {
        // The next flip names itself: every landed batch carries the writing
        // process's pid and command head in backlog_writers.
        let primary = test_primary::start();
        let a = machine();
        on(&primary, &a);
        seed(&a.graph).unwrap();
        add_node(&a, "x-w", "stamped");
        assert_eq!(flush(&a.graph).unwrap(), 1);
        let (pid, cmd): (i64, String) = primary
            .db
            .lock()
            .unwrap()
            .query_row(
                "SELECT pid, cmd FROM backlog_writers ORDER BY at DESC, batch DESC LIMIT 1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(pid, i64::from(std::process::id()), "the flush process");
        assert!(
            !cmd.is_empty() && cmd.len() <= 200,
            "the command head is present and capped: {cmd}"
        );
        // A stamp lives exactly as long as its batch: an aged batch and its
        // stamp prune together, and the fresh batch's change-log row is the
        // newest, so its stamp survives the run.
        let connection = primary.db.lock().unwrap();
        connection
            .execute(
                "INSERT INTO backlog_changes (seq, origin, batch, at, ops) VALUES
                 (1, 'o', 'old', 500, '[]')",
                [],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO backlog_cursors (origin, seq, at) VALUES ('o', 1, 60000)",
                [],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO backlog_writers (batch, pid, cmd, at) VALUES ('old', 1, 'old-cmd', 500)",
                [],
            )
            .unwrap();
        drop(connection);
        prune(&primary.remote, 50_000, 100).unwrap();
        let live: String = primary
            .db
            .lock()
            .unwrap()
            .query_row(
                "SELECT batch FROM backlog_writers ORDER BY at DESC, batch DESC LIMIT 1",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_ne!(live, "old", "the aged stamp went with its batch");
        let batch_of_live: i64 = primary
            .db
            .lock()
            .unwrap()
            .query_row(
                "SELECT COUNT(*) FROM backlog_changes WHERE batch = ?1",
                [&live],
                |r| r.get(0),
            )
            .unwrap_or(0);
        assert_eq!(batch_of_live, 1, "the surviving stamp's batch still logs");
        route_to_primary(None);
    }

    #[test]
    fn the_stale_row_guard_text_names_the_row_instead_of_the_check() {
        let changes = vec![Change {
            op: Op::Update,
            table: "pull_requests".to_string(),
            columns: vec!["note".to_string()],
            old: vec![],
            new: vec![],
        }];
        let message = refusal_message(
            "remote store http://r2d2:18080: SQLite error: CHECK constraint failed: n = 1",
            &changes,
            Some(0),
        );
        assert!(message.contains("pull_requests"), "{message}");
        assert!(!message.contains(CAS_GUARD), "{message}");
    }

    #[test]
    fn the_probe_reads_the_recent_average_batch_size() {
        // LENGTH counts the ops string: 3, 7 and 5 bytes average to 5.
        let primary = test_primary::start();
        primary
            .db
            .lock()
            .unwrap()
            .execute(
                "INSERT INTO backlog_changes (seq, origin, batch, at, ops) VALUES
                 (1, 'o', 'a', 500, '[1]'), (2, 'o', 'b', 600, '[1,2,3]'), (3, 'o', 'c', 700, '[1,2]')",
                [],
            )
            .unwrap();
        let probed = probe(&primary.remote).unwrap();
        assert_eq!(
            probed["change_log"]["recent_avg_batch_bytes"],
            serde_json::json!(5),
            "{probed}"
        );
        route_to_primary(None);
    }
}
