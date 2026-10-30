//! The merge_close arm: one bare reconcile sweep per beat closes a merged
//! PR's node with no worker alive.
//!
//! DonePRGreen means the PR merged; it does not mean the node closed (the
//! specimen: a lead stopped the worker four minutes after its merge, and
//! the node read in_review until a peer ran reconcile by hand). The
//! merge command closes its node in a child bound to the worker's lifetime, so
//! a killed, crashed, or 429-dead worker takes the closer with it. This arm
//! is the session-free floor: it shells the same bare
//! `fno backlog reconcile --json` the SessionStart hook runs, with no
//! session in the loop. The closure decision stays in
//! `drift_scan::scan_merge_drift`; nothing here re-implements it.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::loops_pause::DispatchPause;
use crate::paths::AgentsHome;

/// The arm's beat, matching `RECONCILE_THROTTLE_SECONDS` in
/// scripts/lib/reconcile-throttle.sh, so this arm and the SessionStart hook
/// share one cadence. Worst-case closure lag after a stranded merge is one
/// interval plus one sweep.
pub const MERGE_CLOSE_INTERVAL_S: u64 = 900;

/// The arm as the daemon holds it: cadence stamp plus one-in-flight gate.
#[derive(Default)]
pub struct Arm {
    last_tick: Mutex<Option<Instant>>,
    in_flight: Arc<AtomicBool>,
}

/// One tick's outcome: the tick row's `acted`, `skip_reason` and `detail`.
pub struct CloseOutcome {
    pub acted: u64,
    pub skip_reason: Option<String>,
    pub detail: String,
}

fn short(text: &str) -> String {
    text.chars().take(200).collect()
}

// `unreadable reconcile json` alone was a dead end: the row named the
// symptom but threw away the one thing a reader needs to diagnose it - what
// the command actually printed. Carry a bounded snippet instead.
fn unreadable(stdout: &str) -> CloseOutcome {
    let sample = short(stdout.trim());
    let sample = if sample.is_empty() {
        "<empty>"
    } else {
        &sample
    };
    CloseOutcome {
        acted: 0,
        skip_reason: Some("error".to_string()),
        detail: format!("unreadable reconcile json: {sample}"),
    }
}

/// A rate-limit refusal in a failure row is a backoff, not the sweep's
/// failure: the row reports a wait. Covers the local request budget and
/// GitHub's own refusal, since both clear by waiting.
fn is_rate_limit_wait(row: &serde_json::Value) -> bool {
    row.get("error")
        .or_else(|| row.get("reason"))
        .and_then(serde_json::Value::as_str)
        .map(str::to_ascii_lowercase)
        .is_some_and(|error| {
            error.contains("outcome=budget_refused")
                || error.contains("gh budget:")
                || error.contains("rate limit")
        })
}

