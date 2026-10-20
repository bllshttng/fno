//! `evals-arm` tests: one per surviving deleted phase test plus the two
//! claim paths. No test runs the real bank: `--fno-bin` is a fixture
//! shell script, and tick mode runs run mode through an injected spawner that
//! re-parses the child argv in-process.
use super::*;

use serde_json::Value;
use tempfile::TempDir;

fn base_args(tmp: &TempDir, extra: &[&str]) -> Vec<String> {
    let mut v: Vec<String> = [
        "--history",
        &tmp.path().join("history.jsonl").to_string_lossy(),
        "--events",
        &tmp.path().join("events.jsonl").to_string_lossy(),
        "--fno-bin",
        &tmp.path().join("fno-bin.sh").to_string_lossy(),
        "--schedule-days",
        "7",
        "--stale-days",
        "7",
        "--claims-root",
        &tmp.path().join("claims-root").to_string_lossy(),
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    v.extend(extra.iter().map(|s| s.to_string()));
    v
}

fn reg_row(task_id: &str, ts_rfc3339: &str) -> String {
    serde_json::json!({
        "ts": ts_rfc3339, "task_id": task_id, "tier": "regression",
        "pass": true, "reason": "", "duration_s": 1.0, "repeat_index": 0,
        "bank_rev": null, "worker_provider": null,
    })
    .to_string()
}

/// Fixture `fno-bin`: `doctor evals run` appends ROWS (stamped one second
/// after the run window opens, so `ts >= started` holds at second
/// granularity); `inbox notify` records the call.
fn write_fixture(tmp: &TempDir, rows: &[String], history: &Path, notify_log: &Path) {
    let mut script = String::from("#!/bin/sh\ncase \"$1/$2\" in\n  doctor/evals)\n    sleep 1\n");
    // Rows baked as literal JSON; the ts is stamped at run time (second
    // granularity), which `sleep 1` places strictly after `--started`. The
    // JSON rides a double-quoted echo so `$(date)` substitutes; its own
    // double quotes are escaped for the shell.
    for row in rows {
        let stamped = row
            .replace("\"ts\":\"\"", "\"ts\":\"__TS__\"")
            .replace('"', "\\\"")
            .replace("__TS__", "$(date -u +%Y-%m-%dT%H:%M:%SZ)");
        script += &format!(
            "    echo \"{}\" >> '{}'\n",
            stamped,
            history.to_string_lossy()
        );
    }
    script += "    ;;\n  inbox/notify)\n    echo \"$*\" >> '";
    script += &notify_log.to_string_lossy();
    script += "'\n    ;;\nesac\n";
    crate::write_exec_stub(tmp.path(), "fno-bin.sh", &script);
}

/// The injected spawner: re-parses the child argv (the same contract the real
/// binary exec satisfies) and runs run mode on a detached thread.
fn thread_spawner() -> impl Fn(&[String]) -> Result<u32, String> {
    |argv: &[String]| {
        assert_eq!(argv[1], "evals-arm");
        assert_eq!(argv[2], "--run");
        let child = parse_args(&argv[2..])?;
        std::thread::spawn(move || {
            run_run_mode(&child);
        });
        Ok(std::process::id())
    }
}

fn wait_for_row(events: &Path, kind: &str, timeout: Duration) -> Option<Value> {
    // Committed rows, not journal bytes: the store cutover commits ticks in
    // the store beside the journal.
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        let rows =
            crate::event_store::query_events(events, &crate::event_store::EventQuery::default())
                .unwrap_or_default();
        for r in rows.iter().rev() {
            if let Ok(v) = serde_json::from_str::<Value>(&r.line) {
                if v.get("type").and_then(Value::as_str) == Some(kind) {
                    return Some(v);
                }
            }
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    None
}

fn count_kind(events: &Path, kind: &str) -> usize {
    crate::event_store::query_events(events, &crate::event_store::EventQuery::default())
        .unwrap_or_default()
        .iter()
        .filter(|r| {
            serde_json::from_str::<Value>(&r.line)
                .map(|v| v.get("type").and_then(Value::as_str) == Some(kind))
                .unwrap_or(false)
        })
        .count()
}

fn claim_state(root: &Path) -> claims::ClaimState {
    claims::status(CLAIM_KEY, Some(root)).0
}

fn ok_gate() -> GateReading {
    GateReading {
        slots: 0,
        max_live: 5,
        ram_gb: Some(16.0),
        min_free_gb: 1.0,
    }
}

fn full_gate() -> GateReading {
    GateReading {
        slots: 5,
        max_live: 5,
        ram_gb: Some(16.0),
        min_free_gb: 1.0,
    }
}

/// The clear verdict as a closure run_tick_mode reads; the incident test
/// passes `armed` instead.
fn clear() -> crate::loops_pause::DispatchPause {
    crate::loops_pause::DispatchPause::Clear
}

fn armed() -> crate::loops_pause::DispatchPause {
    crate::loops_pause::DispatchPause::FleetIncident {
        generation: 9,
        reason: "rustc storm".to_string(),
        holds: vec!["spawns".to_string(), "tests".to_string()],
    }
}

// AC2-HP: summary 9 days old, schedule 7, gate headroom, fixture appends 3
// rows -> one evals_scheduled_run with task_count 3, claim free at the end.
#[test]
fn fires_past_window_journals_run_and_frees_claim() {
    let tmp = TempDir::new().unwrap();
    let history = tmp.path().join("history.jsonl");
    let notify_log = tmp.path().join("notify.log");
    let events = tmp.path().join("events.jsonl");
    let rows: Vec<String> = (1..=3)
        .map(|i| reg_row(&format!("reg-task-{i}"), ""))
        .collect();
    write_fixture(&tmp, &rows, &history, &notify_log);
    let summary = r#"{"age_days": 9.0, "never_ran": false}"#;
    let args = base_args(&tmp, &["--summary-json", summary]);
    let o = parse_args(&args).unwrap();

    run_tick_mode(&o, &ok_gate, &clear, &thread_spawner());

    let row = wait_for_row(&events, "evals_scheduled_run", Duration::from_secs(20))
        .expect("scheduled-run row within 20s");
    let data = row.get("data").unwrap();
    assert_eq!(data.get("task_count"), Some(&Value::from(3)));
    assert_eq!(data.get("passes"), Some(&Value::from(3)));
    assert_eq!(data.get("window_days"), Some(&Value::from(7)));
    let claims_root = tmp.path().join("claims-root");
    assert_eq!(claim_state(&claims_root), claims::ClaimState::Free);
}

// The durable stop outranks everything: an armed incident holds the arm
// even with gate headroom. The slot counters lost their veto - the bank's
// reserved slot launches at a full fleet - so the positive control is a
// launch, not the fleet_full skip it used to read.
#[test]
fn incident_outranks_the_gate_and_a_full_fleet_launches_on_the_reservation() {
    let tmp = TempDir::new().unwrap();
    let notify_log = tmp.path().join("notify.log");
    let events = tmp.path().join("events.jsonl");
    write_fixture(&tmp, &[], &tmp.path().join("history.jsonl"), &notify_log);
    let summary = r#"{"age_days": 15.0, "never_ran": false}"#;
    let args = base_args(&tmp, &["--summary-json", summary]);
    let o = parse_args(&args).unwrap();
    let never = |_argv: &[String]| -> Result<u32, String> {
        panic!("spawner must not run under an armed incident")
    };

    let code = run_tick_mode(&o, &ok_gate, &armed, &never);

    assert_eq!(code, 0);
    // The skip token rides the stdout receipt; the journal row carries the
    // pause detail.
    let text = crate::events::committed_journal_text(&events);
    assert!(
        text.contains("fleet incident stopped at generation 9"),
        "{text}"
    );
    assert!(!text.contains("fleet_full"), "{text}");

    // Positive control: a clear verdict and a FULL gate still launch the
    // bank on its reserved slot.
    let launches = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let count = launches.clone();
    let counting = move |_argv: &[String]| -> Result<u32, String> {
        count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Ok(std::process::id())
    };
    run_tick_mode(&o, &full_gate, &clear, &counting);
    assert_eq!(
        launches.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "a full fleet launches the bank on its reserved slot"
    );
}

// The attention row: an escalation-grade skip files exactly one fleet task
// naming the run that clears it, a second skip dedups, and a bank that
// records rows closes it.
#[test]
fn escalated_skip_files_one_task_and_a_bank_run_closes_it() {
    let tmp = TempDir::new().unwrap();
    let events = tmp.path().join("events.jsonl");
    let notify_log = tmp.path().join("notify.log");
    let history = tmp.path().join("history.jsonl");
    let rows: Vec<String> = (1..=2)
        .map(|i| reg_row(&format!("reg-task-{i}"), ""))
        .collect();
    write_fixture(&tmp, &rows, &history, &notify_log);
    let summary = r#"{"age_days": 15.0, "never_ran": false}"#;
    let store = tmp.path().join("questions.jsonl");
    let mut args = base_args(&tmp, &["--summary-json", summary]);
    args.push("--questions-store".into());
    args.push(store.to_string_lossy().into_owned());
    let o = parse_args(&args).unwrap();
    let open = || crate::fleet_task::open_tasks(&store).unwrap();

    // The armed incident skips an escalation-grade bank: the row files.
    run_tick_mode(&o, &ok_gate, &armed, &|_a| panic!("must not spawn"));
    let filed = open();
    assert_eq!(filed.len(), 1, "one task after the first skip: {filed:?}");
    assert_eq!(filed[0].lane, "evals");
    assert_eq!(filed[0].key, "regression-bank");
    assert_eq!(filed[0].run, "fno doctor evals run --tier regression -y");

    // The next skip dedups: still one row.
    run_tick_mode(&o, &ok_gate, &armed, &|_a| panic!("must not spawn"));
    assert_eq!(open().len(), 1, "the second skip dedups");

    // A bank that records rows closes the task.
    run_tick_mode(&o, &ok_gate, &clear, &thread_spawner());
    wait_for_row(&events, "evals_scheduled_run", Duration::from_secs(20))
        .expect("scheduled-run row within 20s");
    assert!(
        open().is_empty(),
        "the bank ran, so the task closes: {:?}",
        open()
    );
}

// A run-mode plumbing failure on a FRESH bank journals the stale row but
// files no attention row: the chore tracks an escalation-grade bank only.
#[test]
fn fresh_bank_run_failure_journals_but_files_no_task() {
    let tmp = TempDir::new().unwrap();
    let events = tmp.path().join("events.jsonl");
    let notify_log = tmp.path().join("notify.log");
    // A fixture whose evals run exits nonzero.
    let mut script = String::from("#!/bin/sh\ncase \"$1/$2\" in\n  doctor/evals)\n    exit 3\n    ;;\n  inbox/notify)\n    echo \"$*\" >> '");
    script += &notify_log.to_string_lossy();
    script += "'\n    ;;\nesac\n";
    crate::write_exec_stub(tmp.path(), "fno-bin.sh", &script);
    let store = tmp.path().join("questions.jsonl");
    let args = base_args(
        &tmp,
        &[
            "--run",
            "--started",
            &Utc::now().to_rfc3339(),
            "--run-timeout-s",
            "5",
            "--summary-json",
            r#"{"age_days": 1.0, "never_ran": false}"#,
            "--questions-store",
            &store.to_string_lossy(),
        ],
    );
    let o = parse_args(&args).unwrap();

    run_run_mode(&o);

    assert_eq!(
        count_kind(&events, "evals_stale"),
        1,
        "the plumbing failure journals"
    );
    assert!(!store.exists(), "a fresh bank files no attention row");
}

#[test]
fn no_notice_when_emit_fails() {
    let tmp = TempDir::new().unwrap();
    let notify_log = tmp.path().join("notify.log");
    write_fixture(&tmp, &[], &tmp.path().join("history.jsonl"), &notify_log);
    // The STORE path is a directory: the emitter's commit cannot open it, so
    // the journal cannot carry the receipt and no notice may ride.
    let mut args = base_args(&tmp, &["--summary-json", r#"{"age_days": 15.0}"#]);
    let idx = args.iter().position(|a| a == "--events").unwrap();
    let events_dir = tmp.path().join("events-dir");
    std::fs::create_dir(&events_dir).unwrap();
    std::fs::create_dir(tmp.path().join("events-dir.db")).unwrap();
    args[idx + 1] = events_dir.to_string_lossy().into_owned();
    let o = parse_args(&args).unwrap();

    // The armed incident is the refusal that reaches journal_stale; a full
    // gate no longer refuses (the bank's reserved slot launches).
    run_tick_mode(&o, &ok_gate, &armed, &|_a| panic!("must not spawn"));

    assert!(!notify_log.exists(), "no notice without its journal row");
}

fn acquire_with(tmp: &TempDir, pid: u32) {
    let out = claims::acquire(
        CLAIM_KEY,
        &pid.to_string(),
        claims::AcquireOpts {
            host: None,
            pid: Some(pid),
            pid_unavailable: false,
            ttl_ms: Some(3_600_000),
            reason: None,
            metadata: None,
            pid_provenance: None,
            identity: None,
            root: Some(tmp.path().join("claims-root")),
            events_dir: Some(tmp.path().to_path_buf()),
        },
    );
    assert!(
        matches!(out, claims::AcquireOutcome::Acquired(_)),
        "{out:?}"
    );
}

// AC2-EDGE, live holder: in_flight, nothing journaled, no child.
#[test]
fn live_holder_skips_in_flight() {
    let tmp = TempDir::new().unwrap();
    acquire_with(&tmp, std::process::id());
    let args = base_args(&tmp, &["--summary-json", r#"{"age_days": 9.0}"#]);
    let o = parse_args(&args).unwrap();

    run_tick_mode(&o, &ok_gate, &clear, &|_a| panic!("must not spawn"));

    assert_eq!(
        count_kind(&tmp.path().join("events.jsonl"), "evals_stale"),
        0
    );
    assert_eq!(
        claim_state(&tmp.path().join("claims-root")),
        claims::ClaimState::Live
    );
}

// The process-group kill: a fixture that outlives the run budget is killed
// and journaled as a timeout, and the claim is released.
#[test]
fn run_timeout_kills_group_journals_timeout() {
    let tmp = TempDir::new().unwrap();
    let events = tmp.path().join("events.jsonl");
    let notify_log = tmp.path().join("notify.log");
    // A fixture whose evals run sleeps far past the budget.
    let mut script = String::from("#!/bin/sh\ncase \"$1/$2\" in\n  doctor/evals)\n    sleep 30\n    ;;\n  inbox/notify)\n    echo \"$*\" >> '");
    script += &notify_log.to_string_lossy();
    script += "'\n    ;;\nesac\n";
    crate::write_exec_stub(tmp.path(), "fno-bin.sh", &script);
    let args = base_args(
        &tmp,
        &[
            "--run",
            "--started",
            &Utc::now().to_rfc3339(),
            "--run-timeout-s",
            "1",
        ],
    );
    let o = parse_args(&args).unwrap();

    run_run_mode(&o);

    let row = wait_for_row(&events, "evals_stale", Duration::from_secs(5)).unwrap();
    assert_eq!(row["data"]["reason"], "timeout");
    assert_eq!(
        claim_state(&tmp.path().join("claims-root")),
        claims::ClaimState::Free
    );
}
