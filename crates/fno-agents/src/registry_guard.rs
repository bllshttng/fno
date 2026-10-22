//! The shared-registry write guard: the two refusals every Rust registry
//! write routes through at [`crate::state::update_registry`], and Python's
//! Python's registry writers commit through `registry-commit`, which runs the
//! same check over the before and after documents.
//!
//! 2026-09-27 a worker's probe called Python's `write_registry` with no
//! pinned state dir and replaced the live `~/.fno/agents/registry.json` with
//! a 1-row fixture; every team stamp since the last backup was lost. Two
//! independent arms, either of which stops that class:
//!
//! 1. A process carrying a test or probe marker (`cargo test`,
//!    `PYTEST_CURRENT_TEST`, `FNO_TEST_HERMETIC=1`) never writes the real
//!    shared registry. The remedy is pinning the process's own state root
//!    (`FNO_AGENTS_HOME`), not an override - the same move-the-target
//!    philosophy as the source-ahead fence.
//! 2. A write that drops most live rows is a fixture or a bug, not a roster
//!    update. Refused unless `FNO_REGISTRY_ALLOW_ROW_LOSS=1` names the drop
//!    deliberate. Loss is by identity, never by status: a live row the write
//!    flips to exited is still present, and a row the reaper retires with a
//!    persisted receipt is declared, so neither counts.
//!
//! The guard stands down unless the target IS the ambient shared registry
//! and the home is not a tempdir sandbox: a pinned `FNO_AGENTS_HOME` or a
//! hermetic `HOME` is the caller's own store, which keeps every existing
//! test's writes legal.

use std::collections::{BTreeSet, HashSet};
use std::ffi::OsStr;
use std::path::Path;

use crate::state::RegistryEntry;
use crate::AgentStatus;

/// The row-loss override, shared verbatim with the Python mirror.
pub const ROW_LOSS_OVERRIDE_ENV: &str = "FNO_REGISTRY_ALLOW_ROW_LOSS";

/// The guard's inputs, all explicit so tests construct the case instead of
/// hoping the ambient environment reproduces it (same shape as
/// `source_ahead_root`).
pub struct GuardInputs<'a> {
    /// The file the write targets.
    pub target: &'a Path,
    /// The ambient `$HOME` the process resolved its root from.
    pub home: Option<&'a OsStr>,
    pub cfg_test: bool,
    pub pytest_current_test: bool,
    pub hermetic_claim: bool,
    pub row_loss_override: bool,
    pub loss: LiveLoss,
}

/// A write's live-row loss. `lost` counts the rows live before whose
/// identity is gone after and was not declared retiring; `after_live` is the
/// status count the refusal prints beside it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LiveLoss {
    pub before_live: usize,
    pub after_live: usize,
    pub lost: usize,
}

/// Decide the write. `Ok(())` proceeds; the `Err` text names the remedy.
pub fn check(inp: GuardInputs) -> Result<(), String> {
    let Some(home) = inp.home else {
        return Ok(());
    };
    let shared = crate::paths::AgentsHome::ambient_from(home).registry_json();
    if crate::paths::under_temp_dir(&shared) || !same_path(inp.target, &shared) {
        return Ok(());
    }
    if inp.cfg_test || inp.pytest_current_test || inp.hermetic_claim {
        return Err(format!(
            "refusing a write of the shared registry at {}: this process runs \
             under a test or probe marker, and fleet state is not a fixture. \
             Pin the process's own state root (FNO_AGENTS_HOME) instead.",
            shared.display()
        ));
    }
    if inp.row_loss_override {
        return Ok(());
    }
    let LiveLoss {
        before_live,
        after_live,
        lost,
    } = inp.loss;
    if before_live >= 2 && lost * 2 > before_live {
        return Err(format!(
            "refusing a write of the shared registry at {} that drops live \
             rows from {before_live} to {after_live} ({lost} live rows gone): \
             a write this destructive is a fixture or a bug, not a roster \
             update. Set {ROW_LOSS_OVERRIDE_ENV}=1 when the drop is deliberate.",
            shared.display(),
        ));
    }
    Ok(())
}

/// The env-reading entry [`crate::state::update_registry`] calls.
pub fn check_env(target: &Path, loss: LiveLoss) -> Result<(), String> {
    check(GuardInputs {
        target,
        home: std::env::var_os("HOME").as_deref(),
        cfg_test: cfg!(test),
        pytest_current_test: std::env::var_os("PYTEST_CURRENT_TEST").is_some(),
        hermetic_claim: std::env::var_os("FNO_TEST_HERMETIC").as_deref() == Some(OsStr::new("1")),
        row_loss_override: std::env::var_os("FNO_REGISTRY_ALLOW_ROW_LOSS").as_deref()
            == Some(OsStr::new("1")),
        loss,
    })
}

