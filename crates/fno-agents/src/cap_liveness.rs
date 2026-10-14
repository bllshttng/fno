//! Which registry rows are live for the worker-slot cap. Split from
//! spawn_gate under the file budget: the cap's one liveness filter, the
//! report-freshness source that reaches rows no probe can, and the tests
//! that pin it. `spawn_gate` re-exports [`live_rows`] so every existing
//! reader keeps its path.

use std::path::Path;
use std::time::Duration;

use crate::claude_roster::ClaudeRoster;
use crate::daemon::pid_is_ours;
use crate::spawn_gate::NOTE;
use crate::state::{load_registry, Registry, RegistryEntry};

/// The liveness-filtered registry rows behind [`slot_count`], exposed so the
/// per-territory cap can read the rows' worked NODES without a second
/// liveness implementation.
pub(crate) fn live_rows(registry_path: &Path, warnings: &mut Vec<String>) -> Vec<RegistryEntry> {
    let live_roster_short_ids: std::collections::HashSet<String> = match ClaudeRoster::load_default(
    ) {
        Ok(roster) => roster
            .workers_deduped()
            .iter()
            .filter(|w| w.pid.map(|p| pid_is_ours(p, w.proc_start)).unwrap_or(false))
            .map(|w| w.short_id().to_string())
            .collect(),
        Err(e) => {
            warnings.push(format!(
                    "{NOTE} claude roster unreadable ({e}); pid-less roster-backed rows uncounted, report-stamped rows still counted"
                ));
            Default::default()
        }
    };
    let now_secs = crate::daemon::now_epoch_secs();
    let mut rows = Vec::new();
    match load_registry(registry_path) {
        Ok(Registry { entries, .. }) => {
            for e in entries {
                if !crate::spawn_gate::status_is_liveish(&e.status) {
                    continue;
                }
                let alive = match e.pid {
                    Some(p) => pid_is_ours(p, e.pid_start_time),
                    None => {
                        let roster_live = e
                            .transport_short()
                            .map(|sid| live_roster_short_ids.contains(sid));
                        if roster_live == Some(true) {
                            true
                        } else if roster_live == Some(false) && is_roster_policed(&e) {
                            // The roster polices this row (a claude fleet
                            // mint) and answers no: dead, at once - a dead
                            // bg worker frees its slot (Codex P1, PR #235).
                            false
                        } else {
                            // No oracle answers for this row: the user's own
                            // session, a non-claude native, or an unreadable
                            // roster. The last session report's freshness
                            // stands in (d-d0c63de7).
                            report_is_fresh(&e, now_secs)
                        }
                    }
                };
                if alive {
                    rows.push(e);
                }
            }
        }
        Err(e) => warnings.push(format!(
            "{NOTE} fno registry unreadable ({e}); slot count degraded to 0"
        )),
    }
    rows
}

/// How long one session report asserts occupancy for a pid-less row the
/// roster cannot answer. SessionStart re-fires on resume, clear and compact,
/// so a working session refreshes the stamp; a vanished session occupies a
/// slot at most this long after its last report. 24h: a user chat spans a
/// workday, and no shorter bound may drop a live chat mid-session
/// (d-d0c63de7). Bounded, never terminal (d-10a72d88): no exit is modeled.
const REPORT_FRESHNESS: Duration = Duration::from_secs(24 * 60 * 60);

/// The row shapes the claude roster POLICES: the fleet mints a claude bg
/// worker with origin `spawn` (claude_ask.rs, claude_stream_entry.rs), so
/// for these a roster miss is a definitive death. Every other pid-less row -
/// the user's own session (origin `operator`, d-d0c63de7), an adopted row, a
/// non-claude native - is outside the roster's jurisdiction.
fn is_roster_policed(e: &RegistryEntry) -> bool {
    e.harness_name() == "claude" && e.origin.as_deref() == Some("spawn")
}

