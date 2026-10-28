//! The user's pin: a row is pinned when a keep mark stands on its session id
//! or its lineage. Read-time projection over the marker files
//! `fno agents reap --keep` writes (fno-agents `keep_mark.rs` owns the
//! format), so the sideline and every reaper read one fact.

use super::{registry_path, RegistryAgent};
use std::path::{Path, PathBuf};

/// Stamp `pinned` onto rows whose session id, a recorded predecessor, or a
/// fork source carries a keep marker. The same lineage the reapers read
/// (`keep_mark::for_row`), so a row never shows a pin the sweep ignores. An
/// unreadable store reads as unpinned here: the reapers fail closed on their
/// own read, and a missing mark is never a wrong one.
pub(crate) fn overlay_keep_marks(rows: &mut [RegistryAgent]) {
    overlay_keep_marks_at(rows, &keep_dir());
}

/// `keep/` beside the registry, resolved the way [`registry_path`] resolves
/// the agents home.
fn keep_dir() -> PathBuf {
    let reg = registry_path();
    reg.parent().unwrap_or(Path::new(".")).join("keep")
}

pub(crate) fn overlay_keep_marks_at(rows: &mut [RegistryAgent], keep_dir: &Path) {
    // One listing per tick, not one stat per row and lineage edge.
    let Ok(entries) = std::fs::read_dir(keep_dir) else {
        return;
    };
    let marked: std::collections::HashSet<String> = entries
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().into_string().ok()?;
            name.strip_suffix(".json").map(str::to_string)
        })
        .collect();
    if marked.is_empty() {
        return;
    }
    for row in rows.iter_mut() {
        row.pinned = row
            .harness_session_id
            .iter()
            .chain(&row.predecessor_session_ids)
            .chain(&row.forked_from_session_id)
            .any(|sid| marked.contains(sid));
    }
}
