//! The one door to every advisory lock a foreground command can wait on.
//!
//! Background work (a reaper, a sweep, a cache server) must never park a
//! foreground command with no bound. A chokepoint is a lock, a shared server or
//! a queue that more than one process waits on. Each one is a row in
//! [`REGISTRY`] that declares its wait bound and what to do at expiry. [`take`]
//! polls to that bound, stamps the holder, and on expiry probes the stamped
//! holder and journals one `chokepoint_wait_exceeded` event naming the row.
//!
//! The raw lock calls live here and nowhere else; clippy's `disallowed-methods`
//! lint and `scripts/ci/check-raw-locks.sh` refuse a new one. fno-agents owns
//! this file; `build.rs` copies it into the fno crate (the `store_conn`
//! pattern), so it uses only std, libc, serde_json and chrono.
//!
//! The stamp stays one JSON line, `{pid, name, acquired_at, chokepoint}`, the
//! shape Python's `fno.agents.lock` parses.

use std::fs::{File, OpenOptions};
use std::io::{Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

const POLL: Duration = Duration::from_millis(25);

/// What a row guards.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Lock,
    Server,
    Queue,
}

/// What a waiter does when the bound passes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OnExpiry {
    /// Exit non-zero and name the holder.
    Refuse,
    /// Skip an append-only write and journal that it was skipped.
    SkipAndJournal,
}

/// One registered chokepoint. A plain foreground row waits 10 seconds; a row held across real work (a spawn census, a gh call) gets a longer bound.
#[derive(Debug)]
pub struct Chokepoint {
    pub name: &'static str,
    pub kind: Kind,
    pub wait: Duration,
    pub on_expiry: OnExpiry,
    /// False when another program owns the lock (cargo's `.cargo-lock`).
    pub shared: bool,
}

const fn lock(name: &'static str, secs: u64, on_expiry: OnExpiry) -> Chokepoint {
    Chokepoint {
        name,
        kind: Kind::Lock,
        wait: Duration::from_secs(secs),
        on_expiry,
        shared: true,
    }
}

pub static REGISTRY_STATE: Chokepoint = lock("registry-state", 10, OnExpiry::Refuse);
/// Held across a census plus a spawn, so a burst of spawns queues behind it.
pub static SPAWN_ADMISSION: Chokepoint = lock("spawn-admission", 30, OnExpiry::Refuse);
pub static LEDGER: Chokepoint = lock("ledger", 10, OnExpiry::Refuse);
/// The session harvest is idempotent: a skipped pass reruns on the next tick.
pub static LEDGER_HARVEST: Chokepoint = lock("ledger-harvest", 10, OnExpiry::SkipAndJournal);
pub static LEAD_STATE: Chokepoint = lock("lead-state", 10, OnExpiry::Refuse);
pub static ACCOUNTS: Chokepoint = lock("accounts", 10, OnExpiry::Refuse);
pub static TEAM_REAP: Chokepoint = lock("team-reap", 10, OnExpiry::Refuse);
/// Held across a whole gh call; many watchers queue on it.
pub static GH_QUOTA: Chokepoint = lock("gh-quota", 60, OnExpiry::Refuse);
pub static HARNESS_DAEMON: Chokepoint = lock("harness-daemon", 10, OnExpiry::Refuse);
pub static FLEET_INCIDENT: Chokepoint = lock("fleet-incident", 10, OnExpiry::Refuse);
pub static TEST_HOLD: Chokepoint = lock("test-hold", 10, OnExpiry::Refuse);
pub static PLUGIN_REGISTRY: Chokepoint = lock("plugin-registry", 10, OnExpiry::Refuse);
pub static CLAUDE_VAULT: Chokepoint = lock("claude-vault", 10, OnExpiry::Refuse);
pub static OPENCODE_ASK: Chokepoint = lock("opencode-ask", 10, OnExpiry::Refuse);
pub static CODEX_CONVERGE: Chokepoint = lock("codex-converge", 10, OnExpiry::Refuse);
pub static SESSION_SPOOL: Chokepoint = lock("session-spool", 10, OnExpiry::SkipAndJournal);
pub static ROLE_MIGRATION: Chokepoint = lock("role-migration", 10, OnExpiry::Refuse);
pub static MUX_RESEAT: Chokepoint = lock("mux-reseat", 10, OnExpiry::Refuse);
/// Held by `BoundedLock`, `AgentLock` and mail send, which poll their own
/// caller-supplied deadline and report expiry through [`report_expiry`].
pub static GRAPH_STORE: Chokepoint = lock("graph-store", 10, OnExpiry::Refuse);
pub static AGENT_ASK: Chokepoint = lock("agent-ask", 30, OnExpiry::Refuse);

