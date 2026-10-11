//! The mail arm: the receiver side of the cross-host bus.
//!
//! With a shared primary set, the daemon pulls the pending `fno_mail_bus`
//! rows addressed to this host in seq order and hands each payload to the
//! receiver's own local send, so lane selection, holds, and receipts stay
//! the local verb's. Two maintenance legs ride the same beat: the flush
//! that drains the sender's local outbox once the primary answers again
//! (edge 7), and the expire that flips rows past their TTL (edge 2). With
//! no primary set, the beat returns at once and opens no socket.

use crate::mail_bus::{self, BusRow};
use crate::mail_remote::{local_send_argv, outbox_path, seen_path};
use crate::store_remote::Remote;
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// The default pull cadence. `agents.remote.pull_interval_secs` overrides it
/// (operator ruling, 2026-10-10: 5 seconds per host is heavy for machines
/// at their limit).
pub const DEFAULT_INTERVAL: Duration = Duration::from_secs(15);

/// The pure interval rule: a positive override wins, anything else keeps
/// the default (a typo degrades, it never stops mail).
pub(crate) fn interval_from_secs(value: Option<i64>) -> Duration {
    match value {
        Some(secs) if secs > 0 => Duration::from_secs(secs as u64),
        _ => DEFAULT_INTERVAL,
    }
}

/// The configured cadence, read once per beat. The key lives in the GLOBAL
/// config only, like `store.remote_url` (a project cannot set store keys).
pub fn pull_interval() -> Duration {
    interval_from_secs(
        crate::agents_config::config_lookup_global(&["agents", "remote", "pull_interval_secs"])
            .and_then(|v| v.as_integer()),
    )
}

#[derive(Default)]
pub struct Arm {
    last_tick: Mutex<Option<Instant>>,
    in_flight: Arc<AtomicBool>,
}

/// One tick. Mirrors `lease_heartbeat::maybe_tick`: gate on the interval
/// and a one-in-flight flag, then run the legs off-loop.
pub fn maybe_tick(arm: &Arm) {
    let remote = match crate::store_remote::configured() {
        Ok(Some(remote)) => remote,
        Ok(None) => return,
        Err(error) => {
            static WARNED: std::sync::Once = std::sync::Once::new();
            WARNED.call_once(|| eprintln!("mail-arm: {error}"));
            return;
        }
    };
    let interval = pull_interval();
    {
        let mut last = arm.last_tick.lock().unwrap_or_else(|e| e.into_inner());
        if last.is_some_and(|tick| tick.elapsed() < interval)
            || arm.in_flight.swap(true, Ordering::SeqCst)
        {
            return;
        }
        *last = Some(Instant::now());
    }
    let flag = Arc::clone(&arm.in_flight);
    tokio::task::spawn_blocking(move || {
        let _gate = crate::daemon::SweepGate(flag);
        let outcome = beat(&remote);
        if outcome.delivered + outcome.flushed + outcome.expired as usize > 0 {
            eprintln!(
                "mail-arm: delivered {} flushed {} expired {}",
                outcome.delivered, outcome.flushed, outcome.expired
            );
        }
    });
}

/// What one beat did, for the event line and the tests.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct PullOutcome {
    pub delivered: usize,
    pub flushed: usize,
    pub expired: u64,
}

/// One beat: pull, then flush, then expire. The legs are independent: a
/// pull failure does not stop the flush from draining the local outbox.
pub fn beat(remote: &Remote) -> PullOutcome {
    let host = mail_bus::normalize_host(&crate::claims::hostname());
    let mut outcome = PullOutcome {
        delivered: 0,
        flushed: 0,
        expired: 0,
    };
    if mail_bus::ensure_schema(remote).is_err() {
        return outcome;
    }
    match mail_bus::fetch_pending(remote, &host) {
        Ok(rows) => outcome.delivered = pull_rows(remote, rows, &home_root()),
        Err(e) => eprintln!("mail-arm: pull: {e}"),
    }
    match flush(remote) {
        Ok(n) => outcome.flushed = n,
        Err(e) => eprintln!("mail-arm: flush: {e}"),
    }
    match mail_bus::expire_stale(remote) {
        Ok(n) => outcome.expired = n,
        Err(e) => eprintln!("mail-arm: expire: {e}"),
    }
    outcome
}

fn home_root() -> PathBuf {
    let home = crate::paths::AgentsHome::from_env();
    let _ = home.ensure_root();
    home.root().to_path_buf()
}

/// Deliver pending rows in seq order. A row leaves the pending pool only
/// through a successful local send (exit 0): a refusal or a send that
/// failed waits for the next beat, and the TTL bounds it. The seen ledger
/// records msg_ids after a zero exit, so a redelivered row never injects
/// twice (at-least-once, edge 5).
pub(crate) fn pull_rows(remote: &Remote, rows: Vec<BusRow>, home: &std::path::Path) -> usize {
    let mut delivered = 0;
    for row in rows {
        if seen(remote, home, &row) {
            continue;
        }
        if inject_row(remote, home, &row) {
            delivered += 1;
        }
    }
    delivered
}

