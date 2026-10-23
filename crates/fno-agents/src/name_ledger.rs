//! The fleet-wide name ledger: every name any machine drew or bound, in one
//! append-only `name_history` table on the shared primary
//! (`store.remote_url`).
//!
//! Why: `name_history.json` is per machine. The MacBook and the iMac each
//! held 14 names and shared one, so the iMac accepted `--name Tamsin` for a
//! new lead the MacBook had already named Tamsin. The `name:` claims did not
//! close it: only the successor draw takes one, and an expired or released
//! claim leaves the `claims` table for `claim_history`, which no naming read
//! looks at. This table is never swept, so a name stays used once it lands.
//!
//! Each machine still writes its local file first. Every append also lands
//! here, and every read pushes the local rows the primary lacks, so a
//! machine's older names migrate on its first naming read or daemon sweep.
//! A configured primary that cannot be read refuses the naming decision: a
//! name judged fresh on half the fleet's history is the bug this closes.

use crate::store_remote::{Remote, SqlValue};
use crate::team_names::NameHistoryEntry;
use std::collections::BTreeSet;
use std::path::Path;

const DDL: &str = "CREATE TABLE IF NOT EXISTS name_history (
  name TEXT NOT NULL,
  scope TEXT NOT NULL,
  session TEXT NOT NULL DEFAULT '',
  kind TEXT NOT NULL,
  ts TEXT NOT NULL,
  host TEXT NOT NULL,
  UNIQUE (name, scope, session, kind, ts, host)
);";

#[cfg(test)]
thread_local! {
    static TEST_PRIMARY: std::cell::RefCell<Option<Remote>> =
        const { std::cell::RefCell::new(None) };
}

/// Test seam: route every store's ledger on this thread to `remote`, so two
/// tempdir stores play two machines sharing one primary.
#[cfg(test)]
pub(crate) fn route_to_primary(route: Option<Remote>) {
    TEST_PRIMARY.with(|p| *p.borrow_mut() = route);
}

/// The primary that holds the ledger for `store_path`. Only the shared
/// `$HOME/.fno/agents` store dials out, so a test or scratch home never
/// writes a name into the fleet's ledger.
fn primary(store_path: &Path) -> Result<Option<Remote>, String> {
    #[cfg(test)]
    {
        let _ = store_path;
        Ok(TEST_PRIMARY.with(|p| p.borrow().clone()))
    }
    #[cfg(not(test))]
    {
        let Some(root) = store_path.parent() else {
            return Ok(None);
        };
        if crate::paths::AgentsHome::at(root).is_sandbox() {
            return Ok(None);
        }
        crate::store_remote::configured()
    }
}

/// One statement on the primary. The table is made on first use, the way
/// the claim tables are.
fn run(
    remote: &Remote,
    sql: &str,
    args: &[SqlValue],
) -> Result<crate::store_remote::Reply, String> {
    match remote.execute(sql, args) {
        Err(error) if error.contains("no such table") => {
            remote.script(DDL)?;
            remote.execute(sql, args)
        }
        reply => reply,
    }
}

fn host() -> String {
    crate::claims::hostname()
}

fn text(value: &str) -> SqlValue {
    SqlValue::Text(value.to_string())
}

fn row_args(entry: &NameHistoryEntry, host: &str) -> [SqlValue; 6] {
    [
        text(&entry.name.to_ascii_lowercase()),
        text(&entry.scope),
        text(entry.session.as_deref().unwrap_or("")),
        text(&entry.kind),
        text(&entry.ts),
        text(host),
    ]
}

/// Insert `entries` as this machine's rows. A row already there is skipped,
/// so a retry or a second backfill writes nothing twice.
fn insert(remote: &Remote, entries: &[&NameHistoryEntry]) -> Result<(), String> {
    if entries.is_empty() {
        return Ok(());
    }
    let host = host();
    let mut args = Vec::with_capacity(entries.len() * 6);
    let mut rows = Vec::with_capacity(entries.len());
    for entry in entries {
        let at = args.len();
        rows.push(format!(
            "(?{},?{},?{},?{},?{},?{})",
            at + 1,
            at + 2,
            at + 3,
            at + 4,
            at + 5,
            at + 6
        ));
        args.extend(row_args(entry, &host));
    }
    run(
        remote,
        &format!(
            "INSERT OR IGNORE INTO name_history (name, scope, session, kind, ts, host) VALUES {}",
            rows.join(",")
        ),
        &args,
    )
    .map(drop)
}

/// Publish one appended entry. Best-effort: the local file already holds it,
/// and the next [`read_merged`] or [`sync`] pushes whatever this write lost.
pub(crate) fn publish(store_path: &Path, entry: &NameHistoryEntry) {
    let Ok(Some(remote)) = primary(store_path) else {
        return;
    };
    if let Err(e) = insert(&remote, &[entry]) {
        eprintln!(
            "name ledger: {} not published to {} ({e}); the next naming read retries",
            entry.name,
            remote.url()
        );
    }
}

