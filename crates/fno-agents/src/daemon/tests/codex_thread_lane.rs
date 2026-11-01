//! The codex thread lane's spawn/registry/resume tests, moved verbatim
//! into their own module (file budget: this file is far over the
//! shrink-only line; test motion is the sanctioned shrink). Covers
//! `spawn_codex_thread_lane`, `build_codex_thread_entry`,
//! `ensure_codex_thread_handle`, and the `agent.spawn --substrate thread`
//! dispatch family. Shared helpers (`tmp_home`, `rentry`, `test_ctx`, ...)
//! stay in the parent tests module and resolve through the glob.
use super::*;
use crate::daemon::codex_thread_resume::{codex_thread_recovery_candidate, recover_codex_threads};

// `thread_entry` stays in the parent tests module (used by the plan_reconcile
// liveness family too, not only this one) and reaches here through the glob.

#[test]
fn reconcile_settles_an_unhosted_thread_without_a_rollout_to_exited() {
    let entries = vec![
        thread_entry("t-gone", AgentStatus::Live, None),
        // the same silence behind an open PR. The hold outranks the
        // no-rollout exit: a thread waiting on CI or a merge keeps its seat.
        thread_entry("t-pr-gone", AgentStatus::Live, None),
    ];
    let (changes, _) = plan_reconcile(
        &entries,
        |_| Ok(false),
        || false,
        |_| true,
        |_| false,
        |e| e.name == "t-pr-gone", // the open-PR hold, read from the graph rows
        |_| false,                 // not hosted
        |_| false,                 // no rollout: the thread never got far enough to persist
        |_| RowLiveness::Unknown,  // no freshness signal: nothing to prove life
        true,                      // roster readable: the flip needs a successful roster read
    );
    assert_eq!(
        changes[0].new_status,
        Some(AgentStatus::Exited),
        "an unhosted thread with no rollout is gone, not Live-forever"
    );
    assert_eq!(
        changes[0].new_liveness, None,
        "absence of a rollout is not a measured death: the word is unmeasured"
    );
    assert_eq!(
        changes[1].new_status, None,
        "the hold outranks the no-rollout exit for a PR-busy thread"
    );
}

#[test]
fn a_measured_alive_rollout_keeps_a_live_row_and_heals_an_orphaned_one() {
    // A rollout written within the freshness window is a positive running
    // marker: the app-server writes it while the thread turns, so losing the
    // hosting entry must not demote the row - the demoted row dropped out of
    // every status-filtered census while the mux showed it working. A row
    // stamped Orphaned while its rollout kept moving heals on the next full
    // sweep: the stamp was the lie, the fresh rollout is the truth.
    let entries = vec![
        thread_entry("t-working", AgentStatus::Live, Some("/tmp/r.jsonl".into())),
        thread_entry(
            "t-heals",
            AgentStatus::Orphaned,
            Some("/tmp/r.jsonl".into()),
        ),
    ];
    let (changes, out) = plan_reconcile(
        &entries,
        |_| Ok(false),
        || false,
        |_| true,
        |_| false,
        |_| false,
        |_| false, // not hosted: the actor entry is gone
        |_| true,  // the rollout exists and is fresh
        |_| RowLiveness::Alive,
        true,
    );
    assert_eq!(
        changes[0].new_status, None,
        "a fresh rollout outranks the unhosted answer"
    );
    assert_eq!(changes[0].new_liveness, Some("alive"));
    assert_eq!(changes[1].new_status, Some(AgentStatus::Live));
    assert_eq!(changes[1].new_liveness, Some("alive"));
    assert_eq!(out.updated, vec!["t-heals".to_string()]);
}

/// AC15: a row whose startup resume FAILED reads Orphaned after the
/// recovery pass, never Live-forever. The resume is made to fail
/// deterministically via a nonexistent cwd (app-server spawn cannot even
/// start there).
/// AC11: a yolo spawn stamps the posture on the row; the resume lane's
/// helper reads it back.
///
/// It drives a fake SHARED daemon. It used to install a stdio `codex` on
/// PATH and let the driver fork it. After the transport moved to the
/// shared daemon that fake was never reached: on a developer machine the
/// driver connected to the operator's REAL daemon and the test passed by
/// starting a real thread, and in CI, where no daemon runs, it panicked.
/// A test that reaches a live daemon is not a unit test, so it takes the
/// same fake every other one here does.
#[test]
fn build_codex_thread_entry_stamps_the_launch_posture() {
    let worktree = tempfile::tempdir().unwrap();
    let _guard = crate::path_test_guard();
    let start = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            // The fake must outlive the start: it owns CODEX_HOME.
            let _daemon = crate::codex_fake_daemon::FakeDaemon::start(
                crate::codex_fake_daemon::Behavior::quick().with_thread_id("thread-p"),
            );
            crate::codex_thread::CodexThread::start(
                worktree.path(),
                None,
                &crate::codex_posture::CodexPosture::full_access(),
                None,
            )
            .await
            .expect("yolo thread starts")
        });
    let yolo = build_codex_thread_entry(
        "t",
        worktree.path(),
        &start,
        None,
        None,
        None,
        None,
        &[],
        &serde_json::Value::Null,
        None,
        None,
        None,
    );
    assert_eq!(yolo.sandbox_posture.as_deref(), Some("danger-full-access"));
    assert_eq!(
        yolo.requested_permission_mode, None,
        "a bare yolo bool names no mode; unset stays unset"
    );
    assert!(
        crate::codex_posture::entry_posture_is_full_access(&yolo)
            && yolo.fno_id.is_none()
            && yolo.mux.is_none()
    );
    // The thread id stays on the harness slot; the registry write mints the
    // row its own id (the write-time fill covers that contract).
    // A typed mode on the request rides the row verbatim (v35): the entry
    // reads the DRIVER's posture, which the lane resolved from that string.
    let second = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            let _daemon = crate::codex_fake_daemon::FakeDaemon::start(
                crate::codex_fake_daemon::Behavior::quick().with_thread_id("thread-p"),
            );
            crate::codex_thread::CodexThread::start(
                worktree.path(),
                None,
                &crate::codex_posture::CodexPosture::from_record(
                    Some("read-only:on-request"),
                    None,
                ),
                None,
            )
            .await
            .expect("read-only thread starts")
        });
    let read_only = build_codex_thread_entry(
        "t",
        worktree.path(),
        &second,
        None,
        None,
        None,
        None,
        &[],
        &serde_json::value::Value::Null,
        None,
        None,
        None,
    );
    assert_eq!(read_only.sandbox_posture.as_deref(), Some("read-only"));
    assert_eq!(
        read_only.requested_permission_mode.as_deref(),
        Some("read-only:on-request"),
        "the operator's exact string is on the row"
    );
    let third = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            let _daemon = crate::codex_fake_daemon::FakeDaemon::start(
                crate::codex_fake_daemon::Behavior::quick().with_thread_id("thread-p"),
            );
            crate::codex_thread::CodexThread::start(
                worktree.path(),
                None,
                &crate::codex_posture::CodexPosture::bounded(),
                None,
            )
            .await
            .expect("bounded thread starts")
        });
    let bounded = build_codex_thread_entry(
        "t",
        worktree.path(),
        &third,
        None,
        None,
        None,
        None,
        &[],
        &serde_json::Value::Null,
        None,
        None,
        None,
    );
    assert_eq!(bounded.sandbox_posture.as_deref(), Some("workspace-write"));
    assert!(!crate::codex_posture::entry_posture_is_full_access(
        &bounded
    ));
    // A requested model stamps its basis on the row; an absent one
    // leaves the basis absent with it.
    let muted = None;
    let modeled = build_codex_thread_entry(
        "t",
        worktree.path(),
        &start,
        Some("gpt-5.6-sol"),
        None,
        muted,
        None,
        &[],
        &serde_json::Value::Null,
        None,
        None,
        None,
    );
    assert_eq!(modeled.model.as_deref(), Some("gpt-5.6-sol"));
    assert_eq!(modeled.model_basis.as_deref(), Some("requested"));
    assert_eq!(bounded.model_basis, None);
    // v25 positive marker: the route identity the spawn actually used,
    // read back non-empty from the minted row - the provider-outage
    // collector refuses evidence on a row whose axes are absent, so an
    // all-None stamp here would keep every daemon codex thread blind.
    assert_eq!(modeled.route_provider_id.as_deref(), Some("openai"));
    assert_eq!(modeled.model_name.as_deref(), Some("gpt-5.6-sol"));
    assert_eq!(modeled.account_record_id.as_deref(), Some("default"));
    // The seed rides the row for the attended guard's exemption: a request
    // message lands verbatim, and a request without one records absence -
    // every row built above ran Null params, which is that absent shape.
    let seeded = build_codex_thread_entry(
        "t",
        worktree.path(),
        &start,
        None,
        None,
        None,
        None,
        &[],
        &json!({"message": "probe the gate, then report"}),
        None,
        None,
        None,
    );
    assert_eq!(
        seeded.spawn_payload.as_deref(),
        Some("probe the gate, then report"),
        "the request's message is the delivered seed, recorded verbatim"
    );
    assert_eq!(
        modeled.spawn_payload, None,
        "a request with no message records no seed, never a guess"
    );
}

