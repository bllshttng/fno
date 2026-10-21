//! Plan bodies on the shared primary.
//!
//! The shared backlog carries node rows, and a row's `plan_path` is a path
//! on the machine that wrote the plan. Without this carrier the other
//! machine reads a pointer to a file it does not have: spawn refuses on an
//! unreadable rung, or target init seats a worker with no plan. So every
//! open node's plan body also rides the primary, keyed on the node id, and
//! each backlog-share tick pushes changed bodies and writes absent ones.
//!
//! Like `attest.rs`, the tables are primary-only and never join
//! `SHARED_TABLES`: a new column there needs a reseed, and a body in a node
//! row would ship with every write of that node. `fno_plan_seen` is the
//! per-host ledger (the body hash each host last synced), so a host can tell
//! its own edit from a peer's. When both moved since that sync, the carrier
//! writes nothing and names the node as a conflict; it never merges.
//!
//! A body lands only at its recorded absolute path, and only under this
//! host's home. A host with a different home spelling counts the node as
//! `unplaced` and keeps no copy.

use crate::store_remote::{Remote, SqlValue};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

const BODIES: &str = "fno_plan_bodies";
const SEEN: &str = "fno_plan_seen";

fn ensure_tables(remote: &Remote) -> Result<(), String> {
    remote.script(&format!(
        "CREATE TABLE IF NOT EXISTS {BODIES} (\
             node_id TEXT PRIMARY KEY, \
             plan_path TEXT NOT NULL, \
             sha TEXT NOT NULL, \
             body TEXT NOT NULL, \
             from_host TEXT NOT NULL, \
             pushed_at INTEGER NOT NULL); \
         CREATE TABLE IF NOT EXISTS {SEEN} (\
             node_id TEXT NOT NULL, \
             host TEXT NOT NULL, \
             sha TEXT NOT NULL, \
             PRIMARY KEY (node_id, host));"
    ))
}

/// What one pass did, for the sync receipt.
#[derive(Debug, Default, PartialEq)]
pub struct Outcome {
    pub pushed: Vec<String>,
    pub pulled: Vec<String>,
    pub conflicts: Vec<String>,
    pub unplaced: Vec<String>,
}

impl Outcome {
    pub fn receipt(&self) -> Value {
        json!({
            "pushed": self.pushed.len(),
            "pulled": self.pulled.len(),
            "conflicts": self.conflicts,
            "unplaced": self.unplaced,
        })
    }
}

/// A primary row without its body: the tick compares hashes, and reads a
/// body only for a node it writes.
struct Row {
    plan_path: String,
    sha: String,
    from_host: String,
}

/// The ledger key: the stable machine id, else the normalized hostname.
fn host_key() -> String {
    let machine = crate::claims::machine_id();
    if machine.is_empty() {
        host_name()
    } else {
        machine
    }
}

fn host_name() -> String {
    crate::mail_bus::normalize_host(&crate::claims::hostname())
}

fn home() -> PathBuf {
    PathBuf::from(std::env::var("HOME").unwrap_or_default())
}

fn sha(body: &str) -> String {
    blake3::hash(body.as_bytes()).to_hex().to_string()
}

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// The file a plan_path names: the `#anchor` dropped, `~` expanded. A plan
/// DIRECTORY is not carried; it reads as None here.
fn plan_file(plan_path: &str, home: &Path) -> Option<PathBuf> {
    let bare = plan_path.split('#').next().unwrap_or("").trim();
    let path = match bare.strip_prefix("~/") {
        Some(rest) => home.join(rest),
        None => PathBuf::from(bare),
    };
    (path.is_absolute() && !bare.is_empty()).then_some(path)
}

/// Writable here: a `.md` file under this home, with no `..` hop and no
/// dot-dir below home. A primary row names the path, so without this a row
/// could write `~/.ssh/authorized_keys` or a shell profile.
fn placeable(path: &Path, home: &Path) -> bool {
    let Ok(rel) = path.strip_prefix(home) else {
        return false;
    };
    !home.as_os_str().is_empty()
        && path.extension().and_then(|e| e.to_str()) == Some("md")
        && rel.components().all(|c| match c {
            std::path::Component::Normal(part) => !part.to_string_lossy().starts_with('.'),
            _ => false,
        })
}

