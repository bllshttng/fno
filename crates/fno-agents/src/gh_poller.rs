//! One GitHub poller per account across machines. With `store.remote_url`
//! set, the host that holds the `gh-budget:poller` lease on the claims
//! primary publishes each composed status row to `gh_status_rows`, and every
//! other host serves that row with zero gh calls. GitHub's secondary limit
//! counts requests per account, so two hosts reading one PR inside one TTL
//! spend twice what one needs.
//!
//! The lease is renewed only by a publish: a poller that stops reading
//! GitHub goes quiet and loses the lease after two status TTLs, and the next
//! live read on another host takes it. Every expiry and row age is computed
//! in SQL on the store clock, never on a host clock. An unset
//! `store.remote_url`, or a primary that errors, keeps today's per-machine
//! polling: a failed call writes a `poller-down` marker and the module skips
//! the primary for a minute.

use crate::claim_store::{self, CLOCK, POLLER_KEY};
use crate::claims;
use crate::store_remote::{Remote, SqlValue};
use serde_json::{json, Value};
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;
use std::time::Duration;

const DOWN_MARKER: &str = "poller-down";
const DOWN_FOR: Duration = Duration::from_secs(60);

const TABLE: &str = "CREATE TABLE IF NOT EXISTS gh_status_rows (
  slug TEXT NOT NULL,
  pr INTEGER NOT NULL,
  key TEXT NOT NULL,
  exit INTEGER NOT NULL,
  output TEXT NOT NULL,
  ts INTEGER NOT NULL,
  PRIMARY KEY (slug, pr)
)";

/// Rows this process published, and rows it served from the poller; the
/// spend notes read them.
static PUBLISHED: AtomicUsize = AtomicUsize::new(0);
static FOLLOWED: AtomicUsize = AtomicUsize::new(0);
static HOLDER: Mutex<Option<String>> = Mutex::new(None);

#[cfg(test)]
thread_local! {
    static TEST_PRIMARY: std::cell::RefCell<Option<(Remote, String)>> =
        const { std::cell::RefCell::new(None) };
}

/// Route this thread's poller calls to a test primary as `machine`.
#[cfg(test)]
pub(crate) fn route_to(primary: Option<(Remote, String)>) {
    TEST_PRIMARY.with(|p| *p.borrow_mut() = primary);
}

/// Two status TTLs in ms: the lease length and the oldest shared row a
/// follower serves.
pub(crate) fn lease_ms() -> i64 {
    crate::pr_status::cache::ttl() as i64 * 2000
}

fn down(marker_dir: &Path) -> bool {
    std::fs::metadata(marker_dir.join(DOWN_MARKER))
        .and_then(|meta| meta.modified())
        .ok()
        .and_then(|at| at.elapsed().ok())
        .is_some_and(|age| age < DOWN_FOR)
}

fn mark_down(marker_dir: &Path) {
    let _ = std::fs::create_dir_all(marker_dir);
    let _ = std::fs::write(marker_dir.join(DOWN_MARKER), b"");
}

/// The primary and this machine's id, or None when polling stays per-machine:
/// the key is unset, the machine has no stable id, or the primary failed in
/// the last minute.
fn primary(marker_dir: &Path) -> Option<(Remote, String)> {
    if down(marker_dir) {
        return None;
    }
    #[cfg(test)]
    if let Some(routed) = TEST_PRIMARY.with(|p| p.borrow().clone()) {
        return Some(routed);
    }
    // The key check comes first: a stock install must not pay the machine
    // id read, which shells out on macOS.
    let remote = crate::store_remote::configured().ok().flatten()?;
    let machine = claims::machine_id();
    (!machine.is_empty()).then_some((remote, machine))
}

/// One statement against the shared-row table, made on first use.
fn on_table(
    remote: &Remote,
    sql: &str,
    args: &[SqlValue],
) -> Result<crate::store_remote::Reply, String> {
    match remote.execute(sql, args) {
        Err(error) if error.contains("no such table") => {
            remote.script(TABLE)?;
            remote.execute(sql, args)
        }
        reply => reply,
    }
}

