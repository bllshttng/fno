//! The state-root layout: one table (`docs/state-root-layout.tsv`, read at
//! build time) that names where every movable top-level file of the state
//! root lives after the migration, plus the one resolver ([`place`]) and the
//! one mover ([`migrate`]). The tidiness law: nothing writes at the top level
//! of the state root when a named subfolder can hold it.
//!
//! `place` is read-old-as-fallback: a migrated root reads the new path, an
//! unmigrated root keeps reading the legacy one, a fresh install gets the
//! new path. Nothing here ever deletes data: the only deletion is an empty
//! legacy lock file after its data file moved and a non-blocking flock
//! succeeds, and every conflict parks into `backups/state-root-migration/`.
//! The `sqlite` kind moves through a backup-API protocol in a later wave;
//! until that lands the migrate pass reports those rows pending.
//!
//! The `fno` crate carries its own copy of the table parse and `place`
//! (`crates/fno/src/state_layout.rs`), same file, same dialect - the
//! dual-implementation inventory pattern (one TSV, two readers, no third
//! mechanism).

use std::os::unix::fs::FileTypeExt;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// The shipped layout table, vendored beside this module: a `cargo package`
/// tarball carries only crate-root files, so the repo's `docs/` copy cannot
/// feed `include_str!` in a packaged build (the publish dry-run proved it).
/// The repo file is the one EDIT source; the drift test pins this copy
/// byte-identical to it, so a table edit that skips the copies fails CI.
pub const LAYOUT_TSV: &str = include_str!("state-root-layout.tsv");

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Sqlite,
    Anchor,
    Append,
    Overwrite,
    Marker,
    Page,
    Lock,
    Park,
    Dir,
}

impl Kind {
    fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "sqlite" => Self::Sqlite,
            "anchor" => Self::Anchor,
            "append" => Self::Append,
            "overwrite" => Self::Overwrite,
            "marker" => Self::Marker,
            "page" => Self::Page,
            "lock" => Self::Lock,
            "park" => Self::Park,
            "dir" => Self::Dir,
            _ => return None,
        })
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Owner {
    Daemon,
    Mux,
}

impl Owner {
    fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "daemon" => Self::Daemon,
            "mux" => Self::Mux,
            _ => return None,
        })
    }
}

#[derive(Clone, Debug)]
pub struct Row {
    pub legacy: String,
    pub new: String,
    pub kind: Kind,
    pub owner: Owner,
}

/// Parse the table. Every failure names its 1-based line so a bad row can
/// never ship silently (the unit tests pin both failure shapes).
pub fn parse_table(tsv: &str) -> Result<Vec<Row>, String> {
    let mut out = Vec::new();
    for (idx, line) in tsv.lines().enumerate() {
        let no = idx + 1;
        let line = line.trim_end_matches('\r');
        if line.trim().is_empty() || line.trim_start().starts_with('#') {
            continue;
        }
        let fields: Vec<&str> = line.split('\t').collect();
        let bad = |why: &str| -> String {
            format!("state-root layout table line {no}: {why} ({line:?})")
        };
        let [legacy, new, kind, owner] = fields.as_slice() else {
            return Err(bad("expected 4 TAB-separated fields"));
        };
        if legacy.is_empty() || new.is_empty() {
            return Err(bad("empty name"));
        }
        let Some(kind) = Kind::parse(kind) else {
            return Err(bad("unknown kind"));
        };
        let Some(owner) = Owner::parse(owner) else {
            return Err(bad("unknown owner"));
        };
        out.push(Row {
            legacy: legacy.to_string(),
            new: new.to_string(),
            kind,
            owner,
        });
    }
    if out.is_empty() {
        return Err("state-root layout table: no rows".to_string());
    }
    Ok(out)
}

/// The shipped rows, parsed once. A malformed shipped table panics loud
/// rather than resolving guessed paths; the unit tests keep that unreachable.
pub fn rows() -> &'static [Row] {
    static ROWS: OnceLock<&'static [Row]> = OnceLock::new();
    *ROWS.get_or_init(|| {
        let parsed = parse_table(LAYOUT_TSV).unwrap_or_else(|e| panic!("{e}"));
        Box::leak(parsed.into_boxed_slice())
    })
}

pub fn find(legacy_name: &str) -> Option<&'static Row> {
    rows().iter().find(|r| r.legacy == legacy_name)
}

/// The ruled top-level categories (d-c74fda50, 2026-10-16) plus `backups`,
/// the park base every stamp lands under. A row naming any other first
/// segment mints a top-level folder in every state root; the closed-set
/// test refuses it.
pub const APPROVED_TOP_LEVEL: &[&str] = &[
    "agents", "backups", "cache", "db", "install", "logs", "mux", "spaces", "state",
];

/// Destinations still in the table that fold into an approved category in a
/// named later slice. The closed-set test refuses an entry the table no
/// longer uses, so the slice that folds a folder drops its entry in the
/// same change.
pub const TRANSITIONAL_TOP_LEVEL: &[(&str, &str)] =
    &[("history", "folds into db in the durable-group slice")];

/// The folders a `dir` row may carry: the ruled fold set. `sessions` is
/// absent (it parks; it does not fold) and `worktrees` is absent (the ruled
/// relocation changes the allocation default only; live checkouts never
/// move). A new name here is a new top-level folder on every machine.
pub const FOLD_CANDIDATES: &[&str] = &[
    "announce-cursors",
    "attention",
    "attest",
    "backups",
    "bin",
    "bus",
    "cargo-build",
    "chats",
    "claim-aux",
    "context-cache",
    "fleet",
    "flight",
    "handoffs",
    "history",
    "inbox",
    "latches",
    "locks",
    "mail-escalations",
    "mail-hold",
    "migrations",
    "notice-route",
    "operator-capture",
    "pages",
    "plugin-stage",
    "postmortems",
    "push-stamps",
    "reclaim",
    "retro-pending",
    "review-invocations",
    "route-settings",
    "saved-sessions",
    "validity-decks",
];

/// Fold candidates whose children can be held by an open flock. The mover
/// will not fold one while a regular-file child's lock is held; the row
/// reports pending and the next daemon-start pass retries.
const LOCK_BEARING_FOLDERS: &[&str] = &["claim-aux", "flight", "fleet", "locks", "mail-hold"];

/// Fold candidates that drain instead of folding: each child's absolute
/// path is recorded elsewhere (a route-settings file is named in an agent
/// registry row, and a relaunch refuses a moved file rather than relaunch
/// on the wrong account), so the mover never renames one. New writes land
/// at the row's destination, the 14-day prune empties the legacy folder,
/// and this pass removes the empty shell.
const DRAIN_ONLY_FOLDERS: &[&str] = &["route-settings"];

/// The anchor probe: a `graph.json`-shaped path exists when its `.db`
/// sibling does - the journal anchor is virtual, the store is the file.
fn db_twin(p: &Path) -> PathBuf {
    p.with_extension("db")
}

/// Where `legacy_name` lives under `root`: the new path when it exists, else
/// the legacy path when it exists, else the new path (fresh install). The
/// `anchor` kind probes the `.db` twin instead of the virtual json. A name
/// the table does not carry resolves to `root/<name>` unchanged.
pub fn place(root: &Path, legacy_name: &str) -> PathBuf {
    let Some(row) = find(legacy_name) else {
        return root.join(legacy_name);
    };
    let new = root.join(&row.new);
    let legacy = root.join(&row.legacy);
    match row.kind {
        Kind::Anchor => {
            if db_twin(&new).exists() {
                new
            } else if db_twin(&legacy).exists() {
                legacy
            } else {
                new
            }
        }
        _ => {
            if new.exists() {
                new
            } else if legacy.exists() {
                legacy
            } else {
                new
            }
        }
    }
}

/// Where a ruled folder lives after its fold: the `dir` row's destination,
/// always, and nothing is created. `None` refuses a name the table does not
/// carry as a folder, so a caller never silently lands a new top-level
/// entry. Slices 4 to 8 route their writers through this.
pub fn place_dir(root: &Path, name: &str) -> Option<PathBuf> {
    let row = find(name)?;
    if row.kind != Kind::Dir {
        return None;
    }
    Some(root.join(&row.new))
}

/// The one-level-down read: `file` at the new dir when it exists, else at
/// the legacy dir when that differs and exists, else the new path (fresh
/// write lands new). The same ladder `place` runs, one level down, so a
/// straggler a stale writer left at the legacy spelling stays readable
/// until the next daemon-start pass folds it.
pub fn place_in(root: &Path, name: &str, file: &str) -> PathBuf {
    let new = place_dir(root, name).unwrap_or_else(|| root.join(name));
    let new_file = new.join(file);
    if new_file.exists() {
        return new_file;
    }
    if let Some(row) = find(name) {
        if row.kind == Kind::Dir && row.legacy != row.new {
            let legacy_file = root.join(&row.legacy).join(file);
            if legacy_file.exists() {
                return legacy_file;
            }
        }
    }
    new_file
}