fn write_atomic(path: &Path, body: &str) -> Result<(), String> {
    let parent = path.parent().ok_or("plan path has no parent")?;
    std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    let tmp = parent.join(format!(
        ".{}.carry-{}",
        path.file_name().and_then(|n| n.to_str()).unwrap_or("plan"),
        std::process::id()
    ));
    std::fs::write(&tmp, body).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, path).map_err(|e| e.to_string())
}

/// Open nodes in this host's replica: id -> plan_path (may be empty).
fn open_nodes(graph: &Path) -> Result<BTreeMap<String, String>, String> {
    let conn = crate::backlog::open(graph)?;
    let mut stmt = conn
        .prepare(
            "SELECT id, COALESCE(plan_path, '') FROM nodes WHERE archived_at IS NULL \
             AND status NOT IN ('done', 'superseded', 'deferred')",
        )
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
        .map_err(|e| e.to_string())?;
    rows.collect::<Result<_, _>>().map_err(|e| e.to_string())
}

fn primary_rows(
    remote: &Remote,
    host: &str,
) -> Result<BTreeMap<String, (Row, Option<String>)>, String> {
    let reply = remote.execute(
        &format!(
            "SELECT b.node_id, b.plan_path, b.sha, b.from_host, s.sha FROM {BODIES} b \
             LEFT JOIN {SEEN} s ON s.node_id = b.node_id AND s.host = ?"
        ),
        &[SqlValue::Text(host.to_string())],
    )?;
    let text = |v: Option<&SqlValue>| v.and_then(SqlValue::text).map(str::to_string);
    Ok(reply
        .rows
        .iter()
        .filter_map(|r| {
            let row = Row {
                plan_path: text(r.get(1))?,
                sha: text(r.get(2))?,
                from_host: text(r.get(3)).unwrap_or_default(),
            };
            Some((text(r.first())?, (row, text(r.get(4)))))
        })
        .collect())
}

/// One node's row and body, or None when no host pushed one.
fn body_row(remote: &Remote, node: &str) -> Result<Option<(Row, String)>, String> {
    let reply = match remote.execute(
        &format!("SELECT plan_path, sha, from_host, body FROM {BODIES} WHERE node_id = ?"),
        &[SqlValue::Text(node.to_string())],
    ) {
        Ok(reply) => reply,
        Err(e) if e.contains("no such table") => return Ok(None),
        Err(e) => return Err(e),
    };
    let text = |r: &[SqlValue], i: usize| r.get(i).and_then(SqlValue::text).map(str::to_string);
    Ok(reply.rows.first().and_then(|r| {
        let row = Row {
            plan_path: text(r, 0)?,
            sha: text(r, 1)?,
            from_host: text(r, 2).unwrap_or_default(),
        };
        Some((row, text(r, 3)?))
    }))
}

fn mark_seen(remote: &Remote, node: &str, host: &str, sha: &str) -> Result<(), String> {
    remote
        .execute(
            &format!("INSERT OR REPLACE INTO {SEEN} (node_id, host, sha) VALUES (?, ?, ?)"),
            &[
                SqlValue::Text(node.to_string()),
                SqlValue::Text(host.to_string()),
                SqlValue::Text(sha.to_string()),
            ],
        )
        .map(|_| ())
}

fn push(
    remote: &Remote,
    node: &str,
    plan_path: &str,
    body: &str,
    host: &str,
) -> Result<(), String> {
    let digest = sha(body);
    remote.execute(
        &format!(
            "INSERT OR REPLACE INTO {BODIES} (node_id, plan_path, sha, body, from_host, pushed_at) \
             VALUES (?, ?, ?, ?, ?, ?)"
        ),
        &[
            SqlValue::Text(node.to_string()),
            SqlValue::Text(plan_path.to_string()),
            SqlValue::Text(digest.clone()),
            SqlValue::Text(body.to_string()),
            SqlValue::Text(host_name()),
            SqlValue::Integer(now()),
        ],
    )?;
    mark_seen(remote, node, host, &digest)
}

