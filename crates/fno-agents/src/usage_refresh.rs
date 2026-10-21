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
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// One minute inside the usage TTL, so a reading never ages out between beats.
pub const INTERVAL: Duration =
    Duration::from_secs(crate::fallback_chain::USAGE_TTL_SECONDS as u64 - 60);

/// The verb probes each claude account over the network (a profile call and a
/// usage call), so it gets far more room than one beat needs.
const VERB_TIMEOUT: Duration = Duration::from_secs(90);

pub struct Arm {
    cwd: PathBuf,
    last_tick: Mutex<Option<Instant>>,
    in_flight: Arc<AtomicBool>,
    /// Whether the last beat failed, so a run of failures logs one line.
    failing: Arc<AtomicBool>,
}

impl Arm {
    pub fn new(cwd: PathBuf) -> Self {
        Self {
            cwd,
            last_tick: Mutex::new(None),
            in_flight: Arc::new(AtomicBool::new(false)),
            failing: Arc::new(AtomicBool::new(false)),
        }
    }
}

pub fn maybe_tick(arm: &Arm) {
    {
        let mut last = arm.last_tick.lock().unwrap_or_else(|e| e.into_inner());
        if last.is_some_and(|tick| tick.elapsed() < INTERVAL)
            || arm.in_flight.swap(true, Ordering::SeqCst)
        {
            return;
        }
        *last = Some(Instant::now());
    }
    let flag = Arc::clone(&arm.in_flight);
    let failing = Arc::clone(&arm.failing);
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
    });
}
