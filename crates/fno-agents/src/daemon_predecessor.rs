//! Names a daemon that died without a trace.
//!
//! A SIGKILL, an OOM kill, or a power loss writes no `daemon_exited` and no
//! stderr, and the arms the daemon carried (the evals gate among them) stop
//! ticking until something starts a replacement. The replacement is the first
//! witness: while it holds the supervisor lock, the lockfile still names the
//! pid of the daemon that held it before, and `events.jsonl` still holds that
//! daemon's last lines.
//!
//! The daemon is detached, so no parent survives to hand back an exit code.
//! The event says so (`exit_code_recovered: false`) rather than guess one.

use crate::paths::AgentsHome;
use serde_json::{json, Value};
use std::io::{Read, Seek, SeekFrom};
use std::sync::OnceLock;

/// How much of `events.jsonl` the clean-exit check reads from the end.
const TAIL_BYTES: u64 = 512 * 1024;

/// The previous lock holder, read before the new daemon overwrote the lockfile.
#[derive(Debug, Clone, PartialEq)]
pub struct Predecessor {
    pub pid: u32,
    pub alive: bool,
}

static CAPTURED: OnceLock<Option<Predecessor>> = OnceLock::new();

/// Record the lockfile's previous holder. Call once, after the flock is held
/// (so the holder is provably gone) and before the lockfile content is
/// rewritten. `pid_alive` guards pid reuse with the recorded start time.
pub fn capture(home: &AgentsHome, pid_alive: impl Fn(u32, Option<u64>) -> bool) {
    let _ = CAPTURED.set(
        crate::paths::supervisor_lock_holder(home).map(|(pid, start)| Predecessor {
            pid,
            alive: pid_alive(pid, start),
        }),
    );
}

/// Write the dated start line and, when the predecessor died unseen, the
/// death line and the `daemon_died_unexpectedly` event. stderr is append-only
/// across every start, so one dated line per start splits it into one segment
/// per pid and a death can be placed in time.
pub fn announce_start(home: &AgentsHome, emitter: &crate::events::EventEmitter) {
    let pid = std::process::id();
    eprintln!(
        "fno-agents-daemon: started pid={pid} at {}",
        chrono::Utc::now().to_rfc3339()
    );
    if let Some(death) = unexpected_death(home, pid) {
        eprintln!(
            "fno-agents-daemon: predecessor pid={} died without daemon_exited; quiet {}s; exit code unrecoverable",
            death["old_pid"],
            death["quiet_secs"].as_i64().unwrap_or(-1)
        );
        let _ = emitter.emit("daemon_died_unexpectedly", &death);
    }
}

/// The `daemon_died_unexpectedly` payload, when the captured predecessor left
/// a `daemon_started` in the events log with no `daemon_exited` after it.
pub fn unexpected_death(home: &AgentsHome, new_pid: u32) -> Option<Value> {
    let pred = CAPTURED.get()?.as_ref()?;
    let tail = read_tail(&home.events_jsonl())?;
    payload_for(pred, &tail, new_pid, chrono::Utc::now())
}

fn payload_for(
    pred: &Predecessor,
    events_tail: &str,
    new_pid: u32,
    now: chrono::DateTime<chrono::Utc>,
) -> Option<Value> {
    if pred.alive || pred.pid == new_pid {
        return None;
    }
    let mut started_pid: Option<u64> = None;
    let mut exited_since = false;
    let mut last_daemon_ts: Option<String> = None;
    for line in events_tail.lines() {
        if !line.contains("\"source\":\"daemon\"") {
            continue;
        }
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        last_daemon_ts = v["ts"].as_str().map(str::to_owned);
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
    // The file's mtime would not do: hooks and clients keep appending to
    // events.jsonl while the daemon is dead. The last daemon-sourced line is
    // the last proof of life, so the span from it bounds the gap from above.
    if let Some(ts) = last_daemon_ts {
        if let Ok(at) = chrono::DateTime::parse_from_rfc3339(&ts) {
            let quiet = (now - at.with_timezone(&chrono::Utc)).num_seconds().max(0);
            payload["quiet_secs"] = json!(quiet);
        }
        payload["last_daemon_event_ts"] = json!(ts);
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
        Predecessor { pid, alive }
    }

    fn now() -> chrono::DateTime<chrono::Utc> {
        chrono::DateTime::parse_from_rfc3339("2026-11-04T22:50:12Z")
            .unwrap()
            .with_timezone(&chrono::Utc)
    }

    const STARTED: &str = r#"{"ts":"2026-11-04T22:00:00Z","type":"daemon_started","source":"daemon","data":{"pid":54761}}"#;
    const LAST_TICK: &str =
        r#"{"ts":"2026-11-04T22:27:12Z","type":"evals_gate_tick","source":"daemon","data":{}}"#;
    const HOOK_AFTER: &str =
        r#"{"ts":"2026-11-04T22:49:00Z","type":"tool_use","source":"hook","data":{}}"#;
    const EXITED: &str = r#"{"ts":"2026-11-04T22:27:13Z","type":"daemon_exited","source":"daemon","data":{"clean":true}}"#;

    #[test]
    fn start_without_exit_names_the_dead_predecessor_and_the_gap() {
        let log = format!("{STARTED}\n{LAST_TICK}\n{HOOK_AFTER}\n");
        let got = payload_for(&pred(54761, false), &log, 1004, now()).expect("death named");
        assert_eq!(got["old_pid"], 54761);
        assert_eq!(got["new_pid"], 1004);
        assert_eq!(got["exit_code_recovered"], false);
        // 22:27:12 to 22:50:12; the later hook line does not shorten it.
        assert_eq!(got["quiet_secs"], 1380);
    }

    #[test]
    fn graceful_exit_or_live_pid_or_unmatched_start_names_nothing() {
        let clean = format!("{STARTED}\n{EXITED}\n");
        assert!(payload_for(&pred(54761, false), &clean, 1004, now()).is_none());
        assert!(payload_for(&pred(54761, true), STARTED, 1004, now()).is_none());
        assert!(payload_for(&pred(99, false), STARTED, 1004, now()).is_none());
    }
}
