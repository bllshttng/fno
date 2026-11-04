//! Short numbers for decisions and questions: `D142` and `Q87` beside the
//! hash ids.
//!
//! The hash stays the durable key. The number is a display name the store
//! mints: the shared primary when `store.remote_url` is set, else this
//! machine's graph.db. With a primary set, a machine never mints locally; its
//! local `fno_numbers` table is a cache of the primary's rows. Two machines
//! therefore read one number for one question.
//!
//! `decisions.seq` is a per-machine rowid and two machines can write the same
//! value, so it is never a number and this module never reads it.

use crate::store_remote::{Remote, SqlValue};
use rusqlite::{params, Connection, OpenFlags};
use serde_json::Value;
use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const DDL: &str = "CREATE TABLE IF NOT EXISTS fno_numbers (\
kind TEXT NOT NULL, \
num INTEGER NOT NULL, \
key TEXT NOT NULL, \
store TEXT NOT NULL, \
host TEXT NOT NULL, \
created_at INTEGER NOT NULL, \
PRIMARY KEY (store, kind, num), \
UNIQUE (store, key))";

const INSERT: &str = "INSERT OR IGNORE INTO fno_numbers \
(kind, num, key, store, host, created_at) \
SELECT ?1, COALESCE(MAX(num), 0) + 1, ?2, ?3, ?4, strftime('%s','now') \
FROM fno_numbers WHERE kind = ?1 AND store = ?3";

/// A node number skips every value that already names a node: an all-digit
/// hash is a real id, so the node due that number takes the next free one. One typed token then reads one node. `substr(?1, 3)` strips the `n:` off the kind.
const NODE_INSERT: &str = "INSERT OR IGNORE INTO fno_numbers \
(kind, num, key, store, host, created_at) \
SELECT ?1, (WITH RECURSIVE c(n) AS (\
SELECT COALESCE(MAX(num), 0) + 1 FROM fno_numbers WHERE kind = ?1 AND store = ?3 \
UNION ALL SELECT n + 1 FROM c \
WHERE EXISTS (SELECT 1 FROM nodes WHERE id = substr(?1, 3) || '-' || n)) \
SELECT n FROM c ORDER BY n DESC LIMIT 1), ?2, ?3, ?4, strftime('%s','now')";

const LOCAL_STORE: &str = "local";
const NODE_KIND: &str = "n:";
const TICK: Duration = Duration::from_secs(60);
const MINT_BATCH: usize = 200;

/// The bare id of a question copy: `q-1234abcd@imac2d2` is `q-1234abcd`. A
/// copy and its original hold one number and one board row.
///
/// A fixed id such as `provider-cap:<lane>@<host>` keeps its host: two machines
/// both use that id for two different questions.
pub fn canonical(id: &str) -> &str {
    match id.split_once('@') {
        Some((bare, _)) if is_question_hash(bare) => bare,
        _ => id,
    }
}

fn is_question_hash(id: &str) -> bool {
    id.strip_prefix("q-")
        .is_some_and(|hex| hex.len() == 8 && hex.bytes().all(|b| b.is_ascii_hexdigit()))
}

fn kind_of(key: &str) -> Option<&'static str> {
    if key.starts_with("r-") {
        None // A retraction prints as `retracts D142`; it has no number.
    } else if key.starts_with("d-") {
        Some("d")
    } else {
        Some("q")
    }
}

fn is_number_text(text: &str) -> Option<(char, &str, Option<&str>)> {
    let (head, store) = match text.split_once('@') {
        Some((head, store)) if !store.is_empty() => (head, Some(store)),
        _ => (text, None),
    };
    let mut chars = head.chars();
    let kind = chars.next()?.to_ascii_lowercase();
    let rest = chars.as_str();
    let digits = rest.strip_prefix('-').unwrap_or(rest);
    let is_digits = (1..=7).contains(&digits.len()) && digits.bytes().all(|b| b.is_ascii_digit());
    (matches!(kind, 'd' | 'q') && is_digits).then_some((kind, digits, store))
}

