use super::*;
use serde_json::json;
use std::sync::{Arc, Barrier, Mutex};

/// `mm:ss` past an hour anchored once, one to two hours ago: inside every
/// prune horizon, and late enough that each coalescing window has expired.
fn at(mm_ss: &str) -> String {
    static ANCHOR: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    let hour = ANCHOR.get_or_init(|| {
        (chrono::Utc::now() - chrono::Duration::hours(1))
            .format("%Y-%m-%dT%H")
            .to_string()
    });
    format!("{hour}:{mm_ss}Z")
}

fn allow(ts: &str, tool: &str) -> serde_json::Value {
    json!({"ts": ts, "type": "guard_decision", "source": "hook",
           "data": {"guard": "git-protection", "decision": "allow", "tool": tool}})
}

#[test]
fn import_coalesces_identical_allow_polls() {
    let dir = tempfile::tempdir().unwrap();
    let live = dir.path().join("events.jsonl");
    append(
        &live,
        &[
            allow(&at("00:00"), "Bash"),
            allow(&at("01:00"), "Bash"),
            allow(&at("02:00"), "Bash"),
        ],
    );
    let receipt = sync(&live).unwrap();
    assert_eq!(receipt.ingested, 1, "only the transition row is stored");
    assert_eq!(receipt.coalesced, 2, "the identical polls are pending");
    let rows = query_events(&live, &EventQuery::default()).unwrap();
    assert_eq!(rows.len(), 2, "transition plus the swept summary");
    assert_eq!(rows[1].r#type, "guard_decision");
    assert!(
        rows[1].line.contains("\"occurrence_count\":2"),
        "{}",
        rows[1].line
    );
    assert!(rows[1].line.contains("\"window_started_ms\":"));
    assert!(rows[1].line.contains("\"window_finished_ms\":"));
    append(&live, &[allow(&at("10:00"), "Bash")]);
    sync(&live).unwrap();
    let rows = query_events(&live, &EventQuery::default()).unwrap();
    assert_eq!(rows.len(), 3, "transition, summary, new transition");
}

#[test]
fn append_gate_suppresses_and_retry_is_idempotent_hit() {
    let dir = tempfile::tempdir().unwrap();
    let live = dir.path().join("events.jsonl");
    let first = allow(&at("00:00"), "Bash").to_string();
    let second = allow(&at("01:00"), "Bash").to_string();
    let first_receipt = append_envelope(&live, &first, None).unwrap();
    assert!(first_receipt.inserted);
    assert!(!first_receipt.suppressed);
    let second_receipt = append_envelope(&live, &second, None).unwrap();
    assert!(!second_receipt.inserted);
    assert!(second_receipt.suppressed);
    assert_eq!(second_receipt.pending_occurrences, 1);
    let retry = append_envelope(&live, &first, None).unwrap();
    assert!(!retry.inserted);
    assert!(
        !retry.suppressed,
        "a byte-identical retry is an idempotent hit"
    );
    assert_eq!(retry.seq, first_receipt.seq);
    assert_eq!(count_events(&store_path(&live)), 1);
}

#[test]
fn blocks_and_malformed_never_coalesce() {
    let dir = tempfile::tempdir().unwrap();
    let live = dir.path().join("events.jsonl");
    for ts in [&at("00:00"), &at("01:00"), &at("02:00")] {
        let block = json!({"ts": ts, "type": "guard_decision", "source": "hook",
            "data": {"guard": "git-protection", "decision": "block", "tool": "Bash"}});
        let receipt = append_envelope(&live, &block.to_string(), None).unwrap();
        assert!(receipt.inserted);
        assert!(
            !receipt.suppressed,
            "each block is a separate attempted action"
        );
    }
    let malformed = json!({"ts": &at("03:00"), "type": "guard_decision",
        "source": "hook", "data": {"guard": "git-protection", "tool": "Bash"}});
    // The judge owns the schema now: a guard_decision with no decision is
    // refused at the commit, so the always-audit tolerance is gone and the
    // row never lands to coalesce with anything.
    let err = append_envelope(&live, &malformed.to_string(), None).unwrap_err();
    assert!(
        err.contains("missing required data field: decision"),
        "err: {err}"
    );
    assert_eq!(count_events(&store_path(&live)), 3);
}

#[test]
fn concurrent_identical_first_polls_yield_one_transition() {
    let dir = tempfile::tempdir().unwrap();
    let live = dir.path().join("events.jsonl");
    let receipts: Arc<Mutex<Vec<Result<AppendReceipt, String>>>> = Arc::new(Mutex::new(Vec::new()));
    let barrier = Arc::new(Barrier::new(8));
    let mut handles = Vec::new();
    for i in 0..8 {
        let live = live.clone();
        let barrier = barrier.clone();
        let receipts = receipts.clone();
        handles.push(std::thread::spawn(move || {
            barrier.wait();
            let ts = at(&format!("00:0{i}"));
            let receipt = append_envelope(&live, &allow(&ts, "Bash").to_string(), None);
            receipts.lock().unwrap().push(receipt);
        }));
    }
    for handle in handles {
        handle.join().unwrap();
    }
    let receipts = receipts.lock().unwrap().drain(..).collect::<Vec<_>>();
    assert_eq!(receipts.len(), 8, "every thread returned a receipt");
    let inserted = receipts
        .iter()
        .filter(|r| matches!(r, Ok(rec) if rec.inserted))
        .count();
    let suppressed = receipts
        .iter()
        .filter(|r| matches!(r, Ok(rec) if rec.suppressed))
        .count();
    assert_eq!(inserted, 1, "exactly one transition row: {receipts:?}");
    assert_eq!(suppressed, 7, "the rest are pending occurrences");
    assert_eq!(count_events(&store_path(&live)), 1);
}

#[test]
fn replay_after_cursor_loss_does_not_double_count() {
    let dir = tempfile::tempdir().unwrap();
    let live = dir.path().join("events.jsonl");
    append(
        &live,
        &[
            allow(&at("00:00"), "Bash"),
            allow(&at("01:00"), "Bash"),
            allow(&at("02:00"), "Bash"),
            allow(&at("03:00"), "Bash"),
        ],
    );
    let receipt = sync(&live).unwrap();
    assert_eq!(receipt.ingested, 1);
    assert_eq!(receipt.coalesced, 3);
    let store = store_path(&live);
    // The end-of-sync sweep already flushed the window: one transition plus
    // one summary carrying the three suppressed polls.
    assert_eq!(count_events(&store), 2, "transition plus the swept summary");
    let summary_line: String = Connection::open(&store)
        .unwrap()
        .query_row(
            "SELECT line FROM events WHERE line LIKE '%occurrence_count%'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert!(
        summary_line.contains("\"occurrence_count\":3"),
        "{summary_line}"
    );
    {
        let conn = Connection::open(&store).unwrap();
        conn.execute("DELETE FROM ingest_cursor", []).unwrap();
    }
    append(&live, &[checkin(&at("05:00"), "x-aaaa", "cursor loss")]);
    sync(&live).unwrap();
    let conn = Connection::open(&store).unwrap();
    let guards: i64 = conn
        .query_row(
            "SELECT count(*) FROM events WHERE type = 'guard_decision'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        guards, 2,
        "transition plus the swept summary, unchanged by the replay"
    );
    let checkins: i64 = conn
        .query_row(
            "SELECT count(*) FROM events WHERE type = 'lead_checkin'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(checkins, 1, "the new checkin imports");
    let pending: i64 = conn
        .query_row("SELECT count(*) FROM event_observation_pending", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(pending, 0, "the replayed polls do not double count");
    drop(conn);
}

#[test]
fn unavailable_observation_state_is_an_explicit_failure() {
    let dir = tempfile::tempdir().unwrap();
    let live = dir.path().join("events.jsonl");
    append_envelope(&live, &allow(&at("00:00"), "Bash").to_string(), None).unwrap();
    let store = store_path(&live);
    {
        let conn = Connection::open(&store).unwrap();
        conn.execute_batch(
            "DROP TABLE event_observation_state;
             CREATE TABLE event_observation_state (x INTEGER);",
        )
        .unwrap();
    }
    let broken = append_envelope(&live, &allow(&at("01:00"), "Bash").to_string(), None);
    let err = broken.unwrap_err();
    assert!(err.contains("event_observation_state"), "err: {err}");
    let conn = Connection::open(&store).unwrap();
    let pending: i64 = conn
        .query_row(
            "SELECT count(*) FROM event_observation_pending
             WHERE obs_type = 'guard_decision'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        pending, 0,
        "nothing counted pending when the state table is broken"
    );
    let guards: i64 = conn
        .query_row(
            "SELECT count(*) FROM events WHERE type = 'guard_decision'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(guards, 1, "no row was added beyond the first");
}
