//! Spine tests: the paths the Python leg pins on the pane lane, replayed
//! against the fakes the spine's injected `MuxCli` seam accepts.
use super::*;

pub(crate) fn out(code: i32, stdout: &str, stderr: &str) -> Output {
    use std::os::unix::process::ExitStatusExt;
    Output {
        status: std::process::ExitStatus::from_raw(code << 8),
        stdout: stdout.as_bytes().to_vec(),
        stderr: stderr.as_bytes().to_vec(),
    }
}

/// A canned transport: `pane ls` answers `ls_ok` with `ls_stdout`; every
/// other call is a `pane kill` answered from the kill fields.
pub(crate) struct FakeMux {
    pub ls_ok: bool,
    pub ls_stdout: String,
    kill_ok: bool,
    kill_stdout: String,
    kill_stderr: String,
}

impl Default for FakeMux {
    fn default() -> Self {
        FakeMux {
            ls_ok: true,
            ls_stdout: "[]".to_string(),
            kill_ok: true,
            kill_stdout: String::new(),
            kill_stderr: String::new(),
        }
    }
}

impl MuxCli for FakeMux {
    fn run(&self, args: &[String], _env: &[(String, String)]) -> std::io::Result<Output> {
        if args.len() > 2 && args[2] == "ls" {
            let code = if self.ls_ok { 0 } else { 1 };
            return Ok(out(code, &self.ls_stdout, ""));
        }
        let code = if self.kill_ok { 0 } else { 1 };
        Ok(out(code, &self.kill_stdout, &self.kill_stderr))
    }
}

fn ls_rows(rows: &[Value]) -> String {
    serde_json::to_string(rows).unwrap()
}

fn live_row(name: &str, cwd: &str, pane_id: u64) -> Value {
    serde_json::json!({"pane_id": pane_id, "cwd": cwd, "name": name, "child_pid": std::process::id()})
}

fn registry_with(rows: Vec<RegistryEntry>) -> state::Registry {
    state::Registry {
        entries: rows,
        ..Default::default()
    }
}

fn mux_row(name: &str, session: &str, pane_id: u64) -> RegistryEntry {
    RegistryEntry {
        name: name.to_string(),
        mux: Some(MuxRef {
            session: session.to_string(),
            pane_id,
        }),
        ..RegistryEntry::new(None, crate::state::Lineage::captured((None, None, None)))
    }
}

#[test]
fn session_resolution_flag_server_legacy_main() {
    assert_eq!(
        resolve_mux_session_from(Some("squad-a"), "other", "legacy"),
        "squad-a"
    );
    assert_eq!(resolve_mux_session_from(None, "other", "legacy"), "other");
    assert_eq!(resolve_mux_session_from(None, "", "legacy"), "legacy");
    assert_eq!(resolve_mux_session_from(None, "", ""), "main");
}

#[test]
fn pane_run_parse_lanes() {
    let plain = parse_pane_run(&out(0, "  \n42\n", ""), false, "main").unwrap();
    assert!(matches!(plain, PaneRunOutcome::Created { pane_id: 42 }));
    let line = r#"{"pane_id": 7}"#;
    let jsoned = parse_pane_run(&out(0, line, ""), true, "main").unwrap();
    assert!(matches!(jsoned, PaneRunOutcome::Created { pane_id: 7 }));
    let twenty = parse_pane_run(&out(20, "", ""), false, "main").unwrap();
    assert!(matches!(twenty, PaneRunOutcome::ControlUnanswered));
    let err = parse_pane_run(&out(1, "", "boom"), false, "main").unwrap_err();
    assert_eq!(err, "boom");
}

#[test]
fn pane_run_parse_plain_lane_unparseable_line() {
    let err = parse_pane_run(&out(0, "nope\n", ""), false, "main").unwrap_err();
    assert_eq!(
        err,
        "mux pane run returned unparseable output 'nope\\n' for session 'main'; a pane may \
         exist without a registry row - inspect with 'fno mux pane ls --session main'"
    );
}

#[test]
fn pane_run_parse_json_lane_unparseable_line() {
    let err = parse_pane_run(&out(0, "{\"pane_id\": \"x\"}\n", ""), true, "sq").unwrap_err();
    assert_eq!(
        err,
        "mux pane run returned unparseable --json '{\"pane_id\": \"x\"}\\n' for session 'sq'; \
         a pane may exist without a registry row - inspect with 'fno mux pane ls --session sq'"
    );
}

