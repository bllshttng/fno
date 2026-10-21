//! The node-occupancy tests: a claimless live worker on the node refuses the
//! second spawn and the refusal names it, an unoccupied node admits, a
//! succession passes its own departing row, and a finished row admits. The
//! fn-6206 collision (2026-10-21) went through a claim lockfile that only
//! sees workers that claimed; these drive the fleet-top read the gate now
//! makes, end to end through `run_gate`.

use super::*;

use std::os::unix::fs::PermissionsExt;

/// A fixture world with one team-less graph and an empty vendor roster. The
/// env is captured and restored on drop; `test_env_lock` serializes against
/// the other env-mutating tests.
struct OccupancyFixture {
    _agent: crate::spawn_gate_admission::AgentSelfFixture,
    _lock: std::sync::MutexGuard<'static, ()>,
    dir: std::path::PathBuf,
    registry: std::path::PathBuf,
    pid: u32,
    prior: Vec<(&'static str, Option<std::ffi::OsString>)>,
}

impl OccupancyFixture {
    fn new() -> Self {
        Self::with_broken_roster(false)
    }

    /// `broken_roster` makes the vendor read fail loudly (a blocking
    /// warning), which is the roster-down world the bounded arm answers.
    fn with_broken_roster(broken: bool) -> Self {
        let lock = crate::claims::test_env_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let dir = std::env::temp_dir().join(format!(
            "fno-gate-occupancy-{}-{}",
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
        std::fs::write(
            fnodir.join("config.toml"),
            "[agents]\nmax_live = 8\nmin_free_gb = 0\nmax_swap_pct = 0\n",
        )
        .unwrap();
        // The vendor roster stub: `claude agents --json` answers an empty
        // fleet, so the fold reads the fixture registry alone and no real
        // roster row can resolve to the fixture node. A broken stub fails
        // loudly instead, the roster-down world the bounded arm answers.
        let bin = dir.join("stub-bin");
        std::fs::create_dir_all(&bin).unwrap();
        let stub = bin.join("claude");
        let body = if broken {
            "#!/bin/sh\necho 'stub roster down' >&2\nexit 1\n"
        } else {
            "#!/bin/sh\necho '{\"agents\": []}'\n"
        };
        std::fs::write(&stub, body).unwrap();
        std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).unwrap();
        // The transcript store redirect: an empty projects dir keeps the
        // reachability read off the real machine's store.
        let claude_dir = dir.join("claude-dir");
        std::fs::create_dir_all(claude_dir.join("projects")).unwrap();
        // The graph the territory check reads at dequeue: one open node per
        // fixture node id, so a node-bound admit is judged on occupancy
        // alone and not on an unreadable territory.
        let fno_home = dir.join("fno-home");
        let graph_db = fno_home.join("db").join("graph.db");
        std::fs::create_dir_all(graph_db.parent().unwrap()).unwrap();

        let path_var = std::env::var_os("PATH");
        let claude_dir_var = std::env::var_os("CLAUDE_DIR_OVERRIDE");
        let fno_home_var = std::env::var_os("FNO_HOME");
        let prior_home = std::env::var_os(crate::paths::HOME_ENV);
        let prior_claims = std::env::var_os("FNO_CLAIMS_ROOT");
        let prior_config = std::env::var_os("FNO_CONFIG");
        let prior_spawn_gate = std::env::var_os("FNO_SPAWN_GATE");
        let prior_payload = std::env::var_os("FNO_TEST_FOOTPRINT_PAYLOAD");
        let prior_node = std::env::var_os("FNO_NODE");
        let pid = std::process::id();
        let mut fixture = Self {
            _agent: crate::spawn_gate_admission::AgentSelfFixture::set(),
            _lock: lock,
            dir,
            registry: std::path::PathBuf::new(),
            pid,
            prior: vec![
                ("PATH", path_var),
                ("CLAUDE_DIR_OVERRIDE", claude_dir_var),
                ("FNO_HOME", fno_home_var),
                (crate::paths::HOME_ENV, prior_home),
                ("FNO_CLAIMS_ROOT", prior_claims),
                ("FNO_CONFIG", prior_config),
                ("FNO_SPAWN_GATE", prior_spawn_gate),
                ("FNO_TEST_FOOTPRINT_PAYLOAD", prior_payload),
                ("FNO_NODE", prior_node),
            ],
        };
        let path_env = std::env::var_os("PATH").unwrap_or_default();
        let mut paths: Vec<std::ffi::OsString> = vec![bin.into()];
        paths.extend(std::env::split_paths(&path_env));
        std::env::set_var("PATH", std::env::join_paths(&paths).unwrap());
        std::env::set_var("CLAUDE_DIR_OVERRIDE", &claude_dir);
        std::env::set_var("FNO_HOME", &fno_home);
        std::env::set_var(crate::paths::HOME_ENV, &home);
        std::env::set_var("FNO_CLAIMS_ROOT", &claims_root);
        std::env::set_var("FNO_CONFIG", fnodir.join("config.toml"));
        std::env::remove_var("FNO_SPAWN_GATE");
        std::env::remove_var("FNO_NODE");
        std::env::set_var(
            "FNO_TEST_FOOTPRINT_PAYLOAD",
            r#"{"admission":{"verdict":"admit","axis":"fleet_cpu_share","reason":"fixture","bound":"exact","ceiling":0.5}}"#,
        );

        crate::graph_store::seed_rows(
            &graph_db,
            &[
                Self::graph_node("x-occ-node"),
                Self::graph_node("x-occ-other"),
            ],
        )
        .expect("seed fixture graph");

        fixture.registry = crate::paths::AgentsHome::from_env().registry_json();
        std::fs::create_dir_all(fixture.registry.parent().unwrap()).unwrap();
        fixture.write_entries(Vec::new());
        fixture
    }

    fn graph_node(id: &str) -> serde_json::Value {
        serde_json::json!({
            "id": id,
            "title": format!("occupancy fixture {id}"),
            "status": "ready",
            "project": "fno",
            "slug": id,
        })
    }

    /// One live claimless worker on `node`: the fork-6206 shape. No claim is
    /// minted for it anywhere; the roster row alone makes it live.
    fn worker_on(&self, name: &str, session: &str, node: &str, status: &str) -> serde_json::Value {
        serde_json::json!({
            "name": name,
            "harness": "claude",
            "cwd": self.dir.join("wt").display().to_string(),
            "status": status,
            "created_at": "2026-01-01T00:00:00Z",
            "node": node,
            "harness_session_id": session,
        })
    }

    fn write_entries(&self, entries: Vec<serde_json::Value>) {
        let registry = serde_json::json!({
            "schema_version": crate::state::REGISTRY_SCHEMA_VERSION,
            "entries": entries,
        });
        crate::registry_store::replace_document(&self.registry, registry);
    }

    fn spawn_on(&self, node: &str, caller_session: Option<&str>) -> Result<GateGuard, Refusal> {
        self.spawn_with(node, caller_session, false)
    }

    fn spawn_on_forced(&self, node: &str) -> Result<GateGuard, Refusal> {
        self.spawn_with(node, None, true)
    }

    fn spawn_with(
        &self,
        node: &str,
        caller_session: Option<&str>,
        force: bool,
    ) -> Result<GateGuard, Refusal> {
        run_gate(
            &self.dir,
            &self.registry,
            GateInput {
                name: "t-second-occ".into(),
                substrate: "bg".into(),
                flags: GateFlags {
                    force,
                    no_wait: true,
                },
                node: Some(node.into()),
                caller_session: caller_session.map(str::to_string),
                holder_pid: Some(self.pid),
                ..Default::default()
            },
        )
    }
}

impl Drop for OccupancyFixture {
    fn drop(&mut self) {
        for (key, value) in self.prior.drain(..) {
            match value {
                Some(value) => std::env::set_var(key, value),
                None => std::env::remove_var(key),
            }
        }
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

#[test]
fn a_claimless_live_worker_refuses_the_second_spawn_and_is_named() {
    let fixture = OccupancyFixture::new();
    fixture.write_entries(vec![fixture.worker_on(
        "fork-occ-fixture",
        "sess-occ-fork",
        "x-occ-node",
        "live",
    )]);

    let refusal = fixture
        .spawn_on("x-occ-node", None)
        .expect_err("a live claimless worker holds the node; the spawn must refuse");
    assert_eq!(refusal.exit_code, EXIT_NODE_OCCUPIED, "{refusal:?}");
    let receipt = refusal.receipt.as_ref().unwrap();
    assert_eq!(receipt["reason"], "node_occupied");
    assert_eq!(receipt["node"], "x-occ-node");
    assert_eq!(receipt["workers"], "fork-occ-fixture");
    let line = verdict_line(&refusal);
    assert!(
        line.contains("fork-occ-fixture"),
        "the verdict names the live worker: {line}"
    );
}

#[test]
fn the_admit_paths_still_admit() {
    // An unoccupied node: no roster row works it, the spawn admits.
    let fixture = OccupancyFixture::new();
    fixture.write_entries(vec![fixture.worker_on(
        "fork-occ-fixture",
        "sess-occ-fork",
        "x-occ-node",
        "live",
    )]);
    fixture
        .spawn_on("x-occ-other", None)
        .expect("no roster row works x-occ-other; the spawn admits")
        .release();

    // The departing worker itself spawns its replacement on the same node:
    // its own roster row is the only live one, and it is the caller.
    fixture
        .spawn_on("x-occ-node", Some("sess-occ-fork"))
        .expect("the caller's own departing row is not a second worker")
        .release();

    // A finished row is positive evidence of the end: the same node admits
    // a fresh spawn once the worker's own row reads exited.
    let finished = OccupancyFixture::new();
    finished.write_entries(vec![finished.worker_on(
        "fork-occ-fixture",
        "sess-occ-fork",
        "x-occ-node",
        "exited",
    )]);
    finished
        .spawn_on("x-occ-node", None)
        .expect("an exited row is positive evidence of the end; the node admits")
        .release();
}

#[test]
fn a_blind_roster_refuses_but_force_passes_it() {
    // The 2026-10-20 CPU-instrument outage bound this arm: a blind
    // instrument refuses closed, but it never blunts the whole fleet. The
    // arm runs for node-bearing spawns only, and --force passes the
    // unreadable-roster refusal.
    let fixture = OccupancyFixture::with_broken_roster(true);
    fixture.write_entries(Vec::new());

    let refusal = fixture
        .spawn_on("x-occ-node", None)
        .expect_err("a blind roster cannot read occupancy; the spawn refuses");
    assert_eq!(refusal.exit_code, EXIT_NODE_OCCUPIED, "{refusal:?}");
    let receipt = refusal.receipt.as_ref().unwrap();
    assert_eq!(receipt["reason"], "node_occupancy_unknown");
    assert_eq!(receipt["node"], "x-occ-node");
    let remedy = receipt["remedy"].as_str().unwrap();
    assert!(
        remedy.contains("stub roster down"),
        "the refusal names the broken read: {remedy}"
    );
    assert!(
        remedy.contains("--force"),
        "the refusal names the bypass: {remedy}"
    );

    // --force is the bypass: the same blind world admits a forced spawn.
    fixture
        .spawn_on_forced("x-occ-node")
        .expect("force passes the roster-down refusal")
        .release();
}
