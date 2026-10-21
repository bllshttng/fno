//! The first-turn watch: a launch either reaches its first turn, or its
//! caller hears the stage it is stuck in, by 5 minutes after the call.
//!
//! The PreToolUse Bash hook writes a launch record naming the caller before
//! the spawn gate runs, so the record is the one trace a spawn leaves from
//! its first moment. Each pass reads the world and names the furthest stage
//! it can prove: gate, launch, harness boot, first prompt. A first turn is
//! kept as `latency_ms` from the call and one `spawn_first_turn` event that
//! names substrate and machine. Contract: docs/architecture/spawn-watch.md.

use crate::launch_record::Record;
use crate::paths::AgentsHome;
use crate::state::{self, RegistryEntry};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashSet;
use std::io::Read;
use std::path::{Path, PathBuf};

/// The notice deadline. With the 60s tick the caller hears by 5 minutes.
const DEADLINE_MS: i64 = 4 * 60_000;
/// A launch with no first turn by then stops being watched.
const HORIZON_MS: i64 = 2 * 60 * 60_000;
/// A closed watch stays readable for a day, then prunes.
const KEEP_MS: i64 = 24 * 60 * 60_000;
/// The gate's 240s queue plus startup: a refused call's own record is
/// written inside this.
const GATE_WAIT_MS: i64 = 5 * 60_000;
const SENDER: &str = "fno-spawn-watch";

/// A launch the caller waits on in its own Bash call (`codex exec`,
/// `pi -p`) already answers it, so only detached launches are watched: a
/// `claude --bg` (the only claude line the hook records), a spawn command, a
/// pane run, or a dispatcher's record, which carries no command.
fn detached(r: &Record) -> bool {
    r.launch.harness == "claude"
        || r.command.is_empty()
        || ["agents spawn", "pane run"]
            .iter()
            .any(|w| r.command.contains(w))
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
struct Watch {
    caller: String,
    harness: String,
    cwd: String,
    name: Option<String>,
    called_at_ms: i64,
    child_session: Option<String>,
    /// The worker row this launch matched, as (created_at, name), so a later
    /// launch from the same caller cannot take it.
    row: Option<(String, String)>,
    stage: Option<String>,
    notified: bool,
    answered: Option<String>,
    first_turn_at_ms: Option<i64>,
    latency_ms: Option<i64>,
    followed_up: bool,
    closed: bool,
}

fn watch_dir(home: &AgentsHome) -> PathBuf {
    home.root().join("spawn-watch")
}

/// The pending and the claimed copy of one launch share this key.
fn record_key(r: &Record) -> String {
    use sha2::{Digest, Sha256};
    let at = r.launched_at_ms.to_string();
    let tail = r.launch.name.as_deref().unwrap_or(&r.launch.cwd);
    let mut h = Sha256::new();
    for part in [
        r.launcher_session.as_str(),
        at.as_str(),
        r.launch.harness.as_str(),
        tail,
    ] {
        h.update(part.as_bytes());
        h.update(b"\0");
    }
    let hex: String = h
        .finalize()
        .iter()
        .take(8)
        .map(|b| format!("{b:02x}"))
        .collect();
    format!("{hex}.json")
}

fn from_record(r: &Record) -> Watch {
    Watch {
        caller: r.launcher_session.clone(),
        harness: r.launch.harness.clone(),
        cwd: r.launch.cwd.clone(),
        name: r.launch.name.clone(),
        called_at_ms: r.launched_at_ms,
        child_session: r.child_session.clone(),
        ..Default::default()
    }
}

fn read(path: &Path) -> Result<Option<Watch>, String> {
    match std::fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map(Some)
            .map_err(|e| e.to_string()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.to_string()),
    }
}

fn millis(stamp: &str) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(stamp)
        .ok()
        .map(|d| d.timestamp_millis())
}

