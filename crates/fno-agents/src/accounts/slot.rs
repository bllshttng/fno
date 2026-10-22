//! The managed slot stamp: which account id is materialized in a harness's
//! shared credential slot (`managed.active_slot_id`).

use std::path::{Path, PathBuf};

/// `managed.store_root`: `<state_dir>/providers`.
pub fn store_root(cwd: &Path) -> Option<PathBuf> {
    crate::agents_config::state_dir(cwd).map(|dir| dir.join("providers"))
}

/// The id stamped into `harness`'s slot, `Ok(None)` when never stamped.
/// Only a missing stamp is `None`; a present but unreadable stamp is an
/// error, because a corrupt store must abort a switch rather than skip the
/// capture before an overwrite.
pub fn active_slot_id(
    harness: &str,
    root: Option<&Path>,
    cwd: &Path,
) -> std::io::Result<Option<String>> {
    let root = match root {
        Some(root) => root.to_path_buf(),
        None => store_root(cwd).ok_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::NotFound, "no state dir resolves")
        })?,
    };
    match std::fs::read_to_string(root.join(format!(".active-{harness}"))) {
        Ok(text) => {
            let id = text.trim();
            Ok((!id.is_empty()).then(|| id.to_string()))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}