pub static REGISTRY: &[&Chokepoint] = &[
    &REGISTRY_STATE,
    &SPAWN_ADMISSION,
    &LEDGER,
    &LEDGER_HARVEST,
    &LEAD_STATE,
    &ACCOUNTS,
    &TEAM_REAP,
    &GH_QUOTA,
    &HARNESS_DAEMON,
    &FLEET_INCIDENT,
    &TEST_HOLD,
    &PLUGIN_REGISTRY,
    &CLAUDE_VAULT,
    &OPENCODE_ASK,
    &CODEX_CONVERGE,
    &SESSION_SPOOL,
    &ROLE_MIGRATION,
    &MUX_RESEAT,
    &GRAPH_STORE,
    &AGENT_ASK,
];

/// Why a wait ended without the lock.
#[derive(Debug, Clone)]
pub struct Refused {
    pub row: &'static str,
    pub path: PathBuf,
    pub waited: Duration,
    pub holder_pid: Option<u64>,
    pub holder_alive: bool,
    pub hold_age_secs: Option<i64>,
    /// The chokepoint row the stamped holder took the lock under.
    pub holder_op: Option<String>,
    pub waiter_pid: u32,
    pub io_error: Option<String>,
}

impl std::fmt::Display for Refused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if let Some(error) = &self.io_error {
            return write!(
                f,
                "{}: cannot open {}: {error}",
                self.row,
                self.path.display()
            );
        }
        write!(
            f,
            "{}: lock {} still held after {:.1}s",
            self.row,
            self.path.display(),
            self.waited.as_secs_f64()
        )?;
        match (self.holder_pid, self.holder_alive, self.hold_age_secs) {
            (Some(pid), true, Some(age)) => write!(f, "; holder pid {pid} is alive, held {age}s"),
            (Some(pid), true, None) => write!(f, "; holder pid {pid} is alive"),
            (Some(pid), false, _) => {
                write!(f, "; holder record pid {pid} is dead (crashed writer)")
            }
            (None, _, _) => write!(f, "; no holder recorded"),
        }
    }
}

impl std::error::Error for Refused {}

impl Refused {
    /// The `chokepoint_wait_exceeded` payload.
    pub fn event_data(&self) -> serde_json::Value {
        serde_json::json!({
            "row": self.row,
            "path": self.path.display().to_string(),
            "waited_ms": self.waited.as_millis() as u64,
            "holder_pid": self.holder_pid,
            "holder_alive": self.holder_alive,
            "hold_age_secs": self.hold_age_secs,
            "holder_op": self.holder_op,
            "waiter_pid": self.waiter_pid,
        })
    }
}

/// A held chokepoint lock. The stamp clears and the lock releases on drop.
#[derive(Debug)]
pub struct Held {
    file: File,
    stamped: bool,
}

impl Held {
    /// The locked file, for a caller that appends to it while it holds the lock.
    pub fn file(&self) -> &File {
        &self.file
    }

    /// Release now. Callers that ended a hold with `File::unlock` keep the call shape.
    pub fn unlock(self) -> std::io::Result<()> {
        drop(self);
        Ok(())
    }
}

impl From<Refused> for std::io::Error {
    fn from(refused: Refused) -> Self {
        std::io::Error::new(std::io::ErrorKind::TimedOut, refused)
    }
}

