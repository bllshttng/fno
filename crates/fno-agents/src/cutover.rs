//! `fno agents incident cutover save|restore`: move the fleet's durable state
//! to another machine without losing the backlog, event provenance or
//! history.
//!
//! `save` quiesces every fno writer on this machine, then snapshots every
//! SQLite store under the state root into a bundle: arm the fleet incident
//! (all scopes), wait until no registry row is mid-reply, stop the daemon,
//! then copy each store through the SQLite backup API and compare integrity
//! and per-table row counts between the live file and the copy. The receipt
//! names each store, its backup path and its counts. The incident stays
//! armed: until a shared store exists the source is read-only, and a daemon
//! that starts again reads the armed record and holds its sweeps.
//!
//! `restore` runs on the target. It verifies every backup against the
//! manifest BEFORE it touches the target, moves each existing store aside
//! (never deletes it), and installs the verified copies.
//!
//! State keys on absolute paths, so `restore` refuses a bundle whose state
//! root or user differs from the target's. The rest of the state root is plain
//! files; the receipt prints the one rsync line that carries them.
//!
//! Reached as an argument of `fleet-incident` (law d-fe66560a allows no new
//! client action): `fno agents incident cutover save|restore`.

use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};
use std::time::{Duration, Instant};

use rusqlite::{Connection, OpenFlags};
use serde::{Deserialize, Serialize};

use crate::paths::AgentsHome;
use crate::state_layout_sqlite::table_counts;
use crate::AgentStatus;

const MANIFEST: &str = "manifest.json";
const MANIFEST_VERSION: u32 = 1;
const SQLITE_MAGIC: &[u8] = b"SQLite format 3\0";
const STORE_EXTS: [&str; 3] = ["db", "sqlite", "sqlite3"];
/// Directories the store walk never enters: the rebuildable cargo cache
/// (3 GB on the measured machine) and the bundles themselves.
const SKIP_DIRS: [&str; 2] = ["cargo-build", "cutover"];
const DEFAULT_IDLE_SECS: u64 = 600;
const IDLE_POLL: Duration = Duration::from_secs(2);
const COPY_PAGES_PER_STEP: i32 = 256;
const COPY_PAUSE: Duration = Duration::from_millis(25);

#[derive(Debug, Serialize, Deserialize)]
struct Manifest {
    version: u32,
    created_at: String,
    user: String,
    state_root: String,
    incident_generation: u64,
    stores: Vec<StoreEntry>,
}

#[derive(Debug, Serialize, Deserialize)]
struct StoreEntry {
    /// Path under the state root, `/`-separated.
    rel: String,
    tables: BTreeMap<String, i64>,
}

impl StoreEntry {
    fn rows(&self) -> i64 {
        self.tables.values().sum()
    }
}

fn usage() {
    println!(
        "usage: fno agents incident cutover save [--out <dir>] [--idle-timeout <secs>] [--ignore <agent>]... | restore <bundle>"
    );
}

/// Binary entry for the `cutover` argument of `fleet-incident`.
pub async fn run_cutover(args: &[String]) -> i32 {
    match args.first().map(String::as_str) {
        Some("save") => save(&args[1..]).await,
        Some("restore") => restore(&args[1..]),
        Some("-h" | "--help" | "help") => {
            usage();
            0
        }
        _ => {
            usage();
            2
        }
    }
}

struct SaveArgs {
    out: Option<PathBuf>,
    idle_secs: u64,
    ignore: Vec<String>,
}

