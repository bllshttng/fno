//! The rm tests that drive `handle_rm_with` to a success response. Every
//! success tail calls `release_stopped_claims_into`, which resolves the
//! claims root from the process-global `FNO_CLAIMS_ROOT`, so each of these
//! holds `test_env_lock` for its whole body. Moved verbatim out of
//! daemon.rs for file budget: test motion is the sanctioned shrink.

use super::*;
#[tokio::test]
async fn rm_cascades_claude_before_removing_the_registry_row() {
    let _env = crate::claims::test_env_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let home = short_home("rmclaude");
    let row = claude_rm_row(
        "stopped-worker",
        "aaaa1111",
        "aaaa1111-1111-2222-3333-444444444444",
    );
    state::update_registry(&home.registry_json(), |registry| registry.entries.push(row)).unwrap();
    let ctx = test_ctx(home.clone(), PathBuf::from("fno-agents-worker"));
    let request = Request::new(1, "agent.rm", json!({"name": "stopped-worker"}));
    let called = std::sync::Mutex::new(Vec::new());
    let snapshots = claude_row_then_absent("aaaa1111", "stopped");

    let response = handle_rm_with(
        &ctx,
        &request,
        &snapshots,
        &|short_id| {
            called.lock().unwrap().push(short_id.to_string());
            Ok(())
        },
        &|_| true,
        &|_, _| Ok(true),
        &|_, _| PaneProbe::Unknown,
    )
    .await;

    assert_eq!(called.into_inner().unwrap(), vec!["aaaa1111"]);
    assert_eq!(response.result().unwrap()["harness_removed"], true);
    assert!(state::load_registry(&home.registry_json())
        .unwrap()
        .entries
        .is_empty());
    std::fs::remove_dir_all(home.root()).ok();
}

#[tokio::test]
async fn rm_never_deletes_a_successors_re_armed_manifest() {
    let _env = crate::claims::test_env_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    // The vacated row is removed with a manifest on disk naming a
    // DIFFERENT session: a successor teamed over the scope after this
    // row went terminal re-armed it. Deleting that file would disarm the
    // live successor's stop gate.
    let home = short_home("rmcrownsucc");
    let project = home.root().join("project");
    let spaces = home.root().join("spaces");
    std::env::set_var("FNO_SPACES_DIR", &spaces);
    let manifest = crate::paths::space_dir_opt(&project)
        .unwrap()
        .join("leads")
        .join("alpha.md");
    std::fs::create_dir_all(manifest.parent().unwrap()).unwrap();
    std::fs::write(
        &manifest,
        "---\nscope: alpha\nharness_session_id: bbbb9999-9999-4999-8999-999999999999\n---\n",
    )
    .unwrap();
    let mut row = claude_rm_row(
        "stopped-worker",
        "aaaa3333",
        "aaaa3333-1111-2222-3333-444444444444",
    );
    row.cwd = project.to_string_lossy().into_owned();
    row.role_level = Some(1);
    row.role_scope = Some("alpha".into());
    row.role_grantor = Some("human".into());
    state::update_registry(&home.registry_json(), |registry| registry.entries.push(row)).unwrap();
    let ctx = test_ctx(home.clone(), PathBuf::from("fno-agents-worker"));
    let request = Request::new(1, "agent.rm", json!({"name": "stopped-worker"}));
    let snapshots = claude_row_then_absent("aaaa3333", "stopped");

    let response = handle_rm_with(
        &ctx,
        &request,
        &snapshots,
        &|_| Ok(()),
        &|_| true,
        &|_, _| Ok(true),
        &|_, _| PaneProbe::Unknown,
    )
    .await;

    assert!(response.error().is_none(), "{response:?}");
    assert!(
        manifest.exists(),
        "rm deleted a manifest naming a different session: the live successor's gate"
    );
    std::env::remove_var("FNO_SPACES_DIR");
    std::fs::remove_dir_all(home.root()).ok();
}

