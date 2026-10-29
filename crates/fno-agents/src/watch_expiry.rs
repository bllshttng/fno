//! Daemon-owned expiry for sessions parked on a local `<watching>` intent.
//!
//! The hook records the declaration in the checked global event stream. This
//! arm folds only a bounded recent window, confirms the same session still
//! owns exactly one live node claim, and uses the burn-watch mail/resume lane.
//! It never signals or kills the watched process.

use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::paths::AgentsHome;

const INTERVAL: Duration = Duration::from_secs(60);
const WINDOW_MS: i64 = 24 * 60 * 60 * 1000;
const MAX_EVENTS: u32 = 10_000;
const MAX_LIVE_JOURNAL_BYTES: u64 = 16 * 1024 * 1024;
pub(crate) const WATCH_IDLE: &str = "loop_check_watch_idle";
pub(crate) const WAKE_EVENT: &str = "loop_check_watch_expiry_wake";
const ACTIVITY_AND_TERMINAL: &[&str] = &[
    "loop_check",
    "termination",
    "mission_complete",
    "agent_stopped",
    "agent_exited",
    "inside_leg_completed",
];

#[derive(Default)]
pub struct Arm {
    last_tick: Mutex<Option<Instant>>,
    in_flight: Arc<AtomicBool>,
}

#[derive(Debug, Clone)]
pub(crate) struct Watch {
    pub event_id: String,
    pub seq: i64,
    pub session_id: String,
    /// The PR the tag names; null when the tag names no positive PR number.
    /// The CI-settle poller reads it; the expiry arm ignores it.
    pub pr: Option<i64>,
    pub node: String,
    pub blocker: String,
    pub task_id: Option<String>,
    pub reason: Option<String>,
    pub expires_at_ms: i64,
    pub ts_ms: i64,
}

#[derive(Debug, Clone)]
pub(crate) struct OverdueWatch {
    pub session_id: String,
    pub overdue_ms: i64,
}

#[derive(Debug, Clone)]
pub(crate) struct Evidence {
    pub event_id: String,
    pub seq: i64,
    pub ts_ms: i64,
    pub kind: String,
    pub session_id: Option<String>,
    pub data: Value,
}

pub(crate) fn should_wake(watch: &Watch, now_ms: i64, evidence: &[Evidence]) -> bool {
    if now_ms < watch.expires_at_ms && !build_finished(watch, evidence) {
        return false;
    }
    is_current_watch(watch, evidence)
        && !evidence.iter().any(|row| {
            row.session_id.as_deref() == Some(watch.session_id.as_str())
                && row.kind == WAKE_EVENT
                && row.data.get("watch_event_id").and_then(Value::as_str)
                    == Some(watch.event_id.as_str())
        })
}

/// A build park wakes when its parked cargo exits, ahead of the deadline.
/// The park row names the cargo pid and when its wait began; a pid that is
/// gone, or now names a process born after the wait began, is a finished
/// build. A pid this process may not probe reads as still running.
fn build_finished(watch: &Watch, evidence: &[Evidence]) -> bool {
    if watch.blocker != "build" {
        return false;
    }
    let Some(data) = evidence
        .iter()
        .find(|row| row.event_id == watch.event_id)
        .map(|row| &row.data)
    else {
        return false;
    };
    let Some(pid) = data
        .get("build_cargo_pid")
        .and_then(Value::as_i64)
        .filter(|pid| *pid > 0 && *pid <= i64::from(i32::MAX))
    else {
        return false;
    };
    let since_ms = data
        .get("build_since_ms")
        .and_then(Value::as_i64)
        .unwrap_or(i64::MAX);
    match crate::claims::probe_pid(pid as i32) {
        crate::claims::PidProbe::Created(created_ms) => created_ms > since_ms,
        crate::claims::PidProbe::Refused => false,
        crate::claims::PidProbe::Absent => true,
    }
}