fn parse_save(args: &[String]) -> Result<SaveArgs, String> {
    let mut parsed = SaveArgs {
        out: None,
        idle_secs: DEFAULT_IDLE_SECS,
        ignore: Vec::new(),
    };
    let mut it = args.iter();
    while let Some(flag) = it.next() {
        let mut value = |name: &str| {
            it.next()
                .cloned()
                .ok_or_else(|| format!("{name} needs a value"))
        };
        match flag.as_str() {
            "--out" => parsed.out = Some(PathBuf::from(value("--out")?)),
            "--idle-timeout" => {
                parsed.idle_secs = value("--idle-timeout")?
                    .parse()
                    .map_err(|_| "--idle-timeout takes whole seconds".to_string())?
            }
            "--ignore" => parsed.ignore.push(value("--ignore")?),
            other => return Err(format!("unrecognized argument {other:?}")),
        }
    }
    Ok(parsed)
}

async fn save(args: &[String]) -> i32 {
    let parsed = match parse_save(args) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("cutover: {e}");
            usage();
            return 2;
        }
    };
    let home = AgentsHome::from_env();
    let root = crate::scratch::fno_state_root();
    let stamp = chrono::Utc::now().format("%Y%m%dT%H%M%S").to_string();
    let out = parsed
        .out
        .clone()
        .unwrap_or_else(|| root.join("cutover").join(&stamp));
    let out = std::path::absolute(&out).unwrap_or(out);
    if out.exists() && std::fs::read_dir(&out).is_ok_and(|mut d| d.next().is_some()) {
        eprintln!(
            "cutover: {} exists and is not empty; pick a new --out",
            out.display()
        );
        return 1;
    }

    // 1. Arm the incident, all scopes. Left armed on every exit below: a
    //    refused cutover is still a quiet machine until an operator clears it.
    let record = match crate::fleet_incident::write_transition_with_metadata(
        &crate::fleet_incident::fleet_stop_path(&home),
        "stopped",
        Some("fleet cutover: quiescing writers to snapshot the stores"),
        Some("cutover"),
        crate::fleet_incident::SCOPES
            .iter()
            .map(|s| s.to_string())
            .collect(),
        Default::default(),
    ) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("cutover refused: cannot arm the fleet incident: {e}");
            return 1;
        }
    };
    println!(
        "cutover: incident stopped (generation {}), spawns tests merges loops held",
        record.generation
    );
    if let Ok(outcome) = crate::test_hold::reconcile(&home) {
        if let Some(line) = outcome.line() {
            println!("cutover: {line}");
        }
    }

    // 2. Wait for every session to go idle. Idle means no tool calls, so no
    //    hook writes. A timeout refuses; the incident stays armed.
    match wait_idle(&home, &parsed.ignore, Duration::from_secs(parsed.idle_secs)).await {
        Ok(checked) => println!("cutover: {checked} registry rows idle"),
        Err(why) => {
            eprintln!("cutover refused: {why}");
            eprintln!(
                "the incident stays armed; rerun save, or `fno agents incident clear` to resume"
            );
            return 1;
        }
    }

    // 3. Stop the daemon.
    match crate::client::stop_daemon(&home).await {
        Ok(crate::client::DaemonStop::WasDown) => println!("cutover: daemon already down"),
        Ok(crate::client::DaemonStop::Exited { pid }) => {
            println!("cutover: daemon stopped (pid {pid})")
        }
        Ok(crate::client::DaemonStop::Escalated { pid }) => {
            println!("cutover: daemon stopped (pid {pid}, SIGKILL after the grace)")
        }
        Err(e) => {
            eprintln!("cutover refused: the daemon would not stop: {e}");
            return 1;
        }
    }

    // 4. Snapshot.
    let manifest = match snapshot(&root, &out, record.generation) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("cutover refused: {e}");
            return 1;
        }
    };

    // A daemon that rebound during the copy wrote beside it; say so rather
    // than hand over a snapshot that may be stale.
    if !crate::client::daemon_lock_is_free(&home) {
        eprintln!(
            "cutover refused: a daemon holds the supervisor lock again; the bundle at {} may be stale. Stop it and rerun save.",
            out.display()
        );
        return 1;
    }
    print_save_receipt(&root, &out, &manifest);
    0
}