#[tokio::test]
async fn rm_removes_a_stored_live_row_provably_gone_from_the_roster() {
    let _env = crate::claims::test_env_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    // AC2-HP (false-refusal mode): a row torn down by hand with `claude
    // stop`/`claude rm` never gets AgentStatus::Live written back. The
    // live gate must reconcile with the roster, not the stored enum.
    let home = short_home("rmprovengone");
    let mut row = claude_rm_row(
        "hand-torn-down",
        "ffff6666",
        "ffff6666-1111-2222-3333-444444444444",
    );
    row.status = AgentStatus::Live;
    state::update_registry(&home.registry_json(), |registry| registry.entries.push(row)).unwrap();
    let ctx = test_ctx(home.clone(), PathBuf::from("fno-agents-worker"));
    let request = Request::new(1, "agent.rm", json!({"name": "hand-torn-down"}));

    let response = handle_rm_with(
        &ctx,
        &request,
        &|| crate::claude_roster::ClaudeAgentsSnapshot::known(Vec::new()),
        &|_| panic!("row already absent from the roster must not reach claude rm"),
        &|_| true,
        &|_, _| Ok(true),
        &|_, _| PaneProbe::Unknown,
    )
    .await;

    assert_eq!(response.result().unwrap()["removed"], true);
    assert!(state::load_registry(&home.registry_json())
        .unwrap()
        .entries
        .is_empty());
    std::fs::remove_dir_all(home.root()).ok();
}

#[tokio::test]
async fn rm_clears_a_stored_live_pane_row_whose_pane_is_provably_absent() {
    let _env = crate::claims::test_env_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    // The fleet-reap deadlock: the row still reads live while the pane it
    // names is gone. The gate must test the referent, so the probe's
    // Absent verdict clears the row without --force.
    let home = short_home("rmpanegone");
    let mut row = ask_row("dead-pane-worker", Some("2020-01-01T00:00:00Z"));
    row.harness = Some("opencode".into());
    row.status = AgentStatus::Live;
    row.mux = Some(state::MuxRef {
        session: "main".into(),
        pane_id: 76,
    });
    state::update_registry(&home.registry_json(), |registry| registry.entries.push(row)).unwrap();
    let ctx = test_ctx(home.clone(), PathBuf::from("fno-agents-worker"));
    let request = Request::new(1, "agent.rm", json!({"name": "dead-pane-worker"}));
    let probed = std::sync::Mutex::new(Vec::new());

    let response = handle_rm_with(
        &ctx,
        &request,
        &|| panic!("non-Claude row must not read the Claude list"),
        &|_| panic!("non-Claude row must not call claude rm"),
        &|_| panic!("no claude stop may run in this test"),
        &|session, pane_id| {
            assert_eq!((session, pane_id), ("main", 76));
            Ok(false)
        },
        &|session, pane_id| {
            probed.lock().unwrap().push((session.to_string(), pane_id));
            PaneProbe::Absent
        },
    )
    .await;

    assert_eq!(probed.into_inner().unwrap(), vec![("main".to_string(), 76)]);
    assert_eq!(response.result().unwrap()["removed"], true);
    assert_eq!(response.result().unwrap()["pane_removed"], false);
    assert_eq!(
        response.result().unwrap()["pane_reason"],
        "mux pane already absent"
    );
    assert!(state::load_registry(&home.registry_json())
        .unwrap()
        .entries
        .is_empty());
    std::fs::remove_dir_all(home.root()).ok();
}

