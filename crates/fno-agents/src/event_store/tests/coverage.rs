use super::*;

#[test]
fn imported_history_reads_partial_until_the_epoch() {
    let dir = tempfile::tempdir().unwrap();
    let live = dir.path().join("events.jsonl");
    append(
        &live,
        &[
            checkin("2026-09-10T12:00:00Z", "x-aaaa", "before"),
            checkin("2026-09-10T12:01:00Z", "x-1cbb", "old"),
        ],
    );
    sync(&live).unwrap();
    // A since before the epoch is only partially proven even though rows
    // were imported from before it.
    let early = coverage(&live, Some(0), &["lead_checkin".to_string()]);
    assert_eq!(early.status, "partial", "{early:?}");
    let epoch = early.complete_since_ms.unwrap();
    assert!(
        early.observed_first_ms.unwrap() < epoch,
        "observed history precedes the epoch: {early:?}"
    );
    // A since at or after the epoch is proven complete.
    let late = coverage(&live, Some(epoch), &["lead_checkin".to_string()]);
    assert_eq!(late.status, "complete", "{late:?}");
    assert_eq!(late.complete_since_ms, Some(epoch));
}

#[test]
fn missing_unopenable_and_pre_epoch_stores_never_read_complete() {
    // No store at all: nothing is proven.
    let dir = tempfile::tempdir().unwrap();
    let live = dir.path().join("events.jsonl");
    let missing = coverage(&live, Some(0), &[]);
    assert_eq!(missing.status, "unreadable", "{missing:?}");
    // A store file that is not a database backs no confident count.
    sync(&live).unwrap();
    let store = store_path(&live);
    std::fs::write(&store, b"not a database").unwrap();
    let garbage = coverage(&live, Some(0), &[]);
    assert_eq!(garbage.status, "unreadable", "{garbage:?}");
}