/// AC16: a codex PANE row (mux ref set) must refuse from the ask lane
/// naming the pane command, never reach ensure_codex_thread_handle and die
/// with the confusing "is not a Codex thread".
#[tokio::test(flavor = "current_thread")]
async fn ask_a_codex_pane_row_refuses_naming_the_pane_command() {
    let home = tmp_home("codex-pane-ask");
    state::update_registry(&home.registry_json(), |registry| {
        let mut entry = thread_entry("t-pane", AgentStatus::Live, None);
        entry.mux = Some(state::MuxRef {
            session: "main".into(),
            pane_id: 3,
        });
        entry.log_path = Some("/tmp/t-pane.log".into());
        registry.entries.push(entry);
    })
    .unwrap();
    let ctx = test_ctx(home.clone(), PathBuf::from("/nonexistent"));
    let resp = handle_ask(
        &ctx,
        &Request::new(1, "agent.ask", json!({"name": "t-pane", "message": "hi"})),
    )
    .await;
    match &resp.payload {
        crate::protocol::ResponsePayload::Err(e) => {
            assert_eq!(e.code, ErrorCode::InvalidStatus);
            assert!(
                e.message.contains("pane worker") && e.message.contains("mux pane send"),
                "refusal must name the pane command: {}",
                e.message
            );
            assert!(
                !e.message.contains("is not a Codex thread"),
                "the confusing thread refusal must not surface: {}",
                e.message
            );
        }
        _ => panic!("a pane row must refuse, got: {resp:?}"),
    }
    std::fs::remove_dir_all(home.root()).ok();
}

/// A resume re-applies the recorded grant (AC3-HP/EDGE) and writes back what
/// it actually resolved, WITHOUT erasing the spawn-time record: the recorded
/// roots are read before the resume and unioned back after, so a narrowed
/// resume cannot destroy the only durable copy of the grant and a second
/// resume can still restore them. The old lane did the opposite: it resumed
/// with no state dirs and overwrote the row with the narrowed result, which
/// is the loss `codex_thread_resumed_without_state_grant` used to announce.
#[tokio::test(flavor = "current_thread")]
async fn ensure_codex_thread_handle_freezes_the_promoting_resolution() {
    // CODEX_HOME is process-global: hold the same guard every other fake
    // user holds, or a parallel test's driver reads THIS test's fake.
    let _guard = crate::path_test_guard();
    let home = tmp_home("codex-resume-records-posture");
    let cwd = tempfile::tempdir().unwrap();
    let behavior = crate::codex_fake_daemon::Behavior::quick()
        .with_thread_id("thread-resumed")
        .with_thread_sandbox(serde_json::json!({"type": "readOnly"}));
    let received = std::sync::Arc::clone(&behavior.received);
    let _daemon = crate::codex_fake_daemon::FakeDaemon::start(behavior);
    // Must match the fake's configured thread_id: `resume()` refuses when
    // `thread/resume` confirms a different id than requested.
    let session_id = "thread-resumed".to_string();
    state::update_registry(&home.registry_json(), |registry| {
        let mut entry = thread_entry("t-resume", AgentStatus::Live, None);
        entry.cwd = cwd.path().to_string_lossy().into_owned();
        entry.project_root = entry.cwd.clone();
        entry.harness_session_id = Some(session_id.clone());
        entry.codex_session_id = Some(session_id.clone());
        // Seeded as a prior spawn left it: roots recorded, bounded posture.
        entry.sandbox_posture = Some("workspace-write".into());
        entry.resolved_sandbox = Some("workspaceWrite".into());
        entry.granted_writable_roots = vec!["/stale/spawn-time/root".into()];
        registry.entries.push(entry);
    })
    .unwrap();
    let ctx = test_ctx(home.clone(), PathBuf::from("/nonexistent"));
    let entry = state::load_registry(&home.registry_json())
        .unwrap()
        .find("t-resume")
        .cloned()
        .unwrap();
    ensure_codex_thread_handle(&ctx, &entry)
        .await
        .expect("the fake daemon answers thread/resume");

    // AC3-HP: the resume frame carries the recorded posture (the fake saw
    // the frame; roots ride the TURN, not the resume).
    let resume = received
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .iter()
        .find(|f| f["method"] == "thread/resume")
        .and_then(|f| f.get("params").cloned())
        .expect("a thread/resume frame");
    assert_eq!(resume["sandbox"], "workspace-write");
    assert_eq!(resume["approvalPolicy"], "never");

    let after = state::load_registry(&home.registry_json())
        .unwrap()
        .find("t-resume")
        .cloned()
        .unwrap();
    // The promoting resolution is FROZEN: the fake resolves readOnly, and the
    // seeded workspaceWrite must survive it - a mid-term refresh would mask
    // the drift a narrowed resolution caused.
    assert_eq!(after.resolved_sandbox.as_deref(), Some("workspaceWrite"));
    // AC3-EDGE: the recorded grant survives the write-back.
    assert!(
        after
            .granted_writable_roots
            .contains(&"/stale/spawn-time/root".to_string()),
        "the recorded roots must survive the write-back: {:?}",
        after.granted_writable_roots
    );
    // AC6: the v35 columns are stamped. The fake answers a sandbox, so the
    // turn policy echoes the server posture, not the replayed request.
    assert_eq!(after.turn_policy_source.as_deref(), Some("resolved"));

    // The other half of the freeze: `unknown` is no baseline, so a real
    // fresh read replaces it at the first resume. A second home, because the
    // registry refuses two rows carrying one session identity.
    let home2 = tmp_home("codex-resume-freeze-unknown");
    let ctx2 = test_ctx(home2.clone(), PathBuf::from("/nonexistent"));
    state::update_registry(&home2.registry_json(), |registry| {
        let mut entry = thread_entry("t-resume-unknown", AgentStatus::Live, None);
        entry.cwd = cwd.path().to_string_lossy().into_owned();
        entry.project_root = entry.cwd.clone();
        entry.harness_session_id = Some(session_id.clone());
        entry.codex_session_id = Some(session_id.clone());
        entry.sandbox_posture = Some("workspace-write".into());
        entry.resolved_sandbox = Some(crate::codex_thread::SANDBOX_POSTURE_UNKNOWN.into());
        registry.entries.push(entry);
    })
    .unwrap();
    let entry = state::load_registry(&home2.registry_json())
        .unwrap()
        .find("t-resume-unknown")
        .cloned()
        .unwrap();
    ensure_codex_thread_handle(&ctx2, &entry)
        .await
        .expect("the fake daemon answers the second thread/resume");
    let after = state::load_registry(&home2.registry_json())
        .unwrap()
        .find("t-resume-unknown")
        .cloned()
        .unwrap();
    // `unknown` is no baseline: the fake's real resolution replaces it.
    assert_eq!(after.resolved_sandbox.as_deref(), Some("readOnly"));
    std::fs::remove_dir_all(home2.root()).ok();
    std::fs::remove_dir_all(home.root()).ok();
}