pub(crate) fn is_current_watch(watch: &Watch, evidence: &[Evidence]) -> bool {
    !evidence.iter().any(|row| {
        if row.session_id.as_deref() != Some(watch.session_id.as_str()) {
            return false;
        }
        let is_later = (row.ts_ms, row.seq) > (watch.ts_ms, watch.seq);
        match row.kind.as_str() {
            WATCH_IDLE => is_later,
            "loop_check" => {
                is_later && row.data.get("intent").and_then(Value::as_str) != Some("watching")
            }
            "termination"
            | "mission_complete"
            | "agent_stopped"
            | "agent_exited"
            | "inside_leg_completed" => is_later,
            _ => false,
        }
    })
}

pub(crate) fn millis_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(i64::MAX as u128) as i64
}

/// The wait blockers whose watch parks a provider seat: the session has
/// pushed and the remaining blocker is external (CI pending, an outstanding
/// review, the merge slot), so the seat sits idle until the settle arm's or
/// expiry arm's wake resumes the session.
const SEAT_WAIT_BLOCKERS: &[&str] = &["ci", "review", "merge_slot"];

/// Registry-row identity -> the blocker of the session's CURRENT watch, for
/// the seat-wait blockers. Two identities per watch: the manifest session id
/// and, for codex, the thread id the registry row keys. Supersession is the
/// same rule the wake arms read: a later loop_check row with a non-watching
/// intent, or any later terminal row, ends the watch and the seat works
/// again.
pub(crate) fn watch_blockers(evidence: &[Evidence]) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    for watch in watches(evidence) {
        if !SEAT_WAIT_BLOCKERS.contains(&watch.blocker.as_str())
            || !is_current_watch(&watch, evidence)
            // A delivered wake ends the park even before the session's next
            // loop_check row: the seat is about to work again, and a wake
            // whose session never resumes must not hold the seat either.
            || evidence.iter().any(|row| {
                row.session_id.as_deref() == Some(watch.session_id.as_str())
                    && row.kind == WAKE_EVENT
                    && row.data.get("watch_event_id").and_then(Value::as_str)
                        == Some(watch.event_id.as_str())
            })
        {
            continue;
        }
        out.insert(watch.session_id.clone(), watch.blocker.clone());
        let thread = evidence
            .iter()
            .find(|row| row.event_id == watch.event_id)
            .and_then(|row| row.data.get("codex_thread_id"))
            .and_then(Value::as_str)
            .filter(|thread| !thread.trim().is_empty());
        if let Some(thread) = thread {
            out.insert(thread.to_string(), watch.blocker.clone());
        }
    }
    out
}

fn event_id(line: &str) -> String {
    format!("sha256:{:x}", Sha256::digest(line.as_bytes()))
}

pub(crate) fn read_evidence(home: &AgentsHome, now_ms: i64) -> Result<Vec<Evidence>, String> {
    read_evidence_since(home, now_ms, WINDOW_MS)
}

