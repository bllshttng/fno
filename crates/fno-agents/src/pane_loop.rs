//! One armed loop per pane session: the schedule store, the
//! `loops arm|disarm|armed` verbs, and the `pane_loop` daemon arm that types
//! each due prompt into the row's pane.
//!
//! The arm keys on the row's live mux address and the hosted harness's own
//! confirm source, never on a harness name: a pane harness whose mail lane
//! confirms gets ticks, and a harness whose lane refuses records the refusal
//! token as the tick's outcome. A schedule never outlives its row: a gone or
//! terminal row, or a schedule past expiry, deletes the file.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::json;

use crate::paths::AgentsHome;
use crate::state::Registry;

/// The daemon arm's cadence, mirrored in the `KNOWN_ARMS` row.
const INTERVAL_S: u64 = 30;
/// The shortest interval an operator may arm: a typo must not turn into a
/// keystroke flood into someone's pane.
const MIN_EVERY_S: u64 = 60;
/// An arm dies a week after it was armed, so an abandoned schedule cannot
/// tick forever.
const EXPIRY_S: u64 = 7 * 24 * 60 * 60;
const EVENT: &str = "pane_loop_tick";
const ARM_NAME: &str = "pane_loop";
const DIR_NAME: &str = "pane-loops";

/// One schedule file: `<agents home>/pane-loops/<session>.json`.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct Schedule {
    session: String,
    every_s: u64,
    prompt: String,
    armed_at: u64,
    next_at: u64,
    expires_at: u64,
}

/// The daemon arm's state: the cadence stamp plus the one-in-flight gate.
#[derive(Default)]
pub struct Arm {
    last_tick: Mutex<Option<Instant>>,
    in_flight: Arc<AtomicBool>,
}

fn loops_dir(home: &AgentsHome) -> PathBuf {
    home.root().join(DIR_NAME)
}

fn schedule_path(home: &AgentsHome, session: &str) -> PathBuf {
    loops_dir(home).join(format!("{session}.json"))
}

/// One store entry as read from disk; `None` marks an unparseable file so
/// the pass can retire it instead of skipping it forever.
fn read_schedules(home: &AgentsHome) -> Vec<(PathBuf, Option<Schedule>)> {
    let mut out: Vec<(PathBuf, Option<Schedule>)> = Vec::new();
    let Ok(entries) = std::fs::read_dir(loops_dir(home)) else {
        return out;
    };
    let mut paths: Vec<PathBuf> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
        .collect();
    paths.sort();
    for path in paths {
        let parsed = std::fs::read_to_string(&path)
            .ok()
            .and_then(|body| serde_json::from_str(&body).ok());
        out.push((path, parsed));
    }
    out
}

/// Write through a temp file and rename, so a tick never reads a half file.
fn write_schedule(path: &Path, schedule: &Schedule) -> Result<(), String> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|error| error.to_string())?;
    }
    let body = serde_json::to_string(schedule).map_err(|error| error.to_string())?;
    let temp = path.with_extension(format!("tmp{}", std::process::id()));
    std::fs::write(&temp, body).map_err(|error| error.to_string())?;
    if let Err(error) = std::fs::rename(&temp, path) {
        let _ = std::fs::remove_file(&temp);
        return Err(error.to_string());
    }
    Ok(())
}

fn now_epoch() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn render_every(every_s: u64) -> String {
    if every_s % 86_400 == 0 {
        format!("{}d", every_s / 86_400)
    } else if every_s % 3_600 == 0 {
        format!("{}h", every_s / 3_600)
    } else {
        format!("{}m", every_s / 60)
    }
}

fn render_epoch(secs: u64) -> String {
    chrono::DateTime::<chrono::Utc>::from_timestamp(secs as i64, 0)
        .map(|date| date.to_rfc3339())
        .unwrap_or_else(|| format!("{secs}"))
}