/// True when the row was already delivered by an earlier beat.
fn seen(remote: &Remote, home: &std::path::Path, row: &BusRow) -> bool {
    let path = seen_path(home);
    let already = std::fs::read_to_string(&path)
        .map(|existing| {
            existing
                .lines()
                .any(|line| line.trim() == row.msg_id.as_str())
        })
        .unwrap_or(false);
    if already {
        let _ = mail_bus::mark(remote, &row.msg_id, mail_bus::DELIVERED);
    }
    already
}

fn record_seen(home: &std::path::Path, msg_id: &str) -> Result<(), String> {
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(seen_path(home))
        .and_then(|mut f| writeln!(f, "{msg_id}"))
        .map_err(|e| e.to_string())
}

/// The delivery leg: the receiver's own local send answers. Its registry
/// resolves the name (an unknown name refuses, exit 13, and the row stays
/// pending for the TTL), and its holds apply untouched (edge 8). Returns
/// true when the row is done (delivered or refused), false to leave it
/// pending for the next beat.
fn inject_row(remote: &Remote, home: &std::path::Path, row: &BusRow) -> bool {
    // Edge 6: a raw row is a command typed into a pane. Anyone on the
    // tailnet can write a bus row, and the sender-written from_id proves
    // nothing: the tailnet is the boundary, the allowlist is the second
    // check (operator ruling, 2026-10-10).
    if row.raw && !raw_sender_allowed(&row.from_id) {
        let _ = mail_bus::mark(remote, &row.msg_id, mail_bus::REFUSED);
        return true;
    }
    let argv = local_send_argv(&row.to_name, &row.payload, row.raw, &row.from_id);
    let mut cmd = std::process::Command::new(&argv[0]);
    cmd.args(&argv[1..]);
    match cmd.output() {
        Ok(output) if output.status.success() => {
            if record_seen(home, &row.msg_id).is_ok() {
                let _ = mail_bus::mark(remote, &row.msg_id, mail_bus::DELIVERED);
                return true;
            }
            // The send landed but the ledger write failed: leave pending, so
            // the next beat at least reconciles the row state (the send
            // itself does not replay: this process ran it once).
            false
        }
        Ok(_) => false,
        Err(e) => {
            eprintln!("mail-arm: local send for {}: {e}", row.msg_id);
            false
        }
    }
}

/// The raw allowlist: `agents.remote.raw_senders`, a list of `user@host`
/// sender heads. Empty is the default and refuses every cross-host raw row.
fn raw_sender_allowed(from_id: &str) -> bool {
    let head = from_id.split('/').next().unwrap_or("");
    crate::agents_config::config_lookup_global(&["agents", "remote", "raw_senders"])
        .and_then(|v| v.as_array().cloned())
        .unwrap_or_default()
        .iter()
        .filter_map(|v| v.as_str())
        .any(|sender| sender == head)
}

/// Drain the sender's local outbox into the bus (edge 7). Each line inserts
/// with `INSERT OR IGNORE`, so a crash between insert and line-drop replays
/// harmlessly. A line the bus refuses (a malformed field) is parked on
/// stderr, never retried: one bad line cannot block the queue.
pub(crate) fn flush(remote: &Remote) -> Result<usize, String> {
    let home = crate::paths::AgentsHome::from_env();
    let path = outbox_path(home.root());
    let Ok(existing) = std::fs::read_to_string(&path) else {
        return Ok(0);
    };
    let mut kept: Vec<String> = Vec::new();
    let mut drained = 0;
    let mut parked = 0;
    for line in existing.lines() {
        if line.trim().is_empty() {
            continue;
        }
        let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
            eprintln!("mail-arm: outbox line parked (not JSON): {line}");
            parked += 1;
            continue;
        };
        let field = |name: &str| value.get(name).and_then(|v| v.as_str()).unwrap_or("");
        match mail_bus::insert(
            remote,
            field("msg_id"),
            field("from_id"),
            field("from_host"),
            field("to_name"),
            field("to_host"),
            field("payload"),
            value.get("raw").and_then(|v| v.as_i64()).unwrap_or(0) != 0,
            None,
        ) {
            Ok(_) => drained += 1,
            Err(e) => {
                if crate::store_remote::is_unreachable(&e) {
                    kept.push(line.to_string());
                } else {
                    eprintln!("mail-arm: outbox line parked: {e}");
                    parked += 1;
                }
            }
        }
    }
    rewrite_outbox(&path, drained, parked, &kept)?;
    Ok(drained)
}

/// Replace the outbox only when its content changed: an all-drained queue
/// removes the file, and a beat with nothing to do writes nothing.
fn rewrite_outbox(
    path: &std::path::Path,
    drained: usize,
    parked: usize,
    kept: &[String],
) -> Result<(), String> {
    if drained == 0 && parked == 0 {
        return Ok(());
    }
    if kept.is_empty() {
        std::fs::remove_file(path).map_err(|e| e.to_string())
    } else {
        std::fs::File::create(path)
            .and_then(|mut f| {
                for line in kept {
                    writeln!(f, "{line}")?;
                }
                Ok(())
            })
            .map_err(|e| e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interval_rule_keeps_default_on_a_bad_value() {
        assert_eq!(interval_from_secs(None), DEFAULT_INTERVAL);
        assert_eq!(interval_from_secs(Some(0)), DEFAULT_INTERVAL);
        assert_eq!(interval_from_secs(Some(-5)), DEFAULT_INTERVAL);
        assert_eq!(interval_from_secs(Some(30)), Duration::from_secs(30));
    }
}