impl Drop for Held {
    fn drop(&mut self) {
        // Clear before unlocking: after the unlock the next holder's stamp is
        // not ours to truncate.
        if self.stamped {
            let _ = self.file.set_len(0);
        }
        let _ = self.file.unlock();
    }
}

type Sink = fn(&Refused);
static SINK: OnceLock<Sink> = OnceLock::new();

/// Each binary registers where `chokepoint_wait_exceeded` rows go. Unset, the
/// row is dropped silently (the refusal still carries the holder).
pub fn set_event_sink(sink: Sink) {
    let _ = SINK.set(sink);
}

fn pid_alive(pid: u64) -> bool {
    if pid == 0 || pid > i32::MAX as u64 {
        return false;
    }
    // SAFETY: signal 0 checks existence and permission only; it delivers nothing.
    if unsafe { libc::kill(pid as libc::pid_t, 0) } == 0 {
        return true;
    }
    std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

/// Write this process as the holder, truncating any earlier line first. Every
/// lock in both crates stamps through here.
pub fn stamp(file: &File, row: &str, label: &str) {
    let mut handle = file;
    if handle.set_len(0).is_err() {
        return;
    }
    let _ = handle.seek(SeekFrom::Start(0));
    let line = serde_json::json!({
        "pid": std::process::id(),
        "name": label,
        "chokepoint": row,
        "acquired_at": chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Micros, false),
    });
    let _ = writeln!(handle, "{line}");
    let _ = handle.flush();
}