/// Poll the registry until no row is `Spawning`, `Busy` or `Restarting`.
/// `Ok(rows checked)` when idle; `Err(reason)` at the deadline. An unreadable
/// registry is a refusal: idle cannot be shown.
async fn wait_idle(home: &AgentsHome, ignore: &[String], bound: Duration) -> Result<usize, String> {
    let start = Instant::now();
    loop {
        let registry = crate::state::load_registry(&home.registry_json())
            .map_err(|e| format!("the registry is unreadable, so idle cannot be shown: {e}"))?;
        let busy: Vec<String> = registry
            .entries
            .iter()
            .filter(|row| !ignore.contains(&row.name))
            .filter(|row| {
                matches!(
                    row.status,
                    AgentStatus::Spawning | AgentStatus::Busy | AgentStatus::Restarting
                )
            })
            .map(|row| row.name.clone())
            .collect();
        if busy.is_empty() {
            return Ok(registry.entries.len());
        }
        if start.elapsed() >= bound {
            return Err(format!(
                "still mid-reply after {}s: {}",
                bound.as_secs(),
                busy.join(", ")
            ));
        }
        tokio::time::sleep(IDLE_POLL).await;
    }
}

/// Every non-empty SQLite file under `root` (relative paths, sorted),
/// found by header magic as well as extension, never through a symlink.
/// The `bundle` directory is skipped so a bundle inside the root is never
/// bundled again.
fn discover_stores(root: &Path, bundle: &Path) -> Vec<PathBuf> {
    fn walk(root: &Path, bundle: &Path, dir: &Path, out: &mut Vec<PathBuf>) {
        let Ok(read) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in read.flatten() {
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            let path = entry.path();
            if kind.is_dir() {
                let skip = dir == root
                    && path
                        .file_name()
                        .and_then(|n| n.to_str())
                        .is_some_and(|n| SKIP_DIRS.contains(&n));
                if !skip && path != bundle {
                    walk(root, bundle, &path, out);
                }
            } else if kind.is_file()
                && path
                    .extension()
                    .and_then(|e| e.to_str())
                    .is_some_and(|e| STORE_EXTS.contains(&e))
                && has_sqlite_magic(&path)
            {
                if let Ok(rel) = path.strip_prefix(root) {
                    out.push(rel.to_path_buf());
                }
            }
        }
    }
    let mut out = Vec::new();
    walk(root, bundle, root, &mut out);
    out.sort();
    out
}

fn has_sqlite_magic(path: &Path) -> bool {
    use std::io::Read;
    let mut head = [0u8; 16];
    std::fs::File::open(path)
        .and_then(|mut f| f.read_exact(&mut head))
        .is_ok()
        && head == SQLITE_MAGIC
}

fn rel_string(rel: &Path) -> String {
    rel.components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("/")
}

/// Copy every store under `root` into `out/stores/`, verify each copy against
/// its source, and write the manifest. Any divergence refuses the whole save.
fn snapshot(root: &Path, out: &Path, generation: u64) -> Result<Manifest, String> {
    let mut stores = Vec::new();
    for rel in discover_stores(root, out) {
        let dst = out.join("stores").join(&rel);
        let tables = backup_verified(&root.join(&rel), &dst)
            .map_err(|e| format!("{}: {e}", rel.display()))?;
        stores.push(StoreEntry {
            rel: rel_string(&rel),
            tables,
        });
    }
    let manifest = Manifest {
        version: MANIFEST_VERSION,
        created_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        user: std::env::var("USER").unwrap_or_default(),
        state_root: root.to_string_lossy().into_owned(),
        incident_generation: generation,
        stores,
    };
    let body = serde_json::to_string_pretty(&manifest).map_err(|e| e.to_string())?;
    std::fs::write(out.join(MANIFEST), body).map_err(|e| format!("manifest: {e}"))?;
    Ok(manifest)
}