/// After a live read wrote its local row: take or renew the lease and, when
/// this machine holds it, share the row. A machine that does not hold the
/// lease publishes nothing.
pub(crate) fn publish(marker_dir: &Path, slug: &str, pr: u64, key: &str, row: &Value) {
    let Some((remote, machine)) = primary(marker_dir) else {
        return;
    };
    let shared = || -> Result<bool, String> {
        if !claim_store::poller_lease_take(&remote, &machine, lease_ms())? {
            return Ok(false);
        }
        on_table(
            &remote,
            &format!(
                "INSERT INTO gh_status_rows (slug, pr, key, exit, output, ts) \
                 VALUES (?1, ?2, ?3, ?4, ?5, {CLOCK}) \
                 ON CONFLICT(slug, pr) DO UPDATE SET key = excluded.key, \
                 exit = excluded.exit, output = excluded.output, ts = excluded.ts"
            ),
            &[
                SqlValue::Text(slug.to_string()),
                SqlValue::Integer(pr as i64),
                SqlValue::Text(key.to_string()),
                SqlValue::Integer(row.get("exit").and_then(Value::as_i64).unwrap_or(0)),
                SqlValue::Text(row.get("output").map(Value::to_string).unwrap_or_default()),
            ],
        )?;
        Ok(true)
    };
    match shared() {
        Ok(true) => {
            PUBLISHED.fetch_add(1, Ordering::SeqCst);
        }
        Ok(false) => {}
        Err(_) => mark_down(marker_dir),
    }
}

/// One SELECT before any network read: the shared row for (slug, pr) when a
/// peer holds a live lease and the row is under two TTLs old on the store
/// clock. The row's `ts` is rewritten to its true age on this host's clock,
/// so a cache hit never reports a stale row as fresh.
pub(crate) fn follow(marker_dir: &Path, slug: &str, pr: u64) -> Option<(String, Value)> {
    let (remote, machine) = primary(marker_dir)?;
    let reply = match on_table(
        &remote,
        &format!(
            "SELECT (SELECT host FROM claims WHERE key = ?1 AND expires_at > {CLOCK} \
                       AND machine_id <> ?2), \
                    r.key, r.exit, r.output, {CLOCK} - r.ts \
             FROM (SELECT 1) LEFT JOIN gh_status_rows r ON r.slug = ?3 AND r.pr = ?4"
        ),
        &[
            SqlValue::Text(POLLER_KEY.to_string()),
            SqlValue::Text(machine),
            SqlValue::Text(slug.to_string()),
            SqlValue::Integer(pr as i64),
        ],
    ) {
        Ok(reply) => reply,
        Err(error) => {
            // No claims table yet: nobody has taken the lease, so nobody is
            // a peer. Any other failure stands this host down for a minute.
            if !error.contains("no such table") {
                mark_down(marker_dir);
            }
            return None;
        }
    };
    let row = reply.rows.first()?;
    let host = row.first().and_then(SqlValue::text)?;
    let (key, exit, output, age_ms) = (
        row.get(1).and_then(SqlValue::text)?,
        row.get(2).and_then(SqlValue::integer)?,
        row.get(3).and_then(SqlValue::text)?,
        row.get(4).and_then(SqlValue::integer)?,
    );
    if age_ms >= lease_ms() {
        return None;
    }
    let output: Value = serde_json::from_str(output).ok()?;
    FOLLOWED.fetch_add(1, Ordering::SeqCst);
    *HOLDER.lock().unwrap_or_else(|e| e.into_inner()) = Some(host.to_string());
    let ts = crate::gh_cache::now_secs() - age_ms.max(0) as f64 / 1000.0;
    Some((
        key.to_string(),
        json!({"ts": ts, "exit": exit, "output": output}),
    ))
}