/// Recovery refuses a codex thread row whose cwd is gone before any
/// `thread/resume` frame leaves, and settles it Orphaned through the same
/// failed-resume arm: a resumed thread with no cwd reads alive and can
/// never run a turn. A row whose cwd still exists
/// resumes and goes Live. Two fake-daemon scopes because the registry
/// forbids two rows sharing one harness_session_id and the fake answers
/// one configured id.
#[tokio::test(flavor = "current_thread")]
async fn recovery_refuses_a_codex_thread_row_whose_cwd_is_gone() {
    // AC1-EDGE: an existing cwd reaches the fake as a thread/resume.
    let live_id = "0198f313-0000-7000-8000-00000000a11e";
    let behavior = crate::codex_fake_daemon::Behavior::quick().with_thread_id(live_id);
    let received = std::sync::Arc::clone(&behavior.received);
    with_fake_codex_daemon(behavior, async {
        let home = tmp_home("codex-recover-live-cwd");
        let ctx = test_ctx(home.clone(), PathBuf::from("/nonexistent"));
        let worktree = home.root().join("worktree");
        std::fs::create_dir_all(&worktree).unwrap();
        state::update_registry(&home.registry_json(), |registry| {
            let mut entry = thread_entry("t-here", AgentStatus::Live, None);
            entry.cwd = worktree.to_string_lossy().into_owned();
            entry.project_root = entry.cwd.clone();
            entry.harness_session_id = Some(live_id.to_string());
            entry.codex_session_id = Some(live_id.to_string());
            entry.pid = None;
            // A fresh real-turn stamp: the recovery gate (x-a8ef) resumes
            // only rows whose last turn is recent.
            entry.last_message_at = Some(now_rfc3339_like());
            registry.entries.push(entry);
        })
        .unwrap();
        recover_codex_threads(&ctx).await;

        let registry = load_registry_offloaded(home.registry_json())
            .await
            .expect("registry readable");
        assert_eq!(
            registry.find("t-here").map(|entry| entry.status),
            Some(AgentStatus::Live),
            "a row whose cwd still exists must resume and go Live"
        );
        assert!(
            ctx.codex_threads.lock().await.contains_key("t-here"),
            "the existing-cwd row holds a hosted handle"
        );
        let resumes: Vec<_> = received
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .iter()
            .filter(|f| f["method"] == "thread/resume")
            .cloned()
            .collect();
        assert_eq!(resumes.len(), 1, "the existing-cwd row resumed once");
        assert_eq!(resumes[0]["params"]["threadId"], live_id);
        ctx.codex_threads.lock().await.remove("t-here");
        std::fs::remove_dir_all(home.root()).ok();
    })
    .await;

    // AC1-HP + AC1-ERR: a gone cwd refuses before any frame leaves.
    let gone_id = "0198f313-0000-7000-8000-00000000b0d2";
    let behavior = crate::codex_fake_daemon::Behavior::quick().with_thread_id(gone_id);
    let received = std::sync::Arc::clone(&behavior.received);
    with_fake_codex_daemon(behavior, async {
        let home = tmp_home("codex-recover-gone-cwd");
        let ctx = test_ctx(home.clone(), PathBuf::from("/nonexistent"));
        state::update_registry(&home.registry_json(), |registry| {
            let mut entry = thread_entry("t-gone", AgentStatus::Live, None);
            entry.cwd = "/nonexistent-cwd-for-gone-f313".into();
            entry.project_root = entry.cwd.clone();
            entry.harness_session_id = Some(gone_id.to_string());
            entry.codex_session_id = Some(gone_id.to_string());
            entry.pid = None;
            // A fresh real-turn stamp: the recovery gate (x-a8ef) resumes
            // only rows whose last turn is recent.
            entry.last_message_at = Some(now_rfc3339_like());
            registry.entries.push(entry);
        })
        .unwrap();
        recover_codex_threads(&ctx).await;

        let registry = load_registry_offloaded(home.registry_json())
            .await
            .expect("registry readable");
        assert_eq!(
            registry.find("t-gone").map(|entry| entry.status),
            Some(AgentStatus::Orphaned),
            "a gone-cwd row must settle Orphaned, not Live-forever"
        );
        assert!(
            !ctx.codex_threads.lock().await.contains_key("t-gone"),
            "a gone-cwd row holds no handle"
        );
        let events = read_events(&home);
        assert!(
            events.iter().any(|event| {
                event["type"] == "daemon_recovery_error"
                    && event["data"]["op"] == "resume_codex_thread"
                    && event["data"]["name"] == "t-gone"
                    && event["data"]["error"]
                        .as_str()
                        .is_some_and(|error| error.contains("no longer exists"))
            }),
            "the refusal event must name the row and the missing cwd: {:?}",
            events
        );

        // The direct call refuses before any resume frame leaves.
        let entry = registry.find("t-gone").cloned().unwrap();
        let error = match ensure_codex_thread_handle(&ctx, &entry).await {
            Ok(_) => panic!("a gone-cwd row must refuse"),
            Err(error) => error,
        };
        assert!(
            error.contains("no longer exists"),
            "refusal must name the missing cwd: {error}"
        );
        let resumes = received
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .iter()
            .filter(|f| f["method"] == "thread/resume")
            .count();
        assert_eq!(resumes, 0, "the fake must receive no resume frame");
        std::fs::remove_dir_all(home.root()).ok();
    })
    .await;
}

// --- startup-recovery freshness (x-a8ef) -----------------------------------

/// The candidate gate's freshness contract at the predicate level: a row
/// with no recent real-turn stamp is never a recovery candidate, whatever
/// its stored status claims. Absent, unparseable and future stamps fail
/// closed (stale), the same contract `InsideLegReport::received_within`
/// runs. Every case builds a full resume identity; only the stamps differ.
#[test]
fn recovery_candidate_requires_a_recent_real_turn_stamp() {
    let now = crate::claude_ask::now_epoch_secs();
    // The fixture's created_at is the unparseable "t", so the stamp under
    // test is the only freshness evidence on each row.
    let candidate = |mutate: &dyn Fn(&mut RegistryEntry)| {
        let mut entry = thread_entry("t-freshness", AgentStatus::Live, None);
        mutate(&mut entry);
        codex_thread_recovery_candidate(&entry, now)
    };
    assert!(
        candidate(&|e| {
            e.last_message_at = Some(now_rfc3339_like());
        }),
        "a row whose last turn just completed resumes"
    );
    assert!(
        !candidate(&|e| {
            e.last_message_at = Some("2026-09-01T00:00:00Z".into());
        }),
        "a month-idle row never resumes at startup: that stamp is age, not life"
    );
    assert!(
        !candidate(&|e| {
            e.last_message_at = None;
            e.inside_leg = None;
            e.created_at = "t".into();
        }),
        "absent and unparseable stamps fail closed: no evidence is not freshness"
    );
    assert!(
        candidate(&|e| {
            e.created_at = now_rfc3339_like();
        }),
        "a young row mid-first-turn (fresh created_at, no turn stamps) resumes"
    );
    assert!(
        !candidate(&|e| {
            e.last_message_at = Some("2026-09-01T00:00:00".into());
        }),
        "an unparseable last_message_at fails closed: with no other fresh stamp the row is not a candidate"
    );
    assert!(
        !candidate(&|e| {
            e.last_message_at = Some("2099-01-01T00:00:00Z".into());
        }),
        "a future stamp is corruption, not freshness: it must not resume the row"
    );
    let mut exited = thread_entry("t-exited", AgentStatus::Exited, None);
    exited.last_message_at = Some(now_rfc3339_like());
    assert!(
        !codex_thread_recovery_candidate(&exited, now),
        "the non-terminal gate outranks freshness: an Exited row never auto-resumes"
    );
}

/// Recovery skips a codex thread row whose last real turn is stale (x-a8ef):
/// no resume frame leaves, no handle is hosted, and the row settles Orphaned
/// instead of reading Live forever. The on-demand resume seam is untouched:
/// the same row still resumes the moment someone actually asks.
#[tokio::test(flavor = "current_thread")]
async fn recovery_skips_a_codex_thread_row_whose_last_turn_is_stale() {
    let stale_id = "0198f313-0000-7000-8000-00000000c0de";
    let behavior = crate::codex_fake_daemon::Behavior::quick().with_thread_id(stale_id);
    let received = std::sync::Arc::clone(&behavior.received);
    with_fake_codex_daemon(behavior, async {
        let home = tmp_home("codex-recover-stale");
        let ctx = test_ctx(home.clone(), PathBuf::from("/nonexistent"));
        let worktree = home.root().join("worktree");
        std::fs::create_dir_all(&worktree).unwrap();
        state::update_registry(&home.registry_json(), |registry| {
            let mut entry = thread_entry("t-stale", AgentStatus::Live, None);
            entry.cwd = worktree.to_string_lossy().into_owned();
            entry.project_root = entry.cwd.clone();
            entry.harness_session_id = Some(stale_id.to_string());
            entry.codex_session_id = Some(stale_id.to_string());
            entry.pid = None;
            entry.last_message_at = Some("2026-09-01T00:00:00Z".into());
            registry.entries.push(entry);
        })
        .unwrap();
        recover_codex_threads(&ctx).await;

        let registry = load_registry_offloaded(home.registry_json())
            .await
            .expect("registry readable");
        let entry = registry.find("t-stale").cloned().unwrap();
        assert_eq!(
            entry.status,
            AgentStatus::Orphaned,
            "a skipped stale row settles Orphaned: resumable, never Live-forever"
        );
        assert!(
            !ctx.codex_threads.lock().await.contains_key("t-stale"),
            "no handle for a stale row"
        );
        let resumes = received
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .iter()
            .filter(|f| f["method"] == "thread/resume")
            .count();
        assert_eq!(resumes, 0, "no resume frame may leave for a stale row");

        // The on-demand seam is untouched: the same row resumes when asked.
        let handle = ensure_codex_thread_handle(&ctx, &entry)
            .await
            .expect("an on-demand ask still resumes the stale row");
        drop(handle);
        ctx.codex_threads.lock().await.remove("t-stale");
        std::fs::remove_dir_all(home.root()).ok();
    })
    .await;
}

