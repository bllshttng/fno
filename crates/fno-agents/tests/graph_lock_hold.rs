//! Graph-lock contention contracts: the hold is a small slice of a
//! whole-graph mutate (AC3/AC8), a foreground write lands while a
//! reconcile-shaped loop runs (AC7), a hold past 2 s journals one
//! `graph_lock_held` row from the holder's side (AC1), and a dead stamp
//! names the live queue head instead (AC2).

use std::sync::mpsc;
use std::time::{Duration, Instant};

use fno_agents::backlog::RowQuery;
use fno_agents::graph_store::{self, BoundedLock, MutateInput, DEFAULT_LOCK_TIMEOUT};
use serde_json::{json, Value};

/// The seeded store size the done-when names: the real graph's row count.
const SEEDED_ROWS: usize = 6_649;

fn seeded_store(n: usize) -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("graph.json");
    let rows: Vec<Value> = (0..n)
        .map(|i| {
            json!({
                "id": format!("x{i:04x}"),
                "slug": format!("node-{i}"),
                "title": format!("generated node {i}"),
                "type": "feature",
                "status": "ready",
                "priority": "p2",
                "domain": "code",
                "created_at": "2026-11-06T00:00:00Z",
            })
        })
        .collect();
    graph_store::seed_rows(&path, &rows).expect("seed lands");
    (dir, path)
}

/// One-field change on one row, the shape of a foreground `backlog update`.
fn bump_first_row(rows: &mut Vec<Value>) -> Result<bool, fno_agents::graph_store::StoreError> {
    if let Some(row) = rows.first_mut() {
        row["priority"] = json!("p1");
    }
    Ok(true)
}

/// AC3/AC8: a one-row mutate holds the lock for under a quarter of its wall
/// time. On the pre-change code the whole pure pipeline sits inside the
/// hold, so this test fails there (AC8) and pins the shrink (AC3) after.
#[test]
fn one_row_edit_holds_the_lock_for_a_small_slice_of_the_call() {
    let (_dir, path) = seeded_store(SEEDED_ROWS);
    let started = Instant::now();
    let outcome = graph_store::mutate_rows(&path, DEFAULT_LOCK_TIMEOUT, None, None, bump_first_row)
        .expect("mutate cycle completes")
        .expect("publish lands");
    let elapsed = started.elapsed();
    assert!(
        outcome.lock_held * 4 < elapsed,
        "lock held {:?} of {:?} wall time; the whole pipeline must not sit inside the hold",
        outcome.lock_held,
        elapsed
    );
}

/// AC7: a foreground `locked_mutate` lands within DEFAULT_LOCK_TIMEOUT
/// while a background thread publishes four whole-graph writes back to
/// back, the way reconcile does. A raw `locked_mutate` has no internal
/// retry, so the caller-side cycle mirrors `mutate_rows`: rebase on
/// Conflict and try again.
#[test]
fn a_foreground_writer_lands_behind_a_reconcile_shaped_loop() {
    let (_dir, path) = seeded_store(SEEDED_ROWS);
    let (tx, rx) = mpsc::channel::<()>();
    let bg_path = path.clone();
    let bg = std::thread::spawn(move || {
        for i in 0..4usize {
            let target = format!("x{i:04x}");
            graph_store::mutate_rows(&bg_path, DEFAULT_LOCK_TIMEOUT, None, None, move |rows| {
                for row in rows.iter_mut() {
                    if row.get("id").and_then(Value::as_str) == Some(target.as_str()) {
                        row["priority"] = json!("p1");
                    }
                }
                Ok(true)
            })
            .expect("background write lands");
            if i == 2 {
                tx.send(()).expect("handhold reaches the main thread");
            }
        }
    });
    rx.recv_timeout(Duration::from_secs(60))
        .expect("background loop reaches its third publish");
    let started = Instant::now();
    let mut landed = false;
    for attempt in 0..5 {
        let base = graph_store::base_version(&path).expect("version reads");
        let mut rows =
            graph_store::read_rows_strict(&path, &RowQuery::default()).expect("rows read");
        bump_first_row(&mut rows).expect("apply runs");
        match graph_store::locked_mutate(
            &path,
            MutateInput {
                entries: rows,
                canonical_path: None,
                base_version: base,
                plan_rungs: None,
            },
            DEFAULT_LOCK_TIMEOUT,
        ) {
            Ok(_) => {
                landed = true;
                break;
            }
            Err(_) if attempt + 1 < 5 => std::thread::sleep(Duration::from_millis(100)),
            Err(err) => panic!("foreground write never lands: {err}"),
        }
    }
    assert!(landed, "foreground write lands behind the loop");
    let budget = DEFAULT_LOCK_TIMEOUT + Duration::from_secs(10);
    assert!(
        started.elapsed() < budget,
        "foreground write took {:?}; the budget is {:?}",
        started.elapsed(),
        budget
    );
    bg.join().expect("background loop finishes");
}

