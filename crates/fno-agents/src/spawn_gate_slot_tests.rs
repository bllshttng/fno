//! The slot-census tests: a dead headless holder frees its max_live slot,
//! every counted reservation is named, and the refusal names the release
//! verb. Split from spawn_gate.rs to keep the over-budget file shrinking.

use super::*;

/// An unknown effective cap at the given ceiling: what the refusal sentence
/// tests want when no capacity state exists.
fn cap_ceiling(ceiling: usize) -> crate::capacity::Effective {
    crate::capacity::Effective {
        cap: ceiling,
        ceiling,
        ..Default::default()
    }
}

struct SuccessionFixture {
    _agent: crate::spawn_gate_admission::AgentSelfFixture,
    _lock: std::sync::MutexGuard<'static, ()>,
    dir: std::path::PathBuf,
    registry: std::path::PathBuf,
    pid: u32,
    pid_start: u64,
    prior_home: Option<std::ffi::OsString>,
    prior_claims: Option<std::ffi::OsString>,
    prior_config: Option<std::ffi::OsString>,
    prior_spawn_gate: Option<std::ffi::OsString>,
    prior_payload: Option<std::ffi::OsString>,
    prior_node: Option<std::ffi::OsString>,
}

impl SuccessionFixture {
    fn new(max_live: usize) -> Self {
        Self::with_config_body(&format!(
            "[agents]\nmax_live = {max_live}\nmin_free_gb = 0\nmax_swap_pct = 0\n"
        ))
    }

