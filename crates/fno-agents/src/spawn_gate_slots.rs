//! The slot axis beside the cap: which live rows take a worker slot, and when
//! the learned cap yields to the machine gates.

use crate::capacity::Effective;
use crate::state::RegistryEntry;

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