/// A young row that never completed a turn (fresh created_at, no turn
/// stamps) still resumes: it was mid-first-flight when the daemon died, and
/// its silence is age, not death (x-a8ef).
#[tokio::test(flavor = "current_thread")]
async fn recovery_resumes_a_young_row_that_never_turned() {
    let young_id = "0198f313-0000-7000-8000-00000000d1ce";
    let behavior = crate::codex_fake_daemon::Behavior::quick().with_thread_id(young_id);
    with_fake_codex_daemon(behavior, async {
        let home = tmp_home("codex-recover-young");
        let ctx = test_ctx(home.clone(), PathBuf::from("/nonexistent"));
        let worktree = home.root().join("worktree");
        std::fs::create_dir_all(&worktree).unwrap();
        state::update_registry(&home.registry_json(), |registry| {
            let mut entry = thread_entry("t-young", AgentStatus::Live, None);
            entry.cwd = worktree.to_string_lossy().into_owned();
            entry.project_root = entry.cwd.clone();
            entry.harness_session_id = Some(young_id.to_string());
            entry.codex_session_id = Some(young_id.to_string());
            entry.pid = None;
            entry.created_at = now_rfc3339_like();
            registry.entries.push(entry);
        })
        .unwrap();
        recover_codex_threads(&ctx).await;

        let registry = load_registry_offloaded(home.registry_json())
            .await
            .expect("registry readable");
        assert_eq!(
            registry.find("t-young").map(|entry| entry.status),
            Some(AgentStatus::Live),
            "a young row resumes and reads Live"
        );
        assert!(
            ctx.codex_threads.lock().await.contains_key("t-young"),
            "a young row holds a hosted handle"
        );
        std::fs::remove_dir_all(home.root()).ok();
    })
    .await;
}

/// The attach lane WITHOUT a harness-owned server (claude) must refuse
/// with the client-side-lane pointer, never reach the codex app-server
/// lane: `thread_lane` answers "attach" for claude too, so a bare lane
/// test would hand a claude thread spawn to codex's app-server.
#[tokio::test(flavor = "current_thread")]
async fn handle_spawn_thread_attach_without_server_refuses_with_client_pointer() {
    let home = tmp_home("spawn-thread-attach-client");
    let ctx = test_ctx(home.clone(), PathBuf::from("fno-agents-worker"));
    let req = Request::new(
        1,
        "agent.spawn",
        json!({"name": "test-agent", "provider": "claude", "substrate": "thread"}),
    );
    let resp = handle_spawn(&ctx, &req).await;
    match &resp.payload {
        crate::protocol::ResponsePayload::Err(e) => {
            assert_eq!(e.code, ErrorCode::InvalidParams);
            assert!(
                e.message.contains("a bare `fno agents spawn`"),
                "attach-without-server must point at the client-side lane; got: {}",
                e.message
            );
            assert!(
                !e.message.contains("retired at G4"),
                "this refusal is a lane split, not PTY retirement; got: {}",
                e.message
            );
        }
        _ => panic!("expected refusal for an attach lane without a server"),
    }
    std::fs::remove_dir_all(home.root()).ok();
}

/// A permission axis the lane cannot resolve is refused at the RPC
/// boundary, before any thread starts. The alternative is a spawn that
/// succeeds at a posture nobody asked for, which reads as a working worker
/// until it cannot write.
#[tokio::test(flavor = "current_thread")]
async fn handle_spawn_thread_refuses_an_unmappable_permission_mode() {
    let home = tmp_home("spawn-thread-permission-mode");
    let ctx = test_ctx(home.clone(), PathBuf::from("fno-agents-worker"));
    let req = Request::new(
        1,
        "agent.spawn",
        json!({
            "name": "test-agent",
            "provider": "codex",
            "substrate": "thread",
            "permission_mode": "acceptEdits",
        }),
    );
    let resp = handle_spawn(&ctx, &req).await;
    match &resp.payload {
        crate::protocol::ResponsePayload::Err(e) => {
            assert!(
                e.message.contains("acceptEdits"),
                "refusal must name the value it could not map; got: {}",
                e.message
            );
        }
        _ => panic!("expected refusal for an unmappable permission_mode"),
    }
    std::fs::remove_dir_all(home.root()).ok();
}

/// The destination's own precondition: a provider the codex lane cannot
/// serve refuses loudly instead of silently starting a codex thread under
/// the caller's name. Pinned by calling the lane directly, because no
/// packaged row today answers attach-with-server except codex - this is
/// the guard a SECOND such row meets until its destination is wired.
#[tokio::test(flavor = "current_thread")]
async fn codex_thread_lane_refuses_a_provider_it_cannot_serve() {
    let home = tmp_home("codex-lane-wrong-provider");
    let ctx = test_ctx(home.clone(), PathBuf::from("fno-agents-worker"));
    let req = Request::new(
        1,
        "agent.spawn",
        json!({"name": "test-agent", "provider": "claude", "substrate": "thread"}),
    );
    let resp =
        spawn_codex_thread_lane(&ctx, &req, "test-agent", Path::new("/tmp"), "claude", None).await;
    match &resp.payload {
        crate::protocol::ResponsePayload::Err(e) => {
            assert_eq!(e.code, ErrorCode::InvalidParams);
            assert!(
                e.message.contains("needs its own thread destination"),
                "the wrong-harness guard must name the missing destination; got: {}",
                e.message
            );
        }
        _ => panic!("expected the codex lane to refuse a provider it cannot serve"),
    }
    std::fs::remove_dir_all(home.root()).ok();
}

/// Spawn a codex thread worker through the real handle_spawn and return
/// its response.
async fn spawn_codex_thread_for_test(ctx: &Ctx, home: &AgentsHome, seed: &str) -> Response {
    let worktree = home.root().join("worktree");
    std::fs::create_dir_all(&worktree).unwrap();
    let req = Request::new(
        1,
        "agent.spawn",
        json!({
            "name": "t",
            "provider": "codex",
            "substrate": "thread",
            "cwd": worktree.to_string_lossy(),
            "message": seed,
        }),
    );
    handle_spawn(ctx, &req).await
}

/// AC4 make-it-fail probe: an ask arriving while the SEED turn is driving
/// STEERS into it. Old mutex shape: the ask queued behind the whole seed
/// turn and drove a SECOND turn - this asserted reply would read REPLY-2
/// and two agent_ask_done events would land. Actor: one shared turn, one
/// event, the ask returns the seed turn's own reply.
#[tokio::test(flavor = "current_thread")]
async fn codex_thread_ask_while_driving_steers_instead_of_queueing() {
    with_fake_codex_daemon(crate::codex_fake_daemon::Behavior::quick(), async {
        let home = tmp_home("codex-steer");
        let ctx = test_ctx(home.clone(), PathBuf::from("/nonexistent"));
        let spawned = spawn_codex_thread_for_test(&ctx, &home, "seed turn").await;
        assert!(spawned.result().is_some(), "spawn failed: {spawned:?}");

        let ask = handle_ask(
            &ctx,
            &Request::new(2, "agent.ask", json!({"name": "t", "message": "follow-up"})),
        )
        .await;
        let res = ask.result().expect("ask errored");
        assert_eq!(
            res["reply"], "REPLY-1",
            "the follow-up must ride the seed turn, not drive a second one: {res:?}"
        );

        // Exactly ONE completed turn: the seed and the steered ask share
        // it, so exactly one agent_ask_done event fires.
        let events = await_ask_done(&home).await;
        let done = events
            .iter()
            .filter(|e| e["type"] == "agent_ask_done")
            .count();
        assert_eq!(done, 1, "one shared turn must emit one event: {events:?}");
        ctx.codex_threads.lock().await.remove("t");
        std::fs::remove_dir_all(home.root()).ok();
    })
    .await;
}

