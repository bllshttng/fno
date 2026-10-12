//! The board_render arm: the board render is background work (law
//! d-bad5a42f), so a daemon tick owns it, never the write path. One tick
//! compares the store's version counter against the `rendered_version`
//! stamp for the canonical graph; a moved version runs the pass - `fno
//! backlog render-views`, which since the native port renders graph.md AND
//! the configured board targets - stamps the marker on success, and
//! journals `graph_render_failed` with backoff on failure. The pass and
//! its backoff moved here from graph_keeper.rs, whose resident-keeper
//! trigger now calls them in this module.

use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::paths::AgentsHome;

/// The arm's beat. Worst case a board reader waits one interval plus one
/// pass behind a landed write.
pub const BOARD_RENDER_INTERVAL_S: u64 = 60;

/// One render command's deadline. Kill on expiry: a wedged render command
/// costs the arm its interval, never the caller's 600s bound.
pub const RENDER_COMMAND_DEADLINE: Duration = Duration::from_secs(300);

/// The quiet window a render waits out after a write burst (the keeper's
/// settle floor; the backoff doubles from here).
pub(crate) const RENDER_SETTLE: Duration = Duration::from_secs(2);

/// The arm as the daemon holds it: cadence stamp, one-in-flight gate, and
/// the failure memory the backoff reads.
#[derive(Default)]
pub struct Arm {
    last_tick: Mutex<Option<Instant>>,
    in_flight: Arc<AtomicBool>,
    state: Arc<ArmState>,
}

#[derive(Default)]
struct ArmState {
    failures: AtomicU32,
    last_attempt: Mutex<Option<Instant>>,
}

/// Backoff between render attempts after consecutive failures: double from
/// the settle floor each time, capped at ten minutes. A permanently failing
/// pass then costs one spawn per cap window, not one per tick. (Moved
/// verbatim from graph_keeper.rs; the keeper's trigger calls it here.)
pub(crate) fn render_backoff(failures: u32) -> Duration {
    let shift = failures.min(10);
    RENDER_SETTLE
        .checked_mul(1u32 << shift)
        .unwrap_or(Duration::from_secs(600))
        .min(Duration::from_secs(600))
}

/// The daemon-facing wrapper: due-check plus one-in-flight gate (the
/// `merge_close::maybe_tick` shape). The pass shells two subprocesses, so
/// the body runs off-loop.
pub fn maybe_tick(arm: &Arm, home: AgentsHome) {
    maybe_tick_with(arm, home, run_render_pass);
}

fn maybe_tick_with(
    arm: &Arm,
    home: AgentsHome,
    run: impl FnOnce() -> Result<(), (i32, String)> + Send + 'static,
) {
    let interval = Duration::from_secs(BOARD_RENDER_INTERVAL_S);
    {
        let mut last = arm.last_tick.lock().unwrap_or_else(|e| e.into_inner());
        if last.is_some_and(|t| t.elapsed() < interval)
            || arm.in_flight.swap(true, Ordering::SeqCst)
        {
            return;
        }
        *last = Some(Instant::now());
    }
    let flag = Arc::clone(&arm.in_flight);
    let state = Arc::clone(&arm.state);
    tokio::task::spawn_blocking(move || {
        let _gate = crate::daemon::SweepGate(flag);
        // The graph resolves here, once, on the blocking thread: tests pass
        // their own path through emit_one, so no test depends on process
        // env another test module may flip mid-run.
        let graph = crate::backlog::settings::graph_path();
        emit_one(&home, &graph, &state, run);
    });
}