/// SQLite online backup `src` -> `dst`, then integrity check and row-count
/// equality. Returns the per-table counts of the copy.
fn backup_verified(src: &Path, dst: &Path) -> Result<BTreeMap<String, i64>, String> {
    if let Some(dir) = dst.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    let source = Connection::open_with_flags(
        src,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|e| format!("cannot open the store: {e}"))?;
    source
        .busy_timeout(Duration::from_secs(30))
        .map_err(|e| e.to_string())?;
    let mut copy = Connection::open(dst).map_err(|e| format!("cannot create the copy: {e}"))?;
    rusqlite::backup::Backup::new(&source, &mut copy)
        .and_then(|b| b.run_to_completion(COPY_PAGES_PER_STEP, COPY_PAUSE, None))
        .map_err(|e| format!("backup failed: {e}"))?;
    let want = table_counts(&source)?;
    let got = verify_copy(&copy)?;
    if want != got {
        return Err(format!(
            "row counts diverge between the store and its copy: {want:?} vs {got:?}"
        ));
    }
    Ok(got)
}

/// Integrity check plus per-table counts of an opened copy.
fn verify_copy(conn: &Connection) -> Result<BTreeMap<String, i64>, String> {
    let integrity: String = conn
        .query_row("PRAGMA integrity_check;", [], |r| r.get(0))
        .map_err(|e| format!("integrity check failed: {e}"))?;
    if integrity != "ok" {
        return Err(format!("integrity_check read {integrity}"));
    }
    table_counts(conn)
}

fn print_save_receipt(root: &Path, out: &Path, manifest: &Manifest) {
    println!("cutover: bundle {}", out.display());
    println!("{:<44} {:>10} {:>7}  backup", "store", "rows", "tables");
    for s in &manifest.stores {
        println!(
            "{:<44} {:>10} {:>7}  {}",
            s.rel,
            s.rows(),
            s.tables.len(),
            out.join("stores").join(&s.rel).display()
        );
    }
    let user = &manifest.user;
    println!();
    println!("The source is read-only now: the incident stays armed and the daemon is down.");
    println!(
        "On the target (same user {user:?}, same state root {}):",
        root.display()
    );
    println!(
        "  rsync -a --exclude '/cargo-build/' {root}/ <target>:{root}/",
        root = root.display()
    );
    println!("  fno agents incident cutover restore {}", out.display());
    println!("  fno agents restart");
    println!("  fno agents incident clear --reason 'cutover complete'");
    println!("  fno mux workspace restore");
}

fn restore(args: &[String]) -> i32 {
    let [bundle] = args else {
        usage();
        return 2;
    };
    let bundle = Path::new(bundle);
    let root = crate::scratch::fno_state_root();
    let home = AgentsHome::from_env();
    if !crate::client::daemon_lock_is_free(&home) {
        eprintln!("cutover restore refused: a daemon is running here; stop it first (`fno agents restart` starts one after the restore)");
        return 1;
    }
    match restore_stores(&root, bundle) {
        Ok(rows) => {
            println!(
                "cutover: restored {} stores from {}",
                rows.len(),
                bundle.display()
            );
            for (rel, count) in &rows {
                println!("  {rel}: {count} rows verified");
            }
            println!("Next: `fno agents restart`, then `fno agents incident clear --reason 'cutover complete'`, then `fno mux workspace restore`.");
            0
        }
        Err(e) => {
            eprintln!("cutover restore refused: {e}");
            1
        }
    }
}