/// The folder read for listers (cursors, latches, tickets): the children of
/// the new dir, then the legacy dir's children whose names the new side is
/// missing, new wins a clash. Skips both dirs when neither exists.
pub fn list_dir_children(root: &Path, name: &str) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let new_dir = place_dir(root, name).unwrap_or_else(|| root.join(name));
    let mut seen: Vec<String> = Vec::new();
    for dir in [Some(new_dir), legacy_dir_of(root, name)] {
        let Some(dir) = dir else { continue };
        let Ok(children) = std::fs::read_dir(&dir) else {
            continue;
        };
        for child in children.flatten() {
            let Some(file_name) = child.file_name().to_str().map(str::to_string) else {
                continue;
            };
            if seen.contains(&file_name) {
                continue;
            }
            seen.push(file_name);
            out.push(child.path());
        }
    }
    out
}

fn legacy_dir_of(root: &Path, name: &str) -> Option<PathBuf> {
    let row = find(name)?;
    if row.kind != Kind::Dir || row.legacy == row.new {
        return None;
    }
    Some(root.join(&row.legacy))
}

// ── The migration ────────────────────────────────────────────────────────

/// One entry's outcome. `absent` = nothing at the legacy path and the new
/// path already in place or equally empty: no work now, none owed, and it
/// does not count toward the pending total. `deleted` = the one lawful
/// deletion (an emptied lock). Everything else is the plan's vocabulary.
#[derive(Debug, PartialEq, Eq)]
pub enum Status {
    Moved,
    Merged,
    Parked,
    Deleted,
    Absent,
    Pending(String),
    Refused(String),
}

impl Status {
    fn word(&self) -> &'static str {
        match self {
            Status::Moved => "moved",
            Status::Merged => "merged",
            Status::Parked => "parked",
            Status::Deleted => "deleted",
            Status::Absent => "absent",
            Status::Pending(_) => "pending",
            Status::Refused(_) => "refused",
        }
    }

    fn detail(&self) -> Option<&str> {
        match self {
            Status::Pending(d) | Status::Refused(d) => Some(d),
            _ => None,
        }
    }

    fn is_pending(&self) -> bool {
        matches!(self, Status::Pending(_))
    }
}

#[derive(Debug)]
pub struct Entry {
    pub legacy: String,
    pub status: Status,
}

#[derive(Debug, Default)]
pub struct Receipt {
    pub entries: Vec<Entry>,
}

impl Receipt {
    /// Entries still owed work: a dry run of movable rows, a later-wave kind,
    /// a mux-owned row waiting for its server, a lock waiting for its data
    /// file. The close probe reads this: `.pending == 0`.
    pub fn pending_count(&self) -> usize {
        self.entries
            .iter()
            .filter(|e| e.status.is_pending())
            .count()
    }

    pub fn refused_count(&self) -> usize {
        self.entries
            .iter()
            .filter(|e| matches!(e.status, Status::Refused(_)))
            .count()
    }

    fn count_matching(&self, pred: fn(&Status) -> bool) -> usize {
        self.entries.iter().filter(|e| pred(&e.status)).count()
    }

    pub fn summary(&self) -> String {
        format!(
            "{} moved, {} merged, {} parked, {} deleted, {} pending, {} refused, {} absent",
            self.count_matching(|s| matches!(s, Status::Moved)),
            self.count_matching(|s| matches!(s, Status::Merged)),
            self.count_matching(|s| matches!(s, Status::Parked)),
            self.count_matching(|s| matches!(s, Status::Deleted)),
            self.pending_count(),
            self.refused_count(),
            self.count_matching(|s| matches!(s, Status::Absent)),
        )
    }
}

/// One migration pass over the whole table against one state root.
/// Dry run (default) reports every owed row `pending`; `apply` moves,
/// merges, parks and deletes per kind. Idempotent: a second apply over a
/// migrated root reports moved/absent and parks nothing new.
pub fn migrate(root: &Path, apply: bool) -> Receipt {
    let stamp = utc_stamp();
    let mut receipt = Receipt::default();
    // The sqlite family's deferred verify: stamps parked on earlier passes
    // get their parked copies compared against their recorded counts here.
    for (name, status) in crate::state_layout_sqlite::verify_sweep(root, &stamp) {
        receipt.entries.push(Entry {
            legacy: format!("{name} (verify)"),
            status,
        });
    }
    // A stale writer's reappeared legacy event-family store imports into the
    // published store, then parks. Dry runs skip it.
    for (name, status) in crate::state_layout_sqlite::straggler_import(root, &stamp, apply) {
        receipt.entries.push(Entry {
            legacy: format!("{name} (straggler)"),
            status,
        });
    }
    // The retired root journal family: a reappeared `events.jsonl*` imports
    // once and parks whole, and the receipt row carries `repeat: true`.
    for (name, status) in crate::state_layout_sqlite::journal_retire(root, &stamp, apply) {
        receipt.entries.push(Entry {
            legacy: format!("{name} (journal)"),
            status,
        });
    }
    for entry in migrate_rows(root, rows(), apply, &stamp) {
        receipt.entries.push(entry);
    }
    for entry in park_nested_duplicates(root, apply, &stamp) {
        receipt.entries.push(entry);
    }
    receipt
}

/// The per-row pass over any table: the shipped rows for `migrate`, a
/// synthetic table for the tests. Glob and plain park rows need the state
/// root's own child scan; everything else dispatches on kind.
fn migrate_rows(root: &Path, table: &[Row], apply: bool, stamp: &str) -> Vec<Entry> {
    let mut out = Vec::new();
    for row in table {
        match row.kind {
            // Glob rows: every root entry the pattern matches parks.
            Kind::Park if row.legacy.contains('*') => {
                let names = glob_matches(root, &row.legacy);
                if names.is_empty() {
                    continue;
                }
                // The mux server owns its residue's timing (change 3.1); the
                // daemon never parks mux rows, glob or not.
                if row.owner == Owner::Mux {
                    out.push(Entry {
                        legacy: row.legacy.to_string(),
                        status: Status::Pending(
                            "owner mux (the mux server parks it at its start)".to_string(),
                        ),
                    });
                    continue;
                }
                for name in names {
                    let status = if apply {
                        park_entry(root, &stamp, &name)
                    } else {
                        Status::Pending("park (dry run)".to_string())
                    };
                    out.push(Entry {
                        legacy: name,
                        status,
                    });
                }
            }
            Kind::Park => {
                let status = if !root.join(&row.legacy).exists() {
                    Status::Absent
                } else if apply {
                    park_entry(root, &stamp, &row.legacy)
                } else {
                    Status::Pending("park (dry run)".to_string())
                };
                out.push(Entry {
                    legacy: row.legacy.to_string(),
                    status,
                });
            }
            _ => {
                let status = migrate_entry(root, row, apply, stamp);
                out.push(Entry {
                    legacy: row.legacy.to_string(),
                    status,
                });
            }
        }
    }
    out
}

fn migrate_entry(root: &Path, row: &Row, apply: bool, stamp: &str) -> Status {
    if row.kind == Kind::Lock {
        return migrate_lock(root, row, apply, stamp);
    }
    if row.kind == Kind::Dir {
        return migrate_dir(root, row, apply, stamp);
    }
    let legacy = root.join(&row.legacy);
    let new = root.join(&row.new);
    let legacy_exists = legacy.exists();
    let new_exists = new.exists();
    if !legacy_exists && !new_exists {
        return Status::Absent;
    }
    // Already migrated: the new path holds the file, nothing at the legacy
    // path owes a move.
    if !legacy_exists && new_exists {
        return Status::Moved;
    }
    // Skipped families. Their legacy file EXISTS here, so the work is real
    // and reads pending: the mux server moves its own rows at its start
    // (change 3.1).
    if row.owner == Owner::Mux {
        return Status::Pending("owner mux (the mux server moves it at its start)".to_string());
    }
    if row.kind == Kind::Sqlite {
        return crate::state_layout_sqlite::migrate_sqlite_row(root, row, apply, stamp);
    }
    if !apply {
        return Status::Pending(match row.kind {
            Kind::Append if legacy_exists && new_exists => "append merge (dry run)".to_string(),
            _ => "move (dry run)".to_string(),
        });
    }
    match row.kind {
        // New wins; the legacy copy parks.
        Kind::Marker | Kind::Page if legacy_exists && new_exists => park_file(root, stamp, &legacy),
        Kind::Overwrite if legacy_exists && new_exists => {
            let legacy_newer = mtime_of(&legacy) > mtime_of(&new);
            if legacy_newer {
                if std::fs::rename(&legacy, &new).is_err() {
                    return Status::Refused("rename over new failed".to_string());
                }
                Status::Moved
            } else {
                park_file(root, stamp, &legacy)
            }
        }
        // Rename keeps open writers on the same inode (launchd fds), so an
        // append merge must copy bytes, never re-open through the old name.
        Kind::Append if legacy_exists && new_exists => match append_merge(&legacy, &new) {
            Ok(()) => match park_file(root, stamp, &legacy) {
                Status::Parked => Status::Merged,
                other => other,
            },
            Err(e) => Status::Refused(format!("merge failed: {e}")),
        },
        _ => match move_entry(&legacy, &new) {
            Ok(()) => Status::Moved,
            Err(e) => Status::Refused(format!("refused: {e}")),
        },
    }
}