/// The store name this machine counts in.
pub fn current_store() -> String {
    match crate::store_remote::configured() {
        Ok(Some(remote)) => remote.host().to_string(),
        _ => LOCAL_STORE.to_string(),
    }
}

/// A read-only connection on the local table. A missing file or table reads
/// as no numbers, so every printer falls back to the hash.
fn open_read() -> Option<Connection> {
    open_read_at(&db_path())
}

/// The graph store's SQLite file. `default_graph_path` names the `graph.json`
/// anchor, which is not the database.
fn db_path() -> std::path::PathBuf {
    crate::backlog::database_path(&crate::graph_get::default_graph_path())
}

fn open_read_at(graph: &Path) -> Option<Connection> {
    Connection::open_with_flags(graph, OpenFlags::SQLITE_OPEN_READ_ONLY).ok()
}

fn open_write_at(graph: &Path) -> Result<Connection, String> {
    let conn = Connection::open(graph).map_err(|e| format!("open {}: {e}", graph.display()))?;
    conn.busy_timeout(Duration::from_secs(5))
        .map_err(|e| e.to_string())?;
    conn.execute_batch(DDL).map_err(|e| e.to_string())?;
    Ok(conn)
}

/// The counter a node id counts in: `n:x` for `x-aaaa`. Each prefix counts
/// alone, so `x-15` and `fb-15` are two nodes.
fn node_kind(id: &str) -> Option<String> {
    let (prefix, tail) = id.split_once('-')?;
    let word = !prefix.is_empty() && prefix.bytes().all(|b| b.is_ascii_lowercase());
    (word && !tail.is_empty()).then(|| format!("{NODE_KIND}{prefix}"))
}

/// The kind and key of every id to number: decisions and questions by hash
/// shape, nodes by the `n:` marker the caller put on them.
fn kind_and_key(key: &str) -> Option<(String, &str)> {
    match key.strip_prefix(NODE_KIND) {
        Some(id) => Some((node_kind(id)?, id)),
        None => Some((kind_of(key)?.to_string(), key)),
    }
}

fn insert_for(kind: &str) -> &'static str {
    if kind.starts_with(NODE_KIND) {
        NODE_INSERT
    } else {
        INSERT
    }
}

/// Mint one key in a local store. A second mint of a key is a no-op. A node
/// key reads `n:<node id>`.
pub fn mint_local(conn: &Connection, key: &str, store: &str, host: &str) -> Result<(), String> {
    let Some((kind, key)) = kind_and_key(key) else {
        return Ok(());
    };
    conn.execute(insert_for(&kind), params![kind, key, store, host])
        .map(drop)
        .map_err(|e| e.to_string())
}

fn mint_remote(remote: &Remote, keys: &[String], host: &str) -> Result<(), String> {
    let store = remote.host().to_string();
    let steps: Vec<(String, Vec<SqlValue>)> = keys
        .iter()
        .filter_map(|key| {
            let (kind, key) = kind_and_key(key)?;
            Some((
                insert_for(&kind).to_string(),
                vec![
                    SqlValue::Text(kind),
                    SqlValue::Text(key.to_string()),
                    SqlValue::Text(store.clone()),
                    SqlValue::Text(host.to_string()),
                ],
            ))
        })
        .collect();
    remote
        .transaction(&steps)
        .map(drop)
        .map_err(|(_, error)| error)
}