/// Turn one reconcile run into a tick outcome. Pure, so tests drive it with
/// strings. The command pretty-prints its JSON across many lines, so the whole
/// trimmed stdout parses as one object - never a search for a `{` line. A
/// run that did not report a readable `closed` array is an error, never a
/// zero: a fabricated zero reads as "in sync" (the stale-summary rule). A
/// GitHub budget refusal is a wait, not a failure: it is counted under
/// `waiting` and never counts toward the failure verdict.
pub fn outcome_from_reconcile(run: Result<String, String>) -> CloseOutcome {
    let stdout = match run {
        Ok(stdout) => stdout,
        Err(why) => {
            return CloseOutcome {
                acted: 0,
                skip_reason: Some("error".to_string()),
                detail: short(&format!("reconcile: {why}")),
            };
        }
    };
    let Ok(parsed) = serde_json::from_str::<serde_json::Value>(stdout.trim()) else {
        return unreadable(&stdout);
    };
    if parsed.get("held").and_then(serde_json::Value::as_bool) == Some(true) {
        // The single-flight receipt: another reconcile owns the scope, and a
        // held tick is not an error.
        let holder = parsed
            .get("holder")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("unknown");
        let held_for_s = parsed
            .get("held_for_s")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0);
        // The key lets the dead-holder repair name the exact claim; an older
        // receipt without it keeps the old text.
        let detail = match parsed.get("key").and_then(serde_json::Value::as_str) {
            Some(key) => format!("flight {key} held by {holder} for {held_for_s}s"),
            None => format!("flight held by {holder} for {held_for_s}s"),
        };
        return CloseOutcome {
            acted: 0,
            skip_reason: Some("held".to_string()),
            detail,
        };
    }
    let Some(closed) = parsed.get("closed").and_then(serde_json::Value::as_array) else {
        return unreadable(&stdout);
    };
    let count_of = |key: &str| {
        parsed
            .get(key)
            .and_then(serde_json::Value::as_array)
            .map_or(0, |a| a.len()) as u64
    };
    let acted = closed.len() as u64;
    let rows = parsed
        .get("failures")
        .and_then(serde_json::Value::as_array)
        .cloned()
        .unwrap_or_default();
    let waiting = rows.iter().filter(|row| is_rate_limit_wait(row)).count() as u64;
    let failures = rows.len() as u64 - waiting;
    // Evidence rows carry their cause under `reason`; a rate-limited
    // evidence read waits the same way.
    let evidence_rows = parsed
        .get("supersession_evidence_failures")
        .and_then(serde_json::Value::as_array)
        .cloned()
        .unwrap_or_default();
    let evidence_waiting = evidence_rows
        .iter()
        .filter(|row| is_rate_limit_wait(row))
        .count() as u64;
    let evidence = evidence_rows.len() as u64 - evidence_waiting;
    let waiting = waiting + evidence_waiting;
    let mut detail = format!(
        "closed={acted} promise_unmet={} failures={failures}",
        count_of("promise_unmet")
    );
    if waiting > 0 {
        detail.push_str(&format!(" waiting={waiting} (GitHub rate limit)"));
    }
    if evidence > 0 {
        detail.push_str(&format!(" evidence_failures={evidence}"));
    }
    if let Some(first) = rows.iter().find(|row| !is_rate_limit_wait(row)) {
        let node = first
            .get("node_id")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("?");
        let pr = match first.get("pr_number").and_then(serde_json::Value::as_u64) {
            Some(0) => "-".to_string(),
            Some(n) => format!("#{n}"),
            None => "?".to_string(),
        };
        let why = first
            .get("error")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("?");
        detail.push_str(&format!("; first: {node} PR {pr}: {why}"));
    }
    let catchup = parsed.get("sync_catchup");
    let outcome = catchup
        .and_then(|s| s.get("outcome"))
        .and_then(serde_json::Value::as_str);
    if matches!(outcome, Some("unknown") | Some("failed") | Some("error")) {
        let why = catchup
            .and_then(|s| s.get("detail"))
            .and_then(serde_json::Value::as_str)
            .unwrap_or("");
        detail.push_str(&format!("; sync_catchup={}: {why}", outcome.unwrap_or("?")));
    }
    CloseOutcome {
        acted,
        skip_reason: if acted > 0 {
            None
        } else if failures > 0 || evidence > 0 {
            Some("failures".to_string())
        } else if waiting > 0 {
            Some("rate_limited".to_string())
        } else {
            Some("none_to_close".to_string())
        },
        detail: short(&detail),
    }
}

/// The production runner: the bare sweep, cwd-free (the graph is one store).
/// The native door, not the wheel: the Python command is deleted, and the arm's
/// JSON contract lives in the Rust arm now.
fn run_reconcile() -> Result<String, String> {
    let output = std::process::Command::new(crate::scrape::fno_bin())
        // The launcher stamp: the sweep's closes and outside-merge records
        // are machine work, so actor_kind reads sweep, not user.
        .env(crate::decision_trace::ACTOR_KIND_STAMP_ENV, "sweep")
        .args(["backlog", "reconcile", "--json"])
        .stdin(std::process::Stdio::null())
        .output()
        .map_err(|e| format!("spawn: {e}"))?;
    reconcile_result(output.status.code(), &output.stdout, &output.stderr)
}

/// Classify a run from its bytes. Exit 0 is the payload. Exit 4 is the
/// command's documented partial status: its JSON is already complete on stdout
/// and holds the failures, so it is kept instead of discarded for the
/// stderr tail. Anything else keeps the stderr lines that diagnose it. The
/// last line alone was a dead end: a Rust panic spends its first line on
/// the location, its second on the message, and its last on the
/// `RUST_BACKTRACE` hint, so the tick read "exit 101: note: run with..."
/// and the diagnosis was thrown away. Carry the first two lines and the
/// last; the hint is worth nothing without the panic above it.
fn reconcile_result(code: Option<i32>, stdout: &[u8], stderr: &[u8]) -> Result<String, String> {
    if code == Some(0) {
        return Ok(String::from_utf8_lossy(stdout).into_owned());
    }
    if code == Some(4)
        && serde_json::from_slice::<serde_json::Value>(stdout).is_ok_and(|v| v.is_object())
    {
        return Ok(String::from_utf8_lossy(stdout).into_owned());
    }
    let lines: Vec<String> = stderr
        .split(|b| *b == b'\n')
        .filter(|l| !l.is_empty())
        .map(|l| String::from_utf8_lossy(l).into_owned())
        .collect();
    let mut keep: Vec<&str> = lines.iter().take(2).map(String::as_str).collect();
    if let Some(last) = lines.last() {
        if !keep.contains(&last.as_str()) {
            keep.push(last);
        }
    }
    Err(format!("exit {}: {}", code.unwrap_or(-1), keep.join(" | ")))
}