/// Every row on the primary, each with the host that wrote it. `local` is
/// this machine's file; its rows the primary lacks are pushed first, so the
/// answer always covers this machine too. `Ok(empty)` when no primary is
/// configured.
pub(crate) fn read_merged(
    store_path: &Path,
    local: &[NameHistoryEntry],
) -> Result<Vec<NameHistoryEntry>, String> {
    let Some(remote) = primary(store_path)? else {
        return Ok(Vec::new());
    };
    let refuse = |e: String| {
        format!(
            "the fleet name ledger at {} cannot be read ({e}); a name is judged against every machine's history, so this naming decision refuses until store.remote_url answers",
            remote.url()
        )
    };
    let shared = select_all(&remote).map_err(refuse)?;
    // A failed push still answers the read: the shared rows plus the
    // caller's own local rows cover every name this decision needs.
    if let Err(e) = push_missing(&remote, &shared, local) {
        eprintln!("name ledger: backfill to {} failed ({e})", remote.url());
    }
    Ok(shared)
}

/// Push every local row the primary lacks. The daemon sweep runs this, so a
/// machine that names nobody for a while still lands its history.
pub fn sync(store_path: &Path, local: &[NameHistoryEntry]) -> Result<usize, String> {
    let Some(remote) = primary(store_path)? else {
        return Ok(0);
    };
    push_missing(&remote, &select_all(&remote)?, local)
}

fn push_missing(
    remote: &Remote,
    shared: &[NameHistoryEntry],
    local: &[NameHistoryEntry],
) -> Result<usize, String> {
    let host = host();
    let seen: BTreeSet<[String; 6]> = shared
        .iter()
        .map(|e| key(e, e.host.as_deref().unwrap_or("")))
        .collect();
    let missing: Vec<&NameHistoryEntry> = local
        .iter()
        .filter(|e| !seen.contains(&key(e, &host)))
        .collect();
    insert(remote, &missing)?;
    Ok(missing.len())
}

fn key(entry: &NameHistoryEntry, host: &str) -> [String; 6] {
    [
        entry.name.to_ascii_lowercase(),
        entry.scope.clone(),
        entry.session.clone().unwrap_or_default(),
        entry.kind.clone(),
        entry.ts.clone(),
        host.to_string(),
    ]
}

fn select_all(remote: &Remote) -> Result<Vec<NameHistoryEntry>, String> {
    let reply = run(
        remote,
        "SELECT name, scope, session, kind, ts, host FROM name_history ORDER BY ts",
        &[],
    )?;
    let cell = |row: &[SqlValue], i: usize| {
        row.get(i)
            .and_then(SqlValue::text)
            .unwrap_or_default()
            .to_string()
    };
    Ok(reply
        .rows
        .iter()
        .map(|row| {
            let session = cell(row, 2);
            NameHistoryEntry {
                name: cell(row, 0),
                scope: cell(row, 1),
                session: (!session.is_empty()).then_some(session),
                kind: cell(row, 3),
                ts: cell(row, 4),
                host: Some(cell(row, 5)),
            }
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::team_names;

    /// Two tempdir stores play the MacBook and the iMac on one primary. A
    /// name one machine bound refuses on the other, naming who and when; a
    /// row written before the ledger existed lands on the first sync; and an
    /// unreachable primary refuses the read instead of answering "no names".
    #[test]
    fn a_name_used_on_one_machine_is_refused_on_the_other() {
        let macbook = tempfile::tempdir().unwrap();
        let imac = tempfile::tempdir().unwrap();
        let mac_store = macbook.path().join("team_names.json");
        let imac_store = imac.path().join("team_names.json");
        let bound = |name: &str, scope: &str, session: &str| {
            NameHistoryEntry::here(name, scope, Some(session.into()), "bound")
        };

        route_to_primary(None);
        team_names::history_append(&imac_store, bound("rowan", "x-aaaa", "sess-r")).unwrap();
        let primary = crate::store_remote::test_primary::start();
        route_to_primary(Some(primary.remote.clone()));
        assert_eq!(team_names::history_sync(&imac_store).unwrap(), 1);
        assert_eq!(team_names::history_sync(&imac_store).unwrap(), 0);
        assert!(team_names::history_names(&mac_store)
            .unwrap()
            .contains("rowan"));

        team_names::history_append(&mac_store, bound("Tamsin", "x-8d35", "sess-mac")).unwrap();
        let err =
            team_names::carry_holder_name(&imac_store, "sess-imac", 2, "x-d811,x32c2", "Tamsin")
                .unwrap_err();
        assert!(
            err.contains("Tamsin is retired") && err.contains("x-8d35") && err.contains("sess-mac"),
            "{err}"
        );
        assert_eq!(
            team_names::carry_holder_name(&imac_store, "sess-imac", 2, "x-d811,x32c2", "Odile")
                .unwrap(),
            Some("Odile".to_string())
        );
        assert!(team_names::history_names(&mac_store)
            .unwrap()
            .contains("odile"));

        route_to_primary(Some(crate::store_remote::test_primary::dead()));
        let err = team_names::history_names(&mac_store).unwrap_err();
        assert!(err.contains("cannot be read"), "{err}");
        route_to_primary(None);
    }
}