/// AC1: a hold past 2 s journals exactly the holder's own `graph_lock_held`
/// row, with hold_ms, the pid, a command line and the named phases.
#[test]
fn a_hold_past_two_seconds_journals_one_graph_lock_held_row() {
    let dir = tempfile::tempdir().unwrap();
    let events = dir.path().join("events.jsonl");
    std::env::set_var("FNO_EVENTS_PATH", &events);
    let lock_dir = tempfile::tempdir().unwrap();
    let lock_graph = lock_dir.path().join("graph.json");
    let (tx, rx) = mpsc::channel::<()>();
    let holder_graph = lock_graph.clone();
    let holder = std::thread::spawn(move || {
        let lock =
            BoundedLock::acquire(&holder_graph, Duration::from_secs(10)).expect("holder acquires");
        let phase_started = Instant::now();
        std::thread::sleep(Duration::from_millis(60));
        lock.note_phase("test_phase", phase_started);
        tx.send(()).expect("held signal reaches the main thread");
        std::thread::sleep(Duration::from_millis(2_300));
        lock
    });
    rx.recv_timeout(Duration::from_secs(10))
        .expect("holder signals");
    drop(holder.join().expect("holder thread finishes"));
    let text = std::fs::read_to_string(&events).expect("events journal exists");
    let row = text
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).expect("each line parses"))
        .find(|row| row["type"] == "graph_lock_held")
        .expect("one graph_lock_held row lands");
    let data = &row["data"];
    assert!(
        data["hold_ms"].as_u64().unwrap() >= 2_000,
        "hold_ms records the hold: {data}"
    );
    assert_eq!(data["pid"].as_u64(), Some(u64::from(std::process::id())));
    assert!(
        data["command"].as_str().is_some_and(|c| !c.is_empty()),
        "the holder's command line is named: {data}"
    );
    let phase_ms = data["phases_ms"]["test_phase_ms"].as_u64().unwrap();
    assert!(phase_ms >= 40, "the named phase records its time: {data}");
}

/// AC2: a lock file stamped with a dead pid while a live process holds the
/// flock - the waiter's refusal names the live queue-head pid, not just the
/// dead stamp.
#[test]
fn a_dead_stamp_names_the_live_queue_head_in_the_refusal() {
    let lock_dir = tempfile::tempdir().unwrap();
    let path = lock_dir.path().join("graph.json");
    let (tx, rx) = mpsc::channel::<()>();
    let holder_path = path.clone();
    let holder = std::thread::spawn(move || {
        let lock =
            BoundedLock::acquire(&holder_path, Duration::from_secs(10)).expect("holder acquires");
        tx.send(()).expect("held signal reaches the main thread");
        std::thread::sleep(Duration::from_millis(2_000));
        lock
    });
    rx.recv_timeout(Duration::from_secs(10))
        .expect("holder signals");
    let lock_file = graph_store::graph_lock_path(&path);
    let dead_pid = i32::MAX as u64; // no real process carries this pid
    let stamp = json!({
        "pid": dead_pid,
        "name": "graph-store",
        "chokepoint": "graph-store",
        "acquired_at": "2026-11-06T00:00:00Z",
    });
    std::fs::write(&lock_file, stamp.to_string()).expect("dead stamp lands");
    let err = BoundedLock::acquire(&path, Duration::from_millis(400))
        .expect_err("a held lock refuses a second acquirer");
    let message = err.to_string();
    let mine = std::process::id();
    assert!(
        message.contains("queue-head pid") && message.contains(&mine.to_string()),
        "the refusal names the live queue head (pid {mine}): {message}"
    );
    assert!(
        message.contains(&dead_pid.to_string()),
        "the refusal still reports the stale stamp it read: {message}"
    );
    drop(holder.join().expect("holder thread finishes"));
}