/// One pass of the arm body: the version check, the owed pass, and exactly
/// one tick row on every owed-or-failed path, so a silent tick cannot be
/// told from one that never ran. A store with no version counter is the one
/// silent return: nothing was ever written, nothing to render, and a row
/// every interval would flood the journal on a store-less machine. The
/// runner is a parameter so tests drive the body without shelling out (the
/// merge_close closure pattern).
fn emit_one(
    home: &AgentsHome,
    graph: &Path,
    state: &ArmState,
    run: impl FnOnce() -> Result<(), (i32, String)>,
) {
    use Ordering::SeqCst;
    let Ok(current) = crate::backlog::version(graph) else {
        return;
    };
    let rendered = crate::backlog::rendered_version(graph).ok().flatten();
    let journal = crate::loop_runtime::Journal::new_raw(
        home.events_jsonl(),
        crate::daemon::global_events_path(home),
    );
    let emit = |acted: u64, skip: Option<&str>, detail: &str| {
        crate::tick_ledger::emit_tick(
            &journal,
            "board_render",
            crate::tick_ledger::SCHED_DAEMON,
            acted,
            skip,
            Some(detail),
            BOARD_RENDER_INTERVAL_S,
        );
    };
    if rendered.as_deref() == Some(current.as_str()) {
        emit(0, Some("fresh"), &format!("rendered {current}"));
        return;
    }
    // Backoff gate: after failures, wait out the doubled quiet window since
    // the last attempt before paying for another one. A fresh write still
    // renders at the settle floor: the backoff only delays RETRIES of a
    // pass that already failed at some version.
    let failures = state.failures.load(SeqCst);
    if failures > 0 {
        let due = state
            .last_attempt
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .map_or(true, |last| last.elapsed() >= render_backoff(failures));
        if !due {
            emit(
                0,
                Some("backoff"),
                &format!("{failures} failure(s); retry past the backoff window"),
            );
            return;
        }
    }
    *state.last_attempt.lock().unwrap_or_else(|e| e.into_inner()) = Some(Instant::now());
    match run() {
        Ok(()) => {
            if let Err(error) = crate::backlog::set_rendered_version(graph, &current) {
                state.failures.fetch_add(1, SeqCst);
                render_failed(
                    home,
                    &current,
                    -1,
                    &format!("rendered_version stamp failed: {error}"),
                );
                emit(0, Some("error"), &format!("stamp failed: {error}"));
            } else {
                state.failures.store(0, SeqCst);
                emit(1, None, &format!("rendered {current}"));
            }
        }
        Err((exit, stderr_tail)) => {
            state.failures.fetch_add(1, SeqCst);
            render_failed(home, &current, exit, &stderr_tail);
            emit(0, Some("error"), &format!("exit {exit}: {stderr_tail}"));
        }
    }
}

/// The failure event the AC names: `graph_render_failed`, daemon-sourced,
/// into BOTH journals the tick row lands in - the project file the folds
/// read and the machine-global path the daemon's consumers read.
fn render_failed(home: &AgentsHome, current: &str, exit: i32, stderr_tail: &str) {
    let payload = serde_json::json!({
        "version": current,
        "exit": exit,
        "stderr_tail": stderr_tail,
    });
    for path in [home.events_jsonl(), crate::daemon::global_events_path(home)] {
        let emitter = crate::events::EventEmitter::new(path, "daemon");
        let _ = emitter.emit("graph_render_failed", &payload);
    }
}

/// Run the canonical view pass as one bounded subprocess: `fno backlog
/// render-views`, which since the native port renders graph.md AND the
/// configured board targets (through `fno board-render`). The daemon is a
/// Rust worker: the pass is config- and vault-aware, so it shells to the
/// `fno` CLI the same way the mux's retired replay did. `Err` carries the
/// exit code and the stderr tail for the failure event.
pub(crate) fn run_render_pass() -> Result<(), (i32, String)> {
    run_render_pass_within(RENDER_COMMAND_DEADLINE)
}