/// `--every <N>m|h|d` into seconds, under a 1m floor.
fn parse_every(raw: &str) -> Result<u64, String> {
    let split = raw.len().saturating_sub(1);
    let (digits, unit) = raw.split_at(split);
    let count: u64 = digits
        .parse()
        .map_err(|_| format!("--every {raw} is not <N>m|h|d"))?;
    let secs = match unit {
        "m" => count * 60,
        "h" => count * 3_600,
        "d" => count * 86_400,
        _ => return Err(format!("--every {raw} is not <N>m|h|d")),
    };
    if secs < MIN_EVERY_S {
        return Err(format!("--every {raw} is under the 1m floor"));
    }
    Ok(secs)
}

/// The schedule file keys on the id verbatim, so it must be one safe
/// filename component.
fn safe_session_token(token: &str) -> Result<(), String> {
    if token.is_empty()
        || token == "."
        || token == ".."
        || token.contains('/')
        || token.contains('\\')
        || token.contains('\0')
    {
        return Err("--session must be one id or row name, not a path".to_string());
    }
    Ok(())
}

fn load_row(home: &AgentsHome, token: &str) -> Result<crate::state::RegistryEntry, String> {
    let registry = crate::state::load_registry(&home.registry_json())
        .map_err(|error| format!("registry unreadable: {error}"))?;
    registry
        .find_name_or_full_session_id(token)
        .cloned()
        .ok_or_else(|| format!("no registry row matches {token}"))
}

/// `loops arm --session <id> --every <N>m|h|d -- <prompt>`
pub fn run_arm(args: &[String]) -> i32 {
    match arm_schedule(args) {
        Ok(line) => {
            println!("{line}");
            0
        }
        Err(message) => {
            eprintln!("fno-agents loops arm: {message}");
            2
        }
    }
}

fn arm_schedule(args: &[String]) -> Result<String, String> {
    let mut session: Option<&String> = None;
    let mut every: Option<&String> = None;
    let mut index = 0;
    let mut prompt: Option<String> = None;
    while index < args.len() {
        match args[index].as_str() {
            "--session" => {
                session = Some(args.get(index + 1).ok_or("--session needs a value")?);
                index += 2;
            }
            "--every" => {
                every = Some(args.get(index + 1).ok_or("--every needs a value")?);
                index += 2;
            }
            "--" => {
                prompt = Some(args[index + 1..].join(" "));
                break;
            }
            other => {
                return Err(format!(
                "unknown argument {other}; usage: arm --session <id> --every <N>m|h|d -- <prompt>"
            ))
            }
        }
    }
    let token = session.ok_or("--session <id> is required")?;
    safe_session_token(token)?;
    let every_s = parse_every(every.ok_or("--every <N>m|h|d is required")?)?;
    let prompt = prompt
        .map(|text| text.trim().to_string())
        .filter(|text| !text.is_empty())
        .ok_or("a prompt after -- is required")?;
    let home = AgentsHome::from_env();
    let row = load_row(&home, token)?;
    let stored = row
        .harness_session_id
        .clone()
        .unwrap_or_else(|| token.clone());
    let armed_at = now_epoch();
    let schedule = Schedule {
        session: stored.clone(),
        every_s,
        prompt,
        armed_at,
        next_at: armed_at,
        expires_at: armed_at + EXPIRY_S,
    };
    write_schedule(&schedule_path(&home, &stored), &schedule)?;
    let display = if row.short_id.is_empty() {
        stored
    } else {
        row.short_id.clone()
    };
    Ok(format!(
        "pane loop armed for {display}: every {} until {}",
        render_every(every_s),
        render_epoch(schedule.expires_at)
    ))
}

/// `loops disarm --session <id>`: exit 0 whether or not one was armed.
pub fn run_disarm(args: &[String]) -> i32 {
    match disarm_session(args) {
        Ok(line) => {
            println!("{line}");
            0
        }
        Err(message) => {
            eprintln!("fno-agents loops disarm: {message}");
            2
        }
    }
}