/// One pass of the arm body: the pause gate, then the run, then exactly one
/// tick row - on every path, so a silent tick cannot be told from one that
/// never ran. The pause and the runner are parameters so tests drive both
/// without shelling out or touching the account-wide pause sentinel (the
/// `stale_sweep` closure pattern).
fn emit_one(
    home: &AgentsHome,
    pause: &DispatchPause,
    backoff_s: i64,
    run: impl FnOnce() -> Result<String, String>,
) -> CloseOutcome {
    let outcome = if pause.is_paused() {
        // A close runs advance_dependents; a pause means no dispatch.
        CloseOutcome {
            acted: 0,
            skip_reason: Some(pause.skip_reason().to_string()),
            detail: pause.detail(),
        }
    } else if backoff_s > 0 {
        CloseOutcome {
            acted: 0,
            skip_reason: Some("rate_limited".to_string()),
            detail: format!("GitHub rate limit backoff, {backoff_s}s left; the sweep waits"),
        }
    } else {
        outcome_from_reconcile(run())
    };
    let journal = crate::loop_runtime::Journal::new_raw(
        home.events_jsonl(),
        crate::daemon::global_events_path(home),
    );
    crate::tick_ledger::emit_tick(
        &journal,
        "merge_close",
        crate::tick_ledger::SCHED_DAEMON,
        outcome.acted,
        outcome.skip_reason.as_deref(),
        Some(&outcome.detail),
        MERGE_CLOSE_INTERVAL_S,
    );
    outcome
}

/// The daemon-facing wrapper: due-check plus one-in-flight gate (the
/// `machine_watch::maybe_tick` shape). The sweep spends tens of seconds of
/// CPU, so the body runs off-loop.
pub fn maybe_tick(arm: &Arm, home: AgentsHome) {
    maybe_tick_with(arm, home, run_reconcile);
}