/// A legacy lock leaves only after its data file moved (so no waiter still
/// needs it) and a non-blocking flock succeeds (so no holder still owns it).
/// A socket never flocks; it parks once its store moved.
///
/// "Moved" is kind-aware. An `anchor` base (`graph.json.lock` waits on
/// `graph.json`) is virtual, so its probe is the `.db` twin at the new
/// location - the same probe `place` uses. Otherwise the data has moved when
/// the new path exists OR the legacy base is gone: a lock whose data file no
/// longer exists at either spelling protects nothing, and the flock below
/// still refuses while a live holder owns it. A base the table does not
/// carry (`graph-archive.json.store.sock`) reads the same way.
fn migrate_lock(root: &Path, row: &Row, apply: bool, stamp: &str) -> Status {
    let legacy = root.join(&row.legacy);
    if !legacy.exists() {
        return Status::Absent;
    }
    let base = row.legacy.strip_suffix(".lock").unwrap_or(&row.legacy);
    let legacy_base_exists = root.join(base).exists();
    let data_moved = match find(base) {
        Some(base_row) if base_row.kind == Kind::Anchor => {
            db_twin(&root.join(&base_row.new)).exists()
        }
        Some(base_row) => root.join(&base_row.new).exists() || !legacy_base_exists,
        None => !legacy_base_exists,
    };
    if !data_moved {
        return Status::Pending(format!("waiting for {base} to move"));
    }
    if !apply {
        return Status::Pending("lock removal (dry run)".to_string());
    }
    let is_socket = std::fs::symlink_metadata(&legacy)
        .map(|m| m.file_type().is_socket())
        .unwrap_or(false);
    if is_socket {
        return park_file(root, stamp, &legacy);
    }
    match std::fs::File::open(&legacy) {
        Ok(f) => {
            let rc = unsafe {
                libc::flock(
                    std::os::unix::io::AsRawFd::as_raw_fd(&f),
                    libc::LOCK_EX | libc::LOCK_NB,
                )
            };
            if rc != 0 {
                return Status::Pending("lock held".to_string());
            }
            match std::fs::remove_file(&legacy) {
                Ok(()) => Status::Deleted,
                Err(e) => Status::Refused(format!("refused: {e}")),
            }
        }
        Err(e) => Status::Refused(format!("refused: {e}")),
    }
}

/// One `dir` row's fold: each child of the legacy folder renames into the
/// new one when the name is free, a clashing child parks (the new side
/// wins), and the legacy folder is removed only once it is empty. A
/// self-mapped row (the shipped state until a group slice retargets it)
/// declares the folder and owes nothing. Runs again on every daemon start,
/// so a straggler a stale writer drops mid-pass folds in next time.
fn migrate_dir(root: &Path, row: &Row, apply: bool, stamp: &str) -> Status {
    if row.legacy == row.new {
        return Status::Absent;
    }
    if row.owner == Owner::Mux {
        return Status::Pending("owner mux (the mux server folds it at its start)".to_string());
    }
    let legacy = root.join(&row.legacy);
    let new = root.join(&row.new);
    if !legacy.exists() {
        return if new.exists() {
            Status::Moved
        } else {
            Status::Absent
        };
    }
    // A drain row never moves a child: a recorded absolute path a registry
    // row still names must stay put until its prune. Only the emptied shell
    // goes.
    if DRAIN_ONLY_FOLDERS.contains(&row.legacy.as_str()) {
        let emptied = std::fs::read_dir(&legacy)
            .map(|mut d| d.next().is_none())
            .unwrap_or(false);
        if !emptied {
            return Status::Pending("drains: recorded paths stay until pruned".to_string());
        }
        if !apply {
            return Status::Pending("fold (dry run)".to_string());
        }
        return match std::fs::remove_dir(&legacy) {
            Ok(()) => Status::Moved,
            Err(e) => Status::Refused(format!("refused: {e}")),
        };
    }
    let mut held_locks: Vec<std::fs::File> = Vec::new();
    if LOCK_BEARING_FOLDERS.contains(&row.legacy.as_str())
        && !dir_children_lockable(&legacy, &mut held_locks)
    {
        return Status::Pending("a child lock is held".to_string());
    }
    // A claim-aux lane dir belongs to a live waiter that polls its path, and
    // every claim-aux child is a dir, so the lock probe never sees a ticket.
    // The fold waits until every child dir is empty; the next daemon-start
    // pass retries.
    if row.legacy == "claim-aux" && !lane_dirs_empty(&legacy) {
        return Status::Pending("a waiter holds a ticket".to_string());
    }
    // The probe fds stay open through the fold: a lock another writer takes
    // between the probe and a rename cannot split a child from its holder.
    // The vec drops when the fold ends and releases them.
    if !apply {
        return Status::Pending("fold (dry run)".to_string());
    }
    // claim-aux folds whole, by lead ruling: a directory rename is atomic,
    // so no waiter's ticket path ever moves mid-write. The emptiness probe
    // re-runs right before the rename, and any lane holding a ticket skips
    // the row for this pass with place() still answering the legacy dir.
    // Only the destination's parent is created first; the rename carries
    // every child in one step.
    if row.legacy == "claim-aux" {
        if !lane_dirs_empty(&legacy) {
            return Status::Pending("a waiter holds a ticket".to_string());
        }
        match new.parent() {
            Some(parent) => {
                if let Err(e) = std::fs::create_dir_all(parent) {
                    return Status::Refused(format!("refused: {e}"));
                }
            }
            None => return Status::Refused("refused: no destination parent".to_string()),
        }
        if new.exists() {
            return Status::Pending("the destination already holds a claim-aux".to_string());
        }
        return match std::fs::rename(&legacy, &new) {
            Ok(()) => Status::Moved,
            Err(e) => Status::Refused(format!("refused: {e}")),
        };
    }
    if let Err(e) = std::fs::create_dir_all(&new) {
        return Status::Refused(format!("refused: {e}"));
    }
    let mut moved_any = false;
    if let Ok(children) = std::fs::read_dir(&legacy) {
        for child in children.flatten() {
            let target = new.join(child.file_name());
            if target.exists() {
                if matches!(park_file(root, stamp, &child.path()), Status::Parked) {
                    moved_any = true;
                }
            } else if std::fs::rename(child.path(), &target).is_ok() {
                moved_any = true;
            }
        }
    }
    let emptied = std::fs::read_dir(&legacy)
        .map(|mut d| d.next().is_none())
        .unwrap_or(false);
    if emptied {
        std::fs::remove_dir(&legacy).ok();
    }
    if moved_any {
        Status::Moved
    } else {
        Status::Absent
    }
}

/// Every regular-file child of `dir` takes a non-blocking exclusive flock,
/// and each taken `File` lands in `held` so the caller keeps the locks
/// until its fold ends. Subdirs and sockets are neither probes nor locks:
/// their writers fold by rename together with the folder, and an fd that
/// cannot open refuses the fold for this pass rather than guessing.
fn dir_children_lockable(dir: &Path, held: &mut Vec<std::fs::File>) -> bool {
    let Ok(children) = std::fs::read_dir(dir) else {
        return false;
    };
    for child in children.flatten() {
        let is_file = child.file_type().map(|t| t.is_file()).unwrap_or(false);
        if !is_file {
            continue;
        }
        match std::fs::OpenOptions::new().read(true).open(child.path()) {
            Ok(f) => {
                let rc = unsafe {
                    libc::flock(
                        std::os::unix::io::AsRawFd::as_raw_fd(&f),
                        libc::LOCK_EX | libc::LOCK_NB,
                    )
                };
                if rc != 0 {
                    return false;
                }
                held.push(f);
            }
            Err(_) => return false,
        }
    }
    true
}

/// Whether every child dir of `dir` is empty: the `claim-aux` fold guard.
/// Tickets live one level down (`<lane>/<NNNNNN>/holder`), a depth the lock
/// probe's file-only walk never reaches.
fn lane_dirs_empty(dir: &Path) -> bool {
    let Ok(children) = std::fs::read_dir(dir) else {
        return false;
    };
    for child in children.flatten() {
        if !child.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            continue;
        }
        let empty = std::fs::read_dir(child.path())
            .map(|mut d| d.next().is_none())
            .unwrap_or(false);
        if !empty {
            return false;
        }
    }
    true
}