/// AC5 + AC6 make-it-fail probe: stop INTERRUPTS the in-flight turn before
/// reporting stopped and names the interrupt outcome in the response. Old
/// shape: no `interrupt` key (remove-and-stamp while the turn task still
/// held an Arc clone), so the `interrupt == "interrupted"` assert fails
/// there.
///
/// It also pins the ownership claim this lane exists for. The row records
/// `pid: None` (it owns no process; the SHARED daemon, still running after
/// the stop, owns the thread). The assertion used to be the opposite (the
/// pid must be GONE), which is what owning a private app-server per worker
/// looked like.
#[tokio::test(flavor = "current_thread")]
async fn codex_thread_stop_interrupts_and_stamps_exited_without_killing_the_daemon() {
    with_fake_codex_daemon(crate::codex_fake_daemon::Behavior::long(), async {
        let home = tmp_home("codex-stop");
        let ctx = test_ctx(home.clone(), PathBuf::from("/nonexistent"));
        let spawned = spawn_codex_thread_for_test(&ctx, &home, "long seed turn").await;
        assert!(spawned.result().is_some(), "spawn failed: {spawned:?}");
        let registry = load_registry_offloaded(home.registry_json())
            .await
            .expect("registry");
        assert_eq!(
            registry.find("t").and_then(|entry| entry.pid),
            None,
            "a thread row records no pid: it owns no process, and this \
             field is a liveness surface"
        );
        let daemon_state: Value = serde_json::from_str(
            &std::fs::read_to_string(
                std::path::PathBuf::from(std::env::var("CODEX_HOME").unwrap())
                    .join("app-server-daemon")
                    .join("app-server.pid"),
            )
            .expect("daemon state"),
        )
        .expect("daemon state json");
        let pid = daemon_state["pid"].as_u64().expect("daemon pid") as u32;

        // Stop mid-turn, once the turn is actually driving.
        await_driving_turn(&ctx, "t").await;
        let stop = handle_stop(&ctx, &Request::new(3, "agent.stop", json!({"name": "t"}))).await;
        let res = stop.result().expect("stop errored");
        assert_eq!(res["stopped"], true, "stop response: {res:?}");
        assert_eq!(
            res["interrupt"], "interrupted",
            "stopped must name the interrupt outcome: {res:?}"
        );

        let registry = load_registry_offloaded(home.registry_json())
            .await
            .expect("registry");
        assert_eq!(
            registry.find("t").map(|e| e.status),
            Some(AgentStatus::Exited)
        );

        // The shared daemon must SURVIVE the stop. Stopping a worker
        // closes one connection; killing the app-server would take every
        // other codex session on the machine with it.
        let alive = std::process::Command::new("kill")
            .args(["-0", &pid.to_string()])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|status| status.success())
            .unwrap_or(false);
        assert!(
            alive,
            "stopping a worker killed the shared app-server daemon {pid}"
        );
        ctx.codex_threads.lock().await.remove("t");
        std::fs::remove_dir_all(home.root()).ok();
    })
    .await;
}

/// The zombie-stop probe: an interrupt the daemon never confirms must NOT
/// report a stop.
///
/// With a private app-server, `kill_on_drop` made every stop terminal, so
/// `stopped: true` was always true. Against the shared daemon nothing ends
/// the turn but the interrupt itself, and an unconfirmed one leaves the
/// model taking that turn in the worker's worktree. Reporting `stopped:
/// true` there marks the row Exited, hides it from recovery, and discards
/// the interrupt handle, while the work continues unobserved.
///
/// The fake acks the interrupt and never completes the turn, which is
/// exactly that state.
#[tokio::test(flavor = "current_thread")]
async fn codex_thread_stop_refuses_over_a_turn_the_interrupt_never_settled() {
    let behavior = crate::codex_fake_daemon::Behavior::long().with_interrupt(
        crate::codex_fake_daemon::Interrupt::AckOnly(std::time::Duration::ZERO),
    );
    with_fake_codex_daemon(behavior, async {
        // A Drop guard, not a teardown line: an assertion below panics
        // out of this body, and a leaked bound would silently shorten
        // every later test's interrupt wait in the same process.
        struct BoundGuard;
        impl Drop for BoundGuard {
            fn drop(&mut self) {
                std::env::remove_var("FNO_CODEX_INTERRUPT_BOUND_MS");
            }
        }
        std::env::set_var("FNO_CODEX_INTERRUPT_BOUND_MS", "1500");
        let _bound = BoundGuard;
        let home = tmp_home("codex-zombie-stop");
        let ctx = test_ctx(home.clone(), PathBuf::from("/nonexistent"));
        let spawned = spawn_codex_thread_for_test(&ctx, &home, "long seed turn").await;
        assert!(spawned.result().is_some(), "spawn failed: {spawned:?}");
        await_driving_turn(&ctx, "t").await;

        let stop = handle_stop(&ctx, &Request::new(3, "agent.stop", json!({"name": "t"}))).await;
        let res = stop.result().expect("stop errored");
        assert_eq!(
            res["stopped"], false,
            "an unsettled interrupt must not report a stop: {res:?}"
        );
        assert_eq!(
            res["interrupt"], "timeout-turn-still-running",
            "the response names why: {res:?}"
        );

        // The row stays non-terminal, so recovery can still see it, and
        // the handle stays so the live turn keeps an interrupt handle.
        let registry = load_registry_offloaded(home.registry_json())
            .await
            .expect("registry");
        assert_ne!(
            registry.find("t").map(|entry| entry.status),
            Some(AgentStatus::Exited),
            "a refused stop must not stamp the row terminal"
        );
        assert!(
            ctx.codex_threads.lock().await.contains_key("t"),
            "the actor must survive a refused stop; it holds the interrupt handle"
        );

        ctx.codex_threads.lock().await.remove("t");
        std::fs::remove_dir_all(home.root()).ok();
    })
    .await;
}

/// AC8 make-it-fail probe: mail arriving MID-TURN answers delivered on the
/// STEER ACK (milliseconds) and drives exactly ONE shared turn. Old shape:
/// the thread fell out of the switchboard as not-a-live-stream-thread, so
/// `delivered` read false - this assert fails there.
#[tokio::test(flavor = "current_thread")]
async fn switchboard_to_codex_thread_delivers_on_steering_ack_mid_turn() {
    with_fake_codex_daemon(crate::codex_fake_daemon::Behavior::quick(), async {
        let home = tmp_home("codex-mail");
        let ctx = test_ctx(home.clone(), PathBuf::from("/nonexistent"));
        let spawned = spawn_codex_thread_for_test(&ctx, &home, "seed turn").await;
        assert!(spawned.result().is_some(), "spawn failed: {spawned:?}");
        let registry = load_registry_offloaded(home.registry_json())
            .await
            .expect("registry");
        let row = registry.find("t").expect("thread row").clone();
        await_driving_turn(&ctx, "t").await;

        let params = json!({
            "to": "t",
            "from": "lead",
            "body": "hello thread",
            "mirror": false,
            "recipient_identity": {
                "harness": "codex",
                "session_id": row.harness_session_id,
                "short_id": "",
                "created_at": row.created_at,
            },
        });
        let started = std::time::Instant::now();
        let resp = handle_switchboard(&ctx, &Request::new(4, "agent.switchboard_v2", params)).await;
        let res = resp.result().expect("switchboard errored");
        assert!(
            started.elapsed() < std::time::Duration::from_secs(5),
            "delivery must answer on the steer ack, took {:?}",
            started.elapsed()
        );
        assert_eq!(res["delivered"], true, "codex mail: {res:?}");
        assert_eq!(res["identity_verified"], true);
        assert_eq!(res["turn_id"], "turn-1", "steered into the shared turn");

        // The body reached the thread: the steered turn carries it, so the
        // completion event names the same single turn.
        let events = await_ask_done(&home).await;
        let done: Vec<_> = events
            .iter()
            .filter(|e| e["type"] == "agent_ask_done")
            .collect();
        assert_eq!(done.len(), 1, "one shared turn: {events:?}");
        assert_eq!(
            done[0]["data"]["turn_id"], "turn-1",
            "the completion must name the turn both submits shared: {events:?}"
        );
        let injected = events.iter().any(|e| {
            e["type"] == "agent_deliver_injected"
                && e["data"]["transport"] == "switchboard"
                && e["data"]["provider"] == "codex"
        });
        assert!(injected, "injected event missing: {events:?}");

        // Cleanup: the actor holds a live daemon connection.
        handle_stop(&ctx, &Request::new(5, "agent.stop", json!({"name": "t"}))).await;
        ctx.codex_threads.lock().await.remove("t");
        std::fs::remove_dir_all(home.root()).ok();
    })
    .await;
}