fn bounded_ctx() -> PaneContext {
    let mut ctx = PaneContext::simple("codex", "/repo");
    ctx.bounded_placement = true;
    ctx
}

fn bare_plan() -> ArmPlan {
    ArmPlan {
        wrapped: vec!["codex".into(), "--version".into()],
        held_seed: None,
        preassigned_id: None,
    }
}

#[test]
fn pane_run_args_bounded_lane_carries_fit_and_fence() {
    let args = pane_run_args(&bounded_ctx(), "main", "w", &bare_plan());
    let joined = args.join(" ");
    assert!(joined.contains("--claim --server main"), "got: {joined}");
    assert!(joined.contains("--cwd /repo"), "got: {joined}");
    assert!(joined.contains("--worker w"), "got: {joined}");
    assert!(joined.contains("--max-panes 4"), "got: {joined}");
    assert!(joined.contains("--fit"), "fit: {joined}");
    assert!(joined.ends_with("-- codex --version"), "got: {joined}");
    assert!(!joined.contains("--json"), "got: {joined}");
}

#[test]
fn pane_run_args_squad_does_not_block_fit() {
    let mut ctx = bounded_ctx();
    ctx.squad = Some("port-crew".to_string());
    let args = pane_run_args(&ctx, "main", "w", &bare_plan());
    let joined = args.join(" ");
    assert!(joined.contains("squad port-crew"), "got: {joined}");
    assert!(joined.contains("--fit"), "squad is not geometry: {joined}");
}

#[test]
fn pane_run_args_tab_id_rides_json_lane() {
    let mut ctx = bounded_ctx();
    ctx.tab_id = Some("id:12".to_string());
    let args = pane_run_args(&ctx, "main", "w", &bare_plan());
    let joined = args.join(" ");
    assert!(joined.contains("--tab id:12"), "got: {joined}");
    assert!(joined.contains("--json"), "got: {joined}");
    assert!(!joined.contains("--fit"), "got: {joined}");
}

fn reconcile(
    session: &str,
    cwd: &str,
    registry: &state::Registry,
    mux: &FakeMux,
) -> Result<u64, PaneRefusal> {
    reconcile_unanswered_run(session, Path::new(cwd), 0, registry, mux)
}

#[test]
fn reconcile_adopts_the_one_candidate() {
    let rows = vec![
        serde_json::json!({"pane_id": 9, "cwd": "/elsewhere", "child_pid": std::process::id()}),
        live_row("other", "/repo", 5),
    ];
    let mux = FakeMux {
        ls_stdout: ls_rows(&rows),
        ..Default::default()
    };
    let adopted = reconcile("main", "/repo", &registry_with(vec![]), &mux).unwrap();
    assert_eq!(adopted, 5);
}

#[test]
fn reconcile_refuses_zero_candidates() {
    let mux = FakeMux {
        ls_stdout: ls_rows(&[live_row("x", "/elsewhere", 5)]),
        ..Default::default()
    };
    let err = reconcile("main", "/repo", &registry_with(vec![]), &mux).unwrap_err();
    assert_eq!(err.exit_code, EXIT_SPAWN_FAILED);
    assert_eq!(
        err.line,
        "no pane was created; the mux never answered and no pane in 'main' matches this \
         spawn. Retry, or use --substrate thread."
    );
}

#[test]
fn reconcile_refuses_many_candidates() {
    let mux = FakeMux {
        ls_stdout: ls_rows(&[live_row("a", "/repo", 5), live_row("b", "/repo", 6)]),
        ..Default::default()
    };
    let err = reconcile("main", "/repo", &registry_with(vec![]), &mux).unwrap_err();
    assert_eq!(
        err.line,
        "the mux never answered and 2 panes in 'main' match this spawn (5, 6); cannot tell \
         them apart - inspect with 'fno mux pane ls --session main' before retrying."
    );
}

#[test]
fn reconcile_ignores_claims_from_other_sessions() {
    // Pane 5 in session "other" is claimed; pane 5 in "main" is a fresh
    // candidate - registry identity is the (session, pane_id) pair.
    let rows = vec![live_row("mine", "/repo", 5)];
    let mux = FakeMux {
        ls_stdout: ls_rows(&rows),
        ..Default::default()
    };
    let registry = registry_with(vec![mux_row("claimed-elsewhere", "other", 5)]);
    let adopted = reconcile("main", "/repo", &registry, &mux).unwrap();
    assert_eq!(adopted, 5);
}