/// One pass over this host's open nodes: push the bodies this host changed,
/// write the bodies it lacks or has not touched since its last sync.
pub(crate) fn sync_with(
    nodes: &BTreeMap<String, String>,
    remote: &Remote,
    host: &str,
    home: &Path,
) -> Result<Outcome, String> {
    ensure_tables(remote)?;
    let mut primary = primary_rows(remote, host)?;
    let mut out = Outcome::default();
    for (node, plan_path) in nodes {
        let remote_row = primary.remove(node);
        let local = plan_file(plan_path, home)
            .filter(|p| p.is_file())
            .and_then(|p| std::fs::read_to_string(p).ok());
        match (local, remote_row) {
            (Some(body), None) => {
                push(remote, node, plan_path, &body, host)?;
                out.pushed.push(node.clone());
            }
            (Some(body), Some((row, seen))) => {
                let mine = sha(&body);
                if row.plan_path != *plan_path {
                    // The node row is the synced binding: a rebind moves the carried body.
                    push(remote, node, plan_path, &body, host)?;
                    out.pushed.push(node.clone());
                } else if mine == row.sha {
                    if seen.as_deref() != Some(mine.as_str()) {
                        mark_seen(remote, node, host, &mine)?;
                    }
                } else if seen.as_deref() == Some(row.sha.as_str()) {
                    push(remote, node, plan_path, &body, host)?;
                    out.pushed.push(node.clone());
                } else if seen.as_deref() == Some(mine.as_str()) {
                    pull_row(remote, node, &row, host, home, &mut out)?;
                } else {
                    out.conflicts.push(node.clone());
                }
            }
            // The file is absent here (or the binding was lost): the
            // primary's row decides where it lands.
            (None, Some((row, _))) => pull_row(remote, node, &row, host, home, &mut out)?,
            (None, None) => {}
        }
    }
    Ok(out)
}

fn pull_row(
    remote: &Remote,
    node: &str,
    row: &Row,
    host: &str,
    home: &Path,
    out: &mut Outcome,
) -> Result<(), String> {
    let Some(path) = plan_file(&row.plan_path, home).filter(|p| placeable(p, home)) else {
        out.unplaced.push(node.to_string());
        return Ok(());
    };
    // Re-read with the body: the row may have moved since the hash read.
    if let Some((row, body)) = body_row(remote, node)? {
        write_atomic(&path, &body)?;
        mark_seen(remote, node, host, &row.sha)?;
        out.pulled.push(node.to_string());
    }
    Ok(())
}

/// One node's body from the primary, for target init on a miss: the path it
/// wrote, or an error naming why none landed (no row, a foreign home, an
/// unreachable primary).
pub(crate) fn fetch_with(
    node: &str,
    remote: &Remote,
    host: &str,
    home: &Path,
) -> Result<String, String> {
    let Some((row, body)) = body_row(remote, node)? else {
        return Err("no host pushed a plan for it".to_string());
    };
    let Some(path) = plan_file(&row.plan_path, home).filter(|p| placeable(p, home)) else {
        return Err(format!(
            "{} pushed it at {}, which is outside this host's home",
            row.from_host, row.plan_path
        ));
    };
    if !path.is_file() {
        write_atomic(&path, &body)?;
    }
    mark_seen(remote, node, host, &row.sha)?;
    Ok(row.plan_path)
}

/// The tick's and the sync command's pass, on the configured shared primary.
pub fn sync(graph: &Path) -> Result<Outcome, String> {
    let Some(remote) = crate::store_remote::share_backlog()? else {
        return Ok(Outcome::default());
    };
    sync_with(&open_nodes(graph)?, &remote, &host_key(), &home())
}

