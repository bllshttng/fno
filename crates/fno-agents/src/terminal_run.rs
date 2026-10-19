//! Terminal-run receipts for spawned workers that finished their run.
//!
//! A released bg `/target` worker parks at its idle prompt with its registry
//! row still `live`; the terminal-stop marker refuses that shape while the
//! PR is open, so the spawn gate's lead_share count charges the row to its
//! lead forever. The receipt changes the COUNT, not the session: `finalize`
//! drops one keyed by the manifest harness session id on a releasing
//! terminal, the gate's `share_reading` skips a held row whose
//! `harness_session_id` carries one, and the next non-terminal loop-check
//! fire in that session clears it (the loop is live, the seat is in use
//! again). Rows without a harness session id never match and keep counting.
//!
//! Mirrors `terminal_stop.rs`: one pure decision, thin I/O helpers, unit
//! tests beside.

use crate::paths::AgentsHome;
use std::collections::HashSet;
use std::path::{Path, PathBuf};

/// Decide whether this fire maintains the receipt. Pure.
///
/// Same population as the terminal-stop marker minus the open-PR gate: the
/// session must be footnote-spawned (`agent_self`), not loop-run-driven
/// (`!driver_lib`), and the manifest must carry a syntactically-valid
/// harness session id so the filename can never escape the dir.
pub fn should_record(agent_self: bool, driver_lib: bool, key: Option<&str>) -> Option<&str> {
    if !agent_self || driver_lib {
        return None;
    }
    key.filter(|k| is_valid_key(k))
}

/// A harness session id usable as a filename: word characters and dashes
/// only, bounded. Rejecting anything else keeps the receipt filename inside
/// the receipt dir (no `/`, `..`, NUL).
fn is_valid_key(key: &str) -> bool {
    !key.is_empty()
        && key.len() <= 128
        && key
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// Write a receipt file named by `key`, content = `reason`. Best-effort at
/// the call site: the caller logs and moves on. Creates the dir if absent.
/// Revalidates the key at this effect boundary so the write is
/// self-defending, not reliant on the caller having run `should_record`.
pub fn write_receipt(home: &AgentsHome, key: &str, reason: &str) -> std::io::Result<PathBuf> {
    if !is_valid_key(key) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("refusing to write terminal-run receipt for invalid key: {key:?}"),
        ));
    }
    let dir = home.terminal_runs_dir();
    std::fs::create_dir_all(&dir)?;
    let path = dir.join(key);
    std::fs::write(&path, reason.as_bytes())?;
    Ok(path)
}

/// Remove a session's receipt (the loop went live again, or the sweep
/// stopped the row). Best-effort; a missing file is not an error. An
/// unexpected failure is logged, never silent: a stuck receipt would keep
/// a live loop off its lead's share.
pub fn remove_receipt(home: &AgentsHome, key: &str) {
    if !is_valid_key(key) {
        return;
    }
    if let Err(e) = std::fs::remove_file(home.terminal_runs_dir().join(key)) {
        if e.kind() != std::io::ErrorKind::NotFound {
            eprintln!("terminal_run: failed to remove receipt {key}: {e}");
        }
    }
}

/// The set of session ids carrying a receipt, read once per gate read. A
/// missing or unreadable dir reads as EMPTY: no skips, today's counting.
pub fn read_keys(dir: &Path) -> HashSet<String> {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return HashSet::new(),
    };
    let mut out = HashSet::new();
    for entry in entries.flatten() {
        let name = entry.file_name();
        if let Some(key) = name.to_str() {
            if is_valid_key(key) {
                out.insert(key.to_string());
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY: &str = "c79bce5b-1682-4a8c-97f4-d6254cf18112";

    #[test]
    fn records_spawned_non_driven_session() {
        assert_eq!(should_record(true, false, Some(KEY)), Some(KEY));
    }

    #[test]
    fn operator_and_driver_sessions_never_record() {
        // The operator's own terminal /target and a loop-run child are never
        // receipt writers: the driver owns its lifecycle, the operator is
        // present.
        assert_eq!(should_record(false, false, Some(KEY)), None);
        assert_eq!(should_record(true, true, Some(KEY)), None);
    }

    #[test]
    fn separator_bearing_key_rejected() {
        // A malformed manifest must never write outside the receipt dir.
        assert_eq!(should_record(true, false, Some("../../etc/passwd")), None);
        assert_eq!(should_record(true, false, Some("a/b")), None);
        assert_eq!(should_record(true, false, Some("")), None);
    }

    #[test]
    fn write_read_remove_roundtrip() {
        let tmp = std::env::temp_dir().join(format!("fno-tr-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let home = AgentsHome::at(&tmp);

        let p = write_receipt(&home, KEY, "DonePRGreen").unwrap();
        assert!(p.exists());

        let keys = read_keys(&home.terminal_runs_dir());
        assert!(keys.contains(KEY));

        remove_receipt(&home, KEY);
        assert!(read_keys(&home.terminal_runs_dir()).is_empty());
        // Removing again is a no-op (NotFound).
        remove_receipt(&home, KEY);
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn missing_dir_reads_as_empty_set() {
        let tmp = std::env::temp_dir().join(format!("fno-tr-missing-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        assert!(read_keys(&tmp.join("nope")).is_empty());
    }
}