/// The spend-note suffix: `, {n} served from poller {host}` and
/// `, published {m}` once this process has done either, empty before, so
/// callers that parse the note never see a new shape on the plain path.
pub(crate) fn note_suffix() -> String {
    let served = FOLLOWED.load(Ordering::SeqCst);
    let published = PUBLISHED.load(Ordering::SeqCst);
    let mut note = String::new();
    if served > 0 {
        let host = HOLDER
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
            .unwrap_or_default();
        note.push_str(&format!(", {served} served from poller {host}"));
    }
    if published > 0 {
        note.push_str(&format!(", published {published}"));
    }
    note
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store_remote::test_primary;

    fn expire(primary: &test_primary::Primary) {
        primary
            .db
            .lock()
            .unwrap()
            .execute(
                &format!("UPDATE claims SET expires_at = {CLOCK} - 1 WHERE key = ?1"),
                [POLLER_KEY],
            )
            .unwrap();
    }

    fn lease_holder(primary: &test_primary::Primary) -> String {
        primary
            .db
            .lock()
            .unwrap()
            .query_row(
                "SELECT machine_id FROM claims WHERE key = ?1",
                [POLLER_KEY],
                |r| r.get(0),
            )
            .unwrap()
    }

    /// The lease is one row: a peer is refused until the row runs out on the
    /// store clock.
    #[test]
    fn the_poller_lease_has_one_holder_until_it_runs_out_on_the_store_clock() {
        let primary = test_primary::start();
        let remote = &primary.remote;
        assert!(claim_store::poller_lease_take(remote, "mac-a", 120_000).unwrap());
        assert!(!claim_store::poller_lease_take(remote, "mac-b", 120_000).unwrap());
        assert!(claim_store::poller_lease_take(remote, "mac-a", 120_000).unwrap());
        expire(&primary);
        assert!(claim_store::poller_lease_take(remote, "mac-b", 120_000).unwrap());
        assert_eq!(lease_holder(&primary), "mac-b");
    }

    /// A failing primary stands the host down: follow returns None, writes the
    /// marker, and for the next minute opens no socket, even to a live primary.
    #[test]
    fn a_dead_primary_writes_the_marker_and_skips_the_primary_for_a_minute() {
        let dir = tempfile::tempdir().unwrap();
        route_to(Some((test_primary::dead(), "mac-a".into())));
        assert!(follow(dir.path(), "o/r", 7).is_none());
        assert!(dir.path().join(DOWN_MARKER).exists());

        let live = test_primary::start();
        let peer_dir = tempfile::tempdir().unwrap();
        route_to(Some((live.remote.clone(), "mac-b".into())));
        publish(
            peer_dir.path(),
            "o/r",
            7,
            "k1",
            &json!({"exit": 0, "output": {"head": "h1"}}),
        );
        route_to(Some((live.remote.clone(), "mac-a".into())));
        assert!(follow(dir.path(), "o/r", 7).is_none(), "the marker holds");
        std::fs::remove_file(dir.path().join(DOWN_MARKER)).unwrap();
        assert!(follow(dir.path(), "o/r", 7).is_some());
        route_to(None);
    }

    /// The lease holder shares its row; a peer reads it, a second writer is
    /// refused while the lease stands, and the holder never follows itself.
    #[test]
    fn the_lease_holder_publishes_and_a_peer_follows() {
        let primary = test_primary::start();
        let (a_dir, b_dir) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        route_to(Some((primary.remote.clone(), "mac-a".into())));
        publish(
            a_dir.path(),
            "o/r",
            7,
            "key-a",
            &json!({"exit": 0, "output": {"head": "h1"}}),
        );
        assert!(
            follow(a_dir.path(), "o/r", 7).is_none(),
            "the holder reads GitHub itself"
        );

        route_to(Some((primary.remote.clone(), "mac-b".into())));
        publish(
            b_dir.path(),
            "o/r",
            7,
            "key-b",
            &json!({"exit": 1, "output": {"head": "h2"}}),
        );
        let (key, row) = follow(b_dir.path(), "o/r", 7).expect("a peer's row serves");
        assert_eq!(key, "key-a");
        assert_eq!(row["exit"], 0);
        assert_eq!(row["output"]["head"], "h1");
        assert!(
            note_suffix().contains("served from poller"),
            "{}",
            note_suffix()
        );
        route_to(None);
    }
}
