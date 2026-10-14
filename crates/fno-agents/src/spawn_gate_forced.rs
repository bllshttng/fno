//! What a forced spawn passed. `--force` passes the soft limits and never the
//! RAM floor. This module words the one line a forced spawn prints.

use std::path::Path;

use crate::spawn_gate::{check_cpu_axis, footprint_cause_raw, slot_reading, NOTE};

/// The line for a forced spawn: each soft limit it passed, then the cap the
/// loop reads. `provider` names a lane cap the spawn passed, when it did.
pub(crate) fn report(
    registry_path: &Path,
    config_cwd: &Path,
    prefetched: Option<&str>,
    probe_err: Option<&str>,
    provider: Option<String>,
) -> String {
    let learned = crate::capacity::effective_for(config_cwd);
    let mut warnings = Vec::new();
    let (live, reservations) = slot_reading(registry_path, &mut warnings);
    let slots = live.len() + reservations.len();
    let mut passed = Vec::new();
    if slots >= learned.cap {
        passed.push(format!("the slot cap, {slots} live of {}", learned.cap));
    }
    passed.extend(provider);
    let cpu = check_cpu_axis(prefetched, probe_err).payload;
    if cpu.verdict == "hold" {
        passed.push(format!("the fleet CPU hold ({})", cpu.reason));
    }
    let tail = match passed.is_empty() {
        true => "no soft limit was in the way".to_string(),
        false => format!("passed {}", passed.join(", ")),
    };
    format!(
        "{NOTE} forced (--force): {tail}. {}. The RAM floor still applies.",
        learned.clause()
    )
}

/// [`report`] for a caller that has not read the footprint probe yet.
pub(crate) fn report_probing(registry_path: &Path, config_cwd: &Path) -> String {
    let (raw, why) = match footprint_cause_raw() {
        Ok(raw) => (Some(raw), None),
        Err(why) => (None, Some(why)),
    };
    report(
        registry_path,
        config_cwd,
        raw.as_deref(),
        why.as_deref(),
        None,
    )
}

#[cfg(test)]
mod tests {
    use crate::spawn_gate::{run_gate, GateFlags, GateInput, EXIT_RAM_REFUSED};

    /// `--force` passes the soft limits and never the RAM floor: a floor no
    /// machine can meet still refuses a forced spawn, with the RAM exit code.
    #[test]
    fn the_force_branch_still_refuses_on_ram() {
        let _g = crate::claims::test_env_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let _id = crate::spawn_gate_admission::AgentSelfFixture::set();
        let dir = tempfile::tempdir().unwrap();
        let fnodir = dir.path().join(".fno");
        std::fs::create_dir_all(&fnodir).unwrap();
        std::fs::write(
            fnodir.join("config.toml"),
            "[agents]\nmax_live = 999\nmin_free_gb = 1000000\nmax_swap_pct = 0\n",
        )
        .unwrap();
        std::env::set_var("FNO_HOME", dir.path());
        let prior_claims = std::env::var_os("FNO_CLAIMS_ROOT");
        std::env::set_var("FNO_CLAIMS_ROOT", dir.path().join("claims-root"));
        let prior_spawn_gate = std::env::var_os("FNO_SPAWN_GATE");
        std::env::remove_var("FNO_SPAWN_GATE");
        let reg = dir.path().join("registry.json");
        std::fs::write(&reg, r#"{"schema_version":1,"entries":[]}"#).unwrap();
        let got = run_gate(
            dir.path(),
            &reg,
            GateInput {
                name: "w-forced".into(),
                substrate: "bg".into(),
                flags: GateFlags {
                    force: true,
                    no_wait: true,
                },
                ..Default::default()
            },
        );
        std::env::remove_var("FNO_HOME");
        std::env::remove_var("FNO_CLAIMS_ROOT");
        if let Some(v) = prior_claims {
            std::env::set_var("FNO_CLAIMS_ROOT", v);
        }
        if let Some(v) = prior_spawn_gate {
            std::env::set_var("FNO_SPAWN_GATE", v);
        }
        let refusal = got
            .err()
            .expect("a forced spawn must not pass the RAM floor");
        assert_eq!(refusal.exit_code, EXIT_RAM_REFUSED, "{:?}", refusal.receipt);
    }
}