/// The dead duplicate the table's root-level park rows cannot reach: the
/// pre-dedup `graph.db.*` rotation copies inside `backups/` (the one home
/// is `db/backups`). Root scan only sees the root's own children, so this
/// runs beside it. `agents/migrations` is deliberately absent: the role
/// migration writes that folder live (its receipt path derives from the
/// agents home's parent), so folding it into `migrations/` fights the
/// writer and re-fills every pass.
fn park_nested_duplicates(root: &Path, apply: bool, stamp: &str) -> Vec<Entry> {
    let mut out = Vec::new();
    let backups = root.join("backups");
    if backups.is_dir() {
        for name in glob_matches(&backups, "graph.db.*") {
            let path = backups.join(&name);
            let status = if apply {
                park_file(root, stamp, &path)
            } else {
                Status::Pending("park backups rotation (dry run)".to_string())
            };
            out.push(Entry {
                legacy: format!("backups/{name}"),
                status,
            });
        }
    }
    out
}

fn move_entry(legacy: &Path, new: &Path) -> std::io::Result<()> {
    if let Some(parent) = new.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::rename(legacy, new)
}

/// Legacy bytes then new bytes, through a tmp file in the new folder, fsync,
/// rename over the new file. The new file's mode carries over (same dir).
fn append_merge(legacy: &Path, new: &Path) -> std::io::Result<()> {
    let perms = std::fs::metadata(new)?.permissions();
    let tmp = new.with_extension(format!("migrating.{}", std::process::id()));
    let mut dst = std::fs::File::create(&tmp)?;
    // Streamed: the logs this merges are documented unbounded, and a full
    // read would spike the daemon's memory by both files' size.
    std::io::copy(&mut std::fs::File::open(legacy)?, &mut dst)?;
    std::io::copy(&mut std::fs::File::open(new)?, &mut dst)?;
    dst.sync_all()?;
    std::fs::set_permissions(&tmp, perms)?;
    std::fs::rename(&tmp, new)?;
    Ok(())
}

fn mtime_of(p: &Path) -> std::time::SystemTime {
    std::fs::metadata(p)
        .and_then(|m| m.modified())
        .unwrap_or(std::time::UNIX_EPOCH)
}

/// The run's backup folder: `backups/state-root-migration/<utc stamp>/`.
fn backup_dir(root: &Path, stamp: &str) -> PathBuf {
    root.join("backups")
        .join("state-root-migration")
        .join(stamp)
}

fn park_entry(root: &Path, stamp: &str, name: &str) -> Status {
    let p = root.join(name);
    if !p.exists() {
        return Status::Absent;
    }
    park_file(root, stamp, &p)
}

/// Rename one legacy path into the run's backup folder, returning the parked
/// path. Never deletes: a name collision in the same stamp gains `.1`, `.2`,
/// ... so a second pass over the same stamp never clobbers the first copy.
pub(crate) fn park_file_at(root: &Path, stamp: &str, legacy: &Path) -> Result<PathBuf, String> {
    let dir = backup_dir(root, stamp);
    let Some(name) = legacy.file_name() else {
        return Err("refused: no file name".to_string());
    };
    if let Err(e) = std::fs::create_dir_all(&dir) {
        return Err(format!("refused: {e}"));
    }
    let mut target = dir.join(name);
    let mut n = 0;
    while target.exists() {
        n += 1;
        let mut os = name.to_os_string();
        os.push(format!(".{n}"));
        target = dir.join(os);
    }
    std::fs::rename(legacy, &target)
        .map(|()| target)
        .map_err(|e| format!("refused: {e}"))
}

/// [`park_file_at`] as a [`Status`], for callers that only report the fold.
fn park_file(root: &Path, stamp: &str, legacy: &Path) -> Status {
    match park_file_at(root, stamp, legacy) {
        Ok(_) => Status::Parked,
        Err(e) => Status::Refused(e),
    }
}

/// Single-`*` glob match: prefix and suffix around the star. Enough for the
/// table's residue rows; no dependency.
fn glob_matches(root: &Path, pattern: &str) -> Vec<String> {
    let Some((prefix, suffix)) = pattern.split_once('*') else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let Ok(read) = std::fs::read_dir(root) else {
        return out;
    };
    for entry in read.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        if name.len() >= prefix.len() + suffix.len()
            && name.starts_with(prefix)
            && name.ends_with(suffix)
        {
            out.push(name.to_string());
        }
    }
    out.sort();
    out
}

fn utc_stamp() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    // Civil-from-days (Howard Hinnant's algorithm); no chrono dependency.
    let days = (now / 86_400) as i64;
    let secs = (now % 86_400) as i64;
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!(
        "{:04}{:02}{:02}T{:02}{:02}{:02}Z",
        y,
        m,
        d,
        secs / 3600,
        (secs % 3600) / 60,
        secs % 60
    )
}

/// The daemon bin calls this once at startup, BEFORE `daemon::run` opens a
/// store: one apply pass over the state root. Best effort - a refusal only
/// prints, and the daemon's reclaim lane retries it (law: the mover runs
/// from the daemon, never an operator verb).
pub fn run_at_daemon_start(home: &crate::paths::AgentsHome) {
    let root = crate::reclaim::reclaim_state_root(home);
    let receipt = migrate(&root, true);
    if receipt.refused_count() > 0 {
        eprintln!(
            "fno-agents-daemon: state-root migration: {}",
            receipt.summary()
        );
    }
}

/// `fno-agents state migrate [--apply] [--json|-J]`: dry run by default.
/// Resolves the state root the way every other state-root reader does.
pub fn run_migrate_cli(args: &[String]) -> i32 {
    let mut apply = false;
    let json = crate::json_output::requested(args);
    for arg in args {
        match arg.as_str() {
            "--apply" => apply = true,
            other if crate::json_output::is_flag(other) => {}
            other => {
                eprintln!("fno-agents state migrate: unknown arg: {other}");
                return 2;
            }
        }
    }
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let root = crate::agents_config::state_dir(&cwd).unwrap_or_else(|| {
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_default();
        home.join(".fno")
    });
    let receipt = migrate(&root, apply);
    if json {
        let entries: Vec<String> = receipt
            .entries
            .iter()
            .map(|e| {
                let detail = match e.status.detail() {
                    Some(d) => format!(",\"detail\":\"{}\"", json_escape(d)),
                    None => String::new(),
                };
                format!(
                    "{{\"legacy\":\"{}\",\"status\":\"{}\"{}}}",
                    json_escape(&e.legacy),
                    e.status.word(),
                    detail
                )
            })
            .collect();
        println!(
            "{{\"root\":\"{}\",\"pending\":{},\"entries\":[{}]}}",
            json_escape(&root.display().to_string()),
            receipt.pending_count(),
            entries.join(",")
        );
    } else {
        for e in &receipt.entries {
            match e.status.detail() {
                Some(d) => println!("  {}: {} ({})", e.status.word(), e.legacy, d),
                None if matches!(e.status, Status::Pending(_)) => {
                    println!("  pending: {}", e.legacy)
                }
                None => println!("  {}: {}", e.status.word(), e.legacy),
            }
        }
        println!(
            "state migrate ({}): {}",
            if apply { "applied" } else { "dry run" },
            receipt.summary()
        );
    }
    if receipt.refused_count() > 0 {
        1
    } else {
        0
    }
}