/// Live rows by the `OWNERSHIP_LIVE_STATUSES` vocabulary.
fn is_live(e: &RegistryEntry) -> bool {
    matches!(
        e.status,
        AgentStatus::Spawning
            | AgentStatus::Ready
            | AgentStatus::Idle
            | AgentStatus::Busy
            | AgentStatus::Live
            | AgentStatus::Restarting
    )
}

/// The row's identity: its harness session id, else its name.
fn identity(e: &RegistryEntry) -> &str {
    match e.harness_session_id.as_deref() {
        Some(sid) if !sid.is_empty() => sid,
        _ => &e.name,
    }
}

/// Measure a write's live-row loss. A row live before is lost when no row
/// after carries its identity, whatever status the survivor holds, unless
/// its name is in `declared` (rows the writer retires on purpose).
pub fn live_loss(
    before: &[RegistryEntry],
    after: &[RegistryEntry],
    declared: &BTreeSet<String>,
) -> LiveLoss {
    let present: HashSet<&str> = after.iter().map(identity).collect();
    let live_before: Vec<&RegistryEntry> = before.iter().filter(|e| is_live(e)).collect();
    LiveLoss {
        before_live: live_before.len(),
        after_live: after.iter().filter(|e| is_live(e)).count(),
        lost: live_before
            .iter()
            .filter(|e| !present.contains(identity(e)) && !declared.contains(&e.name))
            .count(),
    }
}

/// Canonicalized-equal, tolerating an absent file (a fresh install) by
/// falling back to the raw comparison - the same resolve-or-identity the
/// schema-bump guard uses.
fn same_path(a: &Path, b: &Path) -> bool {
    let canon = |p: &Path| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    canon(a) == canon(b)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn inputs(target: &Path, loss: LiveLoss) -> GuardInputs<'_> {
        GuardInputs {
            target,
            home: Some(OsStr::new("/home/operator")),
            cfg_test: false,
            pytest_current_test: false,
            hermetic_claim: false,
            row_loss_override: false,
            loss,
        }
    }

    fn shared_target() -> PathBuf {
        Path::new("/home/operator")
            .join(".fno")
            .join("agents")
            .join("registry.json")
    }

    /// The guard over a real before/after pair on the shared root.
    fn guard(
        before: &[RegistryEntry],
        after: &[RegistryEntry],
        declared: &BTreeSet<String>,
    ) -> Result<(), String> {
        check(inputs(&shared_target(), live_loss(before, after, declared)))
    }

    fn fleet(n: usize) -> Vec<RegistryEntry> {
        (0..n)
            .map(|i| RegistryEntry {
                name: format!("t-s{i}"),
                harness_session_id: Some(format!("s{i}")),
                status: AgentStatus::Live,
                ..Default::default()
            })
            .collect()
    }

    const MASS_DROP: LiveLoss = LiveLoss {
        before_live: 33,
        after_live: 1,
        lost: 32,
    };

    #[test]
    fn a_cargo_test_process_refuses_the_shared_root() {
        let target = shared_target();
        let err = check(GuardInputs {
            cfg_test: true,
            ..inputs(&target, MASS_DROP)
        })
        .unwrap_err();
        assert!(err.contains("test or probe marker"), "{err}");
        assert!(err.contains("FNO_AGENTS_HOME"), "{err}");
    }

    #[test]
    fn the_override_admits_a_deliberate_drop() {
        let target = shared_target();
        assert!(check(GuardInputs {
            row_loss_override: true,
            ..inputs(&target, MASS_DROP)
        })
        .is_ok());
    }

    #[test]
    fn a_status_flip_to_exited_is_not_loss() {
        let before = fleet(96);
        let mut after = before.clone();
        for e in after.iter_mut().take(74) {
            e.status = AgentStatus::Exited;
        }
        assert!(guard(&before, &after, &BTreeSet::new()).is_ok());
    }

    #[test]
    fn a_fixture_replacing_the_fleet_refuses() {
        let mut fixture = fleet(1);
        fixture[0].harness_session_id = Some("fixture".into());
        let err = guard(&fleet(5), &fixture, &BTreeSet::new()).unwrap_err();
        assert!(err.contains("drops live rows from 5 to 1"), "{err}");
        assert!(err.contains(ROW_LOSS_OVERRIDE_ENV), "{err}");
    }

    #[test]
    fn declared_retirements_are_not_loss() {
        let before = fleet(96);
        let after = &before[54..];
        assert!(guard(&before, after, &BTreeSet::new()).is_err());
        let declared = before[..54].iter().map(|e| e.name.clone()).collect();
        assert!(guard(&before, after, &declared).is_ok());
    }
}