fn read_holder(path: &Path) -> (Option<u64>, Option<i64>) {
    let Ok(text) = std::fs::read_to_string(path) else {
        return (None, None);
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(text.lines().next().unwrap_or(""))
    else {
        return (None, None);
    };
    let age = value
        .get("acquired_at")
        .and_then(serde_json::Value::as_str)
        .and_then(|at| chrono::DateTime::parse_from_rfc3339(at).ok())
        .map(|at| {
            (chrono::Utc::now() - at.with_timezone(&chrono::Utc))
                .num_seconds()
                .max(0)
        });
    (value.get("pid").and_then(serde_json::Value::as_u64), age)
}

/// A hold this old is a stall worth naming, whoever waits on it.
const LONG_HOLD_SECS: i64 = 30;

/// The stamped holder's chokepoint row and acquire time, for the long-hold
/// line and the expiry event.
fn read_stamp_fields(path: &Path) -> (Option<String>, Option<String>) {
    let Ok(text) = std::fs::read_to_string(path) else {
        return (None, None);
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(text.lines().next().unwrap_or(""))
    else {
        return (None, None);
    };
    let field = |key: &str| {
        value
            .get(key)
            .and_then(serde_json::Value::as_str)
            .map(str::to_string)
    };
    (field("chokepoint"), field("acquired_at"))
}

/// The holder's command line, cut to 120 characters. The stamp's op names the
/// lock row, which every holder of one lock shares; the command line is what
/// tells an `authorized-merge` from a `registry-commit`.
pub(crate) fn process_command(pid: u64) -> String {
    std::process::Command::new("ps")
        .args(["-o", "command=", "-p", &pid.to_string()])
        .output()
        .ok()
        .map(|out| {
            String::from_utf8_lossy(&out.stdout)
                .trim()
                .chars()
                .take(120)
                .collect()
        })
        .filter(|text: &String| !text.is_empty())
        .unwrap_or_else(|| "command unreadable".to_string())
}

/// Name a live holder that has kept the lock past [`LONG_HOLD_SECS`], once per
/// hold: a `.longhold` sidecar remembers the acquire time already named, so a
/// queue of waiters prints one line, not one each.
fn note_long_hold(refused: &Refused, acquired_at: Option<&str>) -> bool {
    let (Some(pid), Some(age), Some(at)) = (refused.holder_pid, refused.hold_age_secs, acquired_at)
    else {
        return false;
    };
    if !refused.holder_alive || age < LONG_HOLD_SECS {
        return false;
    }
    let marker = PathBuf::from(format!("{}.longhold", refused.path.display()));
    if std::fs::read_to_string(&marker).is_ok_and(|seen| seen.trim() == at) {
        return false;
    }
    if std::fs::write(&marker, at).is_err() {
        return false;
    }
    eprintln!(
        "chokepoint {}: {} held {age}s by pid {pid} (op {}; {})",
        refused.row,
        refused.path.display(),
        refused.holder_op.as_deref().unwrap_or("unknown"),
        process_command(pid)
    );
    true
}

/// Build the refusal for a wait that ran out, probe the stamped holder, and
/// journal one event. `BoundedLock` and `AgentLock` poll their own deadline and
/// call this when it passes.
pub fn report_expiry(row: &Chokepoint, path: &Path, waited: Duration) -> Refused {
    report_expiry_to(row, path, waited, SINK.get().copied())
}

fn report_expiry_to(
    row: &Chokepoint,
    path: &Path,
    waited: Duration,
    sink: Option<Sink>,
) -> Refused {
    let (holder_pid, hold_age_secs) = read_holder(path);
    let (holder_op, acquired_at) = read_stamp_fields(path);
    let refused = Refused {
        row: row.name,
        path: path.to_path_buf(),
        waited,
        holder_pid,
        holder_alive: holder_pid.is_some_and(pid_alive),
        hold_age_secs,
        holder_op,
        waiter_pid: std::process::id(),
        io_error: None,
    };
    let _ = note_long_hold(&refused, acquired_at.as_deref());
    // A sink that itself waits on a chokepoint must not journal its own expiry.
    thread_local! {
        static IN_SINK: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    }
    if let Some(sink) = sink {
        if !IN_SINK.with(|flag| flag.replace(true)) {
            sink(&refused);
            IN_SINK.with(|flag| flag.set(false));
        }
    }
    refused
}

/// Take the exclusive lock at `path`, waiting at most `row.wait`.
pub fn take(row: &Chokepoint, path: &Path) -> Result<Held, Refused> {
    take_within(row, path, row.wait, "", false, SINK.get().copied())
}

/// Take a shared lock (readers share; a writer excludes them). Readers write
/// no stamp, so the file keeps the writer's last line.
pub fn take_shared(row: &Chokepoint, path: &Path) -> Result<Held, Refused> {
    take_within(row, path, row.wait, "", true, SINK.get().copied())
}

/// One non-blocking attempt for a caller that runs its own wait loop (an
/// `on_wait` callback, a ticket queue). `Ok(None)` means another process holds
/// it; the caller reports expiry through [`report_expiry`].
pub fn try_take(row: &Chokepoint, path: &Path, label: &str) -> std::io::Result<Option<Held>> {
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(path)?;
    // chokepoint: the helper owns the one raw lock call.
    #[allow(clippy::disallowed_methods)]
    let locked = file.try_lock();
    if locked.is_err() {
        return Ok(None);
    }
    stamp(&file, row.name, label);
    Ok(Some(Held {
        file,
        stamped: true,
    }))
}

/// [`take`] with a label in the stamp, for a row that guards many files.
pub fn take_labeled(row: &Chokepoint, path: &Path, label: &str) -> Result<Held, Refused> {
    take_within(row, path, row.wait, label, false, SINK.get().copied())
}

fn take_within(
    row: &Chokepoint,
    path: &Path,
    wait: Duration,
    label: &str,
    shared: bool,
    sink: Option<Sink>,
) -> Result<Held, Refused> {
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let started = Instant::now();
    let open_failed = |error: std::io::Error| Refused {
        row: row.name,
        path: path.to_path_buf(),
        waited: started.elapsed(),
        holder_pid: None,
        holder_alive: false,
        hold_age_secs: None,
        holder_op: None,
        waiter_pid: std::process::id(),
        io_error: Some(error.to_string()),
    };
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(path)
        .map_err(open_failed)?;
    loop {
        // chokepoint: the helper owns the one raw lock call.
        #[allow(clippy::disallowed_methods)]
        let locked = if shared {
            file.try_lock_shared()
        } else {
            file.try_lock()
        };
        if locked.is_ok() {
            if !shared {
                stamp(&file, row.name, label);
            }
            return Ok(Held {
                file,
                stamped: !shared,
            });
        }
        if started.elapsed() >= wait {
            return Err(report_expiry_to(row, path, started.elapsed(), sink));
        }
        std::thread::sleep(POLL);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    static SEEN: Mutex<Vec<(String, Option<u64>, bool, u32)>> = Mutex::new(Vec::new());

    fn record(refused: &Refused) {
        SEEN.lock().unwrap().push((
            refused.row.to_string(),
            refused.holder_pid,
            refused.holder_alive,
            refused.waiter_pid,
        ));
    }

    static WEDGE: Chokepoint = lock("wedge-test", 10, OnExpiry::Refuse);

    #[test]
    fn wedged_holder_cannot_block_past_the_bound() {
        let dir = std::env::temp_dir().join(format!("fno-chokepoint-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("wedge.lock");
        let bound = Duration::from_millis(200);

        let holder = take_within(&WEDGE, &path, bound, "", false, None).unwrap();
        let begun = Instant::now();
        let refused = take_within(&WEDGE, &path, bound, "", false, Some(record)).unwrap_err();
        assert!(begun.elapsed() < bound + Duration::from_millis(100));
        assert_eq!(refused.holder_pid, Some(u64::from(std::process::id())));
        assert!(refused.holder_alive);
        assert!(refused.to_string().contains("is alive"), "{refused}");
        {
            let seen = SEEN.lock().unwrap();
            let rows: Vec<_> = seen.iter().filter(|r| r.0 == "wedge-test").collect();
            assert_eq!(rows.len(), 1, "exactly one event per expiry: {rows:?}");
            assert_eq!(rows[0].3, std::process::id());
        }

        drop(holder);
        std::fs::write(
            &path,
            "{\"pid\": 4194000, \"acquired_at\": \"1999-01-01T00:00:00+00:00\"}\n",
        )
        .unwrap();
        let reacquired = take_within(&WEDGE, &path, bound, "", false, None).unwrap();
        let (pid, _) = read_holder(&path);
        assert_eq!(pid, Some(u64::from(std::process::id())), "restamped");
        drop(reacquired);

        let dead = {
            let held = take_within(&WEDGE, &path, bound, "", false, None).unwrap();
            std::fs::write(
                &path,
                "{\"pid\": 4194000, \"acquired_at\": \"1999-01-01T00:00:00+00:00\"}\n",
            )
            .unwrap();
            let refused = take_within(&WEDGE, &path, bound, "", false, None).unwrap_err();
            drop(held);
            refused
        };
        assert!(!dead.holder_alive);
        assert!(dead.to_string().contains("is dead"), "{dead}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_hold_past_thirty_seconds_is_named_once_with_its_op() {
        let dir = std::env::temp_dir().join(format!("fno-longhold-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("long.lock");
        let bound = Duration::from_millis(50);
        let held = take_within(&WEDGE, &path, bound, "", false, None).unwrap();
        let old = (chrono::Utc::now() - chrono::Duration::seconds(120))
            .to_rfc3339_opts(chrono::SecondsFormat::Micros, false);
        std::fs::write(
            &path,
            format!(
                "{{\"pid\": {}, \"chokepoint\": \"graph-store\", \"acquired_at\": \"{old}\"}}\n",
                std::process::id()
            ),
        )
        .unwrap();
        let refused = take_within(&WEDGE, &path, bound, "", false, None).unwrap_err();
        assert_eq!(refused.holder_op.as_deref(), Some("graph-store"));
        assert_eq!(refused.event_data()["holder_op"], "graph-store");
        let (_, at) = read_stamp_fields(&path);
        assert!(
            !note_long_hold(&refused, at.as_deref()),
            "the first wait already named it"
        );
        let marker = PathBuf::from(format!("{}.longhold", path.display()));
        assert_eq!(std::fs::read_to_string(&marker).unwrap().trim(), old);
        drop(held);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