/// Target init's miss: `Ok(None)` when sharing is off.
pub fn fetch_one(node: &str) -> Result<Option<String>, String> {
    let Some(remote) = crate::store_remote::share_backlog()? else {
        return Ok(None);
    };
    fetch_with(node, &remote, &host_key(), &home()).map(Some)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store_remote::test_primary;

    /// One host's open nodes as `open_nodes` reads them: id -> plan_path.
    fn replica(nodes: &[(&str, &str)]) -> BTreeMap<String, String> {
        nodes
            .iter()
            .map(|(id, plan)| (id.to_string(), plan.to_string()))
            .collect()
    }

    #[test]
    fn a_pushed_plan_lands_where_the_peer_lacks_it_even_with_a_lost_binding() {
        let primary = test_primary::start();
        let home = tempfile::tempdir().unwrap();
        let plan = home.path().join("plans/p.md");
        let plan_s = plan.to_str().unwrap();
        // A 64 KB body rides one statement.
        let body = format!("---\nstatus: ready\n---\n{}", "x".repeat(64 * 1024));
        write_atomic(&plan, &body).unwrap();
        let a = replica(&[("x-1", plan_s)]);
        let out = sync_with(&a, &primary.remote, "a", home.path()).unwrap();
        assert_eq!(out.pushed, vec!["x-1"]);
        // B has no file, and its replica lost the binding.
        std::fs::remove_file(&plan).unwrap();
        let b = replica(&[("x-1", "")]);
        let out = sync_with(&b, &primary.remote, "b", home.path()).unwrap();
        assert_eq!(out.pulled, vec!["x-1"]);
        assert_eq!(sha(&std::fs::read_to_string(&plan).unwrap()), sha(&body));
        // A second pass is quiet: nothing moved.
        let out = sync_with(&a, &primary.remote, "a", home.path()).unwrap();
        assert_eq!(out, Outcome::default());
    }

    #[test]
    fn both_hosts_edited_since_the_last_sync_is_a_conflict_and_nothing_is_overwritten() {
        let primary = test_primary::start();
        let home = tempfile::tempdir().unwrap();
        let plan = home.path().join("plans/p.md");
        let plan_s = plan.to_str().unwrap();
        write_atomic(&plan, "v0").unwrap();
        let b = replica(&[("x-1", plan_s)]);
        sync_with(&b, &primary.remote, "b", home.path()).unwrap();
        // The peer pushes v1; B edits its own copy to v2 before it syncs.
        push(&primary.remote, "x-1", plan_s, "v1", "a").unwrap();
        write_atomic(&plan, "v2").unwrap();
        let out = sync_with(&b, &primary.remote, "b", home.path()).unwrap();
        assert_eq!(out.conflicts, vec!["x-1"]);
        assert_eq!(std::fs::read_to_string(&plan).unwrap(), "v2");
        // Untouched since its sync, B takes the peer's edit.
        write_atomic(&plan, "v1").unwrap();
        sync_with(&b, &primary.remote, "b", home.path()).unwrap();
        push(&primary.remote, "x-1", plan_s, "v3", "a").unwrap();
        let out = sync_with(&b, &primary.remote, "b", home.path()).unwrap();
        assert_eq!(out.pulled, vec!["x-1"]);
        assert_eq!(std::fs::read_to_string(&plan).unwrap(), "v3");
        // A rebind to a new path moves the carried body, never a standing conflict.
        let moved = home.path().join("plans/q.md");
        write_atomic(&moved, "v4").unwrap();
        let c = replica(&[("x-1", moved.to_str().unwrap())]);
        let out = sync_with(&c, &primary.remote, "b", home.path()).unwrap();
        assert_eq!(out.pushed, vec!["x-1"]);
        assert_eq!(
            sync_with(&c, &primary.remote, "b", home.path()).unwrap(),
            Outcome::default()
        );
    }

    #[test]
    fn a_foreign_home_is_unplaced_and_fetch_names_the_pushing_host() {
        let primary = test_primary::start();
        let home = tempfile::tempdir().unwrap();
        assert_eq!(
            fetch_with("x-1", &primary.remote, "b", home.path()).unwrap_err(),
            "no host pushed a plan for it"
        );
        ensure_tables(&primary.remote).unwrap();
        push(&primary.remote, "x-1", "/elsewhere/plans/p.md", "v0", "a").unwrap();
        let b = replica(&[("x-1", "/elsewhere/plans/p.md")]);
        let out = sync_with(&b, &primary.remote, "b", home.path()).unwrap();
        assert_eq!(out.unplaced, vec!["x-1"]);
        let err = fetch_with("x-1", &primary.remote, "b", home.path()).unwrap_err();
        assert!(err.contains("outside this host's home"), "{err}");
        // A row naming a dot-dir or a non-plan file under home never lands.
        for evil in [".ssh/authorized_keys.md", "plans/p.sh"] {
            let path = home.path().join(evil);
            push(&primary.remote, "x-1", path.to_str().unwrap(), "pwn", "a").unwrap();
            assert!(fetch_with("x-1", &primary.remote, "b", home.path()).is_err());
            assert!(!path.exists());
        }
        // An unreachable primary is an error the tick logs, never a panic.
        assert!(sync_with(&b, &test_primary::dead(), "b", home.path()).is_err());
    }
}