fn json_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    static COUNTER: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

    fn tmp_root(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "fno-state-layout-{tag}-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn clean(root: &Path) {
        std::fs::remove_dir_all(root).ok();
    }

    // The closed set of top-level destinations a table row may name. The
    // ruled categories (d-c74fda50) plus the park base; the two
    // transitional entries retire with the slices that fold them. A new
    // name is a new top-level folder on every machine: it needs the review
    // a new root row gets.
    #[test]
    fn top_level_set_is_closed() {
        let rows = parse_table(LAYOUT_TSV).expect("shipped table must parse");
        let transitional: Vec<&str> = TRANSITIONAL_TOP_LEVEL.iter().map(|(n, _)| *n).collect();
        for row in &rows {
            if row.kind == Kind::Park {
                assert_eq!(row.new, "-", "park row {} has no destination", row.legacy);
                continue;
            }
            let first = row.new.split('/').next().unwrap_or(&row.new);
            if row.kind == Kind::Dir {
                assert!(
                    FOLD_CANDIDATES.contains(&row.legacy.as_str()),
                    "dir row for {} is not a ruled fold candidate; widening FOLD_CANDIDATES mints a top-level folder on every machine",
                    row.legacy
                );
                assert!(
                    first == row.legacy.as_str() || APPROVED_TOP_LEVEL.contains(&first),
                    "dir row {} lands at unruled top level {first}; fold destinations are the ruled categories",
                    row.legacy
                );
                continue;
            }
            assert!(
                row.new.contains('/'),
                "file row {} must land in a subfolder",
                row.legacy
            );
            assert!(
                APPROVED_TOP_LEVEL.contains(&first) || transitional.contains(&first),
                "row {} names top-level {first} outside the ruled categories",
                row.legacy
            );
        }
        for (name, why) in TRANSITIONAL_TOP_LEVEL {
            assert!(
                rows.iter()
                    .any(|r| r.kind != Kind::Park && r.new.starts_with(&format!("{name}/"))),
                "transitional top-level {name} is unused ({why}); fold it and drop the entry"
            );
        }
        for name in FOLD_CANDIDATES {
            let row =
                find(name).unwrap_or_else(|| panic!("fold candidate {name} has no table row"));
            assert_eq!(row.kind, Kind::Dir, "{name} must carry a dir row");
        }
    }

    #[test]
    fn dir_row_folds_children_parks_the_clash_and_reads_both() {
        let root = tmp_root("dirfold");
        std::fs::create_dir_all(root.join("flight")).unwrap();
        std::fs::write(root.join("flight").join("a.json"), b"A").unwrap();
        std::fs::create_dir_all(root.join("state").join("flight")).unwrap();
        std::fs::write(root.join("state").join("flight").join("a.json"), b"NEW").unwrap();
        std::fs::write(root.join("state").join("flight").join("b.json"), b"B").unwrap();
        let rows = parse_table("flight\tstate/flight\tdir\tdaemon\n").unwrap();
        let dry = migrate_rows(&root, &rows, false, "test");
        assert!(
            dry.iter().all(|e| e.status.is_pending()),
            "a dry run of a retargeted dir row is pending"
        );
        assert!(
            root.join("flight").join("a.json").exists(),
            "a dry run moves nothing"
        );
        let receipt = migrate_rows(&root, &rows, true, "test");
        assert!(
            receipt.iter().all(|e| matches!(e.status, Status::Moved)),
            "the fold happened: {receipt:?}"
        );
        assert!(
            !root.join("flight").exists(),
            "the emptied legacy folder is removed"
        );
        assert_eq!(
            std::fs::read(root.join("state").join("flight").join("a.json")).unwrap(),
            b"NEW",
            "the new side wins the clash"
        );
        let parked = std::fs::read_dir(root.join("backups").join("state-root-migration"))
            .map(|d| d.count())
            .unwrap_or(0);
        assert!(parked >= 1, "the clashing legacy child parked");
        assert_eq!(
            std::fs::read(root.join("state").join("flight").join("b.json")).unwrap(),
            b"B"
        );
        // Readers follow the SHIPPED row: flight now folds under state/, so
        // place_dir answers the destination always, while the legacy dir
        // above still holds files. The synthetic fold must not leak into the
        // reader.
        assert_eq!(
            place_dir(&root, "flight"),
            Some(root.join("state").join("flight"))
        );
        clean(&root);

        // The pre-fold read: the file resolves through the legacy dir until
        // the mover folds it, and the merge-list read sees it too.
        let root = tmp_root("dirlegacy");
        std::fs::create_dir_all(root.join("flight")).unwrap();
        std::fs::write(root.join("flight").join("c.json"), b"C").unwrap();
        let rows = parse_table("flight\tstate/flight\tdir\tdaemon\n").unwrap();
        let _ = migrate_rows(&root, &rows, false, "test");
        assert_eq!(
            place_in(&root, "flight", "c.json"),
            root.join("flight").join("c.json")
        );
        let listed = list_dir_children(&root, "flight");
        assert_eq!(listed.len(), 1, "the list read sees the legacy child");
        clean(&root);
    }

    #[test]
    fn dir_row_waits_for_a_held_child_lock() {
        let root = tmp_root("dirlock");
        std::fs::create_dir_all(root.join("locks")).unwrap();
        let lock_file = root.join("locks").join("plan-abc.lock");
        std::fs::write(&lock_file, b"").unwrap();
        let rows = parse_table("locks\tstate/locks\tdir\tdaemon\n").unwrap();
        let held = std::fs::OpenOptions::new()
            .read(true)
            .open(&lock_file)
            .unwrap();
        let rc = unsafe {
            libc::flock(
                std::os::unix::io::AsRawFd::as_raw_fd(&held),
                libc::LOCK_EX | libc::LOCK_NB,
            )
        };
        assert_eq!(rc, 0, "test setup holds the lock");
        let receipt = migrate_rows(&root, &rows, true, "test");
        assert!(
            receipt
                .iter()
                .any(|e| matches!(e.status, Status::Pending(_))),
            "a held child lock keeps the row pending: {receipt:?}"
        );
        assert!(
            root.join("locks").join("plan-abc.lock").exists(),
            "nothing moved past a held lock"
        );
        drop(held);
        let receipt = migrate_rows(&root, &rows, true, "test");
        assert!(
            receipt.iter().any(|e| matches!(e.status, Status::Moved)),
            "the fold runs once the lock releases: {receipt:?}"
        );
        assert!(!root.join("locks").exists(), "the emptied folder left");
        assert!(
            root.join("state")
                .join("locks")
                .join("plan-abc.lock")
                .exists(),
            "the child landed at the new path"
        );
        clean(&root);
    }

    #[test]
    fn dir_row_folds_claim_aux_only_when_every_lane_dir_is_empty() {
        let root = tmp_root("claimaux");
        let lane = root
            .join("claim-aux")
            .join("build%3Acargo.lock.queue.d")
            .join("000001");
        std::fs::create_dir_all(&lane).unwrap();
        std::fs::write(lane.join("holder"), b"12345").unwrap();
        let rows = parse_table("claim-aux\tstate/claim-aux\tdir\tdaemon\n").unwrap();
        let receipt = migrate_rows(&root, &rows, true, "test");
        assert!(
            receipt
                .iter()
                .any(|e| matches!(e.status, Status::Pending(_))),
            "a lane dir holding a ticket keeps the row pending: {receipt:?}"
        );
        assert!(
            root.join("claim-aux")
                .join("build%3Acargo.lock.queue.d")
                .join("000001")
                .join("holder")
                .exists(),
            "nothing moved past a held ticket"
        );
        std::fs::remove_file(lane.join("holder")).unwrap();
        std::fs::remove_dir(lane).unwrap();
        let receipt = migrate_rows(&root, &rows, true, "test");
        assert!(
            receipt.iter().any(|e| matches!(e.status, Status::Moved)),
            "an empty lane dir folds: {receipt:?}"
        );
        assert!(!root.join("claim-aux").exists(), "the emptied folder left");
        assert!(
            root.join("state").join("claim-aux").exists(),
            "the children landed at the new path"
        );
        clean(&root);
    }

    #[test]
    fn the_root_event_journal_imports_then_parks_with_a_receipt() {
        let root = tmp_root("journal-retire");
        let _guard = crate::claims::test_env_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let prior_home = std::env::var_os("FNO_AGENTS_HOME");
        std::env::set_var("FNO_AGENTS_HOME", root.join("agents"));
        let root = std::fs::canonicalize(&root).unwrap();
        let ts = chrono::Utc::now().to_rfc3339();
        std::fs::write(
            root.join("events.jsonl"),
            format!(
                "{{\"ts\":\"{ts}\",\"type\":\"retire_probe\",\"source\":\"daemon\",\"data\":{{\"marker\":\"live\"}}}}\n"
            ),
        )
        .unwrap();
        std::fs::write(
            root.join("events.jsonl.1"),
            format!(
                "{{\"ts\":\"{ts}\",\"type\":\"retire_probe\",\"source\":\"daemon\",\"data\":{{\"marker\":\"rotated\"}}}}\n"
            ),
        )
        .unwrap();
        std::fs::write(root.join("events.jsonl.lock"), b"").unwrap();

        let receipt = migrate(&root, true);
        let journal_entries: Vec<_> = receipt
            .entries
            .iter()
            .filter(|e| e.legacy.contains("(journal)"))
            .collect();
        assert_eq!(journal_entries.len(), 2, "{receipt:?}");
        assert!(
            journal_entries
                .iter()
                .all(|e| matches!(e.status, Status::Merged)),
            "{receipt:?}"
        );
        let journal = root.join("events.jsonl");
        let rows = crate::event_store::query_events(
            &journal,
            &crate::event_store::EventQuery::of_types(&["retire_probe"]),
        )
        .unwrap();
        assert_eq!(rows.len(), 2, "both seeded rows committed");
        assert!(
            !journal.exists() && !root.join("events.jsonl.1").exists(),
            "the family left the root"
        );
        assert!(
            !root.join("events.jsonl.lock").exists(),
            "the lock parked beside the family"
        );
        let mut stamps = std::fs::read_dir(root.join("backups").join("state-root-migration"))
            .unwrap()
            .flatten();
        let stamp_dir = stamps.next().expect("one parked stamp folder").path();
        assert!(
            stamp_dir.join("events.jsonl").exists() && stamp_dir.join("events.jsonl.1").exists(),
            "the parked copies survive"
        );
        let receipts = crate::event_store::query_events(
            &journal,
            &crate::event_store::EventQuery::of_types(&["legacy_journal_retired"]),
        )
        .unwrap();
        assert_eq!(receipts.len(), 1, "one receipt row");
        assert!(
            serde_json::from_str::<serde_json::Value>(&receipts[0].line)
                .unwrap()
                .pointer("/data/repeat")
                == Some(&serde_json::Value::Bool(false)),
            "the first retire reads repeat false"
        );

        // A recreated file is a repeat: the new bytes ingest, the file parks
        // again, and the receipt says so. A root with no family file reports
        // no journal entry at all.
        std::fs::write(
            &journal,
            format!(
                "{{\"ts\":\"{ts}\",\"type\":\"retire_probe\",\"source\":\"daemon\",\"data\":{{\"marker\":\"again\"}}}}\n"
            ),
        )
        .unwrap();
        let receipt = migrate(&root, true);
        assert!(
            receipt
                .entries
                .iter()
                .any(|e| e.legacy.contains("(journal)")),
            "{receipt:?}"
        );
        let receipts = crate::event_store::query_events(
            &journal,
            &crate::event_store::EventQuery::of_types(&["legacy_journal_retired"]),
        )
        .unwrap();
        assert_eq!(receipts.len(), 2, "the repeat retires once more");
        assert!(
            serde_json::from_str::<serde_json::Value>(&receipts[1].line)
                .unwrap()
                .pointer("/data/repeat")
                == Some(&serde_json::Value::Bool(true)),
            "the recreated file retires as a repeat"
        );
        let quiet = migrate(&root, true);
        assert!(
            !quiet.entries.iter().any(|e| e.legacy.contains("(journal)")),
            "a root with no family file reports no journal entry"
        );
        match prior_home {
            Some(v) => std::env::set_var("FNO_AGENTS_HOME", v),
            None => std::env::remove_var("FNO_AGENTS_HOME"),
        }
        clean(&root);
    }

    #[test]
    fn a_second_pass_over_the_same_stamp_parks_beside_the_first_copy() {
        let root = tmp_root("journal-collision");
        let _guard = crate::claims::test_env_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let prior_home = std::env::var_os("FNO_AGENTS_HOME");
        std::env::set_var("FNO_AGENTS_HOME", root.join("agents"));
        let root = std::fs::canonicalize(&root).unwrap();
        let ts = chrono::Utc::now().to_rfc3339();
        let seed = |marker: &str| {
            format!(
                "{{\"ts\":\"{ts}\",\"type\":\"retire_probe\",\"source\":\"daemon\",\"data\":{{\"marker\":\"{marker}\"}}}}\n"
            )
        };
        std::fs::write(root.join("events.jsonl"), seed("first")).unwrap();
        // One stamp, two passes inside its one-second resolution: the second
        // copy lands beside the first, never on it.
        let first = crate::state_layout_sqlite::journal_retire(&root, "collide", true);
        assert!(
            first
                .iter()
                .any(|(n, s)| n == "events.jsonl" && matches!(s, Status::Merged)),
            "{first:?}"
        );
        let parked = root
            .join("backups")
            .join("state-root-migration")
            .join("collide");
        assert_eq!(
            std::fs::read_to_string(parked.join("events.jsonl")).unwrap(),
            seed("first"),
            "the first copy is in place"
        );
        std::fs::write(root.join("events.jsonl"), seed("second")).unwrap();
        let second = crate::state_layout_sqlite::journal_retire(&root, "collide", true);
        assert!(
            second
                .iter()
                .any(|(n, s)| n == "events.jsonl" && matches!(s, Status::Merged)),
            "{second:?}"
        );
        assert_eq!(
            std::fs::read_to_string(parked.join("events.jsonl")).unwrap(),
            seed("first"),
            "the collision suffix kept the first copy"
        );
        assert_eq!(
            std::fs::read_to_string(parked.join("events.jsonl.1")).unwrap(),
            seed("second"),
            "the second copy parked beside it"
        );
        match prior_home {
            Some(v) => std::env::set_var("FNO_AGENTS_HOME", v),
            None => std::env::remove_var("FNO_AGENTS_HOME"),
        }
        clean(&root);
    }

    #[test]
    fn dir_resolution_helpers_and_self_rows() {
        let root = tmp_root("dirhelp");
        assert_eq!(
            place_dir(&root, "flight"),
            Some(root.join("state").join("flight")),
            "a shipped dir row answers at its destination"
        );
        assert_eq!(
            place_dir(&root, "no-such-folder"),
            None,
            "an unknown name is refused, never a silent root join"
        );
        assert_eq!(
            place_dir(&root, "graph.db"),
            None,
            "a file-only name is not a dir"
        );
        // A self-mapped row declares the folder and owes nothing.
        let rows = parse_table("retro-pending\tretro-pending\tdir\tdaemon\n").unwrap();
        std::fs::create_dir_all(root.join("retro-pending")).unwrap();
        let receipt = migrate_rows(&root, &rows, true, "test");
        assert!(
            receipt.iter().all(|e| matches!(e.status, Status::Absent)),
            "a self-mapped row is absent: {receipt:?}"
        );
        assert!(root.join("retro-pending").is_dir());
        clean(&root);
    }

    #[test]
    fn drain_only_route_settings_never_moves_a_child() {
        // route-settings drains instead of folding: each file's absolute
        // path is recorded in a registry row and a relaunch refuses a moved
        // file, so the mover waits out the prune and removes only the empty
        // shell.
        let root = tmp_root("drain");
        let rows = parse_table("route-settings\tstate/route-settings\tdir\tdaemon\n").unwrap();
        std::fs::create_dir_all(root.join("route-settings")).unwrap();
        std::fs::write(root.join("route-settings").join("cc44.json"), b"{}").unwrap();
        let receipt = migrate_rows(&root, &rows, true, "test");
        let entry = receipt
            .iter()
            .find(|e| e.legacy == "route-settings")
            .unwrap();
        assert!(
            matches!(entry.status, Status::Pending(_)),
            "a recorded file pins the row: {entry:?}"
        );
        assert!(
            root.join("route-settings").join("cc44.json").exists(),
            "the recorded file never moves"
        );
        // The prune empties the legacy folder; the next pass removes it.
        std::fs::remove_file(root.join("route-settings").join("cc44.json")).unwrap();
        let receipt = migrate_rows(&root, &rows, true, "test");
        let entry = receipt
            .iter()
            .find(|e| e.legacy == "route-settings")
            .unwrap();
        assert!(matches!(entry.status, Status::Moved), "{entry:?}");
        assert!(!root.join("route-settings").exists(), "the shell is gone");
        clean(&root);
    }

    #[test]
    fn place_ac2() {
        let root = tmp_root("place");
        assert_eq!(
            place(&root, "graph.json"),
            root.join("db").join("graph.json")
        );
        std::fs::write(root.join("graph.db"), b"x").unwrap();
        assert_eq!(place(&root, "graph.json"), root.join("graph.json"));
        std::fs::create_dir_all(root.join("db")).unwrap();
        std::fs::write(root.join("db").join("graph.db"), b"x").unwrap();
        assert_eq!(
            place(&root, "graph.json"),
            root.join("db").join("graph.json")
        );
        // A table miss resolves through the root unchanged.
        assert_eq!(place(&root, "other.json"), root.join("other.json"));
        clean(&root);
    }

    #[test]
    fn overwrite_newer_mtime_wins() {
        // New is newer: legacy parks, new bytes stay.
        let root = tmp_root("ow-new");
        std::fs::create_dir_all(root.join("state")).unwrap();
        std::fs::write(root.join("notify-signals.json"), b"legacy").unwrap();
        let new_path = root.join("state").join("notify-signals.json");
        std::fs::write(&new_path, b"new").unwrap();
        let newer = std::time::SystemTime::now() + std::time::Duration::from_secs(90);
        set_mtime(&new_path, newer);
        let receipt = migrate(&root, true);
        assert_eq!(std::fs::read(&new_path).unwrap(), b"new");
        assert!(!root.join("notify-signals.json").exists());
        let found = receipt
            .entries
            .iter()
            .find(|e| e.legacy == "notify-signals.json")
            .unwrap();
        assert!(matches!(found.status, Status::Parked));
        clean(&root);
        // Legacy is newer: its bytes land on the new path.
        let root = tmp_root("ow-legacy");
        std::fs::create_dir_all(root.join("state")).unwrap();
        std::fs::write(root.join("notify-signals.json"), b"legacy").unwrap();
        let new_path = root.join("state").join("notify-signals.json");
        std::fs::write(&new_path, b"new").unwrap();
        let newer = std::time::SystemTime::now() + std::time::Duration::from_secs(90);
        set_mtime(root.join("notify-signals.json").as_path(), newer);
        let _ = migrate(&root, true);
        assert_eq!(std::fs::read(&new_path).unwrap(), b"legacy");
        clean(&root);
        // The sqlite kind rides the same move rules through the backup-API
        // protocol (state_layout_sqlite). Branch: committed rows still in
        // the -wal land in the published copy and the trio parks.
        let root = tmp_root("ow-sq-wal");
        let legacy = root.join("graph.db");
        let conn = rusqlite::Connection::open(&legacy).unwrap();
        conn.execute_batch(
            "PRAGMA journal_mode=WAL;
             CREATE TABLE layout_nodes (id TEXT PRIMARY KEY, title TEXT);
             INSERT INTO layout_nodes VALUES ('x-1','one');",
        )
        .unwrap();
        let row = parse_table("graph.db\tdb/graph.db\tsqlite\tdaemon\n")
            .unwrap()
            .remove(0);
        let status = crate::state_layout_sqlite::migrate_sqlite_row(&root, &row, true, "ow-wal");
        assert!(matches!(status, Status::Moved), "{status:?}");
        let new_db = root.join("db").join("graph.db");
        let c = rusqlite::Connection::open(&new_db).unwrap();
        let n: i64 = c
            .query_row("SELECT COUNT(*) FROM layout_nodes", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 1, "the committed row moved");
        assert!(!legacy.exists(), "the legacy file parked");
        assert!(root
            .join("backups")
            .join("state-root-migration")
            .join("ow-wal")
            .join("graph.db")
            .exists());
        drop(c);
        clean(&root);
        // Branch: a writer holding BEGIN IMMEDIATE past the busy timeout
        // parks the row, leaves the trio untouched, and clears the fence.
        let root = tmp_root("ow-sq-busy");
        let legacy = root.join("graph.db");
        let conn = rusqlite::Connection::open(&legacy).unwrap();
        conn.execute_batch("CREATE TABLE t (a); INSERT INTO t VALUES (1);")
            .unwrap();
        conn.execute_batch("BEGIN IMMEDIATE;").unwrap();
        let status = crate::state_layout_sqlite::migrate_sqlite_row(&root, &row, true, "ow-busy");
        assert!(
            matches!(status, Status::Pending(_)),
            "expected pending, got {status:?}"
        );
        assert!(legacy.exists(), "the legacy trio is untouched");
        assert!(!crate::state_layout_sqlite::fence_path(&root).exists());
        conn.execute_batch("ROLLBACK;").unwrap();
        drop(conn);
        clean(&root);
        // Branch: rows on both sides refuse and keep both copies.
        let root = tmp_root("ow-sq-both");
        let legacy = root.join("graph.db");
        let conn = rusqlite::Connection::open(&legacy).unwrap();
        conn.execute_batch("CREATE TABLE t (a); INSERT INTO t VALUES (1);")
            .unwrap();
        drop(conn);
        std::fs::create_dir_all(root.join("db")).unwrap();
        let new_db = root.join("db").join("graph.db");
        let conn = rusqlite::Connection::open(&new_db).unwrap();
        conn.execute_batch("CREATE TABLE t (a); INSERT INTO t VALUES (2);")
            .unwrap();
        drop(conn);
        let status = crate::state_layout_sqlite::migrate_sqlite_row(&root, &row, true, "ow-both");
        assert!(matches!(status, Status::Refused(_)), "{status:?}");
        assert!(legacy.exists() && new_db.exists(), "both copies stay");
        clean(&root);
        // Branch: an empty new copy is a fresh-create race; it parks and the
        // legacy rows win.
        let root = tmp_root("ow-sq-empty");
        let legacy = root.join("graph.db");
        let conn = rusqlite::Connection::open(&legacy).unwrap();
        conn.execute_batch("CREATE TABLE t (a); INSERT INTO t VALUES (1);")
            .unwrap();
        drop(conn);
        std::fs::create_dir_all(root.join("db")).unwrap();
        let new_db = root.join("db").join("graph.db");
        rusqlite::Connection::open(&new_db)
            .unwrap()
            .execute_batch("CREATE TABLE z (a);")
            .unwrap();
        let status = crate::state_layout_sqlite::migrate_sqlite_row(&root, &row, true, "ow-empty");
        assert!(matches!(status, Status::Moved), "{status:?}");
        let c = rusqlite::Connection::open(&new_db).unwrap();
        let n: i64 = c
            .query_row("SELECT COUNT(*) FROM t", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 1, "the legacy rows won");
        drop(c);
        // Branch: the deferred verify compares a parked copy against its
        // park-time counts once the record ages past the delay. Same root:
        // the ow-empty stamp's record is still in place.
        let record = root
            .join("backups")
            .join("state-root-migration")
            .join("ow-empty")
            .join("verify.tsv");
        let f = std::fs::OpenOptions::new()
            .write(true)
            .open(&record)
            .unwrap();
        f.set_times(
            std::fs::FileTimes::new()
                .set_modified(std::time::SystemTime::now() - std::time::Duration::from_secs(60)),
        )
        .unwrap();
        drop(f);
        let swept = crate::state_layout_sqlite::verify_sweep(&root, "ow-later");
        assert_eq!(swept.len(), 1, "{swept:?}");
        assert!(matches!(swept[0].1, Status::Moved), "{swept:?}");
        // Branch: a reappeared legacy event-family store imports by
        // event_id into the published store, then parks.
        let published = root.join("db").join("events.db");
        let mut c = rusqlite::Connection::open(&published).unwrap();
        crate::event_store::ensure_schema(&mut c, &published).unwrap();
        c.execute(
            "INSERT INTO events (seq, event_id, row_hash, ts_ms, type, source, line) VALUES (1, 'e-1', x'01', 0, 'x', 't', '{}'), (2, 'destination-only', x'03', 0, 'x', 't', '{}')",
            [],
        )
        .unwrap();
        drop(c);
        let straggler = root.join("events.db");
        let mut c = rusqlite::Connection::open(&straggler).unwrap();
        crate::event_store::ensure_schema(&mut c, &straggler).unwrap();
        c.execute_batch(
            "INSERT INTO events (seq, event_id, row_hash, ts_ms, type, source, line) VALUES (1, 'e-1', x'01', 0, 'x', 't', '{}'), (2, 'e-2', x'02', 0, 'x', 't', '{}');",
        )
        .unwrap();
        drop(c);
        let receipt = migrate(&root, true);
        let merged: Vec<_> = receipt
            .entries
            .iter()
            .filter(|e| e.legacy.contains("straggler"))
            .collect();
        assert_eq!(merged.len(), 1, "{merged:?}");
        assert!(matches!(merged[0].status, Status::Merged), "{merged:?}");
        let c = rusqlite::Connection::open(&published).unwrap();
        let n: i64 = c
            .query_row("SELECT COUNT(*) FROM events", [], |r| r.get(0))
            .unwrap();
        assert_eq!(
            n, 3,
            "distinct event identities survive sequence collisions"
        );
        let source_rows: i64 = c
            .query_row(
                "SELECT COUNT(*) FROM events WHERE event_id = 'e-2'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(source_rows, 1, "the colliding source event survives once");
        assert!(!straggler.exists(), "the straggler parked");
        let mut source = rusqlite::Connection::open(&straggler).unwrap();
        crate::event_store::ensure_schema(&mut source, &straggler).unwrap();
        source.execute_batch(
            "INSERT INTO events (seq, event_id, row_hash, ts_ms, type, source, line) VALUES (1, 'distinct-conflict', x'03', 0, 'x', 't', '{}');",
        ).unwrap();
        drop(source);
        let refused = migrate(&root, true);
        assert!(
            refused
                .entries
                .iter()
                .any(|e| e.legacy.contains("straggler") && matches!(e.status, Status::Refused(_))),
            "{refused:?}"
        );
        assert!(
            straggler.exists(),
            "a distinct event is retained on an integrity conflict"
        );
        clean(&root);
        // Branch: a zero-length straggler beside a published store parks
        // without a schema write and reads Parked.
        let root = tmp_root("ow-straggler-empty");
        std::fs::create_dir_all(root.join("db")).unwrap();
        let published = root.join("db").join("events.db");
        let mut c = rusqlite::Connection::open(&published).unwrap();
        crate::event_store::ensure_schema(&mut c, &published).unwrap();
        c.execute(
            "INSERT INTO events (seq, event_id, row_hash, ts_ms, type, source, line) VALUES (1, 'e-1', x'01', 0, 'x', 't', '{}')",
            [],
        )
        .unwrap();
        drop(c);
        std::fs::write(root.join("events.db"), b"").unwrap();
        let receipt = migrate(&root, true);
        let parked: Vec<_> = receipt
            .entries
            .iter()
            .filter(|e| e.legacy.contains("straggler"))
            .collect();
        assert_eq!(parked.len(), 1, "{parked:?}");
        assert!(matches!(parked[0].status, Status::Parked), "{parked:?}");
        assert!(
            !root.join("events.db").exists(),
            "the empty straggler left the root"
        );
        let parked_copy = std::fs::read_dir(root.join("backups").join("state-root-migration"))
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.path().join("events.db"))
            .find(|p| p.exists())
            .expect("the empty straggler parked in a stamp folder");
        assert_eq!(
            std::fs::metadata(&parked_copy).unwrap().len(),
            0,
            "the parked copy is still zero length"
        );
        clean(&root);
        // Branch: a zero-length straggler with a live -wal is not parked
        // blind. The guard routes it down the import path, so the entry
        // reads Merged (the import's park), never the blind Parked. Whether
        // the WAL's frames replay is the import's contract, pinned by the
        // ow-straggler-order branch; the verdict here is the routing.
        let root = tmp_root("ow-straggler-wal");
        std::fs::create_dir_all(root.join("db")).unwrap();
        let published = root.join("db").join("events.db");
        let mut c = rusqlite::Connection::open(&published).unwrap();
        crate::event_store::ensure_schema(&mut c, &published).unwrap();
        drop(c);
        let full = root.join("events-full.db");
        let mut conn = rusqlite::Connection::open(&full).unwrap();
        conn.execute_batch(
            "PRAGMA journal_mode=WAL;
             PRAGMA wal_autocheckpoint=0;",
        )
        .unwrap();
        crate::event_store::ensure_schema(&mut conn, &full).unwrap();
        conn.execute(
            "INSERT INTO events (seq, event_id, row_hash, ts_ms, type, source, line) VALUES (1, 'e-9', x'09', 0, 'x', 't', '{}')",
            [],
        )
        .unwrap();
        std::fs::copy(root.join("events-full.db-wal"), root.join("events.db-wal")).unwrap();
        drop(conn);
        std::fs::write(root.join("events.db"), b"").unwrap();
        let receipt = migrate(&root, true);
        let wal_row = receipt
            .entries
            .iter()
            .find(|e| e.legacy.contains("straggler"))
            .expect("the straggler row reads");
        assert!(
            matches!(wal_row.status, Status::Merged),
            "a -wal-bearing straggler routes through the import, not the blind park: {wal_row:?}"
        );
        assert!(
            !root.join("events.db").exists(),
            "the import path left the root clean"
        );
        clean(&root);
        // Branch: a published store whose columns came out of ALTER in
        // another order still imports: the compare is set-wise now.
        let root = tmp_root("ow-straggler-order");
        std::fs::create_dir_all(root.join("db")).unwrap();
        let published = root.join("db").join("events.db");
        let c = rusqlite::Connection::open(&published).unwrap();
        c.execute_batch(
            "CREATE TABLE events (\
                 seq INTEGER PRIMARY KEY, \
                 event_id TEXT UNIQUE NOT NULL, \
                 row_hash BLOB UNIQUE NOT NULL, \
                 ts_ms INTEGER NOT NULL, \
                 type TEXT NOT NULL, \
                 source TEXT NOT NULL, \
                 scope TEXT, \
                 retention_class TEXT NOT NULL DEFAULT 'durable', \
                 session_id TEXT, \
                 node_id TEXT, \
                 pr_number INTEGER, \
                 head_sha TEXT, \
                 repo TEXT, \
                 reject_reason TEXT, \
                 line TEXT NOT NULL, \
                 caused_by TEXT);",
        )
        .unwrap();
        c.execute(
            "INSERT INTO events (seq, event_id, row_hash, ts_ms, type, source, line) VALUES (1, 'dst-1', x'01', 0, 'x', 't', '{}')",
            [],
        )
        .unwrap();
        drop(c);
        let straggler = root.join("events.db");
        let mut c = rusqlite::Connection::open(&straggler).unwrap();
        crate::event_store::ensure_schema(&mut c, &straggler).unwrap();
        c.execute(
            "INSERT INTO events (seq, event_id, row_hash, ts_ms, type, source, line) VALUES (1, 'src-1', x'02', 0, 'x', 't', '{}')",
            [],
        )
        .unwrap();
        drop(c);
        let receipt = migrate(&root, true);
        let merged: Vec<_> = receipt
            .entries
            .iter()
            .filter(|e| e.legacy.contains("straggler"))
            .collect();
        assert_eq!(merged.len(), 1, "{merged:?}");
        assert!(matches!(merged[0].status, Status::Merged), "{merged:?}");
        let c = rusqlite::Connection::open(&published).unwrap();
        let n: i64 = c
            .query_row("SELECT COUNT(*) FROM events", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 2, "the straggler row landed beside the published one");
        drop(c);
        assert!(!straggler.exists(), "the imported straggler parked");
        clean(&root);
        // Branch: a zero-length legacy beside a populated published graph
        // parks as .empty-legacy and reads Parked, not refused.
        let root = tmp_root("ow-empty-legacy");
        std::fs::create_dir_all(root.join("db")).unwrap();
        let new_db = root.join("db").join("graph.db");
        let c = rusqlite::Connection::open(&new_db).unwrap();
        c.execute_batch("CREATE TABLE t (a); INSERT INTO t VALUES (1);")
            .unwrap();
        drop(c);
        let legacy = root.join("graph.db");
        std::fs::write(&legacy, b"").unwrap();
        std::fs::write(root.join("graph.db-shm"), b"").unwrap();
        let row = parse_table("graph.db\tdb/graph.db\tsqlite\tdaemon\n")
            .unwrap()
            .remove(0);
        let status =
            crate::state_layout_sqlite::migrate_sqlite_row(&root, &row, true, "ow-empty-legacy");
        assert!(matches!(status, Status::Parked), "{status:?}");
        let c = rusqlite::Connection::open(&new_db).unwrap();
        let n: i64 = c
            .query_row("SELECT COUNT(*) FROM t", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 1, "the published rows are untouched");
        drop(c);
        assert!(!legacy.exists(), "the empty legacy left the root");
        assert!(
            root.join("backups")
                .join("state-root-migration")
                .join("ow-empty-legacy")
                .join("graph.db.empty-legacy")
                .exists(),
            "the empty legacy parked under its suffix"
        );
        assert!(
            root.join("backups")
                .join("state-root-migration")
                .join("ow-empty-legacy")
                .join("graph.db.empty-legacy-shm")
                .exists(),
            "the zero-length sidecar parked beside the main file"
        );
        clean(&root);
    }

    fn set_mtime(p: &Path, t: std::time::SystemTime) {
        let f = std::fs::File::options().write(true).open(p).unwrap();
        f.set_modified(t).unwrap();
    }

    #[test]
    fn lock_waits_for_its_data_file() {
        let root = tmp_root("lock");
        std::fs::write(root.join("graph.json.lock"), b"").unwrap();
        let receipt = migrate(&root, true);
        assert!(root.join("graph.json.lock").exists(), "lock untouched");
        let found = receipt
            .entries
            .iter()
            .find(|e| e.legacy == "graph.json.lock")
            .unwrap();
        assert!(matches!(found.status, Status::Pending(ref d) if d.contains("graph.json")));
        clean(&root);
        // The anchor base is virtual, so the probe is the `.db` twin at the
        // new location: with db/graph.db live the same lock deletes.
        let root = tmp_root("anchor-lock");
        std::fs::create_dir_all(root.join("db")).unwrap();
        std::fs::write(root.join("db/graph.db"), b"SQLite format 3").unwrap();
        std::fs::write(root.join("graph.json.lock"), b"").unwrap();
        let receipt = migrate(&root, true);
        let found = receipt
            .entries
            .iter()
            .find(|e| e.legacy == "graph.json.lock")
            .unwrap();
        assert_eq!(found.status, Status::Deleted);
        assert!(!root.join("graph.json.lock").exists(), "lock gone");
        clean(&root);
        // A base the table does not carry (graph-archive.json.store.sock)
        // reads the same way: its .lock is residue once the base is gone.
        let root = tmp_root("orphan-lock");
        std::fs::write(root.join("graph-archive.json.store.sock.lock"), b"").unwrap();
        let receipt = migrate(&root, true);
        let found = receipt
            .entries
            .iter()
            .find(|e| e.legacy == "graph-archive.json.store.sock.lock")
            .unwrap();
        assert_eq!(found.status, Status::Deleted);
        assert!(!root.join("graph-archive.json.store.sock.lock").exists());
        clean(&root);
    }

    #[test]
    fn refused_when_rename_fails() {
        if unsafe { libc::geteuid() } == 0 {
            return;
        }
        let root = tmp_root("refuse");
        std::fs::write(root.join("installed-rev"), b"abc\n").unwrap();
        let restore = std::fs::metadata(&root).unwrap().permissions();
        let mut locked = restore.clone();
        use std::os::unix::fs::PermissionsExt;
        locked.set_mode(0o500);
        std::fs::set_permissions(&root, locked).unwrap();
        let receipt = migrate(&root, true);
        std::fs::set_permissions(&root, restore).unwrap();
        let found = receipt
            .entries
            .iter()
            .find(|e| e.legacy == "installed-rev")
            .unwrap();
        assert!(
            matches!(found.status, Status::Refused(ref d) if d.contains("refused:")),
            "{:?}",
            found.status
        );
        assert!(root.join("installed-rev").exists(), "legacy untouched");
        clean(&root);
    }
    #[test]
    fn vendored_table_matches_the_repo_copy() {
        let repo = include_str!("../../../docs/state-root-layout.tsv");
        assert_eq!(
            LAYOUT_TSV, repo,
            "the vendored layout table drifted from docs/state-root-layout.tsv;              edit the repo copy and copy it into both crates"
        );
    }
}