/// The same fold over a caller-chosen window (a bounded seat-park read).
pub(crate) fn read_evidence_since(
    home: &AgentsHome,
    now_ms: i64,
    window_ms: i64,
) -> Result<Vec<Evidence>, String> {
    let mut types = vec![WATCH_IDLE, WAKE_EVENT];
    types.extend_from_slice(ACTIVITY_AND_TERMINAL);
    let since_ms = Some(now_ms.saturating_sub(window_ms));
    let query = crate::event_store::EventQuery {
        since_ms,
        limit: Some(MAX_EVENTS + 1),
        ..crate::event_store::EventQuery::of_types(&types)
    };
    let global_events = crate::daemon::global_events_path(home);
    let mut rows = journal_rows(&global_events, &query)?;
    // The daemon journals its own stops and exits in the agents home, not
    // the global stream the hooks write. Without this read every
    // `agent_stopped` row is invisible here, so a stop never exempts a row.
    // Watch declarations stay global-only: a watch is the hook's to arm.
    let daemon_events = home.events_jsonl();
    if daemon_events != global_events {
        let terminal = crate::event_store::EventQuery {
            since_ms,
            limit: Some(MAX_EVENTS + 1),
            ..crate::event_store::EventQuery::of_types(DAEMON_TERMINAL)
        };
        // A raw journal comes back whole, so the type filter is applied here.
        rows.extend(
            journal_rows(&daemon_events, &terminal)?
                .into_iter()
                .filter(|(line, _)| {
                    serde_json::from_str::<Value>(line).is_ok_and(|value| {
                        value["type"]
                            .as_str()
                            .is_some_and(|kind| DAEMON_TERMINAL.contains(&kind))
                    })
                }),
        );
    }
    if rows.len() > MAX_EVENTS as usize {
        return Err(format!(
            "watch expiry event window exceeds {MAX_EVENTS} rows"
        ));
    }
    let mut evidence = rows
        .iter()
        .map(|(line, stored_line)| {
            let value: Value = serde_json::from_str(line)
                .map_err(|error| format!("watch expiry event row is invalid: {error}"))?;
            // A row known only through the recovered store replay is copied
            // history, not a live declaration; it must never arm a wake.
            if value["_history_only"] == true {
                return Ok(None);
            }
            let data = value.get("data").cloned().unwrap_or(Value::Null);
            let kind = value
                .get("type")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            let session_id = data
                .get("session_id")
                .and_then(Value::as_str)
                .map(str::to_string);
            let ts_ms = timestamp_ms(&value)
                .ok_or_else(|| "watch expiry event has no valid timestamp".to_string())?;
            if ts_ms > now_ms {
                return Err("watch expiry event has a future timestamp".to_string());
            }
            Ok(Some(Evidence {
                event_id: event_id(stored_line),
                seq: 0,
                ts_ms,
                kind,
                session_id,
                data,
            }))
        })
        .filter_map(|row| match row {
            Ok(Some(row)) if row.ts_ms >= now_ms.saturating_sub(window_ms) => Some(Ok(row)),
            Ok(Some(_)) | Ok(None) => None,
            Err(error) => Some(Err(error)),
        })
        .collect::<Result<Vec<_>, String>>()?;
    // Two journals interleave by time; the stable sort keeps each journal's
    // own order on a tie, and `seq` is the merged position.
    evidence.sort_by_key(|row| row.ts_ms);
    for (index, row) in evidence.iter_mut().enumerate() {
        row.seq = index as i64;
    }
    stamp_daemon_sessions(home, &mut evidence);
    Ok(evidence)
}

/// A daemon stop or exit names its row, never its session, so it cannot
/// end a watch or exempt a session. Stamp each one with the session of the
/// row that held that name when it fired: a row born after the event is a
/// respawn under the same name, and the old stop is not its stop.
fn stamp_daemon_sessions(home: &AgentsHome, evidence: &mut [Evidence]) {
    let unstamped =
        |row: &Evidence| DAEMON_TERMINAL.contains(&row.kind.as_str()) && row.session_id.is_none();
    if !evidence.iter().any(unstamped) {
        return;
    }
    let Ok(registry) = crate::state::load_registry(&home.registry_json()) else {
        return;
    };
    for row in evidence.iter_mut().filter(|row| unstamped(row)) {
        let Some(name) = row.data.get("name").and_then(Value::as_str) else {
            continue;
        };
        row.session_id = registry
            .entries
            .iter()
            .filter(|entry| entry.name == name || entry.short_id == name)
            .find(|entry| {
                chrono::DateTime::parse_from_rfc3339(&entry.created_at)
                    .map_or(true, |born| born.timestamp_millis() <= row.ts_ms)
            })
            .and_then(|entry| entry.harness_session_id.clone());
    }
}

/// The kinds the daemon journals in the agents home that end a watch or
/// stand quiet recovery down.
const DAEMON_TERMINAL: &[&str] = &["agent_stopped", "agent_exited", "inside_leg_completed"];

