//! The beat deadline: one reader (or one prefetch) runs bounded by the
//! time left in the beat, so no blocked read can hold `finish_checkin`
//! past the ceiling and partial coverage always journals.

use crate::lead_checkin::Reading;
use serde_json::{json, Value};

/// Wall-clock ceiling for one child a reader spawns. The worst clean call
/// measured 8s (`fno agents list --json`); the ceiling holds at fourteen
/// times that and still sits far inside the 55-minute wake interval that
/// judges a missed beat.
pub(crate) const READER_CALL_BUDGET: std::time::Duration = std::time::Duration::from_secs(120);

/// The whole beat's ceiling, checked between readers in `collect_readings`.
/// 35 readers at the per-call worst case is 70 minutes, longer than the wake
/// interval, so the beat itself must bound: past this, remaining readers
/// report failed and the beat journals partial coverage.
pub(crate) const BEAT_BUDGET: std::time::Duration = std::time::Duration::from_secs(600);

/// One reading could not be taken; the beat continues without it.
pub(crate) struct ReaderError(pub(crate) String);

/// One bounded computation on a detached thread: the result returns through
/// a channel, and past `remaining` the beat abandons the thread and moves
/// on, so work blocked on I/O can never hold the beat past its ceiling.
/// `Ok(None)` is the deadline firing; `Err` names a thread that ended
/// without a result.
pub(crate) fn run_bounded<T: Send + 'static>(
    remaining: std::time::Duration,
    what: &str,
    task: impl FnOnce() -> T + Send + 'static,
) -> Result<Option<T>, String> {
    let (tx, rx) = std::sync::mpsc::channel();
    let label = format!("lead-checkin:{what}");
    std::thread::Builder::new()
        .name(label)
        .spawn(move || {
            let _ = tx.send(task());
        })
        .map_err(|e| format!("reader thread failed to spawn: {e}"))?;
    match rx.recv_timeout(remaining) {
        Ok(value) => Ok(Some(value)),
        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => Ok(None),
        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
            Err("reader thread ended without a result".to_string())
        }
    }
}

/// One reader, behind the beat budget: past it, the reader is skipped with a
/// named failed reading instead of running, and a reader that starts inside
/// the budget still runs bounded by the time left, so the beat always
/// reaches `finish_checkin` and journals partial coverage.
pub(crate) fn take_reading(
    readings: &mut Vec<Reading>,
    beat_started: std::time::Instant,
    name: &'static str,
    read: impl FnOnce() -> Result<Value, String> + Send + 'static,
) {
    take_reading_within(readings, beat_started, BEAT_BUDGET, name, read)
}

/// The parameterized form: a test proves the skip without waiting out the
/// real budget, and a second proves a slow running reader cannot outlive it.
pub(crate) fn take_reading_within(
    readings: &mut Vec<Reading>,
    beat_started: std::time::Instant,
    budget: std::time::Duration,
    name: &'static str,
    read: impl FnOnce() -> Result<Value, String> + Send + 'static,
) {
    let Some(remaining) = budget.checked_sub(beat_started.elapsed()) else {
        readings.push(Reading::failed(
            name,
            format!("beat budget exceeded after {budget:?} (skipped)"),
        ));
        return;
    };
    // The reader runs on a detached thread behind the beat deadline: past
    // `remaining` the reading journals failed and the beat moves on, so a
    // reader that starts just before the limit cannot hold finish_checkin.
    match run_bounded(remaining, name, read) {
        Ok(Some(Ok(value))) => readings.push(Reading::took(name, value)),
        Ok(Some(Err(error))) => readings.push(Reading::failed(name, ReaderError(error).0)),
        Ok(None) => readings.push(Reading::failed(
            name,
            format!("beat deadline exceeded after {remaining:?} (reader timed out)"),
        )),
        Err(error) => readings.push(Reading::failed(name, error)),
    }
}

#[cfg(test)]
mod reader_deadline_tests {
    use super::*;

    /// A reader past the beat budget is skipped with the named reason, and
    /// every skipped reader lands in the readings the payload reports.
    #[test]
    fn the_beat_budget_marks_unrun_readings_failed() {
        let mut readings: Vec<Reading> = Vec::new();
        let started = std::time::Instant::now();
        let zero = std::time::Duration::ZERO;
        take_reading_within(&mut readings, started, zero, "board", || {
            Ok(json!({"ok": 1}))
        });
        take_reading_within(&mut readings, started, zero, "team", || {
            Ok(json!({"ok": 2}))
        });
        assert_eq!(
            readings.len(),
            2,
            "both readers must be skipped: {readings:?}"
        );
        for reading in &readings {
            assert!(!reading.ok, "the skip must read as failed: {reading:?}");
            assert!(
                reading.error.contains("beat budget exceeded"),
                "the skip reason must be named: {}",
                reading.error
            );
        }
    }

    /// A reader that starts inside the budget but runs past the remaining
    /// time is cut off and named: the beat does not wait out the reader's
    /// own runtime, and partial coverage still journals. This is the
    /// running-reader path the zero budget above cannot exercise.
    #[test]
    fn the_beat_deadline_bounds_a_slow_running_reader() {
        let mut readings: Vec<Reading> = Vec::new();
        let started = std::time::Instant::now();
        take_reading_within(
            &mut readings,
            started,
            std::time::Duration::from_secs(1),
            "board",
            || {
                std::thread::sleep(std::time::Duration::from_secs(10));
                Ok(json!({"ok": 1}))
            },
        );
        assert_eq!(
            readings.len(),
            1,
            "the reader must be journaled: {readings:?}"
        );
        let reading = &readings[0];
        assert!(!reading.ok, "the timeout must read as failed: {reading:?}");
        assert!(
            reading.error.contains("beat deadline exceeded"),
            "the deadline must be named: {}",
            reading.error
        );
        assert!(
            started.elapsed() < std::time::Duration::from_secs(5),
            "the bound fired, not the reader's own exit: {:?}",
            started.elapsed()
        );
    }

    /// A reader that finishes inside its remaining time still lands its
    /// value: the thread handoff must not lose a healthy reading.
    #[test]
    fn a_healthy_reader_still_lands_through_the_thread_handoff() {
        let mut readings: Vec<Reading> = Vec::new();
        take_reading_within(
            &mut readings,
            std::time::Instant::now(),
            std::time::Duration::from_secs(10),
            "team",
            || Ok(json!({"ok": 1})),
        );
        assert_eq!(readings.len(), 1, "the read must land: {readings:?}");
        assert!(readings[0].ok, "the healthy read must land: {readings:?}");
        assert_eq!(readings[0].value, json!({"ok": 1}));
    }
}
