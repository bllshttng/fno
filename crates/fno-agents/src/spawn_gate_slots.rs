//! The slot axis beside the cap: which live rows take a worker slot, and when
//! the learned cap yields to the machine gates.

use crate::capacity::Effective;
use crate::state::RegistryEntry;
use std::path::Path;

/// How long the fleet must have been still before one spawn may pass
/// `agents.max_live`: the next CPU sample and RAM reading need the last
/// worker's start in them. A constant, not a config key - nobody has asked
/// to tune it.
pub(crate) const BURST_SETTLE: std::time::Duration = std::time::Duration::from_secs(60);

/// Splits the live rows into the rows that take a worker slot and the lead
/// count. A lead takes no worker slot. A lead is a row with a role, the same
/// test the lead-share axis counts teams by.
pub(crate) fn split_leads(rows: Vec<RegistryEntry>) -> (Vec<RegistryEntry>, usize) {
    let (leads, workers): (Vec<_>, Vec<_>) =
        rows.into_iter().partition(|row| row.role_level.is_some());
    (workers, leads.len())
}

/// True when the learned cap yields for a spawn the CPU axis admitted: the
/// count is at the learned cap, the cap sits under the user's
/// `agents.max_live`, and the RAM floor reads room. The learned cap is a
/// lagging guess; the CPU and RAM gates read the machine now. An unread RAM
/// figure is no room. Once it yields, `agents.max_live` binds.
pub(crate) fn cap_yields(learned: &Effective, counted: usize, floor_gb: f64, swap: f64) -> bool {
    if counted < learned.cap || learned.cap >= learned.ceiling {
        return false;
    }
    let mem = crate::spawn_gate::read_memory(floor_gb, swap);
    (floor_gb <= 0.0 || mem.avail.is_some())
        && crate::spawn_gate::ram_floor_term(mem.avail, floor_gb, mem.swap, mem.swapin_bps, swap)
            .is_none()
}

/// The cap that binds once the learned cap yields: the user's own
/// `agents.max_live`, so a refusal names that key and never the floor.
pub(crate) fn at_ceiling(learned: &Effective) -> Effective {
    Effective {
        cap: learned.ceiling,
        ceiling: learned.ceiling,
        known: true,
        ..Default::default()
    }
}

/// Why a spawn at or above `agents.max_live` may or may not pass this pass.
/// The word rides the receipt, so a refusal at the ceiling names the one
/// thing that held it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Burst {
    Yields,
    Settling { newest_s: u64 },
    SettleUnread,
    MemoryOff,
    MemoryUnread,
    MemoryLow,
}

impl Burst {
    pub(crate) fn word(&self) -> &'static str {
        match self {
            Burst::Yields => "yields",
            Burst::Settling { .. } => "settling",
            Burst::SettleUnread => "settle_unread",
            Burst::MemoryOff => "memory_off",
            Burst::MemoryUnread => "memory_unread",
            Burst::MemoryLow => "memory_low",
        }
    }

    /// The receipt keys a burst read rides into a refusal: `burst` names the
    /// word, and a still-settling fleet adds the settle facts.
    pub(crate) fn annotate(&self, receipt: &mut serde_json::Value) {
        receipt["burst"] = serde_json::json!(self.word());
        if let Burst::Settling { newest_s } = self {
            receipt["burst_settle_s"] = serde_json::json!(BURST_SETTLE.as_secs());
            receipt["newest_worker_s"] = serde_json::json!(newest_s);
        }
    }
}

/// The burst keys a refusal carries when the slot axis read the burst axis
/// this pass. One door for both refusal receipts, so they cannot disagree.
pub(crate) fn annotate_burst(burst: Option<&Burst>, receipt: &mut serde_json::Value) {
    if let Some(b) = burst {
        b.annotate(receipt);
    }
}

/// The clause a queued spawn's tick line gains while the fleet settles.
pub(crate) fn settling_suffix(burst: Option<&Burst>) -> String {
    match burst {
        Some(Burst::Settling { newest_s }) => format!(
            "; burst settling, newest worker {newest_s}s of {}s",
            BURST_SETTLE.as_secs()
        ),
        _ => String::new(),
    }
}

/// The queued tick line for a spawn held on the slot cap, settling note and all.
pub(crate) fn slot_tick_line(slots: usize, cap: usize, burst: Option<&Burst>) -> String {
    format!(
        "the slot cap, {slots} live of {cap}{}",
        settling_suffix(burst)
    )
}