/// Whether the row's last session report is inside [`REPORT_FRESHNESS`].
/// Absent or unparseable fails closed (never counted) - the discipline of
/// `InsideLegReport::is_live_at`.
fn report_is_fresh(e: &RegistryEntry, now_secs: i64) -> bool {
    match e
        .last_report_at
        .as_deref()
        .and_then(crate::state::rfc3339_like_to_secs)
    {
        Some(at) => now_secs.saturating_sub(at as i64) <= REPORT_FRESHNESS.as_secs() as i64,
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::RegistryEntry;
    use crate::{claims, AgentStatus};

    /// A pid-less session-report row: the native-adopt shape. `short_id` is
    /// set (every mint derives one) but absent from the pinned empty roster,
    /// which is exactly the population `live_rows` cannot answer today.
    fn pidless_reported_row(name: &str, origin: &str, harness: &str) -> RegistryEntry {
        let mut e = RegistryEntry::default();
        e.name = name.into();
        e.short_id = format!("{name}-transport");
        e.status = AgentStatus::Busy;
        e.origin = Some(origin.into());
        e.harness = Some(harness.into());
        e
    }

    /// The wall clock `live_rows` reads, rendered as a registry stamp.
    fn stamp_ago(secs: u64) -> String {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let t = chrono::DateTime::from_timestamp((now - secs) as i64, 0).unwrap();
        t.format("%Y-%m-%dT%H:%M:%SZ").to_string()
    }

    /// The empty-roster pin every live_rows liveness test needs: an empty
    /// daemon dir reads as a roster with zero workers, so no real worker on
    /// this machine can answer a row's transport lookup. The caller holds
    /// the env lock.
    fn empty_roster_dir() -> tempfile::TempDir {
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::create_dir_all(dir.path().join("daemon")).unwrap();
        std::env::set_var("FNO_CLAUDE_DAEMON_DIR", dir.path().join("daemon"));
        std::env::set_var("FNO_CLAIMS_ROOT", dir.path().join("claims-root"));
        dir
    }

    fn write_reg(dir: &tempfile::TempDir, entries: &[RegistryEntry]) -> std::path::PathBuf {
        let reg = dir.path().join("registry.json");
        let rows: Vec<serde_json::Value> = entries
            .iter()
            .map(|e| serde_json::to_value(e).unwrap())
            .collect();
        std::fs::write(
            &reg,
            serde_json::to_vec(&serde_json::json!({
                "schema_version": 1,
                "entries": rows
            }))
            .unwrap(),
        )
        .unwrap();
        reg
    }

    #[test]
    fn a_fresh_session_report_makes_a_pidless_native_row_an_occupant() {
        let _g = claims::test_env_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let dir = empty_roster_dir();
        let mut op = pidless_reported_row("user-chat", "operator", "claude");
        op.last_report_at = Some(stamp_ago(3600));
        // A native launch child: origin spawn, but the roster is claude-only,
        // so a codex row is outside its jurisdiction.
        let mut codex_native = pidless_reported_row("codex-native", "spawn", "codex");
        codex_native.last_report_at = Some(stamp_ago(3600));
        let reg = write_reg(&dir, &[op, codex_native]);
        let mut warnings = Vec::new();
        let rows = live_rows(&reg, &mut warnings);
        assert_eq!(rows.len(), 2, "{warnings:?}");
        assert_eq!(crate::spawn_gate::slot_count(&reg, &mut warnings), 2);
        std::env::remove_var("FNO_CLAUDE_DAEMON_DIR");
        std::env::remove_var("FNO_CLAIMS_ROOT");
    }

    #[test]
    fn a_stale_report_drops_the_occupant() {
        let _g = claims::test_env_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let dir = empty_roster_dir();
        let mut op = pidless_reported_row("user-chat", "operator", "claude");
        op.last_report_at = Some(stamp_ago(25 * 3600));
        let reg = write_reg(&dir, &[op]);
        let mut warnings = Vec::new();
        let rows = live_rows(&reg, &mut warnings);
        assert_eq!(rows.len(), 0, "{warnings:?}");
        std::env::remove_var("FNO_CLAUDE_DAEMON_DIR");
        std::env::remove_var("FNO_CLAIMS_ROOT");
    }

    #[test]
    fn a_fresh_stamp_never_overrides_a_roster_negative_for_a_fleet_row() {
        let _g = claims::test_env_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let dir = empty_roster_dir();
        // The claude fleet mint shape (claude_ask.rs:3263): origin spawn. The
        // roster polices these rows, so its negative stays definitive even
        // with a fresh stamp - the PR #235 P1 guard.
        let mut fleet = pidless_reported_row("bg-worker", "spawn", "claude");
        fleet.last_report_at = Some(stamp_ago(60));
        let reg = write_reg(&dir, &[fleet]);
        let mut warnings = Vec::new();
        let rows = live_rows(&reg, &mut warnings);
        assert_eq!(rows.len(), 0, "{warnings:?}");
        std::env::remove_var("FNO_CLAUDE_DAEMON_DIR");
        std::env::remove_var("FNO_CLAIMS_ROOT");
    }
}