fn maybe_tick_with(
    arm: &Arm,
    home: AgentsHome,
    run: impl FnOnce() -> Result<String, String> + Send + 'static,
) {
    let interval = Duration::from_secs(MERGE_CLOSE_INTERVAL_S);
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
    tokio::task::spawn_blocking(move || {
        let _gate = crate::daemon::SweepGate(flag);
        // A sweep inside the backoff only spends reads the limit refuses.
        // Unarmed (a test), the machine's real ledger is never read.
        let backoff_s = if crate::gh_budget::admission_armed() {
            crate::gh_budget::snapshot(&crate::gh_budget::ledger_path(), crate::gh_budget::now_ms())
                .backoff_remaining_s
        } else {
            0
        };
        emit_one(&home, &crate::loops_pause::dispatch_pause(), backoff_s, run);
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn home() -> AgentsHome {
        static SEQ: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let n = SEQ.fetch_add(1, Ordering::SeqCst);
        let dir = std::env::temp_dir().join(format!("merge-close-test-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        AgentsHome::at(dir.join("home"))
    }

    #[test]
    fn reconcile_rows() {
        let stdout = r#"{
  "closed": [],
  "promise_unmet": [],
  "failures": [{"node_id": "x-1", "pr_number": 7, "error": "gh pr view refused"}],
  "supersession_evidence_failures": [],
  "sync_catchup": {"outcome": "unknown", "detail": "gh unavailable or unauthenticated"}
}"#;
        let o = outcome_from_reconcile(Ok(stdout.to_string()));
        assert_eq!(o.acted, 0);
        assert_eq!(o.skip_reason.as_deref(), Some("failures"));
        assert_eq!(
            o.detail,
            "closed=0 promise_unmet=0 failures=1; first: x-1 PR #7: gh pr view refused; sync_catchup=unknown: gh unavailable or unauthenticated"
        );

        // A fresh or not-run catch-up is health, not a finding.
        let stdout = r#"{
  "closed": [],
  "promise_unmet": [],
  "failures": [],
  "sync_catchup": {"outcome": "fresh", "detail": "canonical in sync"}
}"#;
        let o = outcome_from_reconcile(Ok(stdout.to_string()));
        assert_eq!(o.skip_reason.as_deref(), Some("none_to_close"));
        assert_eq!(o.detail, "closed=0 promise_unmet=0 failures=0");

        // A supersession-evidence-only run exits 4 too.
        let stdout = r#"{
  "closed": [],
  "promise_unmet": [],
  "failures": [],
  "supersession_evidence_failures": [{"node_id": "x-2", "reason": "stale evidence"}]
}"#;
        let o = outcome_from_reconcile(Ok(stdout.to_string()));
        assert_eq!(o.acted, 0);
        assert_eq!(o.skip_reason.as_deref(), Some("failures"));
        assert!(
            o.detail.contains("evidence_failures=1"),
            "detail: {}",
            o.detail
        );

        // A rate-limited evidence read waits; it is not an evidence failure.
        let stdout = r#"{
  "closed": [],
  "failures": [],
  "supersession_evidence_failures": [{"node_id": "x-2", "reason": "API rate limit exceeded"}]
}"#;
        let o = outcome_from_reconcile(Ok(stdout.to_string()));
        assert_eq!(o.skip_reason.as_deref(), Some("rate_limited"));
        assert_eq!(
            o.detail,
            "closed=0 promise_unmet=0 failures=0 waiting=1 (GitHub rate limit)"
        );
    }

    #[test]
    fn gate_rows() {
        let arm = Arm::default();
        let h = home();
        *arm.last_tick.lock().unwrap() = Some(Instant::now());
        maybe_tick_with(&arm, h.clone(), || panic!("arm must be gated"));
        assert!(!h.events_jsonl().exists(), "a gated tick wrote no row");

        let arm = Arm::default();
        let h = home();
        arm.in_flight.store(true, Ordering::SeqCst);
        maybe_tick_with(&arm, h.clone(), || panic!("arm must be gated"));
        assert!(!h.events_jsonl().exists(), "a gated tick wrote no row");

        let h = home();
        let pause = DispatchPause::Manual {
            state: "paused".to_string(),
            detail: "loops paused by test".to_string(),
        };
        let o = emit_one(&h, &pause, 0, || panic!("a paused tick spawns no child"));
        assert_eq!(o.acted, 0);
        assert_eq!(o.skip_reason.as_deref(), Some("loops_paused"));
        assert_eq!(o.detail, "loops paused by test");
        let log = crate::events::committed_journal_text(&h.events_jsonl());
        assert!(log.contains("\"arm\":\"merge_close\""), "log: {log}");
        assert!(
            log.contains("\"skip_reason\":\"loops_paused\""),
            "log: {log}"
        );

        let h = home();
        let pause = DispatchPause::Clear;
        let o = emit_one(&h, &pause, 0, || {
            Ok("{\"closed\": [], \"promise_unmet\": [], \"failures\": []}".to_string())
        });
        assert_eq!(o.acted, 0);
        assert_eq!(o.skip_reason.as_deref(), Some("none_to_close"));
        let log = crate::events::committed_journal_text(&h.events_jsonl());
        assert_eq!(
            log.matches("\"arm\":\"merge_close\"").count(),
            1,
            "log: {log}"
        );
        assert!(log.contains("\"interval_s\":900"), "log: {log}");
    }

    #[test]
    fn a_live_backoff_skips_the_sweep() {
        let h = home();
        let pause = DispatchPause::Clear;
        let o = emit_one(&h, &pause, 120, || panic!("a backoff tick spawns no child"));
        assert_eq!(o.acted, 0);
        assert_eq!(o.skip_reason.as_deref(), Some("rate_limited"));
        assert!(o.detail.contains("120s left"), "detail: {}", o.detail);
    }

    #[test]
    fn reconcile_child_carries_the_sweep_stamp() {
        let lock = crate::claims::test_env_lock();
        let _held = lock.lock().unwrap_or_else(|e| e.into_inner());
        let td = tempfile::TempDir::new().unwrap();
        let seen = td.path().join("stamp");
        // The fake fno exits 0 with empty stdout, which reconcile_result
        // keeps; it dumps the one env the real child must carry. The stub
        // goes through write_exec_stub: a sibling test's fork can hold a
        // plain write fd open and the exec fails with Text file busy.
        let script = crate::write_exec_stub(
            td.path(),
            "fake-fno",
            &format!(
                "#!/bin/sh\nprintf '%s' \"$FNO_ACTOR_KIND\" > '{}'\n",
                seen.display()
            ),
        );
        let prev_bin = std::env::var_os("FNO_BIN");
        let prev_stamp = std::env::var_os(crate::decision_trace::ACTOR_KIND_STAMP_ENV);
        std::env::set_var("FNO_BIN", &script);
        std::env::remove_var(crate::decision_trace::ACTOR_KIND_STAMP_ENV);
        let run = run_reconcile();
        match prev_bin {
            Some(v) => std::env::set_var("FNO_BIN", v),
            None => std::env::remove_var("FNO_BIN"),
        }
        if let Some(v) = prev_stamp {
            std::env::set_var(crate::decision_trace::ACTOR_KIND_STAMP_ENV, v);
        }
        run.expect("the fake fno exits 0");
        let stamp = std::fs::read_to_string(&seen).unwrap();
        assert_eq!(stamp, "sweep");
    }
}