/// AC4-HP: a live claude background thread is ended by rm's OWN claude stop
/// (law d-81c6da7e), which runs BEFORE the harness cascade's claude rm, and
/// the row is removed. The post-stop roster read shows the session in a
/// terminal state, so the gate clears and the cascade still reaches
/// `claude rm` (the row is present, just finished).
#[tokio::test]
async fn rm_stops_the_claude_thread_itself_before_the_cascade() {
    let _env = crate::claims::test_env_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let home = short_home("rmselfstop");
    let mut row = claude_rm_row(
        "bg-thread",
        "eeee7777",
        "eeee7777-1111-2222-3333-444444444444",
    );
    row.status = AgentStatus::Live;
    state::update_registry(&home.registry_json(), |registry| registry.entries.push(row)).unwrap();
    let ctx = test_ctx(home.clone(), PathBuf::from("fno-agents-worker"));
    let request = Request::new(1, "agent.rm", json!({"name": "bg-thread"}));
    let order = std::sync::Mutex::new(Vec::new());
    let first = std::sync::atomic::AtomicBool::new(true);
    let second = std::sync::atomic::AtomicBool::new(true);

    let response = handle_rm_with(
        &ctx,
        &request,
        &|| {
            if first.swap(false, std::sync::atomic::Ordering::Relaxed) {
                crate::claude_roster::ClaudeAgentsSnapshot::known(vec![
                    crate::claude_roster::ClaudeAgentRow::new("eeee7777", Some("running")),
                ])
            } else if second.swap(false, std::sync::atomic::Ordering::Relaxed) {
                // The post-stop re-read: the stop worked, the session reads
                // finished, and the row is still listed (claude keeps
                // finished agents).
                crate::claude_roster::ClaudeAgentsSnapshot::known(vec![
                    crate::claude_roster::ClaudeAgentRow::new("eeee7777", Some("stopped")),
                ])
            } else {
                // The cascade's post-removal read: the harness row is gone.
                crate::claude_roster::ClaudeAgentsSnapshot::known(Vec::new())
            }
        },
        &|short_id| {
            order.lock().unwrap().push(format!("rm {short_id}"));
            Ok(())
        },
        &|_| {
            order.lock().unwrap().push("stop".to_string());
            true
        },
        &|_, _| panic!("a background thread has no pane"),
        &|_, _| PaneProbe::Unknown,
    )
    .await;

    assert_eq!(
        order.into_inner().unwrap(),
        vec!["stop".to_string(), "rm eeee7777".to_string()]
    );
    assert_eq!(response.result().unwrap()["removed"], true);
    assert!(state::load_registry(&home.registry_json())
        .unwrap()
        .entries
        .is_empty());
    std::fs::remove_dir_all(home.root()).ok();
}

/// An UNMAPPED live codex thread (the map lost the handle; the thread itself
/// still lives in the shared app-server) is not removed with a trivial
/// teardown: rm re-attaches through the row's durable identity first, and a
/// refused re-attach keeps the row addressable. Old shape: a trivial
/// "no-turn" removed the row while the thread kept running, and with the row
/// gone the session could then be neither stopped nor re-adopted inside the
/// rm grace window.
#[tokio::test]
async fn rm_refuses_an_unmapped_codex_thread_whose_reattach_refuses() {
    let _guard = crate::path_test_guard();
    let home = short_home("rmthreadunmapped");
    let mut row = thread_entry("t-rm-unmapped", AgentStatus::Live, None);
    row.cwd = "/nonexistent-cwd-for-unmapped-f313".into();
    row.project_root = row.cwd.clone();
    state::update_registry(&home.registry_json(), |registry| registry.entries.push(row)).unwrap();
    let ctx = test_ctx(home.clone(), PathBuf::from("/nonexistent"));
    assert!(
        !ctx.codex_threads.lock().await.contains_key("t-rm-unmapped"),
        "precondition: the map holds no handle for this thread"
    );
    let request = Request::new(1, "agent.rm", json!({"name": "t-rm-unmapped"}));

    let response = handle_rm_with(
        &ctx,
        &request,
        &|| panic!("a codex row must not read the claude roster"),
        &|_| panic!("rm must not reach claude rm"),
        &|_| panic!("no claude stop may run for a codex row"),
        &|_, _| panic!("a thread row has no mux ref to kill"),
        &|_, _| PaneProbe::Unknown,
    )
    .await;

    let error = response.error().expect("rm must refuse, not remove");
    assert_eq!(error.code, ErrorCode::Busy, "{:?}", error.message);
    assert!(
        error.message.contains("re-attach refused"),
        "the refusal names the failed re-attach: {:?}",
        error.message
    );
    assert!(
        state::load_registry(&home.registry_json())
            .unwrap()
            .find("t-rm-unmapped")
            .is_some(),
        "the row stays so the thread stays addressable"
    );
    assert!(ctx.codex_threads.lock().await.is_empty());
    std::fs::remove_dir_all(home.root()).ok();
}

