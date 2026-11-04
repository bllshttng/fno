//! The free-disk floor: the spawn gate refuses a new build worker while the
//! data volume holds less than `agents.min_free_disk_gb`, and the janitor
//! runs when free space crosses it instead of on its own clock.
//!
//! A fleet of cargo builds filled the volume to 153 MiB and every build died
//! with "No space left on device"; the gate had read RAM and CPU but never
//! disk.

use std::path::Path;
use std::time::Duration;

use crate::paths::AgentsHome;

/// Gap between two low-disk janitor runs. The refusal path and the daemon
/// arm share one stamp, so a burst of refused spawns sweeps once.
const LOW_DISK_SWEEP_GAP: Duration = Duration::from_secs(10 * 60);

/// Free GB on the volume holding `path`, as an unprivileged writer sees it.
pub fn free_disk_gb(path: &Path) -> Option<f64> {
    use std::os::unix::ffi::OsStrExt;
    let c_path = std::ffi::CString::new(path.as_os_str().as_bytes()).ok()?;
    let mut buf: libc::statvfs = unsafe { std::mem::zeroed() };
    // SAFETY: `c_path` is NUL-terminated and `buf` is a valid out pointer.
    if unsafe { libc::statvfs(c_path.as_ptr(), &mut buf) } != 0 || buf.f_frsize == 0 {
        return None;
    }
    Some(buf.f_bavail as f64 * buf.f_frsize as f64 / 1e9)
}

/// The decision core, pure so a test needs no full disk. `<= 0` disables the
/// floor; an unreadable volume passes (fail open, as the RAM term does).
pub(crate) fn disk_floor_term(free_gb: Option<f64>, floor_gb: f64) -> Option<String> {
    if floor_gb > 0.0 && free_gb.is_some_and(|f| f < floor_gb) {
        return Some(format!(
            "free disk {:.1}GB is below the min_free_disk_gb floor {floor_gb:.1}GB",
            free_gb.unwrap()
        ));
    }
    None
}

/// Run the janitor because free space crossed the floor. Returns the sweep
/// note, or `None` when the floor holds or a sweep ran inside the gap.
pub fn sweep_when_low(home: &AgentsHome, free_gb: Option<f64>, floor_gb: f64) -> Option<String> {
    disk_floor_term(free_gb, floor_gb)?;
    crate::reclaim::run_low_disk_sweep(home, LOW_DISK_SWEEP_GAP)
}

/// The daemon arm: read the volume each tick and sweep when it is low.
pub fn maybe_sweep(home: &AgentsHome, floor_gb: f64) {
    let _ = sweep_when_low(home, free_disk_gb(home.root()), floor_gb);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn floor_term_refuses_below_and_names_the_floor() {
        let term = disk_floor_term(Some(0.15), 20.0).expect("below floor refuses");
        assert!(term.contains("min_free_disk_gb floor 20.0GB"), "{term}");
        assert_eq!(disk_floor_term(Some(34.0), 20.0), None);
        assert_eq!(disk_floor_term(Some(0.1), 0.0), None, "<= 0 disables");
        assert_eq!(disk_floor_term(None, 20.0), None, "unreadable fails open");
    }

    #[test]
    fn gate_refuses_under_the_floor_and_names_floor_and_janitor() {
        let _env = crate::reclaim::tests::ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let td = tempfile::tempdir().unwrap();
        let home = AgentsHome::at(td.path());
        std::env::set_var("FNO_RECLAIM_STATE_ROOT", td.path());
        // A fresh stamp keeps the test from sweeping the real machine.
        let stamp = crate::state_layout::place(td.path(), "reclaim");
        std::fs::create_dir_all(&stamp).unwrap();
        std::fs::write(stamp.join("low-disk-last-run"), b"").unwrap();
        let refusal = crate::spawn_gate::check_disk_floor(&home, 1e12).unwrap_err();
        std::env::remove_var("FNO_RECLAIM_STATE_ROOT");
        let receipt = refusal.receipt.expect("receipt");
        assert_eq!(receipt["reason"], "disk_floor");
        assert_eq!(receipt["min_free_disk_gb"], 1e12);
        assert!(receipt["janitor"].as_str().unwrap().contains("janitor"));
        assert!(crate::spawn_gate::check_disk_floor(&home, 0.0).is_ok());
    }

    #[test]
    fn free_disk_reads_a_real_volume() {
        let td = tempfile::tempdir().unwrap();
        assert!(free_disk_gb(td.path()).is_some_and(|g| g > 0.0));
    }
}