/// [`run_render_pass`] with the per-command deadline as a parameter, so a
/// test can arm a short one.
fn run_render_pass_within(deadline: Duration) -> Result<(), (i32, String)> {
    let bin = match std::env::var_os("FNO_BIN") {
        Some(v) => std::path::PathBuf::from(v),
        None => {
            // A lib unit test declared no fno: refuse before the PATH walk,
            // the same declared-only rule scrape::fno_bin applies.
            if cfg!(test) {
                return Err((-1, "no fno declared under a unit test".into()));
            }
            let path = match std::env::var_os("PATH") {
                Some(p) => p,
                None => return Err((-1, "PATH is unset; the view pass cannot run".into())),
            };
            match std::env::split_paths(&path)
                .map(|dir| dir.join("fno"))
                .find(|candidate| candidate.is_file())
            {
                Some(p) => p,
                None => {
                    return Err((-1, "no fno CLI on PATH; the view pass cannot run".into()));
                }
            }
        }
    };
    // One command since the native port: `render-views` renders graph.md
    // AND the configured board targets (through `fno board-render`), so
    // the pass fails when that one command fails.
    let mut cmd = std::process::Command::new(bin);
    cmd.args(["backlog", "render-views"])
        .stdin(std::process::Stdio::null());
    match crate::bounded_cmd::output_with_timeout(cmd, deadline.as_secs()) {
        Some(out) if out.status.success() => Ok(()),
        Some(out) => {
            let code = out.status.code().unwrap_or(-1);
            let text = String::from_utf8_lossy(&out.stderr);
            // Tail only, char-safe: the emitter caps payloads at 500 B, so
            // the tail must leave room for the envelope fields.
            let tail: String = {
                let skip = text.chars().count().saturating_sub(300);
                text.chars().skip(skip).collect()
            };
            Err((code, tail))
        }
        // Deadline fired: bounded_cmd killed the process group, so the arm
        // is never parked behind a wedged render.
        None => Err((
            -1,
            format!(
                "backlog render-views timed out after {}s",
                deadline.as_secs()
            ),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::path::Path;

    /// One published row, so the store carries a version counter (the
    /// graph_keeper seed pattern).
    fn seed_render_store(graph: &Path) -> String {
        crate::graph_store::locked_mutate(
            graph,
            crate::graph_store::MutateInput {
                entries: vec![json!({
                    "id": "x-rend", "title": "Render me", "slug": "x-rend",
                    "type": "feature", "status": "idea", "priority": "p2",
                    "created_at": "2026-09-11T00:00:00+00:00"
                })],
                canonical_path: None,
                base_version: crate::graph_store::base_version(graph).unwrap(),
                plan_rungs: None,
            },
            Duration::from_secs(2),
        )
        .unwrap();
        crate::backlog::version(graph).unwrap()
    }

    fn arm_home() -> (Arm, AgentsHome, std::path::PathBuf) {
        static SEQ: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let n = SEQ.fetch_add(1, Ordering::SeqCst);
        let dir = std::env::temp_dir().join(format!("board-render-arm-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        (Arm::default(), AgentsHome::at(dir.join("home")), dir)
    }

    /// A moved version renders once and stamps, and a failing pass stays
    /// unstamped, journals `graph_render_failed`, and backs off: the two
    /// phases share one seeded store, the retry phase past the backoff
    /// window shows the stamp landing.
    #[test]
    fn the_arm_renders_a_moved_version_once_and_backs_off_failures() {
        let (arm, home, dir) = arm_home();
        let graph = dir.join("graph.json");
        seed_render_store(&graph);
        // The failing phase: the pass errs, nothing stamps, the failure
        // journals, and the retry waits out the backoff window.
        let mut runs = 0;
        emit_one(&home, &graph, &arm.state, || {
            runs += 1;
            Err((-1, "boom".to_string()))
        });
        assert_eq!(runs, 1, "the owed pass ran");
        assert_eq!(
            crate::backlog::rendered_version(&graph).ok().flatten(),
            None,
            "a failed pass stamps nothing"
        );
        emit_one(&home, &graph, &arm.state, || {
            runs += 1;
            Ok(())
        });
        assert_eq!(
            runs, 1,
            "the next tick inside the backoff window ran no pass"
        );
        let log = crate::events::committed_journal_text(&home.events_jsonl());
        assert!(log.contains("graph_render_failed"), "log: {log}");
        assert!(log.contains("\"skip_reason\":\"error\""), "log: {log}");
        assert!(log.contains("\"skip_reason\":\"backoff\""), "log: {log}");
        // The happy phase, past the backoff: the pass runs once, stamps the
        // current version, and a second tick at the same version runs no
        // pass.
        *arm.state
            .last_attempt
            .lock()
            .unwrap_or_else(|e| e.into_inner()) =
            Some(Instant::now() - render_backoff(1) - Duration::from_secs(1));
        emit_one(&home, &graph, &arm.state, || {
            runs += 1;
            Ok(())
        });
        assert_eq!(runs, 2, "the retry ran once past the backoff window");
        let current = crate::backlog::version(&graph).unwrap();
        assert_eq!(
            crate::backlog::rendered_version(&graph)
                .ok()
                .flatten()
                .as_deref(),
            Some(current.as_str()),
            "the stamp rode the current version"
        );
        emit_one(&home, &graph, &arm.state, || {
            runs += 1;
            Ok(())
        });
        assert_eq!(runs, 2, "a second tick at the same version ran no pass");
        let log = crate::events::committed_journal_text(&home.events_jsonl());
        assert!(log.contains("\"arm\":\"board_render\""), "log: {log}");
        assert!(log.contains("\"acted\":1"), "log: {log}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