/// The admitted slot reading when a burst passes the ceiling.
pub(crate) fn burst_slot_reading(slots: usize, cap: usize, ceiling: usize) -> String {
    format!("{slots}/{cap} ok (burst past agents.max_live {ceiling}: CPU and RAM have room)")
}

/// The pure burst decision, no machine reads: `memory_off` (a disabled floor
/// reads nothing, so it is not room), `memory_unread` (no available-RAM
/// reading), `memory_low` (the floor or swap term fired), `settle_unread` (no
/// age was readable anywhere), `settling` (the newest worker is younger than
/// [`BURST_SETTLE`]), else `yields`.
pub(crate) fn burst_verdict(
    floor_gb: f64,
    mem: &crate::spawn_gate::MemoryReading,
    swap_cap: f64,
    newest_s: Option<u64>,
) -> Burst {
    if floor_gb <= 0.0 {
        return Burst::MemoryOff;
    }
    if mem.avail.is_none() {
        return Burst::MemoryUnread;
    }
    if crate::spawn_gate::ram_floor_term(mem.avail, floor_gb, mem.swap, mem.swapin_bps, swap_cap)
        .is_some()
    {
        return Burst::MemoryLow;
    }
    let Some(newest_s) = newest_s else {
        return Burst::SettleUnread;
    };
    if newest_s < BURST_SETTLE.as_secs() {
        return Burst::Settling { newest_s };
    }
    Burst::Yields
}

/// The youngest age, in seconds, over every input the settle window reads:
/// counted rows by `created_at`, live reservations by `age_s` (the spawn's
/// own redeemable one skipped, as the count already skips it), and launch
/// records written inside the window. A row that does not parse gives no
/// age; when nothing reads, `None` and the burst does not yield.
pub(crate) fn newest_slot_age_s(
    rows: &[RegistryEntry],
    reservations: &[crate::spawn_gate::SlotReservation],
    own: &str,
    launches_root: Option<&Path>,
    now: chrono::DateTime<chrono::Utc>,
) -> Option<u64> {
    let mut newest: Option<u64> = None;
    let note = |age_s: u64, newest: &mut Option<u64>| {
        *newest = Some(newest.map_or(age_s, |n| n.min(age_s)));
    };
    let now_ms = now.timestamp_millis();
    for row in rows {
        if let Ok(created) = chrono::DateTime::parse_from_rfc3339(&row.created_at) {
            note(
                ((now_ms - created.timestamp_millis()).max(0) / 1000) as u64,
                &mut newest,
            );
        }
    }
    for reservation in reservations {
        if reservation.redeemable && reservation.name == own {
            continue;
        }
        if let Some(age_s) = reservation.age_s {
            note(age_s, &mut newest);
        }
    }
    if let Some(root) = launches_root {
        let since_ms = now_ms - BURST_SETTLE.as_millis() as i64;
        for (_, record) in crate::launch_record::records(root, since_ms) {
            note(
                ((now_ms - record.launched_at_ms).max(0) / 1000) as u64,
                &mut newest,
            );
        }
    }
    newest
}

/// The burst read for a spawn whose count sits at or above the user's
/// `agents.max_live`: `None` unless the count is at the ceiling and the cap
/// the gate carries is the ceiling too (the learned cap has already
/// yielded). A disabled memory axis answers `MemoryOff` and reads nothing.
pub(crate) fn burst(
    counted: usize,
    learned: &Effective,
    floor_gb: f64,
    swap_cap: f64,
    rows: &[RegistryEntry],
    reservations: &[crate::spawn_gate::SlotReservation],
    own: &str,
    launches_root: Option<&Path>,
) -> Option<Burst> {
    if counted < learned.ceiling || learned.cap < learned.ceiling {
        return None;
    }
    if floor_gb <= 0.0 {
        return Some(Burst::MemoryOff);
    }
    let mem = crate::spawn_gate::read_memory(floor_gb, swap_cap);
    let newest_s = newest_slot_age_s(rows, reservations, own, launches_root, chrono::Utc::now());
    Some(burst_verdict(floor_gb, &mem, swap_cap, newest_s))
}

/// The cap that binds while a burst passes the ceiling: one slot over the
/// count, so the next waiter sees a fleet at cap again and re-reads the
/// machine gates. One spawn a minute, not two.
pub(crate) fn past_ceiling(learned: &Effective, counted: usize) -> Effective {
    Effective {
        cap: counted + 1,
        ceiling: learned.ceiling,
        known: true,
        burst: true,
        ..Default::default()
    }
}
