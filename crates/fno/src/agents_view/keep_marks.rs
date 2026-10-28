//! The user's pin: a keep mark on a row's session id or its lineage, read
//! from the marker files `fno agents reap --keep` writes (fno-agents
//! `keep_mark.rs` owns the format), so the sideline and every reaper read one
//! fact.

use super::{registry_path, RegistryAgent};
use std::path::Path;

/// Stamp `pinned` from `keep/` beside the registry. An unreadable store reads
/// as unpinned here: the reapers fail closed on their own read.
pub(crate) fn overlay_keep_marks(rows: &mut [RegistryAgent]) {
    let reg = registry_path();
    overlay_keep_marks_at(rows, &reg.parent().unwrap_or(Path::new(".")).join("keep"));
}

/// The same lineage the reapers read (`keep_mark::for_row`), so a row never
/// shows a pin the sweep ignores. One listing per tick, not a stat per row.
pub(crate) fn overlay_keep_marks_at(rows: &mut [RegistryAgent], keep_dir: &Path) {
    let Ok(entries) = std::fs::read_dir(keep_dir) else {
        return;
    };
    let marked: std::collections::HashSet<String> = entries
        .flatten()
        .filter_map(|e| {
            Some(
                e.file_name()
                    .into_string()
                    .ok()?
                    .strip_suffix(".json")?
                    .into(),
            )
        })
        .collect();
    for row in rows.iter_mut() {
        row.pinned = row
            .harness_session_id
            .iter()
            .chain(&row.predecessor_session_ids)
            .chain(&row.forked_from_session_id)
            .any(|sid| marked.contains(sid));
    }
}
