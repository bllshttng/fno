//! Names a daemon that died without a trace.
//!
//! A SIGKILL, an OOM kill, or a power loss writes no `daemon_exited` and no
//! stderr, and the arms the daemon carried (the evals gate among them) stop
//! ticking until something starts a replacement. The replacement is the first
//! witness: while it holds the supervisor lock, the lockfile still names the
//! pid of the daemon that held it before, and `events.jsonl` still ends at that
//! daemon's last write.
//!
//! The daemon is detached, so no parent survives to hand back an exit code.
//! The event says so (`exit_code_recovered: false`) rather than guess one.

use crate::paths::AgentsHome;
use serde_json::{json, Value};
use std::io::{Read, Seek, SeekFrom};
use std::sync::OnceLock;
use std::time::SystemTime;

/// How much of `events.jsonl` the clean-exit check reads from the end.
const TAIL_BYTES: u64 = 512 * 1024;

/// The previous lock holder, read before the new daemon overwrote the lockfile.
#[derive(Debug, Clone, PartialEq)]
pub struct Predecessor {
    pub pid: u32,
    pub alive: bool,
    /// Seconds between the last write to `events.jsonl` and this start.
    pub quiet_secs: Option<u64>,
}

static CAPTURED: OnceLock<Option<Predecessor>> = OnceLock::new();

/// Record the lockfile's previous holder. Call once, after the flock is held
/// (so the holder is provably gone) and before the lockfile content is
/// rewritten. `alive` guards pid reuse with the recorded start time.
pub fn capture(home: &AgentsHome, pid_alive: impl Fn(u32, Option<u64>) -> bool) {
    let _ = CAPTURED.set(
        crate::paths::supervisor_lock_holder(home).map(|(pid, start)| {
            let quiet_secs = std::fs::metadata(home.events_jsonl())
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| SystemTime::now().duration_since(t).ok())
                .map(|d| d.as_secs());
            Predecessor {
                pid,
                alive: pid_alive(pid, start),
                quiet_secs,
            }
        }),
    );
}

/// The `daemon_died_unexpectedly` payload, when the captured predecessor left
/// a `daemon_started` in the events log with no `daemon_exited` after it.
pub fn unexpected_death(home: &AgentsHome, new_pid: u32) -> Option<Value> {
    let pred = CAPTURED.get()?.as_ref()?;
    let tail = read_tail(&home.events_jsonl())?;
    payload_for(pred, &tail, new_pid)
}

fn payload_for(pred: &Predecessor, events_tail: &str, new_pid: u32) -> Option<Value> {
    if pred.alive || pred.pid == new_pid {
        return None;
    }
    let mut started_pid: Option<u64> = None;
    let mut exited_since = false;
    for line in events_tail.lines() {
        let is_started = line.contains("\"daemon_started\"");
        let is_exited = line.contains("\"daemon_exited\"");
        if !is_started && !is_exited {
            continue;
        }
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        match v["type"].as_str() {
            Some("daemon_started") => {
                started_pid = v["data"]["pid"].as_u64();
                exited_since = false;
            }
            Some("daemon_exited") => exited_since = true,
            _ => {}
        }
    }
    // No matching start in the tail means the log rotated or the lockfile is
    // older than the log: not enough to name a death, so name nothing.
    if started_pid != Some(u64::from(pred.pid)) || exited_since {
        return None;
    }
    let mut payload = json!({
        "old_pid": pred.pid,
        "new_pid": new_pid,
        "exit_code_recovered": false,
    });
    if let Some(secs) = pred.quiet_secs {
        payload["quiet_secs"] = json!(secs);
    }
    Some(payload)
}

fn read_tail(path: &std::path::Path) -> Option<String> {
    let mut file = std::fs::File::open(path).ok()?;
    let len = file.metadata().ok()?.len();
    file.seek(SeekFrom::Start(len.saturating_sub(TAIL_BYTES)))
        .ok()?;
    let mut raw = Vec::new();
    file.read_to_end(&mut raw).ok()?;
    Some(String::from_utf8_lossy(&raw).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pred(pid: u32, alive: bool) -> Predecessor {
        Predecessor {
            pid,
            alive,
            quiet_secs: Some(1380),
        }
    }

    const STARTED: &str =
        r#"{"ts":"t","type":"daemon_started","source":"daemon","data":{"pid":54761}}"#;
    const EXITED: &str =
        r#"{"ts":"t","type":"daemon_exited","source":"daemon","data":{"clean":true}}"#;

    #[test]
    fn start_without_exit_names_the_dead_predecessor() {
        let log = format!("{STARTED}\nnoise\n");
        let got = payload_for(&pred(54761, false), &log, 1004).expect("death named");
        assert_eq!(got["old_pid"], 54761);
        assert_eq!(got["new_pid"], 1004);
        assert_eq!(got["quiet_secs"], 1380);
        assert_eq!(got["exit_code_recovered"], false);
    }

    #[test]
    fn graceful_exit_or_live_pid_or_unmatched_start_names_nothing() {
        let clean = format!("{STARTED}\n{EXITED}\n");
        assert!(payload_for(&pred(54761, false), &clean, 1004).is_none());
        assert!(payload_for(&pred(54761, true), STARTED, 1004).is_none());
        assert!(payload_for(&pred(99, false), STARTED, 1004).is_none());
    }
}