/// A codex thread row whose actor is gone (a daemon restart left the handle
/// dead) is removed by rm without --force: nothing can be running in a dead
/// actor, so teardown is already done. Before the fix this refused with
/// `interrupt-failed-turn-still-running` and kept the dead row, which then
/// blocked every dispatch on its node.
#[tokio::test]
async fn rm_drops_a_codex_thread_row_whose_actor_is_gone() {
    let _guard = crate::path_test_guard();
    let home = short_home("rmthreadgone");
    let cwd = tempfile::tempdir().unwrap();
    let mut row = thread_entry("t-rm-gone", AgentStatus::Live, None);
    row.cwd = cwd.path().to_string_lossy().into_owned();
    row.project_root = row.cwd.clone();
    state::update_registry(&home.registry_json(), |registry| registry.entries.push(row)).unwrap();
    let ctx = test_ctx(home.clone(), PathBuf::from("/nonexistent"));
    ctx.codex_threads.lock().await.insert(
        "t-rm-gone".into(),
        std::sync::Arc::new(crate::codex_thread::CodexThreadActor::with_dead_actor()),
    );
    let request = Request::new(1, "agent.rm", json!({"name": "t-rm-gone"}));

    let response = handle_rm_with(
        &ctx,
        &request,
        &|| panic!("a codex row must not read the claude roster"),
        &|_| panic!("rm must not reach claude rm"),
        &|_| panic!("no claude stop may run for a codex row"),
        &|_, _| panic!("a thread row has no mux ref to kill"),
        &|_, _| PaneProbe::Unknown,
    )
    .await;

    assert!(
        response.error().is_none(),
        "{:?}",
        response.error().map(|e| e.message.clone())
    );
    assert_eq!(response.result().unwrap()["removed"], true);
    assert!(state::load_registry(&home.registry_json())
        .unwrap()
        .entries
        .is_empty());
    assert!(ctx.codex_threads.lock().await.is_empty());
    std::fs::remove_dir_all(home.root()).ok();
}

/// rm of a codex thread row stamps a tombstone beside the registry, so the
/// harness-store healer refuses to adopt the same session back under a
/// fresh short-id name (the adopted duplicate that blocked resume). The
/// row here carries the dead-actor shape the gone-row test uses; the
/// tombstone is the point, not the teardown.
#[tokio::test]
async fn rm_stamps_a_tombstone_for_the_removed_codex_session() {
    let _guard = crate::path_test_guard();
    let home = short_home("rmthreadtomb");
    let cwd = tempfile::tempdir().unwrap();
    let mut row = thread_entry("t-rm-tomb", AgentStatus::Live, None);
    row.cwd = cwd.path().to_string_lossy().into_owned();
    row.project_root = row.cwd.clone();
    row.harness_session_id = Some("01a0aba4-0934-7f10-aac3-67c76bfd244c".into());
    row.codex_session_id = Some("01a0aba4-0934-7f10-aac3-67c76bfd244c".into());
    state::update_registry(&home.registry_json(), |registry| registry.entries.push(row)).unwrap();
    let ctx = test_ctx(home.clone(), PathBuf::from("/nonexistent"));
    ctx.codex_threads.lock().await.insert(
        "t-rm-tomb".into(),
        std::sync::Arc::new(crate::codex_thread::CodexThreadActor::with_dead_actor()),
    );
    let request = Request::new(1, "agent.rm", json!({"name": "t-rm-tomb"}));

    let response = handle_rm_with(
        &ctx,
        &request,
        &|| panic!("a codex row must not read the claude roster"),
        &|_| panic!("rm must not reach claude rm"),
        &|_| panic!("no claude stop may run for a codex row"),
        &|_, _| panic!("a thread row has no mux ref to kill"),
        &|_, _| PaneProbe::Unknown,
    )
    .await;

    assert!(
        response.error().is_none(),
        "{:?}",
        response.error().map(|e| e.message.clone())
    );
    let raw = std::fs::read_to_string(home.root().join("rm_tombstones.json"))
        .expect("rm wrote the tombstone file");
    let tombstones: Vec<serde_json::Value> = serde_json::from_str(&raw).unwrap();
    assert_eq!(tombstones.len(), 1);
    assert_eq!(tombstones[0]["harness"], "codex");
    assert_eq!(
        tombstones[0]["session_id"],
        "01a0aba4-0934-7f10-aac3-67c76bfd244c"
    );
    assert!(tombstones[0]["removed_at"].is_u64());
    std::fs::remove_dir_all(home.root()).ok();
}