/// x-fd66: a thread row's status comes from its driver, not from a pane that
/// is not there. A codex thread worker mid-turn reads `working` (ttl set,
/// fresh stamp) with no pane attached at any point, then `done` (no ttl) once
/// the completion routes - through the same seq-gated writer the claude
/// inside-leg hook uses. Asserted against the row's stored report, the
/// driver's own turn state, never a rendered glyph.
#[tokio::test(flavor = "current_thread")]
async fn codex_thread_row_reports_working_then_done_with_no_pane() {
    with_fake_codex_daemon(crate::codex_fake_daemon::Behavior::quick(), async {
        let home = tmp_home("codex-inside-leg-e2e");
        let ctx = test_ctx(home.clone(), PathBuf::from("/nonexistent"));
        let spawned = spawn_codex_thread_for_test(&ctx, &home, "seed turn").await;
        assert!(spawned.result().is_some(), "spawn failed: {spawned:?}");
        await_driving_turn(&ctx, "t").await;

        // Mid-turn: Working, ttl set, seq allocated. The write is offloaded,
        // so poll for the marker instead of betting on a fixed sleep.
        let working = poll_thread_row(&home.registry_json(), |report| {
            report.state == state::InsideLegState::Working
        })
        .await;
        assert_eq!(working.ttl_ms, Some(state::THREAD_TURN_TTL_MS));
        let working_seq = working.seq;
        assert!(working_seq >= 1);

        // After the completion routes: Done, no ttl, seq above the working one.
        await_ask_done(&home).await;
        let done = poll_thread_row(&home.registry_json(), |report| {
            report.state == state::InsideLegState::Done
        })
        .await;
        assert_eq!(done.ttl_ms, None, "a done report never ages out");
        assert!(
            done.seq > working_seq,
            "done seq {} must clear above working seq {working_seq}",
            done.seq
        );

        ctx.codex_threads.lock().await.remove("t");
        std::fs::remove_dir_all(home.root()).ok();
    })
    .await;
}

/// The resume half (x-fd66): the actor's report seq starts ABOVE the row's
/// current seq, so the resumed thread's first report clears the gate instead
/// of dying under the previous incarnation's seq. The row is seeded with a
/// done report at seq 7, as a prior life of this thread would have left it.
#[tokio::test(flavor = "current_thread")]
async fn codex_thread_resume_writes_above_the_row_seq() {
    // CODEX_HOME is process-global: hold the same guard every other fake
    // user holds, or a parallel test's driver reads THIS test's fake.
    let _guard = crate::path_test_guard();
    let _daemon = crate::codex_fake_daemon::FakeDaemon::start(
        crate::codex_fake_daemon::Behavior::quick().with_thread_id("thread-seq"),
    );
    let home = tmp_home("codex-inside-leg-resume-seq");
    state::update_registry(&home.registry_json(), |registry| {
        let mut entry = thread_entry("t-seq", AgentStatus::Live, None);
        let cwd = home.root().join("worktree");
        std::fs::create_dir_all(&cwd).unwrap();
        entry.cwd = cwd.to_string_lossy().into_owned();
        entry.project_root = entry.cwd.clone();
        entry.harness_session_id = Some("thread-seq".into());
        entry.codex_session_id = Some("thread-seq".into());
        entry.inside_leg = Some(state::InsideLegReport {
            state: state::InsideLegState::Done,
            seq: 7,
            reason: None,
            received_at: "2020-01-01T00:00:00Z".into(),
            ttl_ms: None,
            posture: None,
        });
        registry.entries.push(entry);
    })
    .unwrap();
    let ctx = test_ctx(home.clone(), PathBuf::from("/nonexistent"));
    let entry = state::load_registry(&home.registry_json())
        .unwrap()
        .find("t-seq")
        .cloned()
        .unwrap();
    ensure_codex_thread_handle(&ctx, &entry)
        .await
        .expect("the fake daemon answers thread/resume");

    // One completed turn: the seeded counter (8) is what the row accepts.
    let ask = handle_ask(
        &ctx,
        &Request::new(2, "agent.ask", json!({"name": "t-seq", "message": "hi"})),
    )
    .await;
    let res = ask.result().expect("ask errored");
    assert_eq!(
        res["reply"], "REPLY-1",
        "the resumed thread answers: {res:?}"
    );
    let done = poll_thread_row_named(&home.registry_json(), "t-seq", |report| {
        report.state == state::InsideLegState::Done
    })
    .await;
    assert!(
        done.seq >= 8,
        "the resumed thread's write must clear a row at seq 7, got seq {}",
        done.seq
    );

    ctx.codex_threads.lock().await.remove("t-seq");
    std::fs::remove_dir_all(home.root()).ok();
}

/// The episode gate (x-fd66 + x-dd84): notify intent fires on the EDGE into
/// done, once per episode, never on a repeat; a stale-seq write is dropped by
/// the same gate; the scrape verdict is cleared on the flip; a row holding no
/// such session is a no-op. The thread writer and the hook flush share this
/// core, so one test pins both.
#[test]
fn gate_inside_leg_onto_row_notifies_once_per_done_episode() {
    let mut registry = state::Registry::default();
    let mut row = thread_entry("t-gate", AgentStatus::Live, None);
    row.codex_session_id = Some("sid-gate".into());
    registry.entries.push(row);
    registry.entries[0].screen_state = Some(state::ScreenStateReport {
        state: "working".into(),
        rule: "busy".into(),
        seq: 1,
        at: "2020-01-01T00:00:00Z".into(),
        ttl_ms: None,
        answerable: None,
    });

    let rep = |seq, st| state::InsideLegReport {
        state: st,
        seq,
        reason: None,
        received_at: "2020-01-01T00:00:00Z".into(),
        ttl_ms: None,
        posture: None,
    };

    // Working: accepted, no notify, scrape verdict cleared.
    let n = gate_inside_leg_onto_row(
        &mut registry,
        "sid-gate",
        rep(1, state::InsideLegState::Working),
    );
    assert_eq!(n, None, "working is not an episode edge");
    assert_eq!(registry.entries[0].inside_leg.as_ref().unwrap().seq, 1);
    assert!(
        registry.entries[0].screen_state.is_none(),
        "the flip clears the scrape verdict"
    );

    // Done: the episode edge, exactly one intent, and it names is_done.
    let n = gate_inside_leg_onto_row(
        &mut registry,
        "sid-gate",
        rep(2, state::InsideLegState::Done),
    );
    assert_eq!(n, Some(("done".to_string(), true)));

    // A repeat Done (seq 3): accepted by the seq gate but NOT a new episode.
    let n = gate_inside_leg_onto_row(
        &mut registry,
        "sid-gate",
        rep(3, state::InsideLegState::Done),
    );
    assert_eq!(n, None, "a repeat done must not re-fire the episode");
    assert_eq!(registry.entries[0].inside_leg.as_ref().unwrap().seq, 3);

    // A stale-seq write (seq 3 again): dropped entirely.
    let n = gate_inside_leg_onto_row(
        &mut registry,
        "sid-gate",
        rep(3, state::InsideLegState::Working),
    );
    assert_eq!(n, None, "seq <= current is dropped");
    assert_eq!(
        registry.entries[0].inside_leg.as_ref().unwrap().state,
        state::InsideLegState::Done
    );

    // After a reboot the hook's monotonic seq restarts below the stored one.
    // Days later that low seq is a new turn, not a reordered sibling: it lands.
    let after_reboot = state::InsideLegReport {
        received_at: "2020-01-03T00:00:00Z".into(),
        ..rep(1, state::InsideLegState::Working)
    };
    gate_inside_leg_onto_row(&mut registry, "sid-gate", after_reboot);
    let leg = registry.entries[0].inside_leg.as_ref().unwrap();
    assert_eq!((leg.state, leg.seq), (state::InsideLegState::Working, 1));

    // A row holding no such session: no-op.
    let n = gate_inside_leg_onto_row(
        &mut registry,
        "sid-other",
        rep(9, state::InsideLegState::Done),
    );
    assert_eq!(n, None);
}

/// Poll until the row named `t` carries an inside-leg report matching
/// `pred`, and return it. Polling instead of a fixed sleep: the writer runs
/// off the actor task, so a sleep bets the write landed; this waits for the
/// marker itself.
async fn poll_thread_row_named(
    registry_path: &std::path::Path,
    name: &str,
    pred: impl Fn(&state::InsideLegReport) -> bool,
) -> state::InsideLegReport {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        let report = load_registry_offloaded(registry_path.to_path_buf())
            .await
            .ok()
            .and_then(|registry| registry.find(name).cloned())
            .and_then(|entry| entry.inside_leg);
        if let Some(report) = report {
            if pred(&report) {
                return report;
            }
        }
        assert!(
            std::time::Instant::now() < deadline,
            "no matching inside-leg report ever landed for {name}"
        );
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
}

/// [`poll_thread_row_named`] for the default test row name `t`.
async fn poll_thread_row(
    registry_path: &std::path::Path,
    pred: impl Fn(&state::InsideLegReport) -> bool,
) -> state::InsideLegReport {
    poll_thread_row_named(registry_path, "t", pred).await
}

