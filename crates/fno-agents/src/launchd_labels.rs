//! The launchd label derivation: one HOME-derived suffix per install, so two
//! fno HOMEs on one macOS account never share a label.
//!
//! launchd keys jobs by label inside the uid-scoped `gui/<uid>` domain, so a
//! second HOME's install with a fixed label replaces the first HOME's
//! registration (x-781d, measured in the x-f5c6 clean-run events journal).
//! The suffix is the first 8 hex chars of sha256 over the HOME env value:
//! stable per HOME, distinct across HOMEs, and self-consistent inside a job
//! because the plist template pins HOME in its EnvironmentVariables.
//!
//! The plist FILENAME keeps no suffix: the per-HOME `~/Library/LaunchAgents`
//! directory already separates files; only the label is a shared namespace.
//!
//! `FNO_TEST_LAUNCHD_SUFFIX` pins the suffix for tests and golden capture;
//! production code must never set it.
//!
//! The `fno` crate carries a byte-for-byte twin of [`suffix_for`] in
//! `setup_services.rs` (fno cannot depend on this crate); the parity unit
//! tests in both crates pin the same vectors and fail if either drifts.

use std::path::Path;

use sha2::{Digest, Sha256};

/// The test seam env var. Set it to pin the suffix (e.g. `testhome`).
pub const FNO_TEST_LAUNCHD_SUFFIX: &str = "FNO_TEST_LAUNCHD_SUFFIX";

/// The label fno used before x-781d. Installs retire a registration under
/// this label when its plist path points at THIS HOME; a foreign path is
/// another HOME's job and is left alone.
pub const LEGACY_PR_WATCHER_LABEL: &str = "sh.fno.pr-watcher";

/// The groom label before x-781d.
pub const LEGACY_GROOM_LABEL: &str = "sh.fno.groom";

/// The watcher's plist file name (no suffix; the per-HOME LaunchAgents
/// directory already separates files).
pub const PR_WATCHER_PLIST_FILENAME: &str = "sh.fno.pr-watcher.plist";

/// The groom agent's plist file name (no suffix).
pub const GROOM_PLIST_FILENAME: &str = "sh.fno.groom.plist";

/// This HOME's launchd label suffix: 8 hex chars of sha256(HOME). A missing
/// or empty HOME reads as `00000000` and logs nothing.
pub fn launchd_suffix() -> String {
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
pub fn suffix_for(home: &str) -> String {
    hex(&Sha256::digest(home.as_bytes()))[..8].to_string()
}

/// This HOME's pr-watcher launchd label.
pub fn pr_watcher_label() -> String {
    format!("sh.fno.pr-watcher.{}", launchd_suffix())
}

/// This HOME's daily groom launchd label.
pub fn groom_label() -> String {
    format!("sh.fno.groom.{}", launchd_suffix())
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// One receipt line per action taken migrating the legacy fixed label to
/// this HOME's derived label. A registration whose plist path canonicalizes
/// to `expected_plist` (this HOME's own plist) is booted out; a foreign one
/// stays and the receipt names it. No-op off macOS or without a HOME.
///
/// Goes through PATH-launchctl, so the parity harness's stub answers it and
/// the goldens stay machine-independent.
pub fn retire_legacy_fixed_label(legacy: &str, expected_plist: &Path, what: &str) -> Vec<String> {
    let mut receipts = Vec::new();
    if !cfg!(target_os = "macos") {
        return receipts;
    }
    let Some(uid_num) = uid() else {
        return receipts;
    };
    let print = std::process::Command::new("launchctl")
        .args(["print", &format!("gui/{uid_num}/{legacy}")])
        .output();
    let Ok(print) = print else {
        return receipts;
    };
    let text = String::from_utf8_lossy(&print.stdout).to_string();
    let Some(path) = first_path_line(&text) else {
        // Not registered: nothing to migrate.
        return receipts;
    };
    // The registration is this HOME's own only when its plist path IS the
    // expected plist; anything else (another HOME's plist, an unreadable or
    // unknown path) stays and the receipt names where it points. The
    // foreign-vs-unknown split does not change the action, so one compare
    // covers both. This must NOT route through tick_ledger's foreign probe:
    // that one pins the watcher's plist filename, which would misread a
    // groom registration as foreign.
    if !paths_match(&path, expected_plist) {
        receipts.push(format!(
            "left the {legacy} registration in place (registered from {path}, \
             not this HOME's {what} plist); this HOME's {what} now runs under \
             a HOME-derived label"
        ));
        return receipts;
    }
    let boot = std::process::Command::new("launchctl")
        .args(["bootout", &format!("gui/{uid_num}/{legacy}")])
        .output();
    match boot {
        Ok(b) if b.status.success() => receipts.push(format!(
            "retired the legacy {legacy} registration from this HOME's plist; \
             the HOME-derived label now owns the {what}"
        )),
        _ => receipts.push(format!(
            "could not boot out the legacy {legacy} registration; \
             run `launchctl bootout gui/$(id -u)/{legacy}` by hand"
        )),
    }
}

/// The registered plist path from `launchctl print` output: the first
/// `path = ` line (stdout/stderr path lines are the job's logs, not the
/// plist).
fn first_path_line(print_stdout: &str) -> Option<String> {
    print_stdout.lines().find_map(|line| {
        line.trim()
            .strip_prefix("path = ")
            .map(|p| p.trim().to_string())
    })
}

/// Canonical-equality read of two paths; a nonexistent candidate never
/// matches (the tick_ledger foreign probe shares this rule).
fn paths_match(cand: &str, expected: &Path) -> bool {
    let cand = Path::new(cand);
    match (std::fs::canonicalize(cand), std::fs::canonicalize(expected)) {
        (Ok(c), Ok(e)) => c == e,
        _ => false,
    }
}

fn uid() -> Option<u32> {
    // SAFETY: getuid reads a per-process kernel value; it cannot fail or race.
    Some(unsafe { libc::getuid() })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn suffix_pins_the_shared_vectors() {
        // Pinned in crates/fno/src/setup_services.rs too; both must agree.
        assert_eq!(suffix_for("/Users/bb16"), "ba7b41ec");
        assert_eq!(suffix_for("/tmp/fno-a"), "ecbdc45b");
        assert_eq!(suffix_for("/tmp/fno-b"), "ea340a9b");
    }

    #[test]
    fn distinct_homes_derive_distinct_suffixes() {
        let a = suffix_for("/tmp/fno-a");
        let b = suffix_for("/tmp/fno-b");
        assert_ne!(a, b);
        assert_eq!(a.len(), 8);
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn the_same_home_derives_the_same_suffix() {
        assert_eq!(suffix_for("/Users/bb16"), suffix_for("/Users/bb16"));
    }

    #[test]
    fn labels_carry_the_suffix() {
        // No env seam is set in the unit-test process: HOME-derived shape.
        let pw = pr_watcher_label();
        let gr = groom_label();
        assert!(pw.starts_with("sh.fno.pr-watcher."));
        assert!(gr.starts_with("sh.fno.groom."));
        assert_eq!(pw.rsplit('.').next().unwrap().len(), 8);
        assert_eq!(gr.rsplit('.').next().unwrap().len(), 8);
    }
}