#[test]
fn reconcile_skips_this_sessions_claimed_panes() {
    let rows = vec![live_row("mine", "/repo", 5)];
    let mux = FakeMux {
        ls_stdout: ls_rows(&rows),
        ..Default::default()
    };
    let registry = registry_with(vec![mux_row("claimed-here", "main", 5)]);
    let err = reconcile("main", "/repo", &registry, &mux).unwrap_err();
    assert!(err.line.starts_with("no pane was created"), "{}", err.line);
}

#[test]
fn reconcile_unknown_when_listing_is_empty() {
    let mux = FakeMux {
        ls_stdout: "[]".to_string(),
        ..Default::default()
    };
    let err = reconcile("main", "/repo", &registry_with(vec![]), &mux).unwrap_err();
    assert_eq!(err.exit_code, EXIT_SPAWN_FAILED);
    assert_eq!(
        err.line,
        "the mux never answered 'pane run' for session 'main', and 'pane ls' could not \
         confirm whether a pane exists; a pane may be live - inspect with 'fno mux pane ls \
         --session main' before retrying."
    );
}

#[test]
fn reconcile_unknown_when_listing_fails_names_the_detail() {
    let mux = FakeMux {
        ls_ok: false,
        ls_stdout: String::new(),
        ..Default::default()
    };
    let err = reconcile("main", "/repo", &registry_with(vec![]), &mux).unwrap_err();
    assert!(
        err.line
            .contains("could not confirm whether a pane exists;"),
        "{}",
        err.line
    );
    assert!(
        !err.line.contains("unparseable pane ls output"),
        "{}",
        err.line
    );
}

#[test]
fn reconcile_unknown_when_listing_is_unparseable() {
    let mux = FakeMux {
        ls_stdout: "not json".to_string(),
        ..Default::default()
    };
    let err = reconcile("main", "/repo", &registry_with(vec![]), &mux).unwrap_err();
    assert!(
        err.line.contains("unparseable pane ls output: 'not json'"),
        "{}",
        err.line
    );
}

#[test]
fn reconcile_rejects_pids_older_than_the_spawn_clock() {
    let mux = FakeMux {
        ls_stdout: ls_rows(&[live_row("old", "/repo", 5)]),
        ..Default::default()
    };
    // The live pid started in the past; a NOW clock must reject it.
    let err = reconcile_unanswered_run(
        "main",
        Path::new("/repo"),
        now_ms(),
        &registry_with(vec![]),
        &mux,
    )
    .unwrap_err();
    assert!(err.line.starts_with("no pane was created"), "{}", err.line);
}

fn pane() -> PaneFacts {
    PaneFacts {
        session: "main".to_string(),
        pane_id: 7,
        child_pid: Some(std::process::id()),
        spawn_started_ms: 0,
    }
}

fn write_and_load(
    home: &AgentsHome,
    name: &str,
    ctx: &PaneContext,
    pane: &PaneFacts,
    bind: &ArmBind,
    replacing: bool,
) -> Result<state::Registry, String> {
    write_pane_row(home, name, ctx, pane, bind, replacing, &None)?;
    state::load_registry(&home.registry_json()).map_err(|e| e.to_string())
}

#[test]
fn write_pane_row_records_only_a_claude_mode() {
    let tmp = tempfile::tempdir().unwrap();
    let home = AgentsHome::at(tmp.path().join("agents"));
    let fenced = vec!["--permission-mode".to_string(), "acceptEdits".to_string()];
    for (name, provider, passthrough, want) in [
        ("a", "claude", fenced.clone(), Some("acceptEdits")),
        ("b", "claude", Vec::new(), None),
        ("c", "codex", fenced.clone(), None),
    ] {
        let mut ctx = PaneContext::simple(provider, "/repo");
        ctx.passthrough = passthrough;
        let registry =
            write_and_load(&home, name, &ctx, &pane(), &ArmBind::NotApplicable, false).unwrap();
        let row = registry.entries.iter().find(|e| e.name == name).unwrap();
        assert_eq!(row.requested_permission_mode.as_deref(), want, "{name}");
    }
}