/// One journal's rows in the window, each with the line its identity hashes:
/// recovery provenance re-serializes store rows, but a wake receipt names the
/// hash of the stored line, so identity hashes that line instead.
fn journal_rows(
    journal: &std::path::Path,
    query: &crate::event_store::EventQuery,
) -> Result<Vec<(String, String)>, String> {
    if !crate::event_store::store_path(journal).is_file()
        && std::fs::metadata(journal)
            .map(|metadata| metadata.len() > MAX_LIVE_JOURNAL_BYTES)
            .unwrap_or(false)
    {
        return Err(format!(
            "watch expiry live journal exceeds {MAX_LIVE_JOURNAL_BYTES} bytes"
        ));
    }
    let text = crate::event_store::journal_text_checked(journal, query)?;
    let stored_lines: std::collections::HashMap<i64, String> = if text.contains("\"_store_seq\"") {
        crate::event_store::query_events(journal, query)?
            .into_iter()
            .map(|row| (row.seq, row.line))
            .collect()
    } else {
        Default::default()
    };
    Ok(text
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            let stored = serde_json::from_str::<Value>(line)
                .ok()
                .and_then(|value| value.get("_store_seq").and_then(Value::as_i64))
                .and_then(|seq| stored_lines.get(&seq).cloned())
                .unwrap_or_else(|| line.to_string());
            (line.to_string(), stored)
        })
        .collect())
}

fn timestamp_ms(value: &Value) -> Option<i64> {
    value.get("ts_ms").and_then(Value::as_i64).or_else(|| {
        value
            .get("ts")
            .and_then(Value::as_str)
            .and_then(|stamp| chrono::DateTime::parse_from_rfc3339(stamp).ok())
            .map(|stamp| stamp.timestamp_millis())
    })
}

fn watches(evidence: &[Evidence]) -> Vec<Watch> {
    let mut latest = std::collections::BTreeMap::<String, Watch>::new();
    for row in evidence.iter().filter(|row| row.kind == WATCH_IDLE) {
        let Some(session_id) = row.session_id.clone() else {
            continue;
        };
        let data = &row.data;
        let Some(expires_at_ms) = data.get("expires_at_ms").and_then(Value::as_i64) else {
            continue;
        };
        let watch = Watch {
            event_id: row.event_id.clone(),
            seq: row.seq,
            session_id: session_id.clone(),
            pr: data.get("pr").and_then(Value::as_i64),
            node: data
                .get("node")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            blocker: data
                .get("blocker")
                .and_then(Value::as_str)
                .unwrap_or("unknown")
                .to_string(),
            task_id: data
                .get("task_id")
                .and_then(Value::as_str)
                .map(str::to_string),
            reason: data
                .get("reason")
                .and_then(Value::as_str)
                .filter(|reason| !reason.trim().is_empty())
                .map(str::to_string),
            expires_at_ms,
            ts_ms: row.ts_ms,
        };
        if latest
            .get(&session_id)
            .map_or(true, |old| (watch.ts_ms, watch.seq) > (old.ts_ms, old.seq))
        {
            latest.insert(session_id, watch);
        }
    }
    latest.into_values().collect()
}

/// Each session's current `<watching>` bound, past-bound watches included:
/// the node liveness rule decides whether a bound still holds.
pub(crate) fn current_watch_bounds(
    home: &AgentsHome,
    now_ms: i64,
) -> Result<std::collections::HashMap<String, i64>, String> {
    let evidence = read_evidence(home, now_ms)?;
    Ok(watches(&evidence)
        .into_iter()
        .filter(|watch| is_current_watch(watch, &evidence))
        .map(|watch| (watch.session_id, watch.expires_at_ms))
        .collect())
}

