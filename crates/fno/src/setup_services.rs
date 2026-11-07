//! The launchd service facts the service steps and detection share: the
//! label derivation, the platform gate, and bounded `launchctl list`
//! reads. Nothing here writes; the install commands the steps run own
//! that.

use std::time::Duration;

use sha2::{Digest, Sha256};

/// The test seam env var. Mirrors `fno-agents`' `FNO_TEST_LAUNCHD_SUFFIX`;
/// set it to pin the suffix for tests. Production code must never set it.
pub(crate) const FNO_TEST_LAUNCHD_SUFFIX: &str = "FNO_TEST_LAUNCHD_SUFFIX";

/// This HOME's pr-watcher launchd label: the fixed stem plus the
/// HOME-derived suffix.
pub(crate) fn pr_watcher_label() -> String {
    format!("sh.fno.pr-watcher.{}", launchd_suffix())
}

/// This HOME's daily groom launchd label.
pub(crate) fn groom_label() -> String {
    format!("sh.fno.groom.{}", launchd_suffix())
}

/// This HOME's launchd label suffix: 8 hex chars of sha256(HOME). A missing
/// or empty HOME reads as `00000000` and logs nothing.
///
/// Byte-for-byte twin of `fno-agents::launchd_labels::launchd_suffix` (fno
/// cannot depend on that crate). The parity unit tests in both crates pin
/// the same vectors and fail if either drifts.
pub(crate) fn launchd_suffix() -> String {
    if let Ok(pinned) = std::env::var(FNO_TEST_LAUNCHD_SUFFIX) {
        if !pinned.is_empty() {
            return pinned;
        }
    }
    match std::env::var("HOME") {
        Ok(home) if !home.is_empty() => suffix_for(&home),
        _ => "00000000".to_string(),
    }
}

/// The pure derivation the twin pins: first 8 hex of sha256 over `home`.
pub(crate) fn suffix_for(home: &str) -> String {
    hex(&Sha256::digest(home.as_bytes()))[..8].to_string()
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Installer subprocess cap: an install may copy plists or compile, so it
/// gets far more than a detection probe before the poll loop kills it.
pub(crate) const INSTALL_CAP: Duration = Duration::from_secs(30);

/// The service rows are macOS launchd; anywhere else they report
/// needs_human with their command and load nothing.
pub(crate) fn macos() -> bool {
    cfg!(target_os = "macos")
}

/// `launchctl list` once, each line's label read off its last whitespace
/// column. `None` when the read fails or times out: the caller reports
/// rather than guessing.
pub(crate) fn loaded_labels() -> Option<Vec<String>> {
    const LOAD_CAP: Duration = Duration::from_secs(5);
    let out = crate::setup_detect::bounded_for(&["launchctl", "list"], LOAD_CAP).ok()?;
    Some(
        out.lines()
            .filter_map(|l| l.split_whitespace().last().map(String::from))
            .collect(),
    )
}

/// Whether one label is loaded. An unreadable `launchctl list` reads as
/// not loaded, so the step offers the install instead of claiming a
/// service runs when it cannot see.
pub(crate) fn is_loaded(label: &str) -> bool {
    loaded_labels()
        .map(|labels| labels.iter().any(|l| l == label))
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn launchd_label_suffix_pins_the_shared_vectors() {
        // Pinned in crates/fno-agents/src/launchd_labels.rs too; both must
        // agree byte-for-byte.
        assert_eq!(suffix_for("/Users/bb16"), "ba7b41ec");
        assert_eq!(suffix_for("/tmp/fno-a"), "ecbdc45b");
        assert_eq!(suffix_for("/tmp/fno-b"), "ea340a9b");
    }

    #[test]
    fn launchd_label_distinct_homes_derive_distinct_suffixes() {
        let a = suffix_for("/tmp/fno-a");
        let b = suffix_for("/tmp/fno-b");
        assert_ne!(a, b);
        assert_eq!(a.len(), 8);
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn launchd_label_names_carry_the_suffix() {
        let pw = pr_watcher_label();
        let gr = groom_label();
        assert!(pw.starts_with("sh.fno.pr-watcher."));
        assert!(gr.starts_with("sh.fno.groom."));
        assert_eq!(pw.rsplit('.').next().unwrap().len(), 8);
        assert_eq!(gr.rsplit('.').next().unwrap().len(), 8);
    }
}