/// Copy the primary's rows this machine does not hold into the local cache.
/// Numbers only grow and the primary serializes the mint, so the highest
/// cached number per kind is the cursor.
fn pull(remote: &Remote, conn: &Connection) -> Result<usize, String> {
    let store = remote.host().to_string();
    let mut pulled = 0;
    let kinds = remote.execute(
        "SELECT DISTINCT kind FROM fno_numbers WHERE store = ?",
        &[SqlValue::Text(store.clone())],
    )?;
    let kinds: Vec<String> = kinds
        .rows
        .iter()
        .filter_map(|row| row.first().and_then(SqlValue::text).map(str::to_string))
        .collect();
    for kind in kinds.iter().map(String::as_str) {
        let cursor: i64 = conn
            .query_row(
                "SELECT COALESCE(MAX(num), 0) FROM fno_numbers WHERE store = ?1 AND kind = ?2",
                params![store, kind],
                |row| row.get(0),
            )
            .map_err(|e| e.to_string())?;
        let reply = remote.execute(
            "SELECT num, key, host, created_at FROM fno_numbers \
             WHERE store = ? AND kind = ? AND num > ? ORDER BY num",
            &[
                SqlValue::Text(store.clone()),
                SqlValue::Text(kind.to_string()),
                SqlValue::Integer(cursor),
            ],
        )?;
        for row in &reply.rows {
            let (Some(num), Some(key), Some(host), Some(created)) = (
                row.first().and_then(SqlValue::integer),
                row.get(1).and_then(SqlValue::text),
                row.get(2).and_then(SqlValue::text),
                row.get(3).and_then(SqlValue::integer),
            ) else {
                continue;
            };
            pulled += conn
                .execute(
                    "INSERT OR IGNORE INTO fno_numbers \
                     (kind, num, key, store, host, created_at) VALUES (?1,?2,?3,?4,?5,?6)",
                    params![kind, num, key, store, host, created],
                )
                .map_err(|e| e.to_string())?;
        }
    }
    Ok(pulled)
}

fn print_row(kind: &str, num: i64, store: &str, current: &str) -> String {
    let letter = match kind.strip_prefix(NODE_KIND) {
        Some(prefix) => format!("{prefix}-"),
        None => kind.to_ascii_uppercase(),
    };
    if store == current {
        format!("{letter}{num}")
    } else {
        format!("{letter}{num}@{store}")
    }
}

/// The number of one id: the current store's row first, else the newest row
/// from another store, printed qualified.
pub fn label_in(conn: &Connection, id: &str, current: &str) -> Option<String> {
    conn.query_row(
        "SELECT kind, num, store FROM fno_numbers WHERE key = ?1 \
         ORDER BY store = ?2 DESC, created_at DESC LIMIT 1",
        params![canonical(id), current],
        |row| {
            Ok(print_row(
                &row.get::<_, String>(0)?,
                row.get(1)?,
                &row.get::<_, String>(2)?,
                current,
            ))
        },
    )
    .ok()
}

/// Every number this machine holds, keyed by hash, for a listing. The current
/// store's row wins over another store's row for one key.
pub fn labels_in(conn: &Connection, current: &str) -> HashMap<String, String> {
    let mut out = HashMap::new();
    let Ok(mut stmt) = conn
        .prepare("SELECT key, kind, num, store FROM fno_numbers ORDER BY store = ?1, created_at")
    else {
        return out;
    };
    let rows = stmt.query_map(params![current], |row| {
        Ok((
            row.get::<_, String>(0)?,
            print_row(
                &row.get::<_, String>(1)?,
                row.get(2)?,
                &row.get::<_, String>(3)?,
                current,
            ),
        ))
    });
    if let Ok(rows) = rows {
        out.extend(rows.flatten());
    }
    out
}

/// The hash a typed token names. `D142`, `d142`, `d-142` resolve through the
/// table. `d-` plus 8 hex is a hash and passes through lowercased, even when
/// all 8 are digits. `None` means the token reads as a number no row holds.
pub fn resolve_in(
    conn: Option<&Connection>,
    kind: char,
    text: &str,
    current: &str,
) -> Option<String> {
    let text = text.trim();
    let Some((letter, digits, store)) = is_number_text(text) else {
        // A hash in any case reads as its lowercase form; any other word (a
        // subject) passes through as typed.
        let lowered = text.to_ascii_lowercase();
        let is_hash = lowered
            .get(2..)
            .is_some_and(|hex| !hex.is_empty() && hex.bytes().all(|b| b.is_ascii_hexdigit()))
            && (lowered.starts_with("d-") || lowered.starts_with("q-"));
        return Some(if is_hash { lowered } else { text.to_string() });
    };
    if letter != kind {
        return None;
    }
    let num: i64 = digits.parse().ok()?;
    conn?
        .query_row(
            "SELECT key FROM fno_numbers WHERE kind = ?1 AND num = ?2 AND store = ?3",
            params![kind.to_string(), num, store.unwrap_or(current)],
            |row| row.get(0),
        )
        .ok()
}