    /// The same fixture under a hand-written config body, so a test can turn
    /// the memory axis on without touching the others.
    fn with_config_body(body: &str) -> Self {
        let lock = claims::test_env_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let dir = std::env::temp_dir().join(format!(
            "fno-gate-succession-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let home = dir.join("agents-home");
        let claims_root = dir.join("claims-root");
        let fnodir = dir.join(".fno");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(&claims_root).unwrap();
        std::fs::create_dir_all(&fnodir).unwrap();
        let config = fnodir.join("config.toml");
        std::fs::write(&config, body).unwrap();

        let prior_home = std::env::var_os(crate::paths::HOME_ENV);
        let prior_claims = std::env::var_os("FNO_CLAIMS_ROOT");
        let prior_config = std::env::var_os("FNO_CONFIG");
        let prior_spawn_gate = std::env::var_os("FNO_SPAWN_GATE");
        let prior_payload = std::env::var_os("FNO_TEST_FOOTPRINT_PAYLOAD");
        let prior_node = std::env::var_os("FNO_NODE");
        let pid = std::process::id();
        let pid_start = crate::daemon::process_start_time(pid).unwrap_or(0);
        let mut fixture = Self {
            _agent: crate::spawn_gate_admission::AgentSelfFixture::set(),
            _lock: lock,
            dir,
            registry: std::path::PathBuf::new(),
            pid,
            pid_start,
            prior_home,
            prior_claims,
            prior_config,
            prior_spawn_gate,
            prior_payload,
            prior_node,
        };
        std::env::set_var(crate::paths::HOME_ENV, &home);
        std::env::set_var("FNO_CLAIMS_ROOT", &claims_root);
        std::env::set_var("FNO_CONFIG", &config);
        std::env::remove_var("FNO_SPAWN_GATE");
        std::env::remove_var("FNO_NODE");
        std::env::set_var(
            "FNO_TEST_FOOTPRINT_PAYLOAD",
            r#"{"admission":{"verdict":"admit","axis":"fleet_cpu_share","reason":"fixture","bound":"exact","ceiling":0.5}}"#,
        );

        fixture.registry = crate::paths::AgentsHome::from_env().registry_json();
        std::fs::create_dir_all(fixture.registry.parent().unwrap()).unwrap();
        fixture.write_entries(Vec::new());
        fixture
    }

    fn row(&self, name: &str) -> serde_json::Value {
        serde_json::json!({
            "name": name,
            "harness": "claude",
            "cwd": "/tmp",
            "status": "live",
            "created_at": "2026-01-01T00:00:00Z",
            "pid": self.pid,
            "pid_start_time": self.pid_start,
        })
    }

    fn lead(&self, name: &str, session: &str, scope: &str) -> serde_json::Value {
        let mut row = self.row(name);
        row["role_level"] = serde_json::json!(1);
        row["role_scope"] = serde_json::json!(scope);
        row["harness_session_id"] = serde_json::json!(session);
        row
    }

    fn worker(&self, name: &str, session: &str) -> serde_json::Value {
        let mut row = self.row(name);
        row["spawned_by_session"] = serde_json::json!(session);
        row
    }

    fn write_entries(&self, entries: Vec<serde_json::Value>) {
        let registry = serde_json::json!({
            "schema_version": crate::state::REGISTRY_SCHEMA_VERSION,
            "entries": entries,
        });
        crate::registry_store::replace_document(&self.registry, registry);
    }

    fn spawn(
        &self,
        name: &str,
        caller_session: Option<&str>,
        succession_scope: Option<&str>,
    ) -> Result<GateGuard, Refusal> {
        run_gate(
            &self.dir,
            &self.registry,
            GateInput {
                name: name.into(),
                substrate: "bg".into(),
                flags: GateFlags {
                    force: false,
                    no_wait: true,
                },
                caller_session: caller_session.map(str::to_string),
                succession_scope: succession_scope.map(str::to_string),
                holder_pid: Some(self.pid),
                ..Default::default()
            },
        )
    }
}

impl Drop for SuccessionFixture {
    fn drop(&mut self) {
        fn restore(key: &str, value: Option<std::ffi::OsString>) {
            match value {
                Some(value) => std::env::set_var(key, value),
                None => std::env::remove_var(key),
            }
        }
        restore(crate::paths::HOME_ENV, self.prior_home.take());
        restore("FNO_CLAIMS_ROOT", self.prior_claims.take());
        restore("FNO_CONFIG", self.prior_config.take());
        restore("FNO_SPAWN_GATE", self.prior_spawn_gate.take());
        restore("FNO_TEST_FOOTPRINT_PAYLOAD", self.prior_payload.take());
        restore("FNO_NODE", self.prior_node.take());
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

#[test]
fn plain_spawn_stays_refused_while_valid_succession_reuses_one_slot() {
    let fixture = SuccessionFixture::new(2);
    fixture.write_entries(vec![
        fixture.lead("lead", "session-lead", "x-epic"),
        fixture.worker("worker", "session-lead"),
        fixture.worker("worker-2", "session-lead"),
    ]);

    let refusal = fixture
        .spawn("plain", Some("session-lead"), None)
        .expect_err("two workers fill max_live 2; the lead takes no slot");
    assert_eq!(refusal.exit_code, EXIT_NO_WAIT);
    assert_eq!(refusal.receipt.as_ref().unwrap()["axis"], "max_live");

    fixture
        .spawn("successor", Some("session-lead"), Some("x-epic"))
        .expect("a sole teamed caller replaces its row")
        .release();

    // Below the ceiling a refusal names the learned cap, the ceiling and the
    // floor command, and a floor raised in config releases the same spawn on
    // its next gate call.
    let home = crate::paths::AgentsHome::from_env();
    let now = chrono::Utc::now().timestamp();
    let learned_one = crate::capacity::CapacityState {
        ceiling: 2,
        effective: 1,
        reason: Some("CPU-bound".into()),
        since: Some(now - 60),
        slots_learned: None,
        over_since: None,
        calm_since: None,
        memory: None,
        accounts: Vec::new(),
        workers_live: None,
        updated_epoch: now - 60,
    };
    crate::capacity::write_state(&home, &learned_one).unwrap();
    fixture.write_entries(vec![
        fixture.lead("lead", "session-lead", "x-epic"),
        fixture.worker("worker", "session-lead"),
    ]);
    // A RAM floor no machine meets: with no RAM room the learned cap holds.
    std::fs::write(
        fixture.dir.join(".fno").join("config.toml"),
        "[agents]\nmax_live = 2\nmin_free_gb = 1000000\nmax_swap_pct = 0\n",
    )
    .unwrap();
    let refusal = fixture
        .spawn("plain-2", Some("session-lead"), None)
        .expect_err("the learned cap of 1 holds the one worker slot");
    let line = verdict_line(&refusal);
    assert!(line.contains("learned_cap=1, ceiling=2"), "{line}");
    assert!(
        line.ends_with("remedy: fno agents spawn --floor N"),
        "{line}"
    );
    std::fs::write(
        fixture.dir.join(".fno").join("config.toml"),
        "[agents]\nmax_live = 2\nmin_free_gb = 0\nmax_swap_pct = 0\n[agents.capacity]\nfloor = 2\n",
    )
    .unwrap();
    fixture
        .spawn("plain-2", Some("session-lead"), None)
        .expect("a floor of 2 releases the spawn on the next gate call")
        .release();
}

#[test]
fn succession_refusal_reports_an_ineligible_caller() {
    let fixture = SuccessionFixture::new(2);
    fixture.write_entries(vec![
        fixture.lead("lead-a", "session-a", "x-epic"),
        fixture.lead("lead-b", "session-b", "x-epic"),
        fixture.worker("worker-a", "session-a"),
        fixture.worker("worker-b", "session-b"),
    ]);

    let refusal = fixture
        .spawn("successor", Some("session-a"), Some("x-epic"))
        .expect_err("two live holders cannot both succeed the same team");
    assert_eq!(refusal.exit_code, EXIT_NO_WAIT);
    assert_eq!(
        refusal.receipt.as_ref().unwrap()["succession"],
        "caller_not_sole_holder"
    );
}

/// AC2-HP / AC2-EDGE: at the user's max_live with the memory axis on and
/// reading room, one settled spawn passes; a worker created now keeps the
/// ceiling, and the refusal names `settling` with the settle facts.
#[test]
fn max_live_yields_one_spawn_a_minute_when_cpu_and_ram_have_room() {
    let fixture = SuccessionFixture::with_config_body(
        "[agents]\nmax_live = 2\nmin_free_gb = 0.001\nmax_swap_pct = 0\n",
    );
    fixture.write_entries(vec![fixture.worker("w0", "s"), fixture.worker("w1", "s")]);
    fixture
        .spawn("next", None, None)
        .expect("two settled workers of max_live 2: one burst spawn passes")
        .release();

    // A worker created now keeps the ceiling: the fleet is still settling.
    let mut fresh = fixture.worker("w2", "s");
    fresh["created_at"] = serde_json::json!(chrono::Utc::now().to_rfc3339());
    fixture.write_entries(vec![fixture.worker("w0", "s"), fresh]);
    let refusal = fixture
        .spawn("next", None, None)
        .expect_err("a worker created now keeps the ceiling: still settling");
    assert_eq!(refusal.exit_code, EXIT_NO_WAIT);
    let receipt = refusal.receipt.expect("no-wait refusal carries a receipt");
    assert_eq!(
        (
            &receipt["axis"],
            &receipt["max_live"],
            &receipt["burst"],
            &receipt["burst_settle_s"]
        ),
        (
            &serde_json::json!("max_live"),
            &serde_json::json!(2),
            &serde_json::json!("settling"),
            &serde_json::json!(60)
        )
    );
}

/// AC2-EDGE: the refusal sentence names the release remedy for a counted
/// reservation, preferring the suspect one a dead holder left behind.
#[test]
fn slot_refusal_line_names_the_release_remedy() {
    let suspect = SlotReservation {
        name: "w-res".into(),
        holder: "spawn-gate:99:w-res".into(),
        pid: Some(99),
        age_s: Some(120),
        state: "suspect",
        provider: Some("__uncapped__".into()),
        redeemable: true,
    };
    let live = SlotReservation {
        state: "live",
        pid: Some(std::process::id() as i32),
        redeemable: false,
        ..suspect.clone()
    };

    let line = slot_refusal_line(
        4,
        &cap_ceiling(3),
        3,
        &[live.clone(), suspect.clone()],
        0,
        "tail.",
    );
    assert!(
        line.contains("fno agents claim release worker:w-res --force"),
        "{line}"
    );
    assert!(
        line.contains("--reason \"<why>\""),
        "the clause keeps the reason template: {line}"
    );

    // All-live reservations name no remedy: releasing a live worker's slot
    // would leave it running uncounted.
    let line = slot_refusal_line(4, &cap_ceiling(3), 3, &[live], 0, "tail.");
    assert!(!line.contains("claim release"), "{line}");

    // Registry-only saturation names no remedy.
    let line = slot_refusal_line(3, &cap_ceiling(3), 3, &[], 0, "tail.");
    assert!(!line.contains("claim release"), "{line}");
}

/// AC1-HP: a headless reservation whose holder pid is dead reads Stale,
/// counts 0, and frees its slot while the TTL is still unexpired.
#[test]
fn dead_holder_frees_its_headless_slot() {
    let _g = claims::test_env_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    std::env::set_var("FNO_AGENT_SELF", "gate-fixture-worker");
    let dir = std::env::temp_dir().join(format!("fno-gate-deadhold-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let root = dir.join("claims-root");
    std::fs::create_dir_all(&root).unwrap();
    std::env::set_var("FNO_CLAIMS_ROOT", &root);

    let mut guard = GateGuard {
        gate_key: None,
        worker_key: None,
        root: Some(root.clone()),
    };
    acquire_worker_slot(
        &mut guard,
        "dead-judge",
        "spawn-gate:99:dead-judge",
        4_194_321,
        None,
        false,
    )
    .unwrap();

    // Assert while the guard holds: release() would delete the claim and
    // read Free, which proves nothing about the classifier.
    let mut warnings = Vec::new();
    let got = live_worker_slot_claims(&mut warnings);
    assert!(got.is_empty(), "dead holder must free the slot: {got:?}");
    let (state, _) = claims::status("worker:dead-judge", Some(&root));
    assert_eq!(state, claims::ClaimState::Stale);

    guard.release();
    std::env::remove_var("FNO_CLAIMS_ROOT");
    let _ = std::fs::remove_dir_all(&dir);
}
