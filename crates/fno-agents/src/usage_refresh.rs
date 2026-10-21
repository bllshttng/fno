//! The usage refresh arm: the daemon re-measures every account's usage inside
//! the cache TTL, so an idle machine never reads its accounts as unknown.
//!
//! Before this arm the usage cache filled only on the dispatch path, and only
//! with `accounts.quota.observe` on. A machine that dispatched nothing for five
//! minutes read every account as `unknown(stale)` in `accounts list`, the
//! Connections modal and the route gate alike. One `fno config accounts usage
//! --refresh` per beat keeps each reading fresh; the verb owns the probes, the
//! attribution proof and the cache write.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// The verb probes each claude account over the network (a profile call and a
/// usage call), so it gets far more room than one beat needs.
const VERB_TIMEOUT: Duration = Duration::from_secs(90);

/// The beat for a usage TTL: four fifths of it, so a reading never ages out
/// between beats, and never under a minute, so a tiny TTL cannot turn the arm
/// into a probe storm. The TTL is `accounts.quota.probe_ttl_seconds`, the same
/// key every headroom reader judges freshness by.
pub fn beat_interval(ttl_seconds: f64) -> Duration {
    Duration::from_secs_f64((ttl_seconds * 0.8).max(60.0))
}

pub struct Arm {
    cwd: PathBuf,
    last_tick: Mutex<Option<Instant>>,
    in_flight: Arc<AtomicBool>,
    /// The next beat in seconds, re-read from config after each beat so the
    /// tick itself never parses config.
    interval_s: Arc<AtomicU64>,
    /// Whether the last beat failed, so a run of failures logs one line.
    failing: Arc<AtomicBool>,
}

impl Arm {
    pub fn new(cwd: PathBuf) -> Self {
        Self {
            cwd,
            last_tick: Mutex::new(None),
            in_flight: Arc::new(AtomicBool::new(false)),
            interval_s: Arc::new(AtomicU64::new(beat_interval(300.0).as_secs())),
            failing: Arc::new(AtomicBool::new(false)),
        }
    }
}

pub fn maybe_tick(arm: &Arm) {
    let interval = Duration::from_secs(arm.interval_s.load(Ordering::SeqCst));
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
    let failing = Arc::clone(&arm.failing);
    let interval_s = Arc::clone(&arm.interval_s);
    let cwd = arm.cwd.clone();
    tokio::task::spawn_blocking(move || {
        let _gate = crate::daemon::SweepGate(flag);
        let ok = crate::route_capacity::refresh_usage_readings(&cwd, VERB_TIMEOUT).is_some();
        if failing.swap(!ok, Ordering::SeqCst) != !ok {
            eprintln!(
                "usage-refresh: {}",
                if ok {
                    "recovered"
                } else {
                    "`fno config accounts usage --refresh` failed; account usage will read stale"
                }
            );
        }
        let ttl = crate::backlog::quota_route::load_quota_config(cwd.to_str()).probe_ttl_seconds;
        interval_s.store(beat_interval(ttl).as_secs(), Ordering::SeqCst);
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A reading must never age out between beats, and a tiny TTL must never
    /// turn the arm into a probe every daemon tick.
    #[test]
    fn beat_lands_inside_the_ttl_with_a_one_minute_floor() {
        assert_eq!(beat_interval(300.0), Duration::from_secs(240));
        assert!(beat_interval(120.0) < Duration::from_secs(120));
        assert_eq!(beat_interval(10.0), Duration::from_secs(60));
    }
}