/// A number token nothing holds reads as this refusal, never as a hash.
pub fn unresolved(text: &str) -> String {
    format!(
        "no {} numbered {} on this machine yet; use the hash",
        if text.trim().to_ascii_lowercase().starts_with('d') {
            "decision"
        } else {
            "question"
        },
        text.trim().to_ascii_uppercase()
    )
}

/// `resolve_in` over this machine's table.
pub fn resolve(kind: char, text: &str) -> Option<String> {
    resolve_in(open_read().as_ref(), kind, text, &current_store())
}

/// The `<prefix>-<digits>` split of a typed node number: `x-15` is
/// (`x`, 15, no store). A `@store` tail names another machine's count.
fn node_number_text(text: &str) -> Option<(&str, i64, Option<&str>)> {
    let (head, store) = match text.split_once('@') {
        Some((head, store)) if !store.is_empty() => (head, Some(store)),
        _ => (text, None),
    };
    let (prefix, digits) = head.split_once('-')?;
    let word = !prefix.is_empty() && prefix.bytes().all(|b| b.is_ascii_lowercase());
    let digits_ok = (1..=7).contains(&digits.len()) && digits.bytes().all(|b| b.is_ascii_digit());
    (word && digits_ok).then(|| (prefix, digits.parse().unwrap_or(0), store))
}

/// The node id a typed number names: `x-15` is the 15th `x` node. `None`
/// when the token is not a number or nothing holds it. The caller asks the
/// exact-id tier first, so a hash that reads like a number still wins.
pub fn resolve_node_in(conn: Option<&Connection>, text: &str, current: &str) -> Option<String> {
    let (prefix, num, store) = node_number_text(text.trim())?;
    conn?
        .query_row(
            "SELECT key FROM fno_numbers WHERE kind = ?1 AND num = ?2 AND store = ?3",
            params![
                format!("{NODE_KIND}{prefix}"),
                num,
                store.unwrap_or(current)
            ],
            |row| row.get(0),
        )
        .ok()
}

/// `resolve_node_in` over this machine's table.
pub fn resolve_node(text: &str) -> Option<String> {
    node_number_text(text.trim())?;
    resolve_node_in(open_read().as_ref(), text, &current_store())
}

/// Every number a node holds, as text: `x-15`. A new hash must avoid these,
/// or one typed token would name two nodes.
pub fn node_number_texts() -> std::collections::BTreeSet<String> {
    let Some(conn) = open_read() else {
        return Default::default();
    };
    let Ok(mut stmt) =
        conn.prepare("SELECT substr(kind, 3) || '-' || num FROM fno_numbers WHERE kind LIKE 'n:%'")
    else {
        return Default::default();
    };
    let rows = stmt.query_map([], |row| row.get::<_, String>(0));
    rows.map(|rows| rows.flatten().collect())
        .unwrap_or_default()
}

/// `Q87`, or `None` while the arm has not reached this id.
pub fn label(id: &str) -> Option<String> {
    label_in(&open_read()?, id, &current_store())
}

/// `Q87 (q-aae5187b)`, or the id alone while it has no number.
pub fn display(id: &str) -> String {
    match label(id) {
        Some(number) => format!("{number} ({id})"),
        None => id.to_string(),
    }
}

/// Every number this machine holds, for a printer with many rows.
pub fn labels() -> HashMap<String, String> {
    open_read()
        .map(|conn| labels_in(&conn, &current_store()))
        .unwrap_or_default()
}