#[test]
fn write_pane_row_statuses_match_the_bind() {
    let tmp = tempfile::tempdir().unwrap();
    let home = AgentsHome::at(tmp.path().join("agents"));
    let ctx = PaneContext::simple("codex", "/repo");
    let mut message_ctx = PaneContext::simple("codex", "/repo");
    message_ctx.message = "seed text".to_string();
    let registry = write_and_load(
        &home,
        "w",
        &ctx,
        &pane(),
        &ArmBind::Bound {
            session_id: "s-1".to_string(),
        },
        false,
    )
    .unwrap();
    let row = registry.entries.iter().find(|e| e.name == "w").unwrap();
    assert_eq!(row.status, AgentStatus::Live);
    assert_eq!(row.harness_session_id.as_deref(), Some("s-1"));
    assert_eq!(row.spawn_payload.as_deref(), None);
    let registry = write_and_load(
        &home,
        "w2",
        &ctx,
        &pane(),
        &ArmBind::Unbound {
            reason: "binding-window-expired".to_string(),
        },
        false,
    )
    .unwrap();
    let row = registry.entries.iter().find(|e| e.name == "w2").unwrap();
    assert_eq!(row.status, AgentStatus::Spawning);
    assert_eq!(row.harness_session_id, None);
    let registry =
        write_and_load(&home, "w3", &ctx, &pane(), &ArmBind::NotApplicable, false).unwrap();
    let row = registry.entries.iter().find(|e| e.name == "w3").unwrap();
    assert_eq!(row.status, AgentStatus::Live);
    assert_eq!(row.harness_session_id, None);
    assert_eq!(row.harness.as_deref(), Some("codex"));
    assert_eq!(row.origin.as_deref(), Some("spawn"));
    assert_eq!(row.substrate.as_deref(), Some("pane"));
    assert_eq!(row.spawn_trigger, None);
    let registry = write_and_load(
        &home,
        "w4",
        &ctx,
        &pane(),
        &ArmBind::Died {
            reason: "pane-died-before-binding".to_string(),
        },
        false,
    )
    .unwrap();
    let row = registry.entries.iter().find(|e| e.name == "w4").unwrap();
    assert_eq!(row.status, AgentStatus::Failed);
    assert_eq!(row.harness_session_id, None);
}

#[test]
fn pane_run_env_carries_bridges_and_drops_the_grant() {
    let env = pane_run_env();
    let has = |key: &str| env.iter().any(|(k, v)| k == key && !v.is_empty());
    assert!(has("FNO_MUX_SHELL_INTEGRATION"));
    assert!(has("FNO_PROCESS_ADMISSION_MAX"));
    assert!(has("FNO_MUX_PANE_GROUP_MAX"));
    assert!(!env.iter().any(|(k, _)| k == "FNO_WORKER_ADD_DIRS"));
    let max_panes = env
        .iter()
        .find(|(k, _)| k == "FNO_MUX_PANE_GROUP_MAX")
        .map(|(_, v)| v.clone())
        .unwrap_or_default();
    assert_eq!(max_panes, "4");
}

#[test]
fn receipt_keys_follow_the_cli_order() {
    let ctx = PaneContext::simple("codex", "/repo");
    let bound = PaneReceipt::new(
        "w",
        &ctx,
        &pane(),
        &ArmBind::Bound {
            session_id: "s-1".into(),
        },
    );
    assert_eq!(
        bound.to_json(),
        "{\"name\": \"w\", \"short_id\": \"\", \"harness\": \"codex\", \"status\": \"live\", \
         \"mux_session\": \"main\", \"pane_id\": 7, \"bound\": true, \"seed\": null, \
         \"session_id\": \"s-1\"}"
    );
    let mut unbound_ctx = PaneContext::simple("codex", "/repo");
    unbound_ctx.message = "seed".into();
    let unbound = PaneReceipt::new(
        "w",
        &unbound_ctx,
        &pane(),
        &ArmBind::Unbound {
            reason: "binding-window-expired".into(),
        },
    );
    let unbound_json = unbound.to_json();
    assert!(
        unbound_json.contains("\"bound\": false"),
        "got: {unbound_json}"
    );
    assert!(
        unbound_json.contains("\"seed\": \"submitted\""),
        "got: {unbound_json}"
    );
    assert!(
        unbound_json.contains("\"unbound_reason\": \"binding-window-expired\""),
        "got: {unbound_json}"
    );
}