pub(crate) fn overdue(home: &AgentsHome) -> Result<Vec<OverdueWatch>, String> {
    let now_ms = millis_now();
    let evidence = read_evidence(home, now_ms)?;
    let due = watches(&evidence)
        .into_iter()
        .filter(|watch| now_ms >= watch.expires_at_ms && is_current_watch(watch, &evidence))
        .collect::<Vec<_>>();
    if due.is_empty() {
        return Ok(Vec::new());
    }
    let claims = current_node_claims(home)?;
    let mut overdue = Vec::new();
    for watch in due {
        let Some(node) = claims.get(&watch.session_id).cloned().unwrap_or(Ok(None))? else {
            continue;
        };
        if !watch.node.is_empty() && node != watch.node {
            continue;
        }
        overdue.push(OverdueWatch {
            session_id: watch.session_id,
            overdue_ms: now_ms.saturating_sub(watch.expires_at_ms),
        });
    }
    Ok(overdue)
}

pub(crate) fn current_node_claims(
    home: &AgentsHome,
) -> Result<std::collections::HashMap<String, Result<Option<String>, String>>, String> {
    let registry = crate::state::load_registry(&home.registry_json())
        .map_err(|error| format!("registry read failed: {error}"))?;
    let mut sessions = std::collections::HashMap::<String, bool>::new();
    for row in &registry.entries {
        let Some(session_id) = registry_session_id(row) else {
            continue;
        };
        sessions.entry(session_id.to_string()).or_insert(!matches!(
            row.status,
            crate::AgentStatus::Exited
                | crate::AgentStatus::PermanentDead
                | crate::AgentStatus::Failed
                | crate::AgentStatus::Orphaned
        ));
    }
    let live_sessions: std::collections::HashSet<String> = sessions
        .into_iter()
        .filter_map(|(session_id, live)| live.then_some(session_id))
        .collect();
    if live_sessions.is_empty() {
        return Ok(std::collections::HashMap::new());
    }
    let records = crate::claims::list_strict(Some("node:"), None, false)
        .map_err(|error| format!("node claim read failed: {error}"))?;
    let mut owned = std::collections::HashMap::<String, Vec<String>>::new();
    let mut errors = std::collections::HashMap::<String, String>::new();
    for record in &records {
        let Some(session_id) = record
            .session_id
            .as_deref()
            .filter(|session_id| live_sessions.contains(*session_id))
        else {
            continue;
        };
        match crate::claims::status(&record.key, None).0 {
            crate::claims::ClaimState::Live => {
                if let Some(node) = record.key.strip_prefix("node:") {
                    owned
                        .entry(session_id.to_string())
                        .or_default()
                        .push(node.to_string());
                }
            }
            crate::claims::ClaimState::Corrupted => {
                errors
                    .entry(session_id.to_string())
                    .or_insert_with(|| format!("node claim {} is unreadable", record.key));
            }
            crate::claims::ClaimState::Free
            | crate::claims::ClaimState::Suspect
            | crate::claims::ClaimState::Stale => {}
        }
    }
    let mut claims = std::collections::HashMap::new();
    for session_id in live_sessions {
        if let Some(error) = errors.remove(&session_id) {
            claims.insert(session_id, Err(error));
            continue;
        }
        let mut nodes = owned.remove(&session_id).unwrap_or_default();
        nodes.sort();
        let claim = match nodes.as_slice() {
            [node] => Ok(Some(node.clone())),
            [] => Ok(None),
            _ => Err(format!(
                "session {session_id} owns multiple live node claims: {}",
                nodes.join(", ")
            )),
        };
        claims.insert(session_id, claim);
    }
    Ok(claims)
}

pub(crate) fn registry_session_id(entry: &crate::state::RegistryEntry) -> Option<&str> {
    entry
        .harness_session_id
        .as_deref()
        .or(entry.session_id.as_deref())
}