/// The gate refused this caller's spawn and the caller holds the refusal.
/// The launch never runs, so its record leaves the claim pool and its watch
/// closes with no notice. A caller with two unnamed launches in flight is
/// ambiguous and settles nothing.
pub(crate) fn settle_refused(home: &AgentsHome, caller: Option<&str>, name: &str, reason: &str) {
    let Some(caller) = caller.filter(|c| !c.is_empty()) else {
        return;
    };
    let now = crate::claims::now_ms();
    let window = crate::launch_record::WINDOW_MS;
    let pending: Vec<(PathBuf, Record)> = crate::launch_record::records(home.root(), now - window)
        .into_iter()
        .filter(|(_, r)| {
            r.child_session.is_none()
                && r.launcher_session == caller
                && now.saturating_sub(r.launched_at_ms) <= window
        })
        .collect();
    // An unnamed record is this call's only inside the gate's own wait.
    let unnamed: Vec<&(PathBuf, Record)> = pending
        .iter()
        .filter(|(_, r)| {
            r.launch.name.is_none() && now.saturating_sub(r.launched_at_ms) <= GATE_WAIT_MS
        })
        .collect();
    let pick = pending
        .iter()
        .filter(|(_, r)| r.launch.name.as_deref() == Some(name))
        .max_by_key(|(_, r)| r.launched_at_ms)
        .or(match unnamed.as_slice() {
            [one] => Some(*one),
            _ => None,
        });
    let Some((path, record)) = pick else {
        return;
    };
    let watch = Watch {
        answered: Some(format!("refused at the gate ({reason})")),
        closed: true,
        ..from_record(record)
    };
    let dir = watch_dir(home);
    if let Err(e) = std::fs::create_dir_all(&dir)
        .map_err(|e| e.to_string())
        .and_then(|_| {
            state::write_json_atomic(&dir.join(record_key(record)), &watch)
                .map_err(|e| e.to_string())
        })
    {
        eprintln!("spawn-watch: refusal not recorded: {e}");
    }
    let _ = std::fs::remove_file(path);
}

/// Copy each recent launch record into a watch file, so the record's prune
/// loses nothing, and carry a claim's child session onto the watch.
fn adopt(home: &AgentsHome, now: i64) {
    let dir = watch_dir(home);
    let mut records = crate::launch_record::records(home.root(), now - HORIZON_MS);
    records.sort_by_key(|(_, r)| r.launched_at_ms);
    let mut known: Vec<(PathBuf, Watch)> = std::fs::read_dir(&dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| Some((e.path(), read(&e.path()).ok()??)))
        .collect();
    for (_, record) in records {
        if !detached(&record) || now.saturating_sub(record.launched_at_ms) > HORIZON_MS {
            continue;
        }
        let own = dir.join(record_key(&record));
        let path = if known.iter().any(|(p, _)| *p == own) {
            own
        } else {
            twin(&known, &record).unwrap_or(own)
        };
        let watch = match read(&path) {
            Ok(None) => from_record(&record),
            Ok(Some(w)) if w.name.is_none() && record.launch.name.is_some() => Watch {
                name: record.launch.name.clone(),
                child_session: w.child_session.or_else(|| record.child_session.clone()),
                ..w
            },
            Ok(Some(w)) if w.child_session.is_none() && record.child_session.is_some() => Watch {
                child_session: record.child_session.clone(),
                ..w
            },
            // Unchanged, or unreadable: the judge loop reports a bad file.
            _ => continue,
        };
        if let Err(e) = state::write_json_atomic(&path, &watch) {
            eprintln!("spawn-watch: {}: {e}", path.display());
            continue;
        }
        known.retain(|(p, _)| *p != path);
        known.push((path, watch));
    }
}

/// The native lane writes a second record after the gate admits a spawn,
/// beside the hook's record of the same call. One launch keeps one watch:
/// the later record folds into the earlier watch from that caller and
/// harness, inside the gate's wait, whose name matches or is not yet known.
fn twin(known: &[(PathBuf, Watch)], r: &Record) -> Option<PathBuf> {
    let name = r.launch.name.as_deref()?;
    let fits = |w: &Watch| {
        w.caller == r.launcher_session
            && w.harness == r.launch.harness
            && w.called_at_ms < r.launched_at_ms
            && r.launched_at_ms - w.called_at_ms <= GATE_WAIT_MS
    };
    let pick = |named: bool| {
        known
            .iter()
            .filter(|(_, w)| fits(w) && (w.name.as_deref() == Some(name)) == named)
            .filter(|(_, w)| named || w.name.is_none())
            .min_by_key(|(_, w)| w.called_at_ms)
            .map(|(p, _)| p.clone())
    };
    pick(true).or_else(|| pick(false))
}