/// Put the number beside every hash a text cites: `q-aae5187b` reads
/// `Q87 (q-aae5187b)`. A hash already in that form, or one with no number,
/// stays as it is. Machine text keeps the hash, so a reader can still copy it.
pub fn annotate(text: &str) -> String {
    let held = labels();
    annotate_with(text, &held)
}

fn is_question_or_decision(hash: &str) -> bool {
    hash.len() == 10 && (hash.starts_with("d-") || hash.starts_with("q-"))
}

/// A node id sits inside paths (`worktrees/x-aaaa`), branch names
/// (`feature/x-aaaa-slug`), backticked commands and URLs. Only a hash that
/// stands alone between spaces and sentence punctuation gets a number beside it.
fn standalone(text: &str, span: std::ops::Range<usize>) -> bool {
    let before = text[..span.start].chars().next_back();
    let after = text[span.end..].chars().next();
    let after_next = text[span.end..].chars().nth(1);
    let open = before.is_none_or(|c| c.is_whitespace() || matches!(c, '(' | '[' | '"' | '\''));
    let close = match after {
        None => true,
        Some(c) if c.is_whitespace() => true,
        Some(',' | ';' | ':' | ')' | ']' | '"' | '\'') => true,
        Some('.') => after_next.is_none_or(char::is_whitespace),
        _ => false,
    };
    open && close
}

fn annotate_with(text: &str, held: &HashMap<String, String>) -> String {
    static HASH: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let hash = HASH.get_or_init(|| {
        regex::Regex::new(r"\b(?:[dq]-[0-9a-f]{8}|[a-z]{1,8}-[0-9a-f]{4})\b").expect("hash regex")
    });
    if held.is_empty() {
        return text.to_string();
    }
    let mut out = String::with_capacity(text.len());
    let mut last = 0;
    for found in hash.find_iter(text) {
        out.push_str(&text[last..found.start()]);
        match held.get(found.as_str()) {
            Some(number)
                if !text[..found.start()].ends_with('(')
                    && (is_question_or_decision(found.as_str())
                        || standalone(text, found.range())) =>
            {
                out.push_str(&format!("{number} ({})", found.as_str()));
            }
            _ => out.push_str(found.as_str()),
        }
        last = found.end();
    }
    out.push_str(&text[last..]);
    out
}

/// The keys to number, oldest first: every decision, then every question,
/// merged on the timestamp, copies folded to their bare id.
fn wanted(
    decisions: &[Value],
    asked: &serde_json::Map<String, Value>,
    extra: &[String],
) -> Vec<String> {
    let mut stamped: Vec<(String, String)> = Vec::new();
    for row in decisions {
        let id = row.get("decision_id").and_then(Value::as_str).unwrap_or("");
        if id.starts_with("d-") {
            let ts = row.get("ts").and_then(Value::as_str).unwrap_or("");
            stamped.push((ts.to_string(), id.to_string()));
        }
    }
    for (id, row) in asked {
        let ts = row.get("ts").and_then(Value::as_str).unwrap_or("");
        stamped.push((ts.to_string(), canonical(id).to_string()));
    }
    for id in extra {
        stamped.push((String::from("~"), canonical(id).to_string()));
    }
    stamped.sort();
    let mut seen = std::collections::HashSet::new();
    stamped
        .into_iter()
        .filter(|(_, id)| seen.insert(id.clone()))
        .map(|(_, id)| id)
        .collect()
}

fn unnumbered(conn: &Connection, keys: Vec<String>, store: &str) -> Vec<String> {
    let bare = |key: &str| key.strip_prefix(NODE_KIND).unwrap_or(key).to_string();
    let held: std::collections::HashSet<String> = conn
        .prepare("SELECT key FROM fno_numbers WHERE store = ?1")
        .and_then(|mut stmt| {
            let rows = stmt.query_map(params![store], |row| row.get::<_, String>(0))?;
            Ok(rows.flatten().collect())
        })
        .unwrap_or_default();
    keys.into_iter()
        .filter(|key| !held.contains(&bare(key)))
        .collect()
}