/// Verify the whole bundle, then install it under `root`. Returns
/// `(rel, rows)` per installed store. Nothing on the target changes until
/// every backup has passed.
fn restore_stores(root: &Path, bundle: &Path) -> Result<Vec<(String, i64)>, String> {
    let body = std::fs::read_to_string(bundle.join(MANIFEST))
        .map_err(|e| format!("no readable {MANIFEST} in {}: {e}", bundle.display()))?;
    let manifest: Manifest = serde_json::from_str(&body).map_err(|e| format!("{MANIFEST}: {e}"))?;
    if manifest.version != MANIFEST_VERSION {
        return Err(format!(
            "bundle version {} is not {MANIFEST_VERSION}",
            manifest.version
        ));
    }
    let here = root.to_string_lossy();
    if manifest.state_root != here {
        return Err(format!(
            "state keys on absolute paths: the bundle's state root is {} and this machine's is {here}",
            manifest.state_root
        ));
    }
    let user = std::env::var("USER").unwrap_or_default();
    if manifest.user != user {
        return Err(format!(
            "the bundle was saved by user {:?} and this is {user:?}",
            manifest.user
        ));
    }
    for store in &manifest.stores {
        let rel = Path::new(&store.rel);
        if rel.is_absolute() || rel.components().any(|c| !matches!(c, Component::Normal(_))) {
            return Err(format!(
                "{}: the store path escapes the state root",
                store.rel
            ));
        }
    }
    for store in &manifest.stores {
        let src = bundle.join("stores").join(&store.rel);
        let conn = Connection::open_with_flags(&src, OpenFlags::SQLITE_OPEN_READ_ONLY)
            .map_err(|e| format!("{}: cannot open the backup: {e}", store.rel))?;
        let counts = verify_copy(&conn).map_err(|e| format!("{}: {e}", store.rel))?;
        if counts != store.tables {
            return Err(format!(
                "{}: the backup no longer matches the manifest counts",
                store.rel
            ));
        }
    }
    let stamp = chrono::Utc::now().format("%Y%m%dT%H%M%S").to_string();
    let mut installed: Vec<(String, i64)> = Vec::new();
    for store in &manifest.stores {
        install_store(root, bundle, store, &stamp).map_err(|e| {
            let done: Vec<&str> = installed.iter().map(|(rel, _)| rel.as_str()).collect();
            format!(
                "{}: {e}. Installed before this: {}. Replaced stores stay beside them as *.pre-cutover-{stamp}",
                store.rel,
                if done.is_empty() { "none".to_string() } else { done.join(", ") }
            )
        })?;
        installed.push((store.rel.clone(), store.rows()));
    }
    Ok(installed)
}