fn disarm_session(args: &[String]) -> Result<String, String> {
    let mut session: Option<&String> = None;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--session" => {
                session = Some(args.get(index + 1).ok_or("--session needs a value")?);
                index += 2;
            }
            other => {
                return Err(format!(
                    "unknown argument {other}; usage: disarm --session <id>"
                ))
            }
        }
    }
    let token = session.ok_or("--session <id> is required")?;
    safe_session_token(token)?;
    let home = AgentsHome::from_env();
    let mut path = schedule_path(&home, token);
    if !path.exists() {
        // The arm stores under the row's harness session id, so a disarm
        // naming the row itself still finds the file.
        if let Ok(row) = load_row(&home, token) {
            if let Some(stored) = &row.harness_session_id {
                path = schedule_path(&home, stored);
            }
        }
    }
    if path.exists() {
        std::fs::remove_file(&path).map_err(|error| error.to_string())?;
        Ok(format!("pane loop disarmed for {token}"))
    } else {
        Ok(format!("no armed pane loop for {token}"))
    }
}

/// `loops armed [--json]`
pub fn run_armed(args: &[String]) -> i32 {
    let home = AgentsHome::from_env();
    let mut schedules: Vec<Schedule> = read_schedules(&home)
        .into_iter()
        .filter_map(|(_, parsed)| parsed)
        .collect();
    schedules.sort_by(|a, b| a.session.cmp(&b.session));
    if args.iter().any(|arg| arg == "--json") {
        println!(
            "{}",
            serde_json::to_string(&schedules).unwrap_or_else(|_| "[]".to_string())
        );
    } else if schedules.is_empty() {
        println!("no armed pane loops");
    } else {
        for schedule in &schedules {
            println!(
                "pane loop armed for {}: every {}, next {}, expires {}",
                schedule.session,
                render_every(schedule.every_s),
                render_epoch(schedule.next_at),
                render_epoch(schedule.expires_at)
            );
        }
    }
    0
}

/// One due schedule's action this pass, as the tick event's outcome token.
/// `Some(token)` marks the schedule acted; `None` leaves an undone schedule
/// alone.
fn tick_one(
    journal: &crate::loop_runtime::Journal,
    registry: &Registry,
    path: &Path,
    parsed: &Option<Schedule>,
    now: u64,
) -> Option<&'static str> {
    let Some(mut schedule) = parsed.clone() else {
        let _ = std::fs::remove_file(path);
        // No session id survives in the body, so the file stem stands in:
        // the retirement stays visible in the feed.
        let stem = path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .unwrap_or("unknown")
            .to_string();
        let _ = journal.append(
            EVENT,
            json!({"session": stem, "outcome": "corrupt", "every_s": 0}),
        );
        return Some("corrupt");
    };
    // The deliver path below cannot reuse `emit`: it mutates the schedule
    // first, so it appends its own event with the post-write values.
    let emit = |outcome: &'static str| -> Option<&'static str> {
        let _ = journal.append(
            EVENT,
            json!({"session": schedule.session, "outcome": outcome, "every_s": schedule.every_s}),
        );
        Some(outcome)
    };
    if now >= schedule.expires_at {
        let _ = std::fs::remove_file(path);
        return emit("expired");
    }
    let Some(row) = registry.find_name_or_full_session_id(&schedule.session) else {
        let _ = std::fs::remove_file(path);
        return emit("row-gone");
    };
    if crate::state::row_is_terminal(row) {
        let _ = std::fs::remove_file(path);
        return emit("row-gone");
    };
    if now < schedule.next_at {
        return None;
    }
    let Some(mux) = &row.mux else {
        return emit("no-pane");
    };
    let harness = row.harness_name();
    let state = crate::mail_cli::pane_prepare::screen_state(&mux.session, mux.pane_id, harness);
    match state {
        Ok(state) if state == "working" || state == "blocked" => return emit("busy"),
        Ok(_) => {}
        Err(_) => return emit("unreadable"),
    }
    let read_armed_at = schedule.armed_at;
    let read_next_at = schedule.next_at;
    let address = format!("{}:{}", mux.session, mux.pane_id);
    let outcome = match crate::mail_inject::deliver_via_pane(
        &schedule.session,
        &address,
        (!harness.is_empty()).then_some(harness),
        &schedule.prompt,
        crate::mail_inject::DEFAULT_ATTEMPTS,
        crate::mail_inject::DEFAULT_INTERVAL_MS,
        crate::mail_inject::enter_delay_for_harness(harness),
    ) {
        Ok(()) => "delivered",
        Err(reason) => reason,
    };
    schedule.next_at = now + schedule.every_s;
    // A disarm or re-arm during the delivery window owns the file: write
    // the advanced deadline back only over the schedule this tick read.
    let unchanged = std::fs::read_to_string(path)
        .ok()
        .and_then(|body| serde_json::from_str::<Schedule>(&body).ok())
        .is_some_and(|on_disk| {
            on_disk.armed_at == read_armed_at && on_disk.next_at == read_next_at
        });
    if unchanged && write_schedule(path, &schedule).is_err() {
        // The deadline cannot advance: retire the schedule rather than let
        // the same due tick fire again on the next pass.
        let _ = std::fs::remove_file(path);
    }
    let _ = journal.append(
        EVENT,
        json!({"session": schedule.session, "outcome": outcome, "every_s": schedule.every_s}),
    );
    Some(outcome)
}