#[test]
fn died_receipt_names_the_death_and_the_row_reads_failed() {
    let tmp = tempfile::tempdir().unwrap();
    let home = AgentsHome::at(tmp.path().join("agents"));
    write_pane_row(
        &home,
        "w",
        &PaneContext::simple("codex", "/repo"),
        &pane(),
        &ArmBind::Died {
            reason: "pane-died-before-binding".into(),
        },
        false,
        &None,
    )
    .unwrap();
    let registry = state::load_registry(&home.registry_json())
        .map_err(|e| e.to_string())
        .unwrap();
    let row = registry.entries.iter().find(|e| e.name == "w").unwrap();
    assert_eq!(row.status, AgentStatus::Failed);
    let receipt = PaneReceipt::new(
        "w",
        &PaneContext::simple("codex", "/repo"),
        &pane(),
        &ArmBind::Died {
            reason: "pane-died-before-binding".into(),
        },
    );
    let json = receipt.to_json();
    assert!(json.contains("\"status\": \"failed\""), "got: {json}");
    assert!(json.contains("\"bound\": false"), "got: {json}");
    assert!(json.contains("\"pane_alive\": false"), "got: {json}");
    assert!(
        json.contains("\"unbound_reason\": \"pane-died-before-binding\""),
        "got: {json}"
    );
}

#[test]
fn stamp_binds_the_matching_idless_row() {
    let tmp = tempfile::tempdir().unwrap();
    let home = AgentsHome::at(tmp.path().join("agents"));
    let pid = std::process::id();
    write_pane_row(
        &home,
        "w",
        &PaneContext::simple("codex", "/repo"),
        &pane(),
        &ArmBind::Unbound {
            reason: "binding-window-expired".into(),
        },
        false,
        &None,
    )
    .unwrap();
    let stamped = stamp_late_bind(
        &home,
        "w",
        "s-9",
        Some(pid),
        process_start_time(pid),
        "main",
        7,
    );
    assert!(stamped);
    let registry = state::load_registry(&home.registry_json()).unwrap();
    let row = registry.entries.iter().find(|e| e.name == "w").unwrap();
    assert_eq!(row.harness_session_id.as_deref(), Some("s-9"));
    assert_eq!(row.status, AgentStatus::Live);
}

#[test]
fn stamp_refuses_a_foreign_id() {
    let tmp = tempfile::tempdir().unwrap();
    let home = AgentsHome::at(tmp.path().join("agents"));
    let pid = std::process::id();
    // Another row already holds s-9: the stamp must refuse, leaving w's row
    // id-less for reconcile to heal.
    write_pane_row(
        &home,
        "holder",
        &PaneContext::simple("codex", "/repo"),
        &pane(),
        &ArmBind::Bound {
            session_id: "s-9".into(),
        },
        false,
        &None,
    )
    .unwrap();
    write_pane_row(
        &home,
        "w",
        &PaneContext::simple("codex", "/repo"),
        &pane(),
        &ArmBind::Unbound {
            reason: "binding-window-expired".into(),
        },
        false,
        &None,
    )
    .unwrap();
    let stamped = stamp_late_bind(
        &home,
        "w",
        "s-9",
        Some(pid),
        process_start_time(pid),
        "main",
        7,
    );
    assert!(!stamped);
    let registry = state::load_registry(&home.registry_json()).unwrap();
    let row = registry.entries.iter().find(|e| e.name == "w").unwrap();
    assert_eq!(row.harness_session_id, None);
    assert_eq!(row.status, AgentStatus::Spawning);
}

#[test]
fn placement_lease_refuses_a_live_holder_then_reacquires() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().to_path_buf();
    let key = "placement:main:global";
    // A live foreign holder owns the lease.
    let first = crate::claims::acquire(
        key,
        "mux-placement:111:aaa",
        AcquireOpts {
            pid: Some(1),
            ttl_ms: Some(60_000),
            reason: Some("test".into()),
            root: Some(root.clone()),
            ..Default::default()
        },
    );
    assert!(matches!(first, AcquireOutcome::Acquired(_)));
    // The bounded lane refuses with the golden line's holder half.
    let refused = PlacementLease::acquire("main", Some(&root)).unwrap_err();
    assert_eq!(refused, "mux-placement:111:aaa");
    // The foreign holder releases; the lane acquires, and its Drop releases.
    crate::claims::release(key, "mux-placement:111:aaa", Some(&root), None).unwrap();
    let lease = PlacementLease::acquire("main", Some(&root)).unwrap();
    drop(lease);
    let again = crate::claims::acquire(
        key,
        "mux-placement:222:bbb",
        AcquireOpts {
            pid: Some(2),
            ttl_ms: Some(60_000),
            reason: Some("test".into()),
            root: Some(root.clone()),
            ..Default::default()
        },
    );
    assert!(matches!(again, AcquireOutcome::Acquired(_)));
}