/// Move the old store aside, copy the verified backup in, and check it. A
/// failure puts the old store back, so no store is ever left missing.
fn install_store(
    root: &Path,
    bundle: &Path,
    store: &StoreEntry,
    stamp: &str,
) -> Result<(), String> {
    let dest = root.join(&store.rel);
    if let Some(dir) = dest.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    let mut moved = Vec::new();
    let tmp = with_suffix(&dest, ".cutover-tmp");
    let result = (|| {
        for suffix in ["", "-wal", "-shm"] {
            let old = with_suffix(&dest, suffix);
            if old.exists() {
                let aside = with_suffix(&dest, &format!("{suffix}.pre-cutover-{stamp}"));
                std::fs::rename(&old, &aside)
                    .map_err(|e| format!("cannot move the old store aside: {e}"))?;
                moved.push((old, aside));
            }
        }
        std::fs::copy(bundle.join("stores").join(&store.rel), &tmp)
            .and_then(|_| std::fs::rename(&tmp, &dest))
            .map_err(|e| format!("install failed: {e}"))?;
        let conn = Connection::open_with_flags(&dest, OpenFlags::SQLITE_OPEN_READ_ONLY)
            .map_err(|e| format!("cannot reopen the installed store: {e}"))?;
        if verify_copy(&conn)? != store.tables {
            return Err("installed counts diverge from the manifest".to_string());
        }
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
        for (old, aside) in moved.iter().rev() {
            let _ = std::fs::remove_file(old);
            let _ = std::fs::rename(aside, old);
        }
    }
    result
}

fn with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut os = path.as_os_str().to_os_string();
    os.push(suffix);
    PathBuf::from(os)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_store(path: &Path, rows: i64) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let conn = Connection::open(path).unwrap();
        conn.execute_batch("CREATE TABLE t (n INTEGER); CREATE TABLE u (n INTEGER);")
            .unwrap();
        for n in 0..rows {
            conn.execute("INSERT INTO t VALUES (?1)", [n]).unwrap();
        }
    }

    #[test]
    fn save_then_restore_round_trips_and_keeps_the_old_store() {
        let src = tempfile::tempdir().unwrap();
        let dst = tempfile::tempdir().unwrap();
        let bundle = tempfile::tempdir().unwrap();
        make_store(&src.path().join("db/graph.db"), 7);
        make_store(&src.path().join("spaces/x/events.db"), 3);
        // Not stores: wrong magic, empty file, skipped dir.
        std::fs::write(src.path().join("db/notes.db"), "plain text").unwrap();
        std::fs::write(src.path().join("graph.db"), "").unwrap();
        make_store(&src.path().join("cargo-build/cache.db"), 1);

        let out = bundle.path().join("b");
        let manifest = snapshot(src.path(), &out, 4).unwrap();
        let rels: Vec<_> = manifest.stores.iter().map(|s| s.rel.as_str()).collect();
        assert_eq!(rels, ["db/graph.db", "spaces/x/events.db"]);
        assert_eq!(manifest.stores[0].rows(), 7);

        // The target root must match the bundle's, so rewrite the manifest
        // to name the tempdir the restore runs against.
        let mut m: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(out.join(MANIFEST)).unwrap()).unwrap();
        m["state_root"] = dst.path().to_string_lossy().to_string().into();
        std::fs::write(out.join(MANIFEST), m.to_string()).unwrap();

        make_store(&dst.path().join("db/graph.db"), 99);
        let done = restore_stores(dst.path(), &out).unwrap();
        assert_eq!(
            done,
            [
                ("db/graph.db".to_string(), 7),
                ("spaces/x/events.db".to_string(), 3)
            ]
        );
        let restored = Connection::open(dst.path().join("db/graph.db")).unwrap();
        assert_eq!(table_counts(&restored).unwrap()["t"], 7);
        let aside = std::fs::read_dir(dst.path().join("db"))
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().contains(".pre-cutover-"))
            .count();
        assert_eq!(aside, 1, "the replaced store is moved aside, never deleted");
    }

    #[test]
    fn restore_refuses_a_foreign_state_root_and_a_tampered_backup() {
        let src = tempfile::tempdir().unwrap();
        let dst = tempfile::tempdir().unwrap();
        let bundle = tempfile::tempdir().unwrap();
        make_store(&src.path().join("db/graph.db"), 5);
        let out = bundle.path().join("b");
        snapshot(src.path(), &out, 1).unwrap();

        let err = restore_stores(dst.path(), &out).unwrap_err();
        assert!(err.contains("absolute paths"), "{err}");

        let mut m: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(out.join(MANIFEST)).unwrap()).unwrap();
        m["state_root"] = dst.path().to_string_lossy().to_string().into();
        std::fs::write(out.join(MANIFEST), m.to_string()).unwrap();
        let good = m.to_string();
        m["stores"][0]["rel"] = "../evil.db".into();
        std::fs::write(out.join(MANIFEST), m.to_string()).unwrap();
        let err = restore_stores(dst.path(), &out).unwrap_err();
        assert!(err.contains("escapes"), "{err}");
        std::fs::write(out.join(MANIFEST), good).unwrap();
        let backup = Connection::open(out.join("stores/db/graph.db")).unwrap();
        backup.execute("DELETE FROM t", []).unwrap();
        drop(backup);
        let err = restore_stores(dst.path(), &out).unwrap_err();
        assert!(err.contains("no longer matches"), "{err}");
        assert!(
            !dst.path().join("db/graph.db").exists(),
            "nothing installed on refusal"
        );
    }
}