/// One sweep: mint every unnumbered key, then cache the primary's rows. An
/// unreachable primary ends the sweep quietly; the next one finds the same
/// keys, so no outbox holds them.
pub fn sweep(
    graph: &Path,
    remote: Option<&Remote>,
    keys: Vec<String>,
    host: &str,
) -> Result<usize, String> {
    let conn = open_write_at(graph)?;
    let Some(remote) = remote else {
        let missing = unnumbered(&conn, keys, LOCAL_STORE);
        for key in &missing {
            mint_local(&conn, key, LOCAL_STORE, host)?;
        }
        return Ok(missing.len());
    };
    let outcome = (|| {
        // Only a success is remembered: a primary that was down on the first
        // beat must get the create again when it returns.
        static SCHEMA: std::sync::OnceLock<()> = std::sync::OnceLock::new();
        if SCHEMA.get().is_none() {
            remote.script(DDL)?;
            let _ = SCHEMA.set(());
        }
        let missing = unnumbered(&conn, keys, remote.host());
        let (nodes, rest): (Vec<String>, Vec<String>) = missing
            .into_iter()
            .partition(|key| key.starts_with(NODE_KIND));
        // A bound per beat keeps the backfill under the reply wait.
        if !rest.is_empty() {
            mint_remote(remote, &rest[..rest.len().min(MINT_BATCH)], host)?;
        }
        // Nodes mint in their own transaction: their insert reads the
        // primary's `nodes` table, and a primary without it must not stop
        // the decision and question counts.
        let nodes_result = if nodes.is_empty() {
            Ok(())
        } else {
            mint_remote(remote, &nodes[..nodes.len().min(MINT_BATCH)], host)
        };
        let pulled = pull(remote, &conn);
        nodes_result?;
        pulled
    })();
    match outcome {
        Err(error) if crate::store_remote::is_unreachable(&error) => Ok(0),
        other => other,
    }
}

/// Every node id, oldest first, so history reads x-1..x-N in filing order.
fn node_ids_oldest_first(graph: &Path) -> Vec<String> {
    let Some(conn) = open_read_at(graph) else {
        return Vec::new();
    };
    let Ok(mut stmt) = conn.prepare("SELECT id FROM nodes ORDER BY created_at, ordinal, id") else {
        return Vec::new();
    };
    let rows = stmt.query_map([], |row| row.get::<_, String>(0));
    rows.map(|rows| rows.flatten().collect())
        .unwrap_or_default()
}

fn beat(cwd: &Path) -> Result<usize, String> {
    let remote = crate::store_remote::configured()?;
    let decisions = crate::decision_index::default_store_live()?;
    let home = crate::paths::AgentsHome::from_env();
    let (asked, _) =
        crate::question_intake::question_rows(&crate::provider_cap::questions_path(&home));
    // Notes and pins ride the board without an ask row; they get a number too.
    let (items, _, _) = crate::attention_arm::read_items(cwd);
    let extra: Vec<String> = items.into_iter().map(|item| item.id).collect();
    let mut keys = wanted(&decisions.rows, &asked, &extra);
    keys.extend(
        node_ids_oldest_first(&db_path())
            .into_iter()
            .map(|id| format!("{NODE_KIND}{id}")),
    );
    sweep(
        &db_path(),
        remote.as_ref(),
        keys,
        &crate::mail::address::normalize_host(&crate::claims::hostname()),
    )
}

#[derive(Default)]
pub struct Arm {
    last_tick: Mutex<Option<Instant>>,
    in_flight: Arc<AtomicBool>,
}