/// The fenced-token path, end to end on the fake shared daemon: `-c` pairs
/// land in the `thread/start` config map with TOML-typed values, `--add-dir`
/// rides the state-root grant onto every turn, the spawn-request effort rides
/// `turn/start`, and the row stores the raw tokens for startup recovery
/// (AC2-HP, AC3-HP). The request stays UNPROMOTED on purpose: the bounded
/// posture is what carries the `writableRoots` grant this test asserts, and
/// the role door has its own tests below (the request's role half-pair lands
/// on the row AT MINT - grantor read off the request's parent edge, never
/// caller-supplied - so the seed turn can never submit to an unpromoted row;
/// a scope a non-terminal row already terms mints UNPROMOTED, and a lone half
/// refuses with no row written). A promoted spawn now widens to full access,
/// whose turn policy echoes without roots - that shape is the order tests'
/// contract, not this one.
#[tokio::test(flavor = "current_thread")]
async fn codex_thread_spawn_carries_harness_args_config_add_dir_and_effort() {
    let behavior = crate::codex_fake_daemon::Behavior::quick();
    let received = std::sync::Arc::clone(&behavior.received);
    with_fake_codex_daemon(behavior, async {
        let home = tmp_home("codex-harness-args");
        let ctx = test_ctx(home.clone(), PathBuf::from("/nonexistent"));
        let worktree = home.root().join("worktree");
        std::fs::create_dir_all(&worktree).unwrap();
        let req = Request::new(
            1,
            "agent.spawn",
            json!({
                "name": "t",
                "provider": "codex",
                "substrate": "thread",
                "cwd": worktree.to_string_lossy(),
                "message": "seed turn",
                "effort": "high",
                "spawned_by_session": "parent-session-uuid",
                "harness_args": [
                    "-c", "sandbox_workspace_write.network_access=true",
                    "--add-dir", "/tmp/x",
                ],
            }),
        );
        let resp = handle_spawn(&ctx, &req).await;
        assert!(resp.result().is_some(), "spawn failed: {resp:?}");

        let frames = |received: &std::sync::Arc<std::sync::Mutex<Vec<Value>>>| {
            received
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .clone()
        };
        let start = frames(&received)
            .into_iter()
            .find(|f| f["method"] == "thread/start")
            .expect("a thread/start frame");
        assert_eq!(
            start["params"]["config"]["sandbox_workspace_write.network_access"],
            json!(true),
            "the -c pair rides thread/start as a TOML boolean: {start}"
        );
        // The seed submit is async in the actor; poll for the frame rather
        // than racing it.
        let turn = (async {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
            loop {
                let turn = frames(&received)
                    .into_iter()
                    .find(|f| f["method"] == "turn/start");
                if turn.is_some() || std::time::Instant::now() >= deadline {
                    break turn;
                }
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("the seed turn frame");
        assert_eq!(
            turn["params"]["effort"], "high",
            "effort rides turn/start: {turn}"
        );
        assert!(
            turn["params"]["sandboxPolicy"]["writableRoots"]
                .as_array()
                .unwrap()
                .iter()
                .any(|root| root == "/tmp/x"),
            "the add-dir rides the state-root grant: {turn}"
        );

        // The row keeps the raw tokens so startup recovery re-parses them.
        let registry = load_registry_offloaded(home.registry_json())
            .await
            .expect("registry");
        assert_eq!(
            registry
                .find("t")
                .map(|entry| entry.harness_args.as_slice()),
            Some(
                &[
                    "-c".to_string(),
                    "sandbox_workspace_write.network_access=true".to_string(),
                    "--add-dir".to_string(),
                    "/tmp/x".to_string(),
                ][..]
            ),
            "the row stores the fenced tokens verbatim"
        );
        // The mint door's own asserts live in the phases below, whose
        // requests carry the role pair; this phase's request stays
        // unpromoted so the bounded writableRoots grant above keeps its
        // shape.
        ctx.codex_threads.lock().await.remove("t");
        std::fs::remove_dir_all(home.root()).ok();

        // A held scope (fresh home: the fake mints one thread id per
        // process, and the successor must not collide with the phase-1 row):
        // the mint declines to unpromoted, and the sitting lead keeps its
        // role.
        let held = tmp_home("codex-role-held");
        let held_ctx = test_ctx(held.clone(), PathBuf::from("/nonexistent"));
        let held_wt = held.root().join("worktree");
        std::fs::create_dir_all(&held_wt).unwrap();
        let mut lead = thread_entry("sitting-lead", AgentStatus::Live, None);
        lead.role_level = Some(1);
        lead.role_scope = Some("x-aaaa".into());
        let inserted = update_registry_offloaded(held.registry_json(), move |registry| {
            registry.entries.push(lead);
            true
        })
        .await;
        assert!(inserted.is_ok(), "fixture row: {inserted:?}");
        let successor_req = json!({
            "name": "successor",
            "provider": "codex",
            "substrate": "thread",
            "cwd": held_wt.to_string_lossy(),
            "message": "seed turn",
            "role_level": 2,
            "role_scope": "x-aaaa",
        });
        let resp = handle_spawn(&held_ctx, &Request::new(1, "agent.spawn", successor_req)).await;
        assert!(resp.result().is_some(), "spawn failed: {resp:?}");
        let registry = load_registry_offloaded(held.registry_json())
            .await
            .expect("registry");
        let successor = registry.find("successor").expect("the successor row");
        assert_eq!(
            successor.role_level, None,
            "an occupied scope mints unpromoted: {successor:?}"
        );
        assert_eq!(successor.role_scope, None);
        assert_eq!(successor.role_grantor, None);
        assert_eq!(
            registry.find("sitting-lead").unwrap().role_level,
            Some(1),
            "the sitting lead keeps its role"
        );
        ctx.codex_threads.lock().await.remove("successor");
        std::fs::remove_dir_all(held.root()).ok();

        // A lone half, or an out-of-ladder level, refuses: no thread, no
        // row. A scopeless level stamps a role that rules nothing and a
        // levelless scope can never deserialize, so the door fails closed.
        let refusals = tmp_home("codex-role-lone-half");
        let ref_ctx = test_ctx(refusals.clone(), PathBuf::from("/nonexistent"));
        let ref_wt = refusals.root().join("worktree");
        std::fs::create_dir_all(&ref_wt).unwrap();
        for half in ["role_level", "role_scope"] {
            let mut params = json!({
                "name": "t2",
                "provider": "codex",
                "substrate": "thread",
                "cwd": ref_wt.to_string_lossy(),
                "message": "seed turn",
                "role_level": 2,
                "role_scope": "x-bbbb",
            });
            params.as_object_mut().unwrap().remove(half);
            let resp = handle_spawn(&ref_ctx, &Request::new(1, "agent.spawn", params)).await;
            let refused = match &resp.payload {
                crate::protocol::ResponsePayload::Err(error) => error.message.clone(),
                _ => panic!("a lone role half must refuse: {half} -> {resp:?}"),
            };
            assert!(
                refused.contains("both role_level and role_scope"),
                "the refusal names the missing half {half}: {refused}"
            );
        }
        assert_eq!(
            load_registry_offloaded(refusals.registry_json())
                .await
                .expect("registry")
                .find("t2"),
            None,
            "no row for a refused spawn"
        );
        std::fs::remove_dir_all(refusals.root()).ok();
    })
    .await;
}

// ---------------------------------------------------------------------------
// Node-backed hosted Codex target threads start in their node worktree
// (x-ecf4). The fixture gives the lane a REAL canonical checkout (its git
// common dir lives inside it) plus a fake `fno` whose worktree ensure
// answers `answer` and exits `exit`; FNO_BIN is what the ensure resolves, so
// the fake IS the ensure.
// ---------------------------------------------------------------------------

/// A canonical-checkout fixture: a real git repo plus a fake `fno`. The
/// tempdir must stay bound for the fixture's lifetime; the returned path is
/// the binary to publish on FNO_BIN under the crate's env lock.
fn codex_target_launch_cwd_fixture(answer: &str, exit: i32) -> (tempfile::TempDir, PathBuf) {
    let repo = tempfile::tempdir().unwrap();
    std::process::Command::new("git")
        .args(["init", "-q"])
        .current_dir(repo.path())
        .status()
        .unwrap();
    let bin = crate::write_exec_stub(
        repo.path(),
        "fno",
        &format!(
            "#!/bin/sh\ncat >/dev/null\necho '{answer}'\necho 'worktree ensure: reusing ... created=false' >&2\nexit {exit}\n"
        ),
    );
    (repo, bin)
}

/// The Nth `thread/start` frame the lane ended up sending, waited for
/// rather than raced: the seed submit is async in the actor.
async fn codex_target_launch_cwd_thread_start(
    received: &std::sync::Arc<std::sync::Mutex<Vec<serde_json::Value>>>,
    nth: usize,
) -> serde_json::Value {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        let frame = received
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .iter()
            .filter(|f| f["method"] == "thread/start")
            .nth(nth)
            .cloned();
        if frame.is_some() || std::time::Instant::now() >= deadline {
            return frame.expect("thread/start never arrived");
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
}

fn codex_target_launch_cwd_registry_row(home: &AgentsHome, name: &str) -> serde_json::Value {
    let Ok(registry) = crate::registry_store::read(&home.registry_json()) else {
        return serde_json::Value::Null;
    };
    registry["agents"]
        .as_array()
        .and_then(|entries| entries.iter().find(|entry| entry["name"] == name).cloned())
        .unwrap_or(serde_json::Value::Null)
}

/// AC1: a node-backed target seeded from the canonical checkout is born in
/// the node's worktree - the SAME path on the thread/start frame, the
/// registry row, and the agent_spawned birth event.
#[tokio::test(flavor = "current_thread")]
async fn codex_target_launch_cwd_binds_a_canonical_node_target_to_its_worktree() {
    let _env = crate::claims::test_env_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let behavior = crate::codex_fake_daemon::Behavior::quick();
    let received = std::sync::Arc::clone(&behavior.received);
    with_fake_codex_daemon(behavior, async {
        let home = tmp_home("codex-target-cwd-bind");
        let ctx = test_ctx(home.clone(), PathBuf::from("/nonexistent"));
        let (repo, bin) = codex_target_launch_cwd_fixture("", 0);
        let commit = std::process::Command::new("git")
            .args([
                "-c",
                "user.email=t@t",
                "-c",
                "user.name=t",
                "commit",
                "--allow-empty",
                "-q",
                "-m",
                "init",
            ])
            .current_dir(repo.path())
            .status()
            .unwrap();
        assert!(commit.success(), "fixture commit failed: {commit}");
        let worktree = repo.path().join("node-wt");
        let add_worktree = std::process::Command::new("git")
            .args(["worktree", "add", "-q", "-b", "feature/x-ecf4"])
            .arg(&worktree)
            .current_dir(repo.path())
            .status()
            .unwrap();
        assert!(add_worktree.success(), "fixture worktree failed: {add_worktree}");
        let worktree = worktree.to_string_lossy().into_owned();
        std::fs::write(
            &bin,
            format!(
                "#!/bin/sh\ncat >/dev/null\necho '{worktree}'\necho 'worktree ensure: reusing node worktree created=false' >&2\nexit 0\n"
            ),
        )
        .unwrap();
        let _fno_bin = crate::claims::EnvVarGuard::set("FNO_BIN", bin.to_str().unwrap());

        let req = Request::new(
            1,
            "agent.spawn",
            json!({
                "name": "t",
                "provider": "codex",
                "substrate": "thread",
                "cwd": repo.path().to_string_lossy(),
                "node": "x-ecf4",
                "message": "/fno:target do the work",
            }),
        );
        let spawned = handle_spawn(&ctx, &req).await;
        assert!(spawned.result().is_some(), "spawn failed: {spawned:?}");

        let frame = codex_target_launch_cwd_thread_start(&received, 0).await;
        assert_eq!(
            frame["params"]["cwd"], worktree,
            "thread/start is born in the node worktree: {frame}"
        );
        let row = codex_target_launch_cwd_registry_row(&home, "t");
        assert_eq!(
            row["cwd"], worktree,
            "the registry row names the node worktree: {row}"
        );
        let birth = read_events(&home)
            .into_iter()
            .find(|event| event["type"] == "agent_spawned" && event["data"]["name"] == "t")
            .expect("birth event");
        assert_eq!(
            birth["data"]["cwd"], worktree,
            "the birth event names the node worktree: {birth}"
        );

        ctx.codex_threads.lock().await.remove("t");
        std::fs::remove_dir_all(home.root()).ok();
    })
    .await;
}

/// AC2: a refused ensure refuses the WHOLE spawn - no thread, no registry
/// row, no birth event, and the error names the node and the failing ensure
/// condition. Starting on canonical as a fallback would be the exact
/// outcome this lane exists to prevent.
#[tokio::test(flavor = "current_thread")]
async fn codex_target_launch_cwd_refusal_starts_nothing() {
    let _env = crate::claims::test_env_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let behavior = crate::codex_fake_daemon::Behavior::quick();
    with_fake_codex_daemon(behavior, async {
        let home = tmp_home("codex-target-cwd-refusal");
        let ctx = test_ctx(home.clone(), PathBuf::from("/nonexistent"));
        let (repo, bin) = codex_target_launch_cwd_fixture("", 1);
        let _fno_bin = crate::claims::EnvVarGuard::set("FNO_BIN", bin.to_str().unwrap());

        let req = Request::new(
            1,
            "agent.spawn",
            json!({
                "name": "t",
                "provider": "codex",
                "substrate": "thread",
                "cwd": repo.path().to_string_lossy(),
                "node": "x-ecf4",
                "message": "/fno:target seed",
            }),
        );
        let spawned = handle_spawn(&ctx, &req).await;
        let message = match &spawned.payload {
            crate::protocol::ResponsePayload::Err(e) => e.message.clone(),
            _ => panic!("spawn should refuse: {spawned:?}"),
        };
        assert!(
            message.contains("x-ecf4") && message.contains("worktree ensure"),
            "refusal names the node and the failing ensure condition: {message}"
        );
        assert_eq!(
            codex_target_launch_cwd_registry_row(&home, "t"),
            serde_json::Value::Null,
            "no registry row for a refused spawn"
        );
        let events = read_events(&home);
        assert!(
            !events
                .iter()
                .any(|event| event["type"] == "agent_spawned" && event["data"]["name"] == "t"),
            "no birth event for a refused spawn: {events:?}"
        );
        std::fs::remove_dir_all(home.root()).ok();
    })
    .await;
}

/// AC4: if the ensured node worktree disappears before thread creation, the
/// spawn refuses without starting or registering a thread on that missing cwd.
#[tokio::test(flavor = "current_thread")]
async fn codex_target_launch_cwd_refuses_a_worktree_removed_after_ensure() {
    let _env = crate::claims::test_env_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let behavior = crate::codex_fake_daemon::Behavior::quick();
    let received = std::sync::Arc::clone(&behavior.received);
    with_fake_codex_daemon(behavior, async {
        let home = tmp_home("codex-target-cwd-removed");
        let ctx = test_ctx(home.clone(), PathBuf::from("/nonexistent"));
        let (repo, bin) = codex_target_launch_cwd_fixture("", 0);
        let worktree = repo.path().join("node-wt");
        std::fs::create_dir(&worktree).unwrap();
        let worktree = worktree.to_string_lossy().to_string();
        std::fs::write(
            &bin,
            format!("#!/bin/sh\ncat >/dev/null\necho '{worktree}'\nrmdir '{worktree}'\nexit 0\n"),
        )
        .unwrap();
        let _fno_bin = crate::claims::EnvVarGuard::set("FNO_BIN", bin.to_str().unwrap());

        let req = Request::new(
            1,
            "agent.spawn",
            json!({
                "name": "t",
                "provider": "codex",
                "substrate": "thread",
                "cwd": repo.path().to_string_lossy(),
                "node": "x-ecf4",
                "message": "/fno:target do the work",
            }),
        );
        let spawned = handle_spawn(&ctx, &req).await;
        assert!(
            !std::path::Path::new(&worktree).exists(),
            "the fake ensure removed its returned node worktree"
        );
        let message = match &spawned.payload {
            crate::protocol::ResponsePayload::Err(error) => error.message.clone(),
            _ => panic!("spawn should refuse a removed worktree: {spawned:?}"),
        };
        assert!(
            message.contains("x-ecf4") && message.contains("worktree"),
            "refusal names the node and missing worktree: {message}"
        );
        assert!(
            received
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .iter()
                .all(|frame| frame["method"] != "thread/start"),
            "removed worktree must be rejected before thread/start"
        );
        assert_eq!(
            codex_target_launch_cwd_registry_row(&home, "t"),
            serde_json::Value::Null,
            "no registry row for a refused spawn"
        );
        assert!(
            !read_events(&home)
                .iter()
                .any(|event| event["type"] == "agent_spawned" && event["data"]["name"] == "t"),
            "no birth event for a refused spawn"
        );
        std::fs::remove_dir_all(home.root()).ok();
    })
    .await;
}