pub(crate) fn run_pass(
    home: &AgentsHome,
    now: i64,
    runner: crate::burn_watch::Runner<'_>,
) -> Result<(), String> {
    let dir = watch_dir(home);
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    adopt(home, now);
    let registry = state::load_registry(&home.registry_json()).map_err(|e| e.to_string())?;
    let emitter = crate::events::EventEmitter::new(home.events_jsonl(), "daemon");
    let mut first_error = None;
    let mut watches = Vec::new();
    for item in std::fs::read_dir(&dir).map_err(|e| e.to_string())? {
        let path = item.map_err(|e| e.to_string())?.path();
        if path.extension().and_then(|v| v.to_str()) != Some("json") {
            continue;
        }
        match read(&path) {
            Ok(Some(watch)) => watches.push((path, watch)),
            Ok(None) => {}
            Err(e) => {
                first_error.get_or_insert_with(|| format!("{}: {e}", path.display()));
            }
        }
    }
    watches.sort_by_key(|(_, w)| w.called_at_ms);
    let mut taken: HashSet<(String, String)> =
        watches.iter().filter_map(|(_, w)| w.row.clone()).collect();
    for (path, mut watch) in watches {
        if watch.closed {
            if now.saturating_sub(watch.called_at_ms) > KEEP_MS {
                let _ = std::fs::remove_file(&path);
            }
            continue;
        }
        // The judge leaves the watch consistent even when a send fails, so
        // the state is written either way and the send retries next pass.
        let result = judge(
            &mut watch,
            &registry,
            &mut taken,
            now,
            &emitter,
            &mut *runner,
        );
        let written = state::write_json_atomic(&path, &watch).map_err(|e| e.to_string());
        if let Err(error) = result.and(written) {
            first_error.get_or_insert_with(|| format!("{}: {error}", path.display()));
        }
    }
    first_error.map_or(Ok(()), Err)
}

/// The worker row: by session id once one is known (law d-e952ed19), then by
/// the row this watch already matched, then by the first untaken row the
/// caller spawned on this harness after the call.
fn find_row<'r>(
    watch: &Watch,
    registry: &'r state::Registry,
    taken: &HashSet<(String, String)>,
) -> Option<&'r RegistryEntry> {
    if let Some(sid) = watch.child_session.as_deref() {
        if let Some(row) = registry
            .entries
            .iter()
            .find(|r| r.harness_session_id.as_deref() == Some(sid))
        {
            return Some(row);
        }
    }
    if let Some((created, name)) = &watch.row {
        if let Some(row) = registry
            .entries
            .iter()
            .find(|r| &r.created_at == created && &r.name == name)
        {
            return Some(row);
        }
    }
    registry
        .entries
        .iter()
        .filter(|r| {
            r.spawned_by_session.as_deref() == Some(watch.caller.as_str())
                && r.harness_name() == watch.harness.as_str()
                && watch.name.as_ref().is_none_or(|n| &r.name == n)
                && millis(&r.created_at).is_some_and(|ms| ms >= watch.called_at_ms - 5_000)
                && !taken.contains(&(r.created_at.clone(), r.name.clone()))
        })
        .min_by_key(|r| millis(&r.created_at))
}