/// The daemon's numbering beat: every 60 seconds, one in flight, off the
/// loop. The first beat is the backfill, so history reads D1..DN in time
/// order.
pub fn maybe_tick(arm: &Arm, cwd: &Path) {
    {
        let mut last = arm.last_tick.lock().unwrap_or_else(|e| e.into_inner());
        if last.is_some_and(|tick| tick.elapsed() < TICK)
            || arm.in_flight.swap(true, Ordering::SeqCst)
        {
            return;
        }
        *last = Some(Instant::now());
    }
    let flag = Arc::clone(&arm.in_flight);
    let cwd = cwd.to_path_buf();
    tokio::task::spawn_blocking(move || {
        let _gate = crate::daemon::SweepGate(flag);
        if let Err(error) = beat(&cwd) {
            eprintln!("numbers-arm: {error}");
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn conn() -> (tempfile::TempDir, std::path::PathBuf, Connection) {
        let dir = tempfile::tempdir().unwrap();
        let graph = dir.path().join("graph.db");
        let conn = open_write_at(&graph).unwrap();
        (dir, graph, conn)
    }

    #[test]
    fn a_second_mint_keeps_the_number_and_a_copy_reads_the_same_row() {
        let (_dir, _graph, conn) = conn();
        mint_local(&conn, "d-aaaaaaaa", "local", "h").unwrap();
        mint_local(&conn, "d-bbbbbbbb", "local", "h").unwrap();
        mint_local(&conn, "d-aaaaaaaa", "local", "h").unwrap();
        mint_local(&conn, "q-0000abcd", "local", "h").unwrap();
        mint_local(&conn, "r-0000abcd", "local", "h").unwrap();
        assert_eq!(label_in(&conn, "d-aaaaaaaa", "local").unwrap(), "D1");
        assert_eq!(label_in(&conn, "d-bbbbbbbb", "local").unwrap(), "D2");
        assert_eq!(label_in(&conn, "q-0000abcd@imac", "local").unwrap(), "Q1");
        assert_eq!(label_in(&conn, "r-0000abcd", "local"), None);
        assert_eq!(canonical("provider-cap:zai@imac"), "provider-cap:zai@imac");
    }

    #[test]
    fn resolve_reads_every_number_form_and_never_a_hash_as_a_number() {
        let (_dir, _graph, conn) = conn();
        mint_local(&conn, "d-cafe0001", "local", "h").unwrap();
        mint_local(&conn, "q-0000abcd", "local", "h").unwrap();
        for typed in ["D1", "d1", "d-1", " D-1 "] {
            assert_eq!(
                resolve_in(Some(&conn), 'd', typed, "local").unwrap(),
                "d-cafe0001"
            );
        }
        assert_eq!(
            resolve_in(Some(&conn), 'q', "q-1", "local").unwrap(),
            "q-0000abcd"
        );
        // Eight digits is a hash, by length.
        assert_eq!(
            resolve_in(Some(&conn), 'd', "d-12345678", "local").unwrap(),
            "d-12345678"
        );
        assert_eq!(
            resolve_in(Some(&conn), 'q', "Q-0000ABCD", "local").unwrap(),
            "q-0000abcd"
        );
        assert_eq!(resolve_in(Some(&conn), 'd', "D142", "local"), None);
        assert_eq!(resolve_in(None, 'd', "D1", "local"), None);

        // Another store's number prints qualified and resolves with its store.
        assert_eq!(label_in(&conn, "d-cafe0001", "r2d2").unwrap(), "D1@local");
        assert_eq!(resolve_in(Some(&conn), 'd', "D1", "r2d2"), None);
        assert_eq!(
            resolve_in(Some(&conn), 'd', "D1@local", "r2d2").unwrap(),
            "d-cafe0001"
        );
        mint_local(&conn, "d-cafe0001", "r2d2", "h").unwrap();
        assert_eq!(label_in(&conn, "d-cafe0001", "r2d2").unwrap(), "D1");
    }

    #[test]
    fn a_sweep_numbers_oldest_first_and_folds_a_copy_onto_its_original() {
        let (_dir, graph, conn) = conn();
        let decisions = vec![
            json!({"decision_id": "d-00000002", "ts": "2026-02-01"}),
            json!({"decision_id": "d-00000001", "ts": "2026-01-01"}),
            json!({"retraction_id": "r-00000001", "ts": "2026-03-01"}),
        ];
        let mut asked = serde_json::Map::new();
        asked.insert("q-00000001".into(), json!({"ts": "2026-01-02"}));
        asked.insert("q-00000001@imac".into(), json!({"ts": "2026-01-03"}));
        let keys = wanted(&decisions, &asked, &["note-a".to_string()]);
        assert_eq!(sweep(&graph, None, keys.clone(), "h").unwrap(), 4 - 1 + 1);
        assert_eq!(sweep(&graph, None, keys, "h").unwrap(), 0);
        assert_eq!(label_in(&conn, "d-00000001", "local").unwrap(), "D1");
        assert_eq!(label_in(&conn, "d-00000002", "local").unwrap(), "D2");
        assert_eq!(label_in(&conn, "q-00000001", "local").unwrap(), "Q1");
        assert_eq!(label_in(&conn, "note-a", "local").unwrap(), "Q2");
    }

    #[test]
    fn nodes_count_per_prefix_and_skip_numbers_that_are_hashes() {
        let (_dir, graph, conn) = conn();
        // `x-2` is a real hash, so the second `x` node takes 3.
        conn.execute_batch("CREATE TABLE nodes AS SELECT 'x-2' AS id")
            .unwrap();
        let keys: Vec<String> = ["x-aaaa", "x-bbbb", "fb-cccc"]
            .iter()
            .map(|id| format!("n:{id}"))
            .collect();
        assert_eq!(sweep(&graph, None, keys.clone(), "h").unwrap(), 3);
        assert_eq!(sweep(&graph, None, keys, "h").unwrap(), 0);
        assert_eq!(label_in(&conn, "x-aaaa", "local").unwrap(), "x-1");
        assert_eq!(label_in(&conn, "x-bbbb", "local").unwrap(), "x-3");
        assert_eq!(label_in(&conn, "fb-cccc", "local").unwrap(), "fb-1");
        let found = |text| resolve_node_in(Some(&conn), text, "local");
        assert_eq!(found("x-3").as_deref(), Some("x-bbbb"));
        assert_eq!(found("fb-1").as_deref(), Some("fb-cccc"));
        assert_eq!(found("x-2"), None);
        assert_eq!(found("x-aaaa"), None);
    }

    #[test]
    fn annotate_adds_the_number_once() {
        let held = HashMap::from([("q-0000abcd".to_string(), "Q3".to_string())]);
        assert_eq!(
            annotate_with("see q-0000abcd and q-ffffffff", &held),
            "see Q3 (q-0000abcd) and q-ffffffff"
        );
        assert_eq!(annotate_with("Q3 (q-0000abcd)", &held), "Q3 (q-0000abcd)");
    }

    #[test]
    fn a_node_number_beside_a_path_or_branch_is_left_alone() {
        let held = HashMap::from([("x-aaaa".to_string(), "x-12".to_string())]);
        assert_eq!(
            annotate_with("take x-aaaa, then x-aaaa.", &held),
            "take x-12 (x-aaaa), then x-12 (x-aaaa)."
        );
        for kept in [
            "worktrees/footnote/x-aaaa/crates",
            "feature/x-aaaa-numbers",
            "run `fno backlog get x-aaaa`",
        ] {
            assert_eq!(annotate_with(kept, &held), kept);
        }
    }

    #[test]
    fn a_missing_table_reads_as_no_number() {
        let dir = tempfile::tempdir().unwrap();
        let graph = dir.path().join("graph.db");
        Connection::open(&graph)
            .unwrap()
            .execute_batch("CREATE TABLE t (a)")
            .unwrap();
        let conn = open_read_at(&graph).unwrap();
        assert_eq!(label_in(&conn, "q-1", "local"), None);
        assert!(labels_in(&conn, "local").is_empty());
    }
}
