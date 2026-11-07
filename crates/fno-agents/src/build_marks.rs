//! The build-mark arm: the daemon's push leg of the mux cargo marks. On a
//! 5s cadence it reads the cargo admission doors once
//! ([`crate::cargo_builds::collect_jobs`], the read `fno doctor builds`
//! prints), maps each job to the session its checkout's target manifest
//! names, and writes one sidecar per session under `db/build-marks/`
//! (`compiling`, or `waiting` with the 1-based lane rank the marker pid
//! names). The mux server's read loop stamps the rows from these sidecars;
//! it never reads the doors itself. Each sidecar carries a 30s TTL
//! refreshed each pass, so a dead daemon's last mark ages out instead of
//! pinning a frozen hourglass, and a session that left the doors has its
//! sidecar deleted (nothing when idle, the user's 2026-10-09 ruling).

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub(crate) const INTERVAL: Duration = Duration::from_secs(5);
pub(crate) const MARK_TTL_MS: i64 = 30_000;

#[derive(Default)]
pub struct Arm {
    last_tick: Mutex<Option<Instant>>,
    in_flight: Arc<AtomicBool>,
}

pub fn maybe_tick(arm: &Arm) {
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
        if let Err(error) = run_pass() {
            eprintln!("build-marks: {error}");
        }
    });
}

/// One session's mark as the arm derived it from the doors.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SessionMark {
    pub session: String,
    pub state: &'static str,
    pub position: Option<u64>,
}

/// The marks the doors imply right now, keyed by the mail-hold identity key
/// (the sidecar file stem: `ses_` ids verbatim, else lowercased). A job
/// whose checkout names no session marks nothing.
pub(crate) fn session_marks(cwd: &Path) -> Vec<SessionMark> {
    let build_lock = crate::claims::claim_path(crate::test_run::BUILD_SLOT_KEYS[0], None).ok();
    let mut seen = std::collections::HashMap::<String, SessionMark>::new();
    for job in crate::cargo_builds::collect_jobs(cwd) {
        let Some(session) = crate::cargo_builds::manifest_ids(&job.worktree).1 else {
            continue;
        };
        let key = crate::mail_hold::identity_key(&session);
        let (state, position) = match job.waiting.as_deref() {
            Some(lane) => {
                let pos = lane_position(build_lock.as_deref(), lane, job.marker_pid);
                ("waiting", pos)
            }
            None => ("compiling", None),
        };
        let mark = SessionMark {
            session: key.clone(),
            state,
            position,
        };
        // Two cargo jobs in one checkout fold to one mark: compiling wins
        // over waiting (the throbber is the busier truth), else the first
        // in the pid-ordered fold stands.
        match seen.get(&key) {
            Some(prev) if prev.state == "compiling" => {}
            _ => {
                seen.insert(key, mark);
            }
        }
    }
    seen.into_values().collect()
}

/// The waiter's 1-based position: its index among `waiter_pids` (ticket order,
/// which is admission order), matched by the marker pid the admitting
/// process stamped. An unreadable lane or a ticket the scan does not name
/// leaves the position off; the hourglass alone is honest.
fn lane_position(build_lock: Option<&Path>, lane: &str, marker_pid: Option<i32>) -> Option<u64> {
    let pid = marker_pid?;
    let dir = crate::claim_queue::lane_dir_for(build_lock?, lane);
    crate::claim_queue::waiter_pids(&dir)
        .iter()
        .position(|p| *p as i32 == pid)
        .map(|i| i as u64 + 1)
}

fn marks_dir() -> PathBuf {
    // The same root resolver the mail-hold sidecars use, so the reader in
    // crates/fno finds the files the arm writes.
    let root = crate::mail_hold::state_root();
    crate::state_layout::place_dir(&root, "build-marks").unwrap_or_else(|| root.join("build-marks"))
}

/// Write one sidecar per mark and delete the stragglers, so a session that
/// left the doors carries no mark the moment the arm next runs.
pub(crate) fn run_pass() -> Result<(), String> {
    let marks = session_marks(&std::env::current_dir().unwrap_or_default());
    let dir = marks_dir();
    write_marks(&dir, &marks)
}

/// The testable write half: create the dir, atomic-write each mark's
/// sidecar (tmp + rename, write_waiter_marker's shape), prune the rest.
pub(crate) fn write_marks(dir: &Path, marks: &[SessionMark]) -> Result<(), String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    let stamp = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();
    let mut fresh = BTreeSet::new();
    for mark in marks.iter() {
        fresh.insert(format!("{}.json", mark.session));
        let body = serde_json::json!({
            "state": mark.state,
            "position": mark.position,
            "at": &stamp,
            "ttl_ms": MARK_TTL_MS,
        });
        let path = dir.join(format!("{}.json", mark.session));
        let tmp = path.with_extension(format!("json.{}.tmp", std::process::id()));
        let written =
            std::fs::write(&tmp, body.to_string()).and_then(|()| std::fs::rename(&tmp, &path));
        if let Err(e) = written {
            let _ = std::fs::remove_file(&tmp);
            return Err(format!("cannot write {}: {e}", path.display()));
        }
    }
    for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let Some(name) = entry.file_name().to_str().map(str::to_string) else {
            continue;
        };
        if name.ends_with(".json") && !fresh.contains(&name) {
            let _ = std::fs::remove_file(entry.path());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_session_key_matches_the_reader_side_rule() {
        // The overlay in crates/fno lowercases uuid-family ids and keeps
        // `ses_` ids verbatim (the mail-hold rule); the file stem must be
        // that key, or the row never finds its mark.
        assert_eq!(
            crate::mail_hold::identity_key("6DC89BD0-4993-414A-92F5-52EAE1DCCE6D"),
            "6dc89bd0-4993-414a-92f5-52eae1dcce6d"
        );
        assert_eq!(crate::mail_hold::identity_key("ses_A1B2C3"), "ses_A1B2C3");
    }

    #[test]
    fn write_marks_writes_each_mark_and_prunes_the_stragglers() {
        let dir = std::env::temp_dir().join(format!("fno-build-marks-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let marks = vec![
            SessionMark {
                session: "ses_one".to_string(),
                state: "compiling",
                position: None,
            },
            SessionMark {
                session: "ses_two".to_string(),
                state: "waiting",
                position: Some(2),
            },
        ];
        write_marks(&dir, &marks).unwrap();
        let body: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(dir.join("ses_two.json")).unwrap())
                .unwrap();
        assert_eq!(body.get("state").and_then(|v| v.as_str()), Some("waiting"));
        assert_eq!(body.get("position").and_then(|v| v.as_i64()), Some(2));
        assert!(body
            .get("at")
            .and_then(|v| v.as_str())
            .is_some_and(|s| s.ends_with('Z')));
        assert_eq!(
            body.get("ttl_ms").and_then(|v| v.as_i64()),
            Some(MARK_TTL_MS)
        );
        // A second pass with no marks prunes every sidecar: an idle fleet
        // leaves the dir empty.
        write_marks(&dir, &[]).unwrap();
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 0);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