pub(crate) fn message(watch: &Watch) -> String {
    if watch.blocker == "build" {
        return format!(
            "Automatic wake from the fno daemon for node {}. The cargo build this session parked on at the admission door finished, or its park deadline passed. Read its output and continue. If it still waits, stop it and push instead: commit, run `fno do pr push`, and CI builds and tests it. Every cargo job and its place in line: fno doctor builds.",
            watch.node
        );
    }
    let task = watch.task_id.as_deref().unwrap_or("missing; do not guess");
    let blocker = match watch.reason.as_deref() {
        Some(blocker @ ("ci" | "review" | "merge_slot" | "local")) => blocker,
        _ => watch.blocker.as_str(),
    };
    format!(
        "Automatic watch-expiry notice from the fno daemon. Your {blocker} watch expired for node {} (reason: {}). Harness task id: {task}. Check the watcher output or status. If a local task is still running, kill it; then continue or re-arm a bounded watch.",
        watch.node,
        watch.reason.as_deref().unwrap_or("watch deadline expired")
    )
}

pub(crate) fn run_pass(home: &AgentsHome) -> Result<(), String> {
    run_pass_with(home, &mut crate::burn_watch::run_command)
}

/// `runner` is injected so replay tests record wakes instead of sending them.
pub(crate) fn run_pass_with(
    home: &AgentsHome,
    runner: crate::burn_watch::Runner<'_>,
) -> Result<(), String> {
    let mut runner = runner;
    let now_ms = millis_now();
    let evidence = read_evidence(home, now_ms)?;
    let due = watches(&evidence)
        .into_iter()
        .filter(|watch| should_wake(watch, now_ms, &evidence))
        .collect::<Vec<_>>();
    if due.is_empty() {
        return Ok(());
    }
    let claims = current_node_claims(home)?;
    let emitter =
        crate::events::EventEmitter::new(crate::daemon::global_events_path(home), "daemon");
    let mut delivery_failures = 0usize;
    let mut eligibility_failures = 0usize;
    let mut receipt_failures = 0usize;
    let mut first_error = None;
    for watch in due {
        let node = match claims.get(&watch.session_id).cloned().unwrap_or(Ok(None)) {
            Ok(Some(node)) => node,
            Ok(None) => continue,
            Err(error) => {
                eligibility_failures += 1;
                first_error.get_or_insert(error);
                continue;
            }
        };
        if !watch.node.is_empty() && node != watch.node {
            continue;
        }
        let mut claimed_watch = watch.clone();
        claimed_watch.node = node;
        let text = message(&claimed_watch);
        let (delivered, via) = crate::burn_watch::wake_with_text(
            &watch.session_id,
            false,
            &text,
            "burn-watch",
            &mut runner,
        );
        if !delivered && via != "durable" {
            delivery_failures += 1;
            first_error.get_or_insert_with(|| {
                format!(
                    "wake delivery failed for watch {} via {via}",
                    watch.event_id
                )
            });
            continue;
        }
        if let Err(error) = emitter.emit(
            WAKE_EVENT,
            &serde_json::json!({
                "session_id": watch.session_id,
                "node": claimed_watch.node,
                "watch_event_id": watch.event_id,
                "blocker": claimed_watch.blocker,
                "task_id": watch.task_id,
                "expires_at_ms": watch.expires_at_ms,
                "delivered": delivered,
                "via": via,
                "reason": watch.reason.as_deref().unwrap_or("watch deadline expired")
            }),
        ) {
            receipt_failures += 1;
            first_error.get_or_insert_with(|| {
                format!(
                    "wake {} was accepted via {via}, but its receipt could not be stored: {error}",
                    watch.event_id
                )
            });
        }
    }
    if eligibility_failures > 0 || delivery_failures > 0 || receipt_failures > 0 {
        return Err(format!(
            "eligibility failures={eligibility_failures}, delivery failures={delivery_failures}, receipt failures={receipt_failures}; accepted wakes with missing receipts retry at least once and may redeliver; first error: {}",
            first_error.unwrap_or_else(|| "unknown watch-expiry failure".into())
        ));
    }
    Ok(())
}