fn judge(
    watch: &mut Watch,
    registry: &state::Registry,
    taken: &mut HashSet<(String, String)>,
    now: i64,
    emitter: &crate::events::EventEmitter,
    runner: crate::burn_watch::Runner<'_>,
) -> Result<(), String> {
    let row = find_row(watch, registry, taken);
    if let Some(row) = row {
        let id = (row.created_at.clone(), row.name.clone());
        taken.insert(id.clone());
        watch.row = Some(id);
        if watch.child_session.is_none() {
            watch.child_session = row.harness_session_id.clone();
        }
    }
    let transcript = row.and_then(crate::first_check::transcript).or_else(|| {
        watch
            .child_session
            .as_deref()
            .and_then(|sid| crate::context_run::session_transcript(sid, &watch.harness))
    });
    let name = watch
        .name
        .clone()
        .or_else(|| row.map(|r| r.name.clone()))
        .unwrap_or_else(|| format!("(unnamed, in {})", watch.cwd));
    if watch.first_turn_at_ms.is_none() {
        if let Some(at) = transcript
            .as_deref()
            .and_then(|p| first_turn(p, watch.called_at_ms, now))
        {
            emitter
                .emit(
                    "spawn_first_turn",
                    &json!({
                        "name": name,
                        "harness": watch.harness,
                        "substrate": row.and_then(|r| r.substrate.clone()).unwrap_or_else(|| "unknown".into()),
                        "machine": crate::session_origin::this_machine(),
                        "caller": watch.caller,
                        "called_at_ms": watch.called_at_ms,
                        "first_turn_at_ms": at,
                        "latency_ms": at - watch.called_at_ms,
                        "row_ms": row.and_then(|r| millis(&r.created_at)).map(|ms| ms - watch.called_at_ms),
                    }),
                )
                .map_err(|e| e.to_string())?;
            watch.first_turn_at_ms = Some(at);
            watch.latency_ms = Some(at - watch.called_at_ms);
        }
    }
    if let Some(latency) = watch.latency_ms {
        if watch.notified && !watch.followed_up {
            let stage = watch.stage.as_deref().unwrap_or("an earlier stage");
            crate::first_check::send_mail(
                runner,
                SENDER,
                &watch.caller,
                &format!(
                    "spawn {name}: first turn {}s after the call (reported stuck at {stage} earlier).",
                    latency / 1000
                ),
            )?;
            watch.followed_up = true;
        }
        watch.closed = true;
        return Ok(());
    }
    let age = now.saturating_sub(watch.called_at_ms);
    if age >= HORIZON_MS {
        watch.closed = true;
        return Ok(());
    }
    if age < DEADLINE_MS || watch.notified {
        return Ok(());
    }
    let (stage, evidence) = stage(watch, row, transcript.is_some());
    crate::first_check::send_mail(
        runner,
        SENDER,
        &watch.caller,
        &format!(
            "spawn {name}: no first turn {}m after the call. Stuck at {stage}: {evidence}. Inspect `fno agents peek {name}`.",
            age / 60_000
        ),
    )?;
    watch.stage = Some(stage.to_string());
    watch.notified = true;
    Ok(())
}

/// The furthest stage the world proves, with the reading that proves it.
fn stage(
    watch: &Watch,
    row: Option<&RegistryEntry>,
    has_transcript: bool,
) -> (&'static str, String) {
    if let Some(sid) = watch.child_session.as_deref() {
        let short: String = sid.chars().take(8).collect();
        let seen = if has_transcript {
            "its transcript holds no assistant line yet"
        } else {
            "no transcript is found for it yet"
        };
        return ("first prompt", format!("session {short} started; {seen}"));
    }
    if let Some(row) = row {
        return (
            "harness boot",
            format!(
                "worker row {} exists ({:?}) with no harness session id",
                row.name, row.status
            ),
        );
    }
    if let Some(name) = &watch.name {
        let root = crate::claims::global_claims_root();
        let (held, _) = crate::claims::status(&format!("worker:{name}"), root.as_deref());
        if held == crate::claims::ClaimState::Live {
            return (
                "launch",
                format!("the gate admitted it (worker:{name} held) and no worker row exists yet"),
            );
        }
    }
    (
        "gate",
        "no worker row and no gate admission held; a refused or abandoned call reads the same"
            .into(),
    )
}

/// The first assistant row stamped at or after the call; a row with no stamp
/// reads as now. A resumed transcript holds the prior run's turns, and its
/// new rows sit at the end, so the read takes the file's tail.
fn first_turn(path: &Path, since: i64, now: i64) -> Option<i64> {
    use std::io::{Seek, SeekFrom};
    const TAIL: u64 = 16 * 1024 * 1024;
    let mut file = std::fs::File::open(path).ok()?;
    let len = file.metadata().ok()?.len();
    file.seek(SeekFrom::Start(len.saturating_sub(TAIL))).ok()?;
    let mut bytes = Vec::new();
    file.take(TAIL).read_to_end(&mut bytes).ok()?;
    String::from_utf8_lossy(&bytes)
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .filter(crate::first_check::is_assistant)
        .map(|row| {
            row.get("timestamp")
                .and_then(Value::as_str)
                .and_then(millis)
                .unwrap_or(now)
        })
        .find(|at| *at >= since)
}

#[cfg(test)]
#[path = "spawn_watch_tests.rs"]
mod tests;
