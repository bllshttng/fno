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
//! The recorded `plan_path` is portable. A file under the plans root is
//! stored relative to that root, and every host resolves the row against its
//! own `plansDirectory -> plans_dir` chain, so the same row lands in each
//! machine's own home, whatever the username. Absolute rows from older
//! writers stay readable.
//!
//! When the store cannot place a body here, an opt-in ssh rescue can still
//! fetch the file from a peer: `store.ssh_peers` maps a host key (the
//! pushing host, one from `fno_plan_seen`, or the store host itself) to an
//! ssh destination. Off by default; the shared store stays the only
//! required transport.

use crate::store_remote::{Remote, SqlValue};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

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
    pub fetched: Vec<String>,
}

impl Outcome {
    pub fn receipt(&self) -> Value {
        json!({
            "pushed": self.pushed.len(),
            "pulled": self.pulled.len(),
            "conflicts": self.conflicts,
            "unplaced": self.unplaced,
            "fetched": self.fetched.len(),
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

/// The bare path a recorded pointer names: the `#anchor` dropped, `~`
/// expanded. A plan DIRECTORY is not carried; it reads as None here.
/// A row relative to the plans root resolves against THIS host's own root.
fn plan_file(plan_path: &str, home: &Path, plans_root: Option<&Path>) -> Option<PathBuf> {
    let bare = plan_path.split('#').next().unwrap_or("").trim();
    if bare.is_empty() {
        return None;
    }
    match bare.strip_prefix("~/") {
        Some(rest) => Some(home.join(rest)),
        None => {
            let path = Path::new(bare);
            if path.is_absolute() {
                Some(path.to_path_buf())
            } else {
                plans_root.map(|root| root.join(path))
            }
        }
    }
}

/// The form the carrier stores for `plan_path`: under the plans root, the
/// path relative to that root, which every host resolves against its own
/// chain. Anything else stays as written, so absolute rows keep reading.
fn stored_form(plan_path: &str, plans_root: Option<&Path>, home: &Path) -> String {
    let (bare, anchor) = match plan_path.split_once('#') {
        Some((b, a)) => (b.trim(), Some(a)),
        None => (plan_path.trim(), None),
    };
    let resolved = match bare.strip_prefix("~/") {
        Some(rest) => home.join(rest),
        None => PathBuf::from(bare),
    };
    if let (Some(root), true) = (plans_root, resolved.is_absolute()) {
        if let Ok(rel) = resolved.strip_prefix(root) {
            let rel = rel.to_string_lossy().replace('\\', "/");
            if !rel.is_empty() {
                return match anchor {
                    Some(a) => format!("{rel}#{a}"),
                    None => rel,
                };
            }
        }
    }
    if bare.starts_with("~/") && resolved.is_absolute() {
        // A tilde binding normalizes to its absolute form, so two hosts
        // binding one file compare equal instead of rebinding every tick.
        return match anchor {
            Some(a) => format!("{}#{}", resolved.display(), a),
            None => resolved.display().to_string(),
        };
    }
    plan_path.trim().to_string()
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

/// Paths to ask the peer for: the recorded path as written, then the same
/// tail under the peer's own home when the writer's differs (`/Users/<name>`
/// or `/home/<name>` prefix).
fn remote_candidates(recorded: &str, home: &Path) -> Vec<String> {
    let mut out = vec![recorded.to_string()];
    let mut parts = Path::new(recorded).components();
    if parts.next() != Some(std::path::Component::RootDir) {
        return out;
    }
    let Some(first) = parts.next() else {
        return out;
    };
    if parts.next().is_none() {
        return out;
    }
    let first = first.as_os_str().to_string_lossy();
    if first != "Users" && first != "home" {
        return out;
    }
    let tail: PathBuf = parts.collect();
    if tail.as_os_str().is_empty() {
        return out;
    }
    let swapped = home.join(tail).to_string_lossy().to_string();
    if swapped != recorded {
        out.push(swapped);
    }
    out
}

/// Where a rescued row lands here: the writer's absolute path with this
/// host's home swapped for the writer's. A relative row carries no home
/// prefix to swap; the store is its only transport.
fn rescued_local(recorded: &str, home: &Path) -> Option<PathBuf> {
    remote_candidates(recorded, home)
        .into_iter()
        .nth(1)
        .map(PathBuf::from)
}

/// The pointer init binds: the local file, with the row's `#group` anchor kept.
fn with_anchor(path: &Path, recorded: &str) -> String {
    match recorded.split_once('#') {
        Some((_, anchor)) => format!("{}#{}", path.display(), anchor),
        None => path.display().to_string(),
    }
}

/// Host keys in the seen ledger for one node: machines that carried the body.
fn seen_hosts(remote: &Remote, node: &str) -> Result<Vec<String>, String> {
    let reply = match remote.execute(
        &format!("SELECT host FROM {SEEN} WHERE node_id = ?"),
        &[SqlValue::Text(node.to_string())],
    ) {
        Ok(reply) => reply,
        Err(e) if e.contains("no such table") => return Ok(Vec::new()),
        Err(e) => return Err(e),
    };
    Ok(reply
        .rows
        .iter()
        .filter_map(|r| r.first().and_then(SqlValue::text).map(str::to_string))
        .collect())
}

/// `store.ssh_peers`: host key -> ssh destination. Empty unless configured.
fn ssh_peers() -> BTreeMap<String, String> {
    crate::agents_config::config_lookup_global(&["store", "ssh_peers"])
        .and_then(|value| value.as_table().cloned())
        .map(|table| {
            table
                .into_iter()
                .filter_map(|(key, value)| value.as_str().map(|alias| (key, alias.to_string())))
                .collect()
        })
        .unwrap_or_default()
}

/// The production transport: one `cat` over ssh. BatchMode refuses instead
/// of hanging on a prompt, and the connect bound keeps a dead peer short.
fn ssh_cat(alias: &str, remote_path: &str) -> Result<String, String> {
    let out = Command::new("ssh")
        .args([
            "-o",
            "BatchMode=yes",
            "-o",
            "ConnectTimeout=5",
            alias,
            "cat",
            &shell_quote(remote_path),
        ])
        .output()
        .map_err(|e| format!("ssh {alias}: {e}"))?;
    if out.status.success() {
        return String::from_utf8(out.stdout).map_err(|e| format!("ssh {alias}: {e}"));
    }
    Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
}

/// Single-quote for the remote shell: the recorded path is data, never code.
fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

/// The opt-in ssh rescue: this host cannot place the store's copy, so ask a
/// peer named in `store.ssh_peers` - the pushing host's key first, then the
/// seen ledger's, then the store's own host. Returns the local path the file
/// lands at and the body the peer served. Never a requirement: without the
/// config key nothing is asked.
fn ssh_rescue(
    remote: &Remote,
    node: &str,
    recorded: &str,
    from_host: &str,
    host: &str,
    home: &Path,
    peers: &BTreeMap<String, String>,
    ssh: &dyn Fn(&str, &str) -> Result<String, String>,
) -> Option<(PathBuf, String)> {
    let bare = recorded.split('#').next().unwrap_or("").trim();
    if bare.is_empty() {
        return None;
    }
    let local = rescued_local(bare, home).filter(|p| placeable(p, home))?;
    let mut keys: Vec<String> = Vec::new();
    if !from_host.is_empty() {
        keys.push(from_host.to_string());
    }
    if let Ok(hosts) = seen_hosts(remote, node) {
        for seen in hosts {
            if !keys.contains(&seen) {
                keys.push(seen);
            }
        }
    }
    let store_host = remote.host().to_string();
    if !keys.contains(&store_host) {
        keys.push(store_host);
    }
    for key in keys {
        if key == host {
            continue;
        }
        let Some(alias) = peers.get(&key) else {
            continue;
        };
        for candidate in remote_candidates(bare, home) {
            if let Ok(body) = ssh(alias, &candidate) {
                return Some((local, body));
            }
        }
    }
    None
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

fn push(remote: &Remote, node: &str, stored: &str, body: &str, host: &str) -> Result<(), String> {
    let digest = sha(body);
    remote.execute(
        &format!(
            "INSERT OR REPLACE INTO {BODIES} (node_id, plan_path, sha, body, from_host, pushed_at) \
             VALUES (?, ?, ?, ?, ?, ?)"
        ),
        &[
            SqlValue::Text(node.to_string()),
            SqlValue::Text(stored.to_string()),
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
    plans_root: Option<&Path>,
    peers: &BTreeMap<String, String>,
    ssh: &dyn Fn(&str, &str) -> Result<String, String>,
) -> Result<Outcome, String> {
    ensure_tables(remote)?;
    let mut primary = primary_rows(remote, host)?;
    let mut out = Outcome::default();
    for (node, plan_path) in nodes {
        let remote_row = primary.remove(node);
        let stored = stored_form(plan_path, plans_root, home);
        let local = plan_file(plan_path, home, plans_root)
            .filter(|p| p.is_file())
            .and_then(|p| std::fs::read_to_string(p).ok());
        match (local, remote_row) {
            (Some(body), None) => {
                push(remote, node, &stored, &body, host)?;
                out.pushed.push(node.clone());
            }
            (Some(body), Some((row, seen))) => {
                let mine = sha(&body);
                if row.plan_path != stored {
                    // The node row is the synced binding: a rebind moves the
                    // carried body. The compare also migrates an absolute row
                    // to the relative form on the first tick after an upgrade.
                    push(remote, node, &stored, &body, host)?;
                    out.pushed.push(node.clone());
                } else if mine == row.sha {
                    if seen.as_deref() != Some(mine.as_str()) {
                        mark_seen(remote, node, host, &mine)?;
                    }
                } else if seen.as_deref() == Some(row.sha.as_str()) {
                    push(remote, node, &stored, &body, host)?;
                    out.pushed.push(node.clone());
                } else if seen.as_deref() == Some(mine.as_str()) {
                    pull_row(remote, node, host, home, plans_root, peers, ssh, &mut out)?;
                } else {
                    out.conflicts.push(node.clone());
                }
            }
            // The file is absent here (or the binding was lost): the
            // primary's row decides where it lands.
            (None, Some(_)) => {
                pull_row(remote, node, host, home, plans_root, peers, ssh, &mut out)?
            }
            // No row and no body: only the opt-in ssh rescue can still find
            // the file, asking a peer the seen ledger names.
            (None, None) => {
                if let Some((path, body)) =
                    ssh_rescue(remote, node, plan_path, "", host, home, peers, ssh)
                {
                    write_atomic(&path, &body)?;
                    mark_seen(remote, node, host, &sha(&body))?;
                    out.fetched.push(node.clone());
                }
            }
        }
    }
    Ok(out)
}

fn pull_row(
    remote: &Remote,
    node: &str,
    host: &str,
    home: &Path,
    plans_root: Option<&Path>,
    peers: &BTreeMap<String, String>,
    ssh: &dyn Fn(&str, &str) -> Result<String, String>,
    out: &mut Outcome,
) -> Result<(), String> {
    // Re-read with the body: the row may have moved since the hash read.
    let Some((row, body)) = body_row(remote, node)? else {
        return Ok(());
    };
    let local = plan_file(&row.plan_path, home, plans_root).filter(|p| placeable(p, home));
    if let Some(path) = local {
        // Already here, hash for hash: a file-sync tool or an earlier pass
        // wrote this body, so the carrier does nothing.
        let same = path.is_file()
            && std::fs::read_to_string(&path).map_or(false, |text| sha(&text) == row.sha);
        if !same {
            write_atomic(&path, &body)?;
            out.pulled.push(node.to_string());
        }
        mark_seen(remote, node, host, &row.sha)?;
        return Ok(());
    }
    // This host cannot place the store's copy (a legacy absolute row naming
    // another home): the opt-in ssh rescue asks the machine that pushed.
    if let Some((path, _)) = ssh_rescue(
        remote,
        node,
        &row.plan_path,
        &row.from_host,
        host,
        home,
        peers,
        ssh,
    ) {
        write_atomic(&path, &body)?;
        mark_seen(remote, node, host, &row.sha)?;
        out.fetched.push(node.to_string());
        return Ok(());
    }
    out.unplaced.push(node.to_string());
    Ok(())
}

/// One node's body from the primary, for target init on a miss: the local
/// path it wrote, or an error naming why none landed (no row anywhere, a
/// foreign home with no ssh rescue, an unreachable primary).
pub(crate) fn fetch_with(
    node: &str,
    bound_plan: &str,
    remote: &Remote,
    host: &str,
    home: &Path,
    plans_root: Option<&Path>,
    peers: &BTreeMap<String, String>,
    ssh: &dyn Fn(&str, &str) -> Result<String, String>,
) -> Result<String, String> {
    let carried = body_row(remote, node)?;
    let (recorded, from_host) = match &carried {
        Some((row, _)) => (row.plan_path.clone(), row.from_host.clone()),
        None => (bound_plan.trim().to_string(), String::new()),
    };
    if recorded.trim().is_empty() {
        return Err("no host pushed a plan for it".to_string());
    }
    if let Some((row, body)) = carried {
        let local = plan_file(&row.plan_path, home, plans_root).filter(|p| placeable(p, home));
        if let Some(path) = local {
            if !path.is_file() {
                write_atomic(&path, &body)?;
            }
            mark_seen(remote, node, host, &row.sha)?;
            return Ok(with_anchor(&path, &row.plan_path));
        }
        if let Some((path, _)) = ssh_rescue(
            remote,
            node,
            &row.plan_path,
            &row.from_host,
            host,
            home,
            peers,
            ssh,
        ) {
            write_atomic(&path, &body)?;
            mark_seen(remote, node, host, &row.sha)?;
            return Ok(with_anchor(&path, &row.plan_path));
        }
        return Err(format!(
            "{} pushed it at {}, which is outside this host's home",
            row.from_host, row.plan_path
        ));
    }
    if let Some((path, body)) =
        ssh_rescue(remote, node, &recorded, &from_host, host, home, peers, ssh)
    {
        write_atomic(&path, &body)?;
        mark_seen(remote, node, host, &sha(&body))?;
        return Ok(with_anchor(&path, &recorded));
    }
    Err("no host pushed a plan for it".to_string())
}

/// The plans root this pass records against: the canonical checkout's own
/// chain, so a worktree session records the same form the canonical tick does.
fn plans_root_for(anchor: Option<&Path>) -> Option<PathBuf> {
    let anchor = anchor?;
    let root = crate::paths::canonical_repo_root(anchor).unwrap_or_else(|| anchor.to_path_buf());
    crate::plans_path::plans_content_dir(&root)
}

/// The tick's and the sync command's pass, on the configured shared primary.
pub fn sync(graph: &Path) -> Result<Outcome, String> {
    let Some(remote) = crate::store_remote::share_backlog()? else {
        return Ok(Outcome::default());
    };
    let cwd = std::env::current_dir().ok();
    let plans_root = plans_root_for(cwd.as_deref());
    let peers = ssh_peers();
    sync_with(
        &open_nodes(graph)?,
        &remote,
        &host_key(),
        &home(),
        plans_root.as_deref(),
        &peers,
        &ssh_cat,
    )
}

/// Target init's miss: `Ok(None)` when sharing is off.
pub fn fetch_one(node: &str, bound_plan: &str) -> Result<Option<String>, String> {
    let Some(remote) = crate::store_remote::share_backlog()? else {
        return Ok(None);
    };
    let cwd = std::env::current_dir().ok();
    let plans_root = plans_root_for(cwd.as_deref());
    let peers = ssh_peers();
    fetch_with(
        node,
        bound_plan,
        &remote,
        &host_key(),
        &home(),
        plans_root.as_deref(),
        &peers,
        &ssh_cat,
    )
    .map(Some)
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

    /// The transport when nothing is configured: every ask fails, and the
    /// rescue treats that as "this peer does not serve the file".
    fn no_ssh(alias: &str, path: &str) -> Result<String, String> {
        Err(format!("ssh off: {alias} {path}"))
    }

    fn bare_peers() -> BTreeMap<String, String> {
        BTreeMap::new()
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
        let out = sync_with(
            &a,
            &primary.remote,
            "a",
            home.path(),
            None,
            &bare_peers(),
            &no_ssh,
        )
        .unwrap();
        assert_eq!(out.pushed, vec!["x-1"]);
        // B has no file, and its replica lost the binding.
        std::fs::remove_file(&plan).unwrap();
        let b = replica(&[("x-1", "")]);
        let out = sync_with(
            &b,
            &primary.remote,
            "b",
            home.path(),
            None,
            &bare_peers(),
            &no_ssh,
        )
        .unwrap();
        assert_eq!(out.pulled, vec!["x-1"]);
        assert_eq!(sha(&std::fs::read_to_string(&plan).unwrap()), sha(&body));
        // A second pass is quiet: nothing moved.
        let out = sync_with(
            &a,
            &primary.remote,
            "a",
            home.path(),
            None,
            &bare_peers(),
            &no_ssh,
        )
        .unwrap();
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
        sync_with(
            &b,
            &primary.remote,
            "b",
            home.path(),
            None,
            &bare_peers(),
            &no_ssh,
        )
        .unwrap();
        // The peer pushes v1; B edits its own copy to v2 before it syncs.
        push(&primary.remote, "x-1", plan_s, "v1", "a").unwrap();
        write_atomic(&plan, "v2").unwrap();
        let out = sync_with(
            &b,
            &primary.remote,
            "b",
            home.path(),
            None,
            &bare_peers(),
            &no_ssh,
        )
        .unwrap();
        assert_eq!(out.conflicts, vec!["x-1"]);
        assert_eq!(std::fs::read_to_string(&plan).unwrap(), "v2");
        // Untouched since its sync, B takes the peer's edit.
        write_atomic(&plan, "v1").unwrap();
        sync_with(
            &b,
            &primary.remote,
            "b",
            home.path(),
            None,
            &bare_peers(),
            &no_ssh,
        )
        .unwrap();
        push(&primary.remote, "x-1", plan_s, "v3", "a").unwrap();
        let out = sync_with(
            &b,
            &primary.remote,
            "b",
            home.path(),
            None,
            &bare_peers(),
            &no_ssh,
        )
        .unwrap();
        assert_eq!(out.pulled, vec!["x-1"]);
        assert_eq!(std::fs::read_to_string(&plan).unwrap(), "v3");
        // A rebind to a new path moves the carried body, never a standing conflict.
        let moved = home.path().join("plans/q.md");
        write_atomic(&moved, "v4").unwrap();
        let c = replica(&[("x-1", moved.to_str().unwrap())]);
        let out = sync_with(
            &c,
            &primary.remote,
            "b",
            home.path(),
            None,
            &bare_peers(),
            &no_ssh,
        )
        .unwrap();
        assert_eq!(out.pushed, vec!["x-1"]);
        assert_eq!(
            sync_with(
                &c,
                &primary.remote,
                "b",
                home.path(),
                None,
                &bare_peers(),
                &no_ssh
            )
            .unwrap(),
            Outcome::default()
        );
    }

    #[test]
    fn a_foreign_home_is_unplaced_and_fetch_names_the_pushing_host() {
        let primary = test_primary::start();
        let home = tempfile::tempdir().unwrap();
        assert_eq!(
            fetch_with(
                "x-1",
                "",
                &primary.remote,
                "b",
                home.path(),
                None,
                &bare_peers(),
                &no_ssh
            )
            .unwrap_err(),
            "no host pushed a plan for it"
        );
        ensure_tables(&primary.remote).unwrap();
        push(&primary.remote, "x-1", "/elsewhere/plans/p.md", "v0", "a").unwrap();
        let b = replica(&[("x-1", "/elsewhere/plans/p.md")]);
        let out = sync_with(
            &b,
            &primary.remote,
            "b",
            home.path(),
            None,
            &bare_peers(),
            &no_ssh,
        )
        .unwrap();
        assert_eq!(out.unplaced, vec!["x-1"]);
        let err = fetch_with(
            "x-1",
            "",
            &primary.remote,
            "b",
            home.path(),
            None,
            &bare_peers(),
            &no_ssh,
        )
        .unwrap_err();
        assert!(err.contains("outside this host's home"), "{err}");
        // A row naming a dot-dir or a non-plan file under home never lands.
        for evil in [".ssh/authorized_keys.md", "plans/p.sh"] {
            let path = home.path().join(evil);
            push(&primary.remote, "x-1", path.to_str().unwrap(), "pwn", "a").unwrap();
            assert!(fetch_with(
                "x-1",
                "",
                &primary.remote,
                "b",
                home.path(),
                None,
                &bare_peers(),
                &no_ssh
            )
            .is_err());
            assert!(!path.exists());
        }
        // An unreachable primary is an error the tick logs, never a panic.
        assert!(sync_with(
            &b,
            &test_primary::dead(),
            "b",
            home.path(),
            None,
            &bare_peers(),
            &no_ssh
        )
        .is_err());
    }

    #[test]
    fn a_relative_row_places_the_plan_under_each_reader_s_own_plans_root() {
        let primary = test_primary::start();
        let home_a = tempfile::tempdir().unwrap();
        let home_b = tempfile::tempdir().unwrap();
        let root_a = home_a.path().join("vault/fno/plans");
        let root_b = home_b.path().join("vault/fno/plans");
        std::fs::create_dir_all(&root_a).unwrap();
        std::fs::create_dir_all(&root_b).unwrap();
        let plan = root_a.join("20261021-portable-plans-row.md");
        write_atomic(&plan, "v1").unwrap();
        // The writer's node row carries its absolute path; the carrier stores
        // the plans-root-relative form.
        let a = replica(&[("x-1", plan.to_str().unwrap())]);
        let out = sync_with(
            &a,
            &primary.remote,
            "a",
            home_a.path(),
            Some(&root_a),
            &bare_peers(),
            &no_ssh,
        )
        .unwrap();
        assert_eq!(out.pushed, vec!["x-1"]);
        let stored = primary_rows(&primary.remote, "a")
            .unwrap()
            .get("x-1")
            .unwrap()
            .0
            .plan_path
            .clone();
        assert_eq!(stored, "20261021-portable-plans-row.md");
        assert!(!stored.contains(home_a.path().to_str().unwrap()));
        // A reader whose home differs places the body under its own root,
        // even though its node row still names the writer's absolute path.
        let b = replica(&[("x-1", plan.to_str().unwrap())]);
        let out = sync_with(
            &b,
            &primary.remote,
            "b",
            home_b.path(),
            Some(&root_b),
            &bare_peers(),
            &no_ssh,
        )
        .unwrap();
        assert_eq!(out.pulled, vec!["x-1"]);
        let placed = root_b.join("20261021-portable-plans-row.md");
        assert_eq!(sha(&std::fs::read_to_string(&placed).unwrap()), sha("v1"));
        // The second pass is quiet even though B's node row still names the
        // writer's path: the placed file already holds the row's hash.
        let out = sync_with(
            &b,
            &primary.remote,
            "b",
            home_b.path(),
            Some(&root_b),
            &bare_peers(),
            &no_ssh,
        )
        .unwrap();
        assert_eq!(out, Outcome::default());
        // Target init's miss fetch resolves and returns B's own path.
        let got = fetch_with(
            "x-1",
            "",
            &primary.remote,
            "b",
            home_b.path(),
            Some(&root_b),
            &bare_peers(),
            &no_ssh,
        )
        .unwrap();
        assert_eq!(got, placed.to_str().unwrap());
    }

    #[test]
    fn a_lost_body_is_fetched_over_ssh_from_the_configured_peer() {
        let primary = test_primary::start();
        let home = tempfile::tempdir().unwrap();
        let root = home.path().join("vault/plans");
        std::fs::create_dir_all(&root).unwrap();
        ensure_tables(&primary.remote).unwrap();
        // The store lost the body, but the seen ledger names the host that
        // has the file.
        mark_seen(&primary.remote, "x-1", "macbook", &sha("v1")).unwrap();
        let b = replica(&[("x-1", "/Users/alice/vault/plans/p.md")]);
        let mut peers = BTreeMap::new();
        peers.insert("macbook".to_string(), "macbook.lan".to_string());
        let fetched = |alias: &str, path: &str| {
            assert_eq!(alias, "macbook.lan");
            assert_eq!(path, "/Users/alice/vault/plans/p.md");
            Ok::<String, String>("rescued".to_string())
        };
        let out = sync_with(
            &b,
            &primary.remote,
            "b",
            home.path(),
            Some(&root),
            &peers,
            &fetched,
        )
        .unwrap();
        assert_eq!(out.fetched, vec!["x-1"]);
        assert_eq!(
            std::fs::read_to_string(root.join("p.md")).unwrap(),
            "rescued"
        );
        // With no peers configured nothing is asked, and the pass stays quiet.
        let out = sync_with(
            &b,
            &primary.remote,
            "b",
            home.path(),
            Some(&root),
            &bare_peers(),
            &fetched,
        )
        .unwrap();
        assert_eq!(out, Outcome::default());
    }

    #[test]
    fn fetch_rescues_a_foreign_absolute_row_over_ssh_when_configured() {
        let primary = test_primary::start();
        let home = tempfile::tempdir().unwrap();
        ensure_tables(&primary.remote).unwrap();
        push(
            &primary.remote,
            "x-1",
            "/Users/alice/vault/plans/p.md",
            "v1",
            "a",
        )
        .unwrap();
        let mut peers = BTreeMap::new();
        peers.insert(host_name(), "alice.lan".to_string());
        let fetched = |alias: &str, path: &str| {
            assert_eq!(alias, "alice.lan");
            assert_eq!(path, "/Users/alice/vault/plans/p.md");
            Ok::<String, String>("v1".to_string())
        };
        let got = fetch_with(
            "x-1",
            "",
            &primary.remote,
            "b",
            home.path(),
            None,
            &peers,
            &fetched,
        )
        .unwrap();
        let placed = home.path().join("vault/plans/p.md");
        assert_eq!(got, placed.to_str().unwrap());
        assert_eq!(std::fs::read_to_string(&placed).unwrap(), "v1");
        // Unconfigured, the error still names the pushing host and path.
        let err = fetch_with(
            "x-1",
            "",
            &primary.remote,
            "b",
            home.path(),
            None,
            &bare_peers(),
            &fetched,
        )
        .unwrap_err();
        assert!(err.contains("outside this host's home"), "{err}");
        assert!(!placed.exists());
    }

    #[test]
    fn a_tilde_binding_and_an_absolute_binding_of_one_file_stay_quiet() {
        let primary = test_primary::start();
        let home = tempfile::tempdir().unwrap();
        let plan = home.path().join("vault/plans/p.md");
        write_atomic(&plan, "v1").unwrap();
        let absolute = plan.to_str().unwrap().to_string();
        let tilde = format!("~/vault/plans/p.md");
        // Host A binds the tilde form and pushes; the stored row carries the
        // absolute normalization.
        let a = replica(&[("x-1", tilde.as_str())]);
        let out = sync_with(
            &a,
            &primary.remote,
            "a",
            home.path(),
            None,
            &bare_peers(),
            &no_ssh,
        )
        .unwrap();
        assert_eq!(out.pushed, vec!["x-1"]);
        let stored = primary_rows(&primary.remote, "a")
            .unwrap()
            .get("x-1")
            .unwrap()
            .0
            .plan_path
            .clone();
        assert_eq!(stored, absolute);
        // Host B binds the absolute form of the same file: equal stored
        // forms, so the pass takes the hash path and stays quiet.
        let b = replica(&[("x-1", absolute.as_str())]);
        let out = sync_with(
            &b,
            &primary.remote,
            "b",
            home.path(),
            None,
            &bare_peers(),
            &no_ssh,
        )
        .unwrap();
        assert_eq!(out, Outcome::default());
    }
}