pub fn maybe_tick(arm: &Arm, home: AgentsHome) {
    {
        let mut last = arm
            .last_tick
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if last.is_some_and(|tick| tick.elapsed() < INTERVAL)
            || arm.in_flight.swap(true, Ordering::SeqCst)
        {
            return;
        }
        *last = Some(Instant::now());
    }
    let flag = Arc::clone(&arm.in_flight);
    tokio::task::spawn_blocking(move || {
        let _gate = crate::daemon::SweepGate(flag);
        if let Err(error) = run_pass(&home) {
            eprintln!("watch-expiry: {error}");
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stage(path: &std::path::Path, row: Value) {
        use std::io::Write;
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .unwrap();
        writeln!(f, "{row}").unwrap();
    }

    #[test]
    fn a_daemon_stop_ends_its_session_watch_and_spares_a_same_name_respawn() {
        // The daemon journals agent_stopped in the agents home, by row name;
        // the hooks journal watches globally, by session. A stop read only
        // from the global stream, or never tied to a session, let quiet
        // recovery and the expiry arm wake a worker its lead stopped.
        let td = tempfile::TempDir::new().unwrap();
        let home = AgentsHome::at(td.path().join("agents"));
        let now = chrono::Utc::now();
        let at = |minutes: i64| (now - chrono::Duration::minutes(minutes)).to_rfc3339();
        let row = |name: &str, session: &str, born: String| {
            let mut entry = crate::state::RegistryEntry::default();
            entry.name = name.into();
            entry.harness_session_id = Some(session.into());
            entry.created_at = born;
            entry
        };
        let mut registry = crate::state::Registry::default();
        registry
            .entries
            .push(row("t-x-1-glm", "s-stopped", at(120)));
        // Born after its name's stop: a respawn the old stop must not touch.
        registry.entries.push(row("t-x-2-glm", "s-respawn", at(5)));
        home.ensure_root().unwrap();
        crate::registry_store::seed_raw(
            &home.registry_json(),
            serde_json::to_vec(&registry).unwrap(),
        );
        let watch = |session: &str| {
            serde_json::json!({"ts": at(30), "type": WATCH_IDLE, "source": "hook",
                "data": {"session_id": session, "node": "x-1", "blocker": "ci",
                    "expires_at_ms": now.timestamp_millis() + 60_000}})
        };
        stage(
            &crate::daemon::global_events_path(&home),
            watch("s-stopped"),
        );
        stage(&home.events_jsonl(), watch("s-daemon"));
        let stop = |name: &str, minutes: i64| {
            serde_json::json!({"ts": at(minutes), "type": "agent_stopped",
                "source": "daemon", "data": {"name": name}})
        };
        stage(&home.events_jsonl(), stop("t-x-1-glm", 10));
        stage(&home.events_jsonl(), stop("t-x-2-glm", 10));

        let evidence = read_evidence(&home, now.timestamp_millis() + 1000).unwrap();
        let stopped: Vec<_> = evidence
            .iter()
            .filter(|e| e.kind == "agent_stopped")
            .map(|e| {
                (
                    e.data.get("name").and_then(Value::as_str).unwrap(),
                    e.session_id.as_deref(),
                )
            })
            .collect();
        assert_eq!(
            stopped,
            vec![("t-x-1-glm", Some("s-stopped")), ("t-x-2-glm", None)]
        );
        let found = watches(&evidence);
        assert_eq!(
            found
                .iter()
                .map(|w| w.session_id.as_str())
                .collect::<Vec<_>>(),
            vec!["s-stopped"],
            "a watch is the hook's to arm, never the daemon journal's"
        );
        assert!(!is_current_watch(&found[0], &evidence));
        let seqs: Vec<i64> = evidence.iter().map(|e| e.seq).collect();
        assert_eq!(seqs, (0..evidence.len() as i64).collect::<Vec<_>>());
    }
}
