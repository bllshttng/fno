//! Hand the allocator's freed pages back to the OS on a slow tick.
//!
//! The daemon's live footprint is tens of MB, but libmalloc keeps every page a
//! freed allocation ever touched in the resident set (MADV_FREE_REUSABLE), so
//! one burst's high-water pins RSS at ~20x live use for the process's whole
//! life - the 1.7 GB plateau. The kernel reclaims such pages under real
//! pressure, but ps and any RSS-based watcher stay alarmed until then.
//! `malloc_zone_pressure_relief` returns free ranges at once; a tick that runs
//! only above a resident threshold keeps RSS near live use without churning a
//! quiet daemon.
//!
//! macOS only: the relief API is libmalloc's. Elsewhere `resident_kb` answers
//! `None` and the arm never fires - Linux allocators trim on their own free
//! paths.

use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::paths::AgentsHome;

/// Relief runs at most this often. The startup sweep's pages leave RSS within
/// one interval of the burst, and any mid-life burst decays the same way.
const RELIEF_INTERVAL: Duration = Duration::from_secs(60);

/// Relief fires only above this resident size: under it the daemon is already
/// near live use (measured 73-90 MB over a two-hour fleet daemon), and
/// pressure_relief would only churn page tables.
const RELIEF_ABOVE_RSS_KB: u64 = 160 * 1024;

/// Probes at these sizes, one per libmalloc zone class, so the pass reaches
/// every zone the daemon allocates in (tiny/nano, small, medium, large) and
/// not just the default one.
#[cfg(target_os = "macos")]
const PROBE_SIZES: [usize; 4] = [16, 4096, 65_536, 1 << 20];

pub struct Arm {
    last_tick: Mutex<Option<Instant>>,
}

impl Default for Arm {
    fn default() -> Self {
        Self {
            last_tick: Mutex::new(None),
        }
    }
}

/// The gate as a pure function: relief matters only above the threshold.
fn should_relief(rss_kb: Option<u64>) -> bool {
    rss_kb.is_some_and(|kb| kb > RELIEF_ABOVE_RSS_KB)
}

/// This process's resident size in KB, or `None` where the platform has no
/// cheap self read (the arm then never fires).
#[cfg(target_os = "macos")]
fn resident_kb() -> Option<u64> {
    let mut task: libc::proc_taskinfo = unsafe { std::mem::zeroed() };
    let size = std::mem::size_of::<libc::proc_taskinfo>() as libc::c_int;
    let written = unsafe {
        libc::proc_pidinfo(
            libc::getpid(),
            libc::PROC_PIDTASKINFO,
            0,
            &mut task as *mut _ as *mut libc::c_void,
            size,
        )
    };
    (written == size).then(|| task.pti_resident_size / 1024)
}

#[cfg(not(target_os = "macos"))]
fn resident_kb() -> Option<u64> {
    None
}

/// Touch one allocation per zone class, collect the distinct zones behind
/// them, drop the probes, then relief every collected zone. Relief frees only
/// free ranges, so live allocations are untouched; returning the zones is
/// enough because the pass runs where the daemon just freed a burst.
#[cfg(target_os = "macos")]
fn relief_pass() -> usize {
    let mut zones: Vec<*mut libc::c_void> = Vec::new();
    for size in PROBE_SIZES {
        let layout = std::alloc::Layout::from_size_align(size, 16).expect("constant layout");
        let probe = unsafe { std::alloc::alloc(layout) };
        if probe.is_null() {
            continue;
        }
        let zone = unsafe { malloc_zone_from_ptr(probe.cast()) };
        if !zone.is_null() && !zones.contains(&zone) {
            zones.push(zone);
        }
        unsafe { std::alloc::dealloc(probe, layout) };
    }
    let default = unsafe { malloc_default_zone() };
    if !default.is_null() && !zones.contains(&default) {
        zones.push(default);
    }
    let count = zones.len();
    for zone in zones {
        // blanket = 0: return as many free pages as the zone can, keeping the
        // zone itself alive for future allocations.
        unsafe { malloc_zone_pressure_relief(zone, 0) };
    }
    count
}

#[cfg(not(target_os = "macos"))]
fn relief_pass() -> usize {
    0
}

/// One gated relief: at most once per [`RELIEF_INTERVAL`], only above the
/// resident threshold, counted and emitted so a RSS drop the meter reads is
/// attributable.
pub fn maybe_tick(arm: &Arm, home: AgentsHome) {
    {
        let mut last = arm.last_tick.lock().unwrap_or_else(|e| e.into_inner());
        if last.is_some_and(|t| t.elapsed() < RELIEF_INTERVAL) {
            return;
        }
        *last = Some(Instant::now());
    }
    let Some(before_kb) = resident_kb() else {
        return;
    };
    if !should_relief(Some(before_kb)) {
        return;
    }
    let zones = relief_pass();
    let after_kb = resident_kb().unwrap_or(0);
    let _ = crate::events::EventEmitter::new(home.events_jsonl(), "daemon").emit(
        "page_relief_done",
        &serde_json::json!({
            "rss_kb_before": before_kb,
            "rss_kb_after": after_kb,
            "zones": zones,
        }),
    );
}

#[cfg(target_os = "macos")]
extern "C" {
    fn malloc_zone_from_ptr(ptr: *const libc::c_void) -> *mut libc::c_void;
    fn malloc_default_zone() -> *mut libc::c_void;
    fn malloc_zone_pressure_relief(zone: *mut libc::c_void, blanket: libc::c_int);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relief_gates_on_the_resident_threshold() {
        assert!(!should_relief(None), "no self read: never fire");
        assert!(!should_relief(Some(0)), "an empty daemon needs no relief");
        assert!(
            !should_relief(Some(RELIEF_ABOVE_RSS_KB)),
            "at the threshold: hold"
        );
        assert!(should_relief(Some(RELIEF_ABOVE_RSS_KB + 1)));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn relief_reaches_more_than_the_default_zone_and_keeps_live_pages() {
        // Live allocations the pass must not disturb, one per zone class.
        let mut keep = Vec::new();
        for size in PROBE_SIZES {
            let layout = std::alloc::Layout::from_size_align(size, 16).expect("constant layout");
            let probe = unsafe { std::alloc::alloc(layout) };
            assert!(!probe.is_null());
            let slots = unsafe { std::slice::from_raw_parts_mut(probe, size) };
            for (i, slot) in slots.iter_mut().enumerate().step_by(16) {
                *slot = (i % 251) as u8;
            }
            keep.push((probe, layout));
        }
        let zones = relief_pass();
        assert!(
            zones >= 2,
            "expected tiny/small/medium/large to span zones, got {zones}"
        );
        for (probe, layout) in keep {
            let slots = unsafe { std::slice::from_raw_parts_mut(probe, layout.size()) };
            for (i, slot) in slots.iter_mut().enumerate().step_by(16) {
                assert_eq!(
                    *slot,
                    (i % 251) as u8,
                    "live page {probe:p} byte {i} disturbed"
                );
            }
            unsafe { std::alloc::dealloc(probe, layout) };
        }
    }
}