/// One pass's receipt for the arm's tick row.
pub(crate) struct PassReport {
    pub acted: u64,
    pub error: Option<String>,
}

/// Read every schedule file and act on the due ones. `paused` gates before
/// any read, so the sentinel halts the arm even on a broken store.
pub(crate) fn run_pass_checked(home: &AgentsHome, now: u64, paused: bool) -> PassReport {
    if paused {
        return PassReport {
            acted: 0,
            error: None,
        };
    }
    let registry = match crate::state::load_registry(&home.registry_json()) {
        Ok(registry) => registry,
        // An unreadable registry must never mass-delete schedules: report
        // the pass as failed and leave every file alone.
        Err(error) => {
            return PassReport {
                acted: 0,
                error: Some(format!("registry unreadable: {error}")),
            };
        }
    };
    let journal = crate::loop_runtime::Journal::new_raw(
        home.events_jsonl(),
        crate::daemon::global_events_path(home),
    );
    let mut acted = 0;
    for (path, parsed) in read_schedules(home) {
        if tick_one(&journal, &registry, &path, &parsed, now).is_some() {
            acted += 1;
        }
    }
    PassReport { acted, error: None }
}

pub fn maybe_tick(arm: &Arm, home: AgentsHome) {
    let mut last = arm.last_tick.lock().unwrap_or_else(|e| e.into_inner());
    if last.is_some_and(|tick| tick.elapsed() < Duration::from_secs(INTERVAL_S))
        || arm.in_flight.swap(true, Ordering::SeqCst)
    {
        return;
    }
    *last = Some(Instant::now());
    let flag = Arc::clone(&arm.in_flight);
    tokio::task::spawn_blocking(move || {
        let _gate = crate::daemon::SweepGate(flag);
        let journal = crate::loop_runtime::Journal::new_raw(
            home.events_jsonl(),
            crate::daemon::global_events_path(&home),
        );
        let paused = crate::loops_pause::is_paused();
        let report = run_pass_checked(&home, now_epoch(), paused);
        if paused {
            crate::tick_ledger::emit_tick(
                &journal,
                ARM_NAME,
                crate::tick_ledger::SCHED_DAEMON,
                0,
                Some("paused"),
                None,
                INTERVAL_S,
            );
        } else if let Some(error) = report.error {
            crate::tick_ledger::emit_tick(
                &journal,
                ARM_NAME,
                crate::tick_ledger::SCHED_DAEMON,
                report.acted,
                Some("unreadable"),
                Some(&error),
                INTERVAL_S,
            );
        } else {
            crate::tick_ledger::emit_tick(
                &journal,
                ARM_NAME,
                crate::tick_ledger::SCHED_DAEMON,
                report.acted,
                None,
                None,
                INTERVAL_S,
            );
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    struct Fixture {
        _guard: crate::AgentsHomeEnvGuard,
        _dir: tempfile::TempDir,
        home: AgentsHome,
        scratch: PathBuf,
        frame: PathBuf,
        sends: PathBuf,
    }

    impl Fixture {
        fn new() -> Self {
            let dir = tempfile::tempdir().unwrap();
            let home = AgentsHome::at(dir.path().join("agents"));
            home.ensure_root().unwrap();
            let _guard = crate::AgentsHomeEnvGuard::set(home.root());
            let scratch = dir.path().join("scratch");
            std::fs::create_dir_all(&scratch).unwrap();
            let frame = scratch.join("frame.txt");
            std::fs::write(&frame, IDLE_FRAME).unwrap();
            let sends = scratch.join("sends.log");
            Self {
                _guard,
                _dir: dir,
                home,
                scratch,
                frame,
                sends,
            }
        }

        fn seed_row(&self, row: Value) {
            crate::registry_store::seed_raw(
                self.home.registry_json(),
                json!({
                    "schema_version": crate::state::REGISTRY_SCHEMA_VERSION,
                    "agents": [row],
                })
                .to_string(),
            );
        }

        fn footnote_row(&self, session: &str, status: &str) -> Value {
            json!({
                "name": "fn99", "status": status, "cwd": "/repo",
                "harness": "footnote", "harness_session_id": session,
                "created_at": "2026-10-17T00:00:00Z",
                "mux": {"session": "srv", "pane_id": 3},
            })
        }

        /// A fake `fno`: `mux pane read` answers with the frame file, every
        /// other call is a pane send whose bytes land in sends.log. The send
        /// also appends the `user_input` record the footnote confirm greps,
        /// so the confirm lands on its first poll with no sleep race.
        fn install_fake_fno(
            &self,
            prompt: &str,
        ) -> (tempfile::TempDir, Option<std::ffi::OsString>) {
            let stub_dir = tempfile::tempdir().unwrap();
            // The confirm greps the footnote transcript beside the sessions
            // root, so the record lands there and the confirm reads it on
            // its first poll.
            let dir = crate::footnote_harness::transcript::sessions_root()
                .join("proj")
                .join(SESSION);
            std::fs::create_dir_all(&dir).unwrap();
            let record = crate::footnote_harness::transcript::transcript_file(&dir, SESSION);
            std::fs::write(&record, "{}\n").unwrap();
            let script = [
                "#!/bin/sh".to_string(),
                "if [ \"$3\" = \"read\" ]; then".to_string(),
                format!("cat '{}'", self.frame.display()),
                "exit 0".to_string(),
                "fi".to_string(),
                format!("cat >> '{}'", self.sends.display()),
                format!("printf '\\n' >> '{}'", self.sends.display()),
                format!(
                    "echo '{{\"type\":\"user_input\",\"data\":{{\"text\":\"{prompt}\"}}}}' >> '{}'",
                    record.display()
                ),
                "exit 0".to_string(),
                "".to_string(),
            ]
            .join("\n");
            let fake = crate::write_exec_stub(stub_dir.path(), "fno", &script);
            let prior = std::env::var_os("FNO_BIN");
            std::env::set_var("FNO_BIN", &fake);
            (stub_dir, prior)
        }

        fn schedule(&self, session: &str, every_s: u64, now: u64) {
            write_schedule(
                &schedule_path(&self.home, session),
                &Schedule {
                    session: session.to_string(),
                    every_s,
                    prompt: CHECK_PROMPT.to_string(),
                    armed_at: now - every_s,
                    next_at: now - 1,
                    expires_at: now + 86_400,
                },
            )
            .unwrap();
        }

        fn stored_next_at(&self, session: &str) -> u64 {
            let body = std::fs::read_to_string(schedule_path(&self.home, session)).unwrap();
            let value: Value = serde_json::from_str(&body).unwrap();
            value["next_at"].as_u64().unwrap()
        }

        fn last_outcome(&self) -> Option<String> {
            // The journal writes into the event store beside the events
            // path, so the read goes through the same store, not the file.
            let query = crate::event_store::EventQuery {
                types: vec![EVENT.to_string()],
                ..Default::default()
            };
            let rows = crate::event_store::query_events(&self.home.events_jsonl(), &query)
                .unwrap_or_default();
            rows.last()
                .and_then(|row| serde_json::from_str::<Value>(&row.line).ok())
                .and_then(|event| event["data"]["outcome"].as_str().map(str::to_string))
        }
    }

    const IDLE_FRAME: &str = "Churned for 4s · done (0 files changed)";
    const ASK_FRAME: &str = "model reply\n allow Bash ls?  [y] yes  [n] no";
    const BUSY_FRAME: &str = "Churning… 12s  (esc to interrupt)";
    const CHECK_PROMPT: &str = "check CI";
    const SESSION: &str = "ses-1";
    const NOW: u64 = 1_760_000_000;
    const EVERY_S: u64 = 300;

    fn footnote_transcript() {
        let dir = crate::footnote_harness::transcript::sessions_root()
            .join("proj")
            .join(SESSION);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            crate::footnote_harness::transcript::transcript_file(&dir, SESSION),
            "{}\n",
        )
        .unwrap();
    }

    #[test]
    fn a_due_tick_types_the_prompt_and_moves_next_at() {
        let fixture = Fixture::new();
        fixture.seed_row(fixture.footnote_row(SESSION, "live"));
        footnote_transcript();
        let (_stub_dir, prior_bin) = fixture.install_fake_fno(CHECK_PROMPT);
        fixture.schedule(SESSION, EVERY_S, NOW);
        let report = run_pass_checked(&fixture.home, NOW, false);
        match prior_bin {
            Some(value) => std::env::set_var("FNO_BIN", value),
            None => std::env::remove_var("FNO_BIN"),
        }
        assert_eq!(report.acted, 1, "{:?}", report.error);
        let sends = std::fs::read_to_string(&fixture.sends).unwrap();
        assert!(sends.contains(CHECK_PROMPT), "{sends}");
        assert!(sends.contains('\r'), "the submit CR is missing: {sends}");
        assert_eq!(fixture.stored_next_at(SESSION), NOW + EVERY_S);
        assert_eq!(fixture.last_outcome(), Some("delivered".to_string()));
    }

    #[test]
    fn a_pass_skips_a_working_or_asking_pane() {
        let fixture = Fixture::new();
        fixture.seed_row(fixture.footnote_row(SESSION, "live"));
        let (_stub_dir, prior_bin) = fixture.install_fake_fno(CHECK_PROMPT);
        for frame in [ASK_FRAME, BUSY_FRAME] {
            std::fs::write(&fixture.frame, frame).unwrap();
            fixture.schedule(SESSION, EVERY_S, NOW);
            let report = run_pass_checked(&fixture.home, NOW, false);
            assert_eq!(report.acted, 1, "{frame:?}");
            assert_eq!(fixture.last_outcome(), Some("busy".to_string()));
            assert_eq!(fixture.stored_next_at(SESSION), NOW - 1, "next_at moved");
            let _ = std::fs::remove_file(schedule_path(&fixture.home, SESSION));
        }
        match prior_bin {
            Some(value) => std::env::set_var("FNO_BIN", value),
            None => std::env::remove_var("FNO_BIN"),
        }
        let sends = std::fs::read_to_string(&fixture.sends).unwrap_or_default();
        assert!(sends.is_empty(), "a busy pane was typed: {sends}");
    }

    #[test]
    fn a_schedule_never_outlives_its_row_or_its_expiry() {
        let fixture = Fixture::new();
        fixture.seed_row(fixture.footnote_row(SESSION, "exited"));
        fixture.schedule(SESSION, EVERY_S, NOW);
        let report = run_pass_checked(&fixture.home, NOW, false);
        assert_eq!(report.acted, 1);
        assert!(!schedule_path(&fixture.home, SESSION).exists(), "kept");
        assert_eq!(fixture.last_outcome(), Some("row-gone".to_string()));

        fixture.schedule(SESSION, EVERY_S, NOW);
        let path = schedule_path(&fixture.home, SESSION);
        let body = std::fs::read_to_string(&path).unwrap().replace(
            &format!("\"expires_at\":{}", NOW + 86_400),
            &format!("\"expires_at\":{}", NOW - 1),
        );
        std::fs::write(&path, body).unwrap();
        let report = run_pass_checked(&fixture.home, NOW, false);
        assert_eq!(report.acted, 1);
        assert!(!path.exists(), "kept");
        assert_eq!(fixture.last_outcome(), Some("expired".to_string()));
    }

    #[test]
    fn a_paused_pass_reads_and_types_nothing() {
        let fixture = Fixture::new();
        fixture.seed_row(fixture.footnote_row(SESSION, "live"));
        fixture.schedule(SESSION, EVERY_S, NOW);
        let report = run_pass_checked(&fixture.home, NOW, true);
        assert_eq!(report.acted, 0);
        assert!(report.error.is_none());
        assert!(schedule_path(&fixture.home, SESSION).exists(), "deleted");
        assert!(fixture.last_outcome().is_none());
    }

    #[test]
    fn a_corrupt_schedule_retires_visibly() {
        let fixture = Fixture::new();
        fixture.seed_row(fixture.footnote_row(SESSION, "live"));
        let path = schedule_path(&fixture.home, SESSION);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "{\"session\": ").unwrap();
        let report = run_pass_checked(&fixture.home, NOW, false);
        assert_eq!(report.acted, 1);
        assert!(!path.exists(), "kept");
        assert_eq!(fixture.last_outcome(), Some("corrupt".to_string()));
    }

    #[test]
    fn arm_refusals_write_nothing_and_a_valid_arm_lists() {
        let fixture = Fixture::new();
        fixture.seed_row(fixture.footnote_row(SESSION, "live"));
        let argv =
            |parts: &[&str]| -> Vec<String> { parts.iter().map(|p| p.to_string()).collect() };
        for parts in [
            vec!["--session", SESSION, "--every", "30s", "--", "x"],
            vec!["--session", SESSION, "--every", "5m", "--", "   "],
            vec!["--session", "nope", "--every", "5m", "--", "x"],
            vec!["--session", SESSION, "--every", "5m"],
        ] {
            let code = run_arm(&argv(&parts));
            assert_eq!(code, 2, "{parts:?}");
            assert!(!schedule_path(&fixture.home, SESSION).exists(), "{parts:?}");
        }
        let code = run_arm(&argv(&[
            "--session",
            SESSION,
            "--every",
            "5m",
            "--",
            "check",
            "CI",
        ]));
        assert_eq!(code, 0);
        let stored: Vec<Schedule> = read_schedules(&fixture.home)
            .into_iter()
            .filter_map(|(_, parsed)| parsed)
            .collect();
        assert_eq!(stored.len(), 1);
        assert_eq!(stored[0].session, SESSION);
        assert_eq!(stored[0].prompt, CHECK_PROMPT);
        assert_eq!(stored[0].every_s, EVERY_S);
        let code = run_disarm(&argv(&["--session", SESSION]));
        assert_eq!(code, 0);
        assert!(!schedule_path(&fixture.home, SESSION).exists(), "kept");
        let code = run_disarm(&argv(&["--session", SESSION]));
        assert_eq!(code, 0, "disarm without an arm still exits 0");
    }
}
