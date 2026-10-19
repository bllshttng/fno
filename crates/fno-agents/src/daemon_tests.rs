#[path = "daemon/tests/blocking_bound_tests.rs"]
mod blocking_bound_tests;
#[path = "daemon/tests/store_socket_sweep_tests.rs"]
mod store_socket_sweep_tests;
use super::blocking_bound::directory_bytes_within;
use super::*;
use crate::client_verbs::RowLiveness;
use crate::codex_thread_entry::build_codex_thread_entry;

use crate::state::AgentState;

fn tmp_home(tag: &str) -> AgentsHome {
    let mut p = std::env::temp_dir();
    p.push(format!(
        "fno-agents-daemon-{}-{}-{}",
        tag,
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let home = AgentsHome::at(&p);
    home.ensure_root().unwrap();
    crate::paths::pin_test_claims_root(&p.join("claims-root"));
    home
}

// x-ef7f: the connect-probe singleton guard let a busy-but-alive
// incumbent read as absent, so every losing race added a new supervisor
// instead of replacing the incumbent. The flock-based guard must resolve
// N concurrent binders to exactly one winner.
#[tokio::test]
async fn bind_supervisor_socket_concurrent_only_one_survives() {
    // A real UnixListener::bind needs its path to fit sockaddr_un's short
    // sun_path buffer (104 bytes on macOS); tmp_home()'s long
    // tag+pid+nanos name overflows that once `/supervisor.sock` is
    // appended, so this test (the first here to actually bind a socket)
    // builds a short path directly under /tmp instead.
    static COUNTER: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let dir = std::path::PathBuf::from(format!("/tmp/fa-cb-{}-{n}", std::process::id()));
    let home = AgentsHome::at(&dir);
    home.ensure_root().unwrap();
    let mut handles = Vec::new();
    for _ in 0..8 {
        let h = home.clone();
        handles.push(tokio::spawn(
            async move { bind_supervisor_socket(&h).await },
        ));
    }
    // Collect every result FIRST, then count: counting inside the join loop
    // dropped the first winner's lock guard to a task still mid-retry and
    // read as three winners. The production holder keeps its guard for life.
    let mut results = Vec::new();
    for handle in handles {
        results.push(handle.await.unwrap());
    }
    let mut ok_count = 0;
    let mut already_running = 0;
    for r in &results {
        match r {
            Ok(_) => ok_count += 1,
            Err(DaemonError::AlreadyRunning(_)) => already_running += 1,
            Err(e) => panic!("unexpected error: {e:?}"),
        }
    }
    assert_eq!(ok_count, 1, "exactly one bind must survive the race");
    assert_eq!(already_running, 7);
    drop(results);
    std::fs::remove_dir_all(home.root()).ok();
}

// x-ef7f / x-e98b: the bound-inode check is what lets a daemon detect
// that its socket path was unlinked and rebound out from under it (by an
// operator `rm`, or a departing incumbent's blind unlink) instead of
// continuing to serve unreachable, and lets the exit-time cleanup refuse
// to unlink a live successor's fresh socket.
#[test]
fn socket_inode_matches_detects_unlink_and_rebind() {
    let home = tmp_home("inode-retire");
    let sock = home.supervisor_sock();
    // HOLD the original open for the whole test. In production the daemon
    // is listening on this inode, which is what keeps its number from being
    // recycled. Dropping the handle first frees the number, and Linux hands
    // the very same one to the next file at that path -- the rebind then
    // reads as a match and the test fails there while passing on macOS.
    let held = std::fs::File::create(&sock).unwrap();
    let ino = held.metadata().unwrap().ino();
    assert!(socket_inode_matches(&sock, ino));

    std::fs::remove_file(&sock).unwrap();
    std::fs::write(&sock, b"").unwrap(); // a new inode takes the same path
    assert!(
        !socket_inode_matches(&sock, ino),
        "a rebound path must not match the old inode"
    );
    drop(held);
}

fn read_events(home: &AgentsHome) -> Vec<Value> {
    // Committed rows, not journal bytes: the store cutover stopped journal
    // appends, so emitted events live only in the store beside the journal.
    let journal = home.events_jsonl();
    let _ = crate::event_store::import_all(&journal);
    crate::event_store::query_events(&journal, &crate::event_store::EventQuery::default())
        .unwrap_or_default()
        .iter()
        .filter_map(|r| serde_json::from_str::<Value>(&r.line).ok())
        .collect()
}

// Generic one-shot ask row builder (empty short_id + no pid, owns no
// worktree). The `exited_at` argument predates reverse-join retirement;
// it now only feeds the liveness ladder's heartbeat rung.
fn ask_row(name: &str, exited_at: Option<&str>) -> RegistryEntry {
    RegistryEntry {
        substrate: None,
        node: None,
        spawned_by_session: None,
        spawned_by_harness: None,
        spawned_by_cwd: None,
        launch_account: None,
        related_session_id: None,
        origin: None,
        name: name.into(),
        short_id: String::new(),
        legacy_provider: "claude".into(),
        provider: None,
        model: None,
        model_basis: None,
        effort: None,
        harness: None,
        // x-7bcd: needs a resolvable handle (leg 3); deterministic per
        // name so two rows never collide.
        harness_session_id: Some(format!("{name}-sess")),
        predecessor_session_ids: Vec::new(),
        forked_from_session_id: None,
        route_provider_id: None,
        model_name: None,
        account_record_id: None,
        cwd: "/tmp".into(),
        project_root: String::new(),
        session_id: None,
        spawn_trigger: None,
        legacy_claude_short_id: None,
        claude_session_uuid: None,
        messaging_socket_path: None,
        codex_session_id: None,
        gemini_session_id: None,
        mcp_channel_id: None,
        cc_session_id: None,
        host_mode: None,
        status: AgentStatus::Exited,
        last_message_at: None,
        created_at: "2020-01-01T00:00:00Z".into(),
        pid: None,
        pid_start_time: None,
        keeper_child_pid: None,
        log_path: None,
        last_reconciled_at: None,
        inside_leg: None,
        exited_at: exited_at.map(str::to_string),
        mux: None,
        screen_state: None,
        role_level: None,
        role_scope: None,
        role_grantor: None,
        route_settings_path: None,
        fno_id: None,
        delivery_policy: None,
        sandbox_posture: None,
        ..Default::default()
    }
}

fn claude_rm_row(name: &str, short_id: &str, session_id: &str) -> RegistryEntry {
    let mut row = ask_row(name, Some("2020-01-01T00:00:00Z"));
    row.short_id = short_id.to_string();
    row.harness = Some("claude".into());
    row.harness_session_id = Some(session_id.into());
    row
}

fn claude_row_then_absent(
    short_id: &'static str,
    state: &'static str,
) -> impl Fn() -> crate::claude_roster::ClaudeAgentsSnapshot {
    let calls = std::sync::atomic::AtomicUsize::new(0);
    move || {
        if calls.fetch_add(1, std::sync::atomic::Ordering::Relaxed) == 0 {
            crate::claude_roster::ClaudeAgentsSnapshot::known(vec![
                crate::claude_roster::ClaudeAgentRow::new(short_id, Some(state)),
            ])
        } else {
            crate::claude_roster::ClaudeAgentsSnapshot::known(Vec::new())
        }
    }
}

#[tokio::test]
async fn rm_requires_the_post_list_to_prove_the_claude_row_is_gone() {
    let home = short_home("rmpostlist");
    let row = claude_rm_row(
        "stopped-worker",
        "aaabbb11",
        "aaabbb11-1111-2222-3333-444444444444",
    );
    state::update_registry(&home.registry_json(), |registry| registry.entries.push(row)).unwrap();
    let ctx = test_ctx(home.clone(), PathBuf::from("fno-agents-worker"));
    let request = Request::new(1, "agent.rm", json!({"name": "stopped-worker"}));

    let response = handle_rm_with(
        &ctx,
        &request,
        &|| {
            crate::claude_roster::ClaudeAgentsSnapshot::known(vec![
                crate::claude_roster::ClaudeAgentRow::new("aaabbb11", Some("stopped")),
            ])
        },
        &|_| Ok(()),
        &|_| true,
        &|_, _| Ok(true),
        &|_, _| PaneProbe::Unknown,
    )
    .await;

    assert!(response
        .error()
        .unwrap()
        .message
        .contains("survives successful claude rm"));
    assert_eq!(
        state::load_registry(&home.registry_json())
            .unwrap()
            .entries
            .len(),
        1
    );
    std::fs::remove_dir_all(home.root()).ok();
}

#[tokio::test]
async fn rm_keeps_registry_row_when_claude_refuses_without_force() {
    let home = short_home("rmrefuse");
    let row = claude_rm_row(
        "stopped-worker",
        "bbbb2222",
        "bbbb2222-1111-2222-3333-444444444444",
    );
    state::update_registry(&home.registry_json(), |registry| registry.entries.push(row)).unwrap();
    let ctx = test_ctx(home.clone(), PathBuf::from("fno-agents-worker"));
    let request = Request::new(1, "agent.rm", json!({"name": "stopped-worker"}));

    let response = handle_rm_with(
        &ctx,
        &request,
        &|| {
            crate::claude_roster::ClaudeAgentsSnapshot::known(vec![
                crate::claude_roster::ClaudeAgentRow::new("bbbb2222", Some("stopped")),
            ])
        },
        &|_| Err("claude rm exited 1".into()),
        &|_| true,
        &|_, _| Ok(true),
        &|_, _| PaneProbe::Unknown,
    )
    .await;

    assert!(response
        .error()
        .unwrap()
        .message
        .contains("claude rm exited 1"));
    assert_eq!(
        state::load_registry(&home.registry_json())
            .unwrap()
            .entries
            .len(),
        1
    );

    let forced_request = Request::new(
        2,
        "agent.rm",
        json!({"name": "stopped-worker", "force": true}),
    );
    let forced_response = handle_rm_with(
        &ctx,
        &forced_request,
        &|| {
            crate::claude_roster::ClaudeAgentsSnapshot::known(vec![
                crate::claude_roster::ClaudeAgentRow::new("bbbb2222", Some("stopped")),
            ])
        },
        &|_| Err("claude rm exited 1".into()),
        &|_| true,
        &|_, _| Ok(true),
        &|_, _| PaneProbe::Unknown,
    )
    .await;
    assert_eq!(forced_response.result().unwrap()["harness_removed"], false);
    assert_eq!(
        forced_response.result().unwrap()["harness_reason"],
        "claude rm exited 1"
    );
    assert!(state::load_registry(&home.registry_json())
        .unwrap()
        .entries
        .is_empty());
    std::fs::remove_dir_all(home.root()).ok();
}

/// A temp dir whose `.git` is a FILE: linked-worktree shape, no real repo
/// behind it (the gate and the removal are injected, so none is needed).
fn fake_worktree(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("fno-rm-wt-{}-{name}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join(".git"), "gitdir: /elsewhere/worktrees/x.git\n").unwrap();
    dir
}

#[test]
fn output_with_timeout_bounds_a_stalled_subprocess() {
    // The rm path's budget: a fast child answers, a stalled one is
    // bounded instead of parking the daemon's rm handler.
    let fast = output_with_timeout(
        {
            let mut c = std::process::Command::new("echo");
            c.arg("ok");
            c
        },
        10,
    );
    assert!(fast.is_some(), "a fast child answers");
    let stalled = output_with_timeout(
        {
            let mut c = std::process::Command::new("sleep");
            c.arg("30");
            c
        },
        1,
    );
    let stalled = stalled.expect("a stalled child is killed, not left running");
    assert!(
        !stalled.status.success(),
        "the killed child reads as a failed call"
    );
}

#[test]
fn branch_merged_answers_real_repos() {
    // The rm door's half of the third bucket: a fresh branch is
    // unmerged; a fast-forward into the main line flips it.
    let root = std::env::temp_dir().join(format!("fno-rm-mg-{}", std::process::id()));
    let repo = root.join("repo");
    let wt = root.join("leaf");
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&repo).unwrap();
    let git =
        |args: &[&str], cwd: &std::path::Path| crate::git_test_helpers::git_run(args, cwd).unwrap();
    let commit_args = [
        "-c",
        "user.email=t@example.com",
        "-c",
        "user.name=t",
        "commit",
        "-qm",
        "seed",
    ];
    assert!(git(&["init", "-q", "-b", "main"], &repo).status.success());
    std::fs::write(repo.join("a.txt"), "a\n").unwrap();
    assert!(git(&["add", "-A"], &repo).status.success());
    assert!(git(&commit_args, &repo).status.success());
    let worktree_add = [
        "worktree",
        "add",
        "-q",
        wt.to_str().unwrap(),
        "-b",
        "feature",
    ];
    assert!(git(&worktree_add, &repo).status.success());
    std::fs::write(wt.join("b.txt"), "b\n").unwrap();
    assert!(git(&["add", "-A"], &wt).status.success());
    assert!(git(&commit_args, &wt).status.success());

    assert_eq!(
        branch_merged(wt.to_str().unwrap()),
        Some(false),
        "an unmerged branch blocks the rm door"
    );

    assert!(git(&["merge", "-q", "feature"], &repo).status.success());
    assert_eq!(
        branch_merged(wt.to_str().unwrap()),
        Some(true),
        "a merged branch passes"
    );

    std::fs::remove_dir_all(&root).ok();
}

#[test]
fn rm_take_worktree_keeps_the_tree_when_the_probe_cannot_answer() {
    // AC7-ERR: an unanswerable probe keeps the tree. Removal never guesses.
    let wt = fake_worktree("mute");
    let mut row = ask_row("wt3", Some("2020-01-01T00:00:00Z"));
    row.cwd = wt.to_string_lossy().into_owned();
    let receipt = rm_take_worktree_with(
        &row,
        &|_| WorktreeGate::Unanswerable("the reapable probe could not answer".into()),
        &|_| Ok(()),
    );
    assert_eq!(
        receipt.map(|o| o.receipt()),
        Some(format!(
            "worktree kept: {} (the reapable probe could not answer)",
            wt.to_string_lossy()
        ))
    );
    std::fs::remove_dir_all(&wt).ok();
}

#[tokio::test]
async fn rm_refuses_to_report_removed_when_the_row_is_already_gone() {
    // AC1-NEG (silent-no-op mode): the row entry_for_lifecycle resolved is no
    // longer in the file by the time the write runs (raced away by a
    // concurrent teardown, e.g. another rm or a direct `claude rm`). The
    // injected claude_rm closure deletes the row as its side effect,
    // reproducing that race deterministically: retain() then has nothing
    // to drop, and the handler must refuse rather than report removed:true.
    let home = short_home("rmalreadygone");
    let row = claude_rm_row(
        "raced-worker",
        "ccccdddd",
        "ccccdddd-1111-2222-3333-444444444444",
    );
    state::update_registry(&home.registry_json(), |registry| registry.entries.push(row)).unwrap();
    let ctx = test_ctx(home.clone(), PathBuf::from("fno-agents-worker"));
    let request = Request::new(1, "agent.rm", json!({"name": "raced-worker"}));
    let raced_home = home.clone();
    let snapshots = claude_row_then_absent("ccccdddd", "stopped");

    let response = handle_rm_with(
        &ctx,
        &request,
        &snapshots,
        &move |_| {
            state::update_registry(&raced_home.registry_json(), |registry| {
                registry.entries.retain(|e| e.name != "raced-worker");
            })
            .unwrap();
            Ok(())
        },
        &|_| true,
        &|_, _| Ok(true),
        &|_, _| PaneProbe::Unknown,
    )
    .await;

    let message = &response.error().unwrap().message;
    assert!(message.contains("registry does not hold"));
    assert!(state::load_registry(&home.registry_json())
        .unwrap()
        .entries
        .is_empty());
    std::fs::remove_dir_all(home.root()).ok();
}

#[test]
fn stop_refusal_names_a_pane_kill_the_mux_parser_accepts() {
    // The refusal string and the parser drift independently: the refusal
    // once printed `main:76` while the parser demanded a bare number, so
    // the instrument named a way out that errored with EXIT_USAGE. Hold
    // both sides in one test: build the refusal through the same renderer
    // handle_stop prints (its probe-unanswered branch) and feed the
    // command to the real parse_pane_args, no hardcoded expected string
    // anywhere. Kept off handle_stop itself: the mux pane probe shells
    // bare `fno` from PATH, so on a machine whose live mux runs a session
    // named `main` the probe answers Absent and the pid branch, which
    // names no kill command, replaces this one.
    let printed =
        stop_refusal_detail::pane_row_refusal("pane-worker", "main", 76, PaneProbe::Unknown, None);
    let selector = printed
        .split("Kill the pane: `")
        .nth(1)
        .expect("refusal names the kill command")
        .split('`')
        .next()
        .expect("the printed command is backtick-closed")
        .strip_prefix("fno mux pane kill ")
        .expect("the printed command is the pane kill verb");
    let op = fno::cli_args::PaneOp::Kill(fno::cli_args::MuxTail { tail: Vec::new() });
    let parsed = fno::mux_cli::parse_pane_args(&op, &[selector.into()])
        .expect("the refusal's own command must parse");
    let cmd = parsed.cmd;
    assert_eq!(parsed.session.as_deref(), Some("main"));
    assert!(matches!(cmd, fno::mux_cli::PaneCmd::Kill { pane: 76, .. }));
}

#[test]
fn stale_summary_parses_the_asked_line() {
    // The verb's ACTUAL --json output, embedded summary text and all -
    // not a synthetic standalone Summary line the parser could never see.
    let line = r#"{"outcome": "asked", "question_id": "q-37222570", "stale_count": 12, "oldest_h": 1829, "summary": "Summary: 12 stale, outcome asked, oldest 1829h"}"#;
    let r = parse_stale_sweep(line).expect("parses");
    assert_eq!(r.stale, 12);
    assert_eq!(r.oldest_h, 1829);
    assert_eq!(r.outcome, "asked");
}

#[test]
fn linked_worktree_detection_separates_owners_from_passers_through() {
    // The whole ownership test is `.git` being a FILE (a `gitdir:` pointer)
    // rather than a directory. Getting it backwards would either pin every
    // row again or reap rows that DO own a dirty worktree.
    let dir = tempfile::tempdir().expect("tmpdir");
    let base = dir.path();

    let canonical = base.join("canonical");
    std::fs::create_dir_all(canonical.join(".git")).unwrap();
    assert!(
        !is_linked_worktree(canonical.to_str().unwrap()),
        "a .git DIRECTORY is the canonical checkout; the row owns nothing removable"
    );

    let linked = base.join("linked");
    std::fs::create_dir_all(&linked).unwrap();
    std::fs::write(linked.join(".git"), "gitdir: /somewhere/.git/worktrees/x\n").unwrap();
    assert!(
        is_linked_worktree(linked.to_str().unwrap()),
        "a .git FILE is a linked worktree; cleanliness decides its row"
    );

    let plain = base.join("plain");
    std::fs::create_dir_all(&plain).unwrap();
    assert!(!is_linked_worktree(plain.to_str().unwrap()));

    // Unreadable and empty both fail closed toward "owns nothing", so the row
    // is judged on terminal status and grace instead of waiting forever on a
    // cleanliness answer that can never arrive.
    assert!(!is_linked_worktree(""));
    assert!(!is_linked_worktree("/nonexistent/path/that/cannot/be/read"));
}

// The gc ladder, reap-receipt gate and plan_reconcile families, moved
// verbatim into their own module (file budget: test motion is the
// sanctioned shrink; this file is far over the shrink-only line).
#[path = "daemon/tests/gc_mux_member.rs"]
mod gc_mux_member;
#[path = "daemon/tests/gc_receipts.rs"]
mod gc_receipts;
#[path = "daemon/tests/keeper_sweep.rs"]
pub(in crate::daemon) mod keeper_sweep;
#[path = "daemon/tests/reap_holds.rs"]
mod reap_holds;
#[path = "daemon/tests/reap_session.rs"]
mod reap_session;
#[path = "daemon/tests/rm_refusal.rs"]
mod rm_refusal;
// The rm success family: same module motion as gc_receipts (file budget).
#[path = "daemon/tests/rm_success.rs"]
mod rm_success;
#[path = "daemon/tests/stop_claims.rs"]
mod stop_claims;
// The stale-sweep test family (x-39f4): same file-budget motion as the
// families above.
#[path = "daemon/tests/incident_pause.rs"]
mod incident_pause;

// The codex thread lane's spawn/registry/resume test family, moved
// verbatim into its own module for the same reason as gc_receipts above:
// file budget, test motion is the sanctioned shrink.
#[path = "daemon/tests/codex_thread_lane.rs"]
mod codex_thread_lane;

// --- plan_reconcile (US6.9): tri-state, status-aware transitions, budget ---

fn rentry(name: &str, status: AgentStatus, last_reconciled: Option<&str>) -> RegistryEntry {
    RegistryEntry {
        substrate: None,
        node: None,
        spawned_by_session: None,
        spawned_by_harness: None,
        spawned_by_cwd: None,
        launch_account: None,
        related_session_id: None,
        origin: None,
        name: name.into(),
        short_id: name.into(),
        legacy_provider: "codex".into(),
        provider: None,
        model: None,
        model_basis: None,
        effort: None,
        harness: None,
        harness_session_id: None,
        predecessor_session_ids: Vec::new(),
        forked_from_session_id: None,
        route_provider_id: None,
        model_name: None,
        account_record_id: None,
        cwd: "/tmp".into(),
        project_root: "/tmp".into(),
        session_id: Some("sid".into()),
        spawn_trigger: None,
        legacy_claude_short_id: None,
        claude_session_uuid: None,
        messaging_socket_path: None,
        codex_session_id: None,
        gemini_session_id: None,
        mcp_channel_id: None,
        host_mode: None,
        cc_session_id: None,
        status,
        last_message_at: None,
        created_at: "t".into(),
        pid: None,
        pid_start_time: None,
        keeper_child_pid: None,
        log_path: None,
        last_reconciled_at: last_reconciled.map(String::from),
        inside_leg: None,
        exited_at: None,
        mux: None,
        screen_state: None,
        role_level: None,
        role_scope: None,
        role_grantor: None,
        route_settings_path: None,
        fno_id: None,
        delivery_policy: None,
        sandbox_posture: None,
        ..Default::default()
    }
}

/// A codex THREAD row: no short_id, interactive host mode, full session
/// id, and a recorded rollout path (the durable resume object). Shared by
/// the reconcile/liveness family here and by the codex-thread-lane family
/// in `codex_thread_lane.rs`, which reaches it through the glob.
fn thread_entry(name: &str, status: AgentStatus, log_path: Option<String>) -> RegistryEntry {
    let mut entry = rentry(name, status, None);
    entry.pid = Some(999_999_999);
    entry.short_id = String::new();
    entry.legacy_provider = String::new();
    entry.harness = Some("codex".into());
    entry.host_mode = Some(crate::state::HOST_MODE_INTERACTIVE.into());
    entry.session_id = None;
    entry.harness_session_id = Some(format!("0198thread-{name}-00000000000000"));
    entry.log_path = log_path;
    entry
}

fn probe_err() -> crate::provider::ReachabilityProbeError {
    crate::provider::ReachabilityProbeError::new("codex", "store unavailable")
}

// --- find_uuid_backfill_row (x-c393): backfill a null-uuid bg row ---------

/// A `claude --bg` row: jobId in `short_id`, `claude_session_uuid` null.
/// A transcript file for a fixture row that is genuinely finished.
///
/// The corroboration gate needs a POSITIVE reading that a worker stopped
/// writing, and these tests run with a zero grace window, so any file that
/// already exists reads as stale. A fixture with no transcript at all reads
/// as UNKNOWN and is kept, which is the correct production behaviour and
/// would silently hollow out these assertions.
fn stale_log(dir: &std::path::Path) -> String {
    let p = dir.join("transcript.jsonl");
    std::fs::write(&p, "{}\n").unwrap();
    // BACKDATE IT. A file written this second reads as fresh even against a
    // zero-length window, so a fixture that only creates the file proves the
    // opposite of what it claims.
    assert!(std::process::Command::new("touch")
        .args(["-t", "200001010000", &p.to_string_lossy()])
        .status()
        .expect("touch runs")
        .success());
    p.to_string_lossy().into_owned()
}

fn bg_claude_row(name: &str, short_id: &str) -> RegistryEntry {
    let mut e = rentry(name, AgentStatus::Live, None);
    e.legacy_provider = "claude".into();
    e.short_id = short_id.into();
    e.claude_session_uuid = None;
    // A row fno itself spawned: the retirement origin gate retires only
    // these, so every retirement-path fixture starts from spawn.
    e.origin = Some("spawn".into());
    // x-7bcd: needs a resolvable handle; short_id is the transport key
    // this row is actually tested against, not one of the three legs.
    e.log_path = Some(format!("/tmp/{name}.log"));
    e
}

#[test]
fn find_uuid_backfill_row_refuses_ambiguous_short_collision() {
    // AC1-ERR: two null-uuid rows share the short-id -> refuse, don't guess.
    let rows = vec![
        bg_claude_row("w1", "3228ccad"),
        bg_claude_row("w2", "3228ccad"),
    ];
    assert!(matches!(
        find_uuid_backfill_row(&rows, "3228ccad-c078-4b53-a8c9-7199b831eae4"),
        UuidBackfill::Ambiguous
    ));
}

#[test]
fn reconcile_flips_unreachable_live_to_orphaned_and_recovers_orphaned() {
    let entries = vec![
        rentry("live-but-gone", AgentStatus::Live, None),
        rentry("back-from-dead", AgentStatus::Orphaned, None),
    ];
    let (changes, out) = plan_reconcile(
        &entries,
        |e| match e.name.as_str() {
            "live-but-gone" => Ok(false), // unreachable
            _ => Ok(true),                // reachable
        },
        || false,
        |_| true,
        |_| false,
        |_| false,
        |_| false,
        |_| false,
        |_| RowLiveness::Alive, // x-5d96 liveness: Alive flips nothing
        true,                   // roster readable: the flip needs a successful roster read
    );
    assert_eq!(out.orphans, vec!["live-but-gone".to_string()]);
    assert_eq!(out.recovered, vec!["back-from-dead".to_string()]);
    assert_eq!(out.updated.len(), 2);
    // Both probed -> both get a status change recorded.
    assert_eq!(
        changes[0].new_status,
        Some(AgentStatus::Orphaned),
        "unreachable live agent should orphan"
    );
    assert_eq!(changes[1].new_status, Some(AgentStatus::Live));
}

/// AC12: a PRE-v19 row (no posture key) still parses and reads the safe
/// default - never a parse failure, never an accidental escalation.
#[test]
fn pre_v19_row_without_posture_parses_with_the_safe_default() {
    let raw = json!({
        "name": "legacy-thread",
        "short_id": "",
        "legacy_provider": "",
        "harness": "codex",
        "harness_session_id": "0198old-0000-7000-8000-000000000001",
        "cwd": "/tmp",
        "project_root": "/tmp",
        "host_mode": "interactive",
        "status": "live",
        "created_at": "2026-08-01T00:00:00Z",
    });
    let entry: RegistryEntry = serde_json::from_value(raw).expect("pre-v19 row parses");
    assert_eq!(entry.sandbox_posture, None);
    assert!(
        !crate::codex_posture::entry_posture_is_full_access(&entry),
        "an unrecorded posture reads the safe default, never full access"
    );
}

#[test]
fn reconcile_mux_pane_liveness_follows_the_pid_not_the_store() {
    // Codex P1/P2 (#603): a mux-hosted pane is PTY-governed, so on a
    // session-store miss a live pid keeps it Live and a dead pid reaps to
    // Exited. A pid-less pane (_lookup_child_pid best-effort miss) has no PTY
    // signal and must NOT be preserved -- pid_live maps None to true, so that
    // would keep a maybe-dead pane immortal; it defers to store liveness
    // (orphan) instead.
    let mk = |name: &str, pid: Option<u32>| {
        let mut e = rentry(name, AgentStatus::Live, None);
        e.mux = Some(crate::state::MuxRef {
            session: "main".into(),
            pane_id: 7,
        });
        e.pid = pid;
        e
    };
    let entries = vec![
        mk("live-pane", Some(4242)), // pid present + alive
        mk("dead-pane", Some(4243)), // pid present + dead
        mk("pidless-pane", None),    // pid capture missed
    ];
    let (changes, out) = plan_reconcile(
        &entries,
        |_| Ok(false), // session_index miss for all
        || false,
        |e| e.name == "live-pane", // only live-pane's pid is alive
        |_| false,
        |_| false,
        |_| false,
        |_| false,
        |_| RowLiveness::Alive, // x-5d96 liveness: Alive flips nothing
        true,                   // roster readable: the flip needs a successful roster read
    );
    assert_eq!(
        changes[0].new_status, None,
        "a live-pid mux pane is preserved"
    );
    assert_eq!(
        changes[1].new_status,
        Some(AgentStatus::Exited),
        "a dead-pid mux pane is reaped to Exited"
    );
    assert_eq!(
        changes[2].new_status,
        Some(AgentStatus::Orphaned),
        "a pid-less mux pane defers to store liveness (orphan), not immortal"
    );
    assert_eq!(out.orphans, vec!["pidless-pane".to_string()]);
}

#[test]
fn reconcile_store_hit_does_not_resurrect_a_pid_dead_row() {
    // x-830c: a store that never evicts (opencode keeps its session rows
    // forever) answers Ok(true) long after the pane is gone. Recovery needs
    // the pid too, or every sweep would flip a dead orphan back to Live and
    // discovery would hand out a recipient nobody drains.
    let entries = vec![
        rentry("dead-orphan", AgentStatus::Orphaned, None),
        rentry("live-orphan", AgentStatus::Orphaned, None),
    ];
    let (changes, out) = plan_reconcile(
        &entries,
        |_| Ok(true), // session still in the store for both
        || false,
        |e| e.name == "live-orphan",
        |_| false,
        |_| false,
        |_| false,
        |_| false,
        |_| RowLiveness::Alive, // x-5d96 liveness: Alive flips nothing
        true,                   // roster readable: the flip needs a successful roster read
    );
    assert_eq!(
        changes[0].new_status, None,
        "a store hit must not recover a row whose pid is dead"
    );
    assert_eq!(
        changes[1].new_status,
        Some(AgentStatus::Live),
        "a store hit on a live pid still recovers"
    );
    assert_eq!(out.recovered, vec!["live-orphan".to_string()]);
}

#[test]
fn reconcile_inconclusive_preserves_status() {
    let entries = vec![rentry("flaky", AgentStatus::Live, None)];
    let (changes, out) = plan_reconcile(
        &entries,
        |_| Err(probe_err()),
        || false,
        |_| true,
        |_| false,
        |_| false,
        |_| false,
        |_| false,
        |_| RowLiveness::Alive, // x-5d96 liveness: Alive flips nothing
        true,                   // roster readable: the flip needs a successful roster read
    );
    assert_eq!(changes[0].new_status, None, "must NOT flip on inconclusive");
    assert!(out.orphans.is_empty());
    assert_eq!(out.inconsistent.len(), 1);
    assert_eq!(out.inconsistent[0].0, "flaky");
}

#[test]
fn reconcile_does_not_reap_a_bg_thread_that_is_live_in_claudes_roster() {
    // x-beb7: a `claude --substrate bg` thread has claude's harness, no
    // footnote pid and no mux, so it matches `is_one_shot_ask` exactly like a
    // finished ask -- but it is a RUNNING process owned by claude's daemon.
    // Reaping it unprobed made `fno-agents wait --state done` return
    // "done (via exit)" within seconds for a worker whose transcript was
    // still growing, which reads to a waiting lead as a dead teammate.
    let entries = vec![bg_claude_row("think-web-copy", "35570a01")];
    assert!(
        entries[0].is_one_shot_ask(),
        "a bg thread must still match the ask shape, or this test proves nothing"
    );

    // Present in the roster == running: leave the row alone.
    let (changes, out) = plan_reconcile(
        &entries,
        |_| Ok(true),
        || false,
        |_| true,
        |_| true,
        |_| false,
        |_| false,
        |_| false,
        |_| RowLiveness::Alive, // x-5d96 liveness: Alive flips nothing
        true,                   // roster readable: the flip needs a successful roster read
    );
    assert_eq!(
        changes[0].new_status, None,
        "a bg thread claude's daemon still lists must not be reaped to exited"
    );
    assert!(out.updated.is_empty());

    // Absent from the roster == genuinely gone: the ask reap still applies,
    // so this is a liveness check, not a blanket exemption for claude rows.
    let (changes, out) = plan_reconcile(
        &entries,
        |_| Ok(true),
        || false,
        |_| true,
        |_| false,
        |_| false,
        |_| false,
        |_| false,
        |_| RowLiveness::Alive, // x-5d96 liveness: Alive flips nothing
        true,                   // roster readable: the flip needs a successful roster read
    );
    assert_eq!(changes[0].new_status, Some(AgentStatus::Exited));
    assert_eq!(out.updated, vec!["think-web-copy".to_string()]);
}

#[test]
fn apply_reconcile_change_clears_pid_only_on_exited() {
    // Locked Decision #7: a row reconciled to Exited drops its pid; any other
    // transition keeps it. Every applied change freshens last_reconciled_at.
    let mut to_exited = rentry("x", AgentStatus::Live, None);
    to_exited.pid = Some(4242);
    to_exited.pid_start_time = Some(99);
    to_exited.inside_leg = Some(state::InsideLegReport {
        state: state::InsideLegState::Working,
        seq: 3,
        reason: None,
        received_at: "2026-06-27T00:00:00Z".into(),
        ttl_ms: None,
        posture: None,
    });
    apply_reconcile_change(&mut to_exited, Some(AgentStatus::Exited), None, "T1");
    assert_eq!(to_exited.status, AgentStatus::Exited);
    assert_eq!(to_exited.pid, None, "exited row must drop its pid");
    assert_eq!(to_exited.pid_start_time, None);
    assert_eq!(
        to_exited.inside_leg, None,
        "exited row must clear the inside-leg authority (E3.3 / AC-X2-4)"
    );
    assert_eq!(to_exited.last_reconciled_at.as_deref(), Some("T1"));
    assert_eq!(
        to_exited.exited_at.as_deref(),
        Some("T1"),
        "the Exited transition stamps exited_at; CHECKED alone must not pose as one"
    );

    let mut to_orphaned = rentry("y", AgentStatus::Live, None);
    to_orphaned.pid = Some(4242);
    to_orphaned.inside_leg = Some(state::InsideLegReport {
        state: state::InsideLegState::Working,
        seq: 1,
        reason: None,
        received_at: "2026-06-27T00:00:00Z".into(),
        ttl_ms: None,
        posture: None,
    });
    apply_reconcile_change(&mut to_orphaned, Some(AgentStatus::Orphaned), None, "T2");
    assert_eq!(to_orphaned.status, AgentStatus::Orphaned);
    assert_eq!(
        to_orphaned.pid,
        Some(4242),
        "non-exited transition keeps pid"
    );
    assert!(
        to_orphaned.inside_leg.is_some(),
        "a non-exit transition keeps the inside-leg report (only exit tears it down)"
    );
    assert_eq!(
        to_orphaned.exited_at, None,
        "a non-exit transition writes no exit stamp"
    );

    // No status change: status held, but CHECKED still freshens (AC2-FR).
    let mut no_change = rentry("z", AgentStatus::Live, Some("OLD"));
    no_change.pid = Some(4242);
    apply_reconcile_change(&mut no_change, None, None, "T3");
    assert_eq!(no_change.status, AgentStatus::Live);
    assert_eq!(no_change.pid, Some(4242));
    assert_eq!(no_change.last_reconciled_at.as_deref(), Some("T3"));
    assert_eq!(
        no_change.exited_at, None,
        "a CHECKED-only probe must not write an exit stamp"
    );
}

#[test]
fn buffer_pending_report_highest_seq_wins_and_is_bounded() {
    use std::collections::HashMap;
    let rep = |seq| state::InsideLegReport {
        state: state::InsideLegState::Working,
        seq,
        reason: None,
        received_at: "2026-06-27T00:00:00Z".into(),
        ttl_ms: None,
        posture: None,
    };
    let mut map: HashMap<String, state::InsideLegReport> = HashMap::new();

    // First buffer for a session: stored.
    assert!(matches!(
        buffer_pending_report(&mut map, "s1", rep(2)),
        BufferOutcome::Buffered
    ));
    assert_eq!(map["s1"].seq, 2);

    // A reordered/duplicate early push (seq <= buffered) is dropped, buffer unchanged.
    assert!(matches!(
        buffer_pending_report(&mut map, "s1", rep(1)),
        BufferOutcome::StaleSeq { last: 2 }
    ));
    assert_eq!(
        map["s1"].seq, 2,
        "stale early push must not regress the buffer"
    );

    // A newer push for the same session advances it.
    assert!(matches!(
        buffer_pending_report(&mut map, "s1", rep(5)),
        BufferOutcome::Buffered
    ));
    assert_eq!(map["s1"].seq, 5);

    // Fill to cap with distinct sessions, then a NEW session is dropped (Full),
    // while an existing session still advances.
    for i in 0..PENDING_INSIDE_LEG_CAP {
        buffer_pending_report(&mut map, &format!("fill{i}"), rep(1));
    }
    assert!(map.len() >= PENDING_INSIDE_LEG_CAP);
    assert!(matches!(
        buffer_pending_report(&mut map, "brand-new", rep(1)),
        BufferOutcome::Full
    ));
    assert!(!map.contains_key("brand-new"));
    assert!(
        matches!(
            buffer_pending_report(&mut map, "s1", rep(9)),
            BufferOutcome::Buffered
        ),
        "an already-buffered session advances even at cap (no new key)"
    );
}

#[test]
fn reconcile_defers_remaining_when_budget_exhausted() {
    let entries = vec![
        rentry("a", AgentStatus::Live, None),
        rentry("b", AgentStatus::Live, None),
        rentry("c", AgentStatus::Live, None),
    ];
    // Budget allows exactly one probe, then reports exhausted.
    let mut probes = 0;
    let (changes, out) = plan_reconcile(
        &entries,
        |_| {
            probes += 1;
            Ok(true)
        },
        {
            let mut checked = 0;
            move || {
                let exhausted = checked >= 1;
                checked += 1;
                exhausted
            }
        },
        |_| true,
        |_| false,
        |_| false,
        |_| false,
        |_| false,
        |_| RowLiveness::Alive, // x-5d96 liveness: Alive flips nothing
        true,                   // roster readable: the flip needs a successful roster read
    );
    assert_eq!(out.deferred, 2, "two trailing entries should defer");
    assert_eq!(changes.len(), 1, "only one entry probed before budget");
}

/// A pane-hosted codex row: empty short_id (matches the real spawn shape),
/// a mux ref, and whatever pid/session id the caller sets afterward.
fn codex_pane_row(name: &str) -> RegistryEntry {
    let mut e = ask_row(name, None);
    e.harness = Some("codex".into());
    // x-7bcd: a real codex pane starts id-less (late-bind is what sets
    // harness_session_id), so undo ask_row's default and use a log_path
    // for the resolvable handle instead.
    e.harness_session_id = None;
    e.log_path = Some(format!("/tmp/{name}.log"));
    e.status = AgentStatus::Live;
    e.mux = Some(state::MuxRef {
        session: "main".into(),
        pane_id: 1,
    });
    e
}

#[test]
fn late_bind_surfaces_registry_write_failure() {
    let home = tmp_home("late-bind-write-failure");
    let emitter = EventEmitter::new(home.events_jsonl(), "daemon");
    let me = std::process::id();
    let Some(my_start) = process_start_time(me) else {
        return;
    };
    state::update_registry(&home.registry_json(), |r| {
        let mut existing = ask_row("existing", None);
        existing.harness = Some("codex".into());
        existing.harness_session_id = Some("duplicate-session".into());
        r.entries.push(existing);

        let mut candidate = codex_pane_row("pane-a");
        candidate.pid = Some(me);
        candidate.pid_start_time = Some(my_start);
        r.entries.push(candidate);
    })
    .unwrap();

    let error =
        late_bind_codex_sessions(&home, &emitter, &|_| Some("duplicate-session".to_string()))
            .expect_err("duplicate session identity must fail the late-bind write");

    let reg = state::load_registry(&home.registry_json()).unwrap();
    assert!(reg.find("pane-a").unwrap().harness_session_id.is_none());
    assert!(error.contains("late-bind registry write failed for pane-a"));
    let events = read_events(&home);
    assert!(events.iter().any(|e| {
        e.get("type").and_then(Value::as_str) == Some("agent_late_bind_failed")
            && e.get("data")
                .and_then(|data| data.get("name"))
                .and_then(Value::as_str)
                == Some("pane-a")
    }));
    std::fs::remove_dir_all(home.root()).ok();
}

#[test]
fn late_bind_applies_a_dead_predecessor_succession() {
    let home = tmp_home("late-bind-succession");
    let emitter = EventEmitter::new(home.events_jsonl(), "daemon");
    let me = std::process::id();
    let Some(my_start) = process_start_time(me) else {
        return;
    };
    state::update_registry(&home.registry_json(), |r| {
        let mut entry = codex_pane_row("pane-a");
        entry.pid = Some(me);
        entry.pid_start_time = Some(my_start);
        entry.harness_session_id = Some("session-a".into());
        entry.fno_id = Some("thread-a".into());
        r.entries.push(entry);
    })
    .unwrap();

    late_bind_codex_sessions_with_transition(
        &home,
        &emitter,
        &|_| Some("session-b".to_string()),
        &|session| (session == "session-a").then_some(false),
    )
    .unwrap();

    let reg = state::load_registry(&home.registry_json()).unwrap();
    let entry = reg.find("pane-a").unwrap();
    assert_eq!(entry.harness_session_id.as_deref(), Some("session-b"));
    assert_eq!(entry.predecessor_session_ids, vec!["session-a"]);
    assert!(read_events(&home).iter().any(|event| {
        event.get("type").and_then(Value::as_str) == Some("agent_late_bind")
            && event
                .get("data")
                .and_then(|data| data.get("transition"))
                .and_then(Value::as_str)
                == Some("succession")
    }));
    std::fs::remove_dir_all(home.root()).ok();
}

#[test]
fn late_bind_does_not_apply_liveness_sampled_from_a_stale_predecessor() {
    let home = tmp_home("late-bind-stale-predecessor");
    let emitter = EventEmitter::new(home.events_jsonl(), "daemon");
    let me = std::process::id();
    let Some(my_start) = process_start_time(me) else {
        return;
    };
    state::update_registry(&home.registry_json(), |r| {
        let mut entry = codex_pane_row("pane-a");
        entry.pid = Some(me);
        entry.pid_start_time = Some(my_start);
        entry.harness_session_id = Some("session-a".into());
        r.entries.push(entry);
    })
    .unwrap();

    let switched = std::cell::Cell::new(false);
    late_bind_codex_sessions_with_transition(
        &home,
        &emitter,
        &|_| {
            if !switched.replace(true) {
                state::update_registry(&home.registry_json(), |r| {
                    r.find_mut("pane-a").unwrap().harness_session_id = Some("session-c".into());
                })
                .unwrap();
            }
            Some("session-b".to_string())
        },
        &|session| (session == "session-a").then_some(false),
    )
    .unwrap();

    let reg = state::load_registry(&home.registry_json()).unwrap();
    assert_eq!(reg.entries.len(), 1);
    assert_eq!(
        reg.find("pane-a").unwrap().harness_session_id.as_deref(),
        Some("session-c")
    );
    std::fs::remove_dir_all(home.root()).ok();
}

// ---------------------------------------------------------------------------
// poll_until_ready unit tests (Task 1.1: readiness-detector wiring)
// ---------------------------------------------------------------------------

/// A detector that reports ready as soon as the visible text ends with "❯".
struct PromptDetector;
impl crate::readiness::ReadinessDetector for PromptDetector {
    fn provider_name(&self) -> &str {
        "test-cli"
    }
    fn is_ready(
        &self,
        screen: &crate::readiness::ScreenView,
    ) -> Result<bool, crate::readiness::ReadinessError> {
        Ok(screen.visible_text.trim_end().ends_with('\u{276f}'))
    }
}

/// A detector that always returns not-ready (simulates a hung CLI).
struct NeverReadyDetector;
impl crate::readiness::ReadinessDetector for NeverReadyDetector {
    fn provider_name(&self) -> &str {
        "never"
    }
    fn is_ready(
        &self,
        _screen: &crate::readiness::ScreenView,
    ) -> Result<bool, crate::readiness::ReadinessError> {
        Ok(false)
    }
}

/// AC1-HP: poll_until_ready returns the settled screen text once the
/// detector reports ready. The reply must come from the ready snapshot,
/// NOT from an intermediate partial snapshot.
#[tokio::test(flavor = "current_thread")]
async fn poll_until_ready_returns_settled_reply_on_ready_prompt() {
    // Three snapshots: two "not ready" then one showing the idle prompt.
    let snapshots: &[&str] = &["loading...", "still loading...", "done \u{276f}"];
    let idx = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let idx2 = idx.clone();
    let fetcher = move || {
        let i = idx2.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let text = snapshots[i.min(snapshots.len() - 1)].to_string();
        std::future::ready(Some(text))
    };
    let result = poll_until_ready(
        fetcher,
        Box::new(PromptDetector),
        Duration::from_millis(1),
        Duration::from_secs(5),
    )
    .await;
    assert!(result.is_ok(), "expected Ok, got {result:?}");
    let reply = result.unwrap();
    assert_eq!(
        reply, "done \u{276f}",
        "reply must be the settled snapshot text, got {reply:?}"
    );
}

/// AC2-ERR: poll_until_ready returns Err when the timeout elapses before
/// the detector ever reports ready. It must NOT silently return an empty or
/// partial reply.
#[tokio::test(flavor = "current_thread")]
async fn poll_until_ready_returns_error_on_timeout() {
    let fetcher = || std::future::ready(Some("still thinking...".to_string()));
    let result = poll_until_ready(
        fetcher,
        Box::new(NeverReadyDetector),
        Duration::from_millis(10),
        Duration::from_millis(40), // very short timeout
    )
    .await;
    assert!(
        result.is_err(),
        "expected Err on timeout, got Ok({:?})",
        result.ok()
    );
}

// -----------------------------------------------------------------------
// -----------------------------------------------------------------------

/// A stub family-1 probe answer for the tests that only pin the state.
/// `observed_model` null here stands for "this probe did not answer it",
/// which the row renders as `no-transcript` rather than inventing a model.
///
/// `reachability: None` deliberately exercises the COMPATIBILITY FALLBACK in
/// `rendered_status_from_truth` (a `fno` too old to emit the verdict), which
/// is what keeps these pre-existing state-mapping assertions meaningful.
/// `probe_reachable` below covers the current wire.
pub(super) fn probe(state: &str) -> Option<crate::truth_probe::TruthProbe> {
    Some(crate::truth_probe::TruthProbe {
        state: state.into(),
        reachability: None,
        basis: None,
        last_activity_age_s: None,
        last_activity_basis: None,
        last_event_at: None,
        last_message: None,
        observed_model: Value::Null,
        provider_refusal: None,
        harness_title: None,
    })
}

/// The dormant gate's batch seam answering for nobody: no row's tail is
/// readable. Every caller below has no live rows to probe, so this is the
/// same "the probe said nothing" input the per-handle `None` used to be.
// -- The shared liveness ladder (x-5d96) ---------------------------------
use crate::client_verbs::row_liveness;

/// A per-row prober mirroring [`fn@live_liveness_prober`] for the staged
/// answer: same positive-death fold, index built per call.
fn ladder_claude_row(name: &str, short_id: &str) -> RegistryEntry {
    let mut e = bg_claude_row(name, short_id);
    e.status = AgentStatus::Live;
    e
}

fn heartbeat(state: state::InsideLegState, received_at: &str) -> state::InsideLegReport {
    state::InsideLegReport {
        state,
        seq: 3,
        reason: None,
        received_at: received_at.into(),
        ttl_ms: None,
        posture: None,
    }
}

/// Write a claude bg session file whose messaging socket is `sock`, so the
/// in-process socket rung can connect to a REAL listener.
fn write_bg_session(root: &Path, short_id: &str, sock: &Path) {
    let sessions = root.join(".claude").join("sessions");
    std::fs::create_dir_all(&sessions).unwrap();
    let body = format!(
            "{{\"jobId\":\"{short_id}\",\"kind\":\"bg\",\"messagingSocketPath\":\"{}\",\"sessionId\":\"sess-{short_id}\",\"cwd\":\"/tmp\"}}",
            sock.to_str().unwrap()
        );
    std::fs::write(sessions.join("111.json"), body).unwrap();
}

/// A bindable unix socket path short enough for SUN_LEN (104): the
/// sandbox tmp roots run long, and a socket path that long refuses to
/// bind. Pid-suffixed so parallel test runs never share one.
fn short_sock(tag: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!("fno5d96-{}-{tag}.sock", std::process::id()))
}

#[test]
fn the_ladder_answers_alive_for_a_known_live_socket() {
    // POSITIVE CONTROL (x-5d96): an absence-only suite proves nothing - a
    // probe that answers Unknown for every input passes every negative
    // test. Assert Alive on a session known to be live first.
    let home = tmp_home("ladder-positive-control");
    let sock = short_sock("alive");
    let listener = std::os::unix::net::UnixListener::bind(&sock).unwrap();
    write_bg_session(home.root(), "alive01", &sock);
    let e = ladder_claude_row("positive", "alive01");
    let answer = row_liveness(&e, &crate::claude_ask::ClaudeHome::at(home.root()));
    assert_eq!(answer, RowLiveness::Alive);
    drop(listener);
    let _ = std::fs::remove_file(&sock);
    std::fs::remove_dir_all(home.root()).ok();
}

#[test]
fn a_heartbeat_advancing_past_exited_at_is_alive_x_d3ad() {
    // The adopted specimen: exited_at two seconds after created_at while
    // the heartbeat kept advancing past it. No socket answers; the
    // heartbeat rung does.
    let home = tmp_home("ladder-d3ad");
    let mut e = ladder_claude_row("resurrected", "d3adrow");
    e.created_at = "2026-08-01T00:00:00Z".into();
    e.exited_at = Some("2026-08-01T00:00:02Z".into());
    e.inside_leg = Some(heartbeat(
        state::InsideLegState::Working,
        "2026-08-01T00:00:30Z",
    ));
    let answer = row_liveness(&e, &crate::claude_ask::ClaudeHome::at(home.root()));
    assert_eq!(answer, RowLiveness::Alive);
    assert_ne!(answer, RowLiveness::Dead);
    std::fs::remove_dir_all(home.root()).ok();
}

// -- Rung 4: codex rollout freshness (x-798a) ----------------------------

fn codex_truth_none(_uuid: &str) -> Option<String> {
    None
}

/// A codex registry row in the measured x-798a shape: live-ish status,
/// no claude surfaces, one harness session id, no exit stamp (no pid to
/// confirm dead, no reconcile to terminal - the row never gets one).
fn ladder_codex_row(name: &str, session_id: &str) -> RegistryEntry {
    let mut e = rentry(name, AgentStatus::Live, None);
    e.harness = Some("codex".into());
    e.harness_session_id = Some(session_id.into());
    e.claude_session_uuid = None;
    e
}

/// The real codex store shape: nested date dirs, the session id embedded
/// in a `rollout-*.jsonl` filename - what `HarnessStoreIndex` resolves.
fn write_rollout(root: &Path, session_id: &str) -> std::path::PathBuf {
    let dir = root.join("2026").join("09").join("01");
    std::fs::create_dir_all(&dir).unwrap();
    let p = dir.join(format!("rollout-2026-09-01T10-00-00-{session_id}.jsonl"));
    std::fs::write(&p, "{\"type\":\"session_meta\"}\n").unwrap();
    p
}

#[test]
fn an_advancing_codex_rollout_answers_alive_and_the_sweep_names_it_live() {
    // AC1-HP (x-798a): a rollout jsonl written within the window proves
    // the worker is advancing. The heartbeat rung cannot fire here (no
    // exited_at to advance past) and the claude rungs cannot see the row;
    // without this rung the row is kept but never probeable.
    let home = tmp_home("ladder-codex-alive");
    let codex = tempfile::tempdir().unwrap();
    write_rollout(codex.path(), "cdx-alive");
    let e = ladder_codex_row("codex-live", "cdx-alive");
    let answer = crate::client_verbs::row_liveness_with_codex_root(
        &e,
        &crate::claude_ask::ClaudeHome::at(home.root()),
        codex.path(),
        codex_truth_none,
    );
    assert_eq!(answer, RowLiveness::Alive);
    std::fs::remove_dir_all(home.root()).ok();
}

#[test]
fn a_codex_row_with_no_readable_rollout_stays_unknown_and_kept() {
    // AC1-EDGE + AC2-EDGE: a missing rollout and an unreadable store both
    // read Unknown (an absent store has two explanations and only one of
    // them is a dead worker), and the policy names the row not-terminal.
    // This IS the measured x-798a baseline: named, kept, never reaped.
    let home = tmp_home("ladder-codex-absent");
    let codex = tempfile::tempdir().unwrap();
    let e = ladder_codex_row("codex-gone", "cdx-gone");
    let answer = crate::client_verbs::row_liveness_with_codex_root(
        &e,
        &crate::claude_ask::ClaudeHome::at(home.root()),
        codex.path(),
        codex_truth_none,
    );
    assert_eq!(answer, RowLiveness::Unknown);
    let answer = crate::client_verbs::row_liveness_with_codex_root(
        &e,
        &crate::claude_ask::ClaudeHome::at(home.root()),
        &codex.path().join("does-not-exist"),
        codex_truth_none,
    );
    assert_eq!(answer, RowLiveness::Unknown);
    assert_ne!(answer, RowLiveness::Dead);
    std::fs::remove_dir_all(home.root()).ok();
}

#[test]
fn a_claude_row_is_never_judged_by_the_codex_store() {
    // AC3 (harness keying): a claude row whose session id has a FRESH
    // rollout in the codex store must still answer Unknown. The rung
    // fires only for codex rows; keyed on session id alone, one fresh
    // codex store would vouch for every claude row on the machine.
    let home = tmp_home("ladder-codex-keying");
    let codex = tempfile::tempdir().unwrap();
    write_rollout(codex.path(), "shared-sess");
    let mut e = ladder_claude_row("claude-row", "");
    e.harness = Some("claude".into());
    e.harness_session_id = Some("shared-sess".into());
    e.claude_session_uuid = None;
    let answer = crate::client_verbs::row_liveness_with_codex_root(
        &e,
        &crate::claude_ask::ClaudeHome::at(home.root()),
        codex.path(),
        codex_truth_none,
    );
    assert_eq!(answer, RowLiveness::Unknown);
    std::fs::remove_dir_all(home.root()).ok();
}

#[test]
fn a_stamped_codex_row_is_never_resurrected_by_a_fresh_rollout() {
    // The stop verb writes a POSITIVE exit stamp; the rollout it leaves
    // behind stays fresh-written for the whole window. Rung 4 must go
    // silent on a stamped row, or the sweep would clear the stamp and
    // resurrect a deliberately stopped row on freshness alone (the codex
    // review finding). The heartbeat rung draws the same line - only
    // ADVANCEMENT past the stamp answers, never a quiet file.
    let home = tmp_home("ladder-codex-stamped");
    let codex = tempfile::tempdir().unwrap();
    write_rollout(codex.path(), "cdx-stopped");
    let mut e = ladder_codex_row("codex-stopped", "cdx-stopped");
    e.exited_at = Some("2026-09-01T10:30:00Z".into());
    let answer = crate::client_verbs::row_liveness_with_codex_root(
        &e,
        &crate::claude_ask::ClaudeHome::at(home.root()),
        codex.path(),
        codex_truth_none,
    );
    assert_eq!(answer, RowLiveness::Unknown);
    assert_ne!(answer, RowLiveness::Alive);
    std::fs::remove_dir_all(home.root()).ok();
}

#[test]
fn the_ladder_never_returns_dead() {
    // Only a positive death proof may answer Dead, and absence never is
    // one. Sweep every rung combination the ladder can see; no input
    // yields Dead.
    let home = tmp_home("ladder-never-dead");
    let ch = crate::claude_ask::ClaudeHome::at(home.root());
    for truth in [
        None,
        Some("done"),
        Some("stalled"),
        Some("unreachable"),
        Some("working"),
        Some("watching"),
        Some("your-move"),
    ] {
        for exited in [None, Some("2026-08-01T00:00:02Z")] {
            for beat in [
                None,
                Some(("2026-07-31T00:00:00Z", false)), // before exited: silence
                Some(("2026-08-01T00:00:30Z", true)),  // after exited: Alive
            ] {
                let mut e = ladder_claude_row("row", "quiet02");
                e.exited_at = exited.map(String::from);
                e.inside_leg = beat.map(|(at, _)| heartbeat(state::InsideLegState::Working, at));
                e.claude_session_uuid = Some("3228ccad-c078-4b53-a8c9-7199b831eae4".into());
                let answer =
                    crate::client_verbs::row_liveness_with(&e, &ch, |_| truth.map(String::from));
                assert_ne!(
                    answer,
                    RowLiveness::Dead,
                    "ladder answered Dead for truth={truth:?} exited={exited:?} beat={beat:?}"
                );
            }
        }
    }
    std::fs::remove_dir_all(home.root()).ok();
}

#[test]
fn an_unreadable_roster_never_flips_the_zombie_arm() {
    // codex P1 (PR 1329): an unreadable roster is unknown liveness. The
    // fail-closed bg_live reading must not combine with a transient
    // ladder failure to orphan every live bg worker - the flip needs a
    // roster read that SUCCEEDED.
    let entries = vec![bg_claude_row("maybe", "mayb0001")];
    let (changes, _) = plan_reconcile(
        &entries,
        |_| Ok(true),
        || false,
        |_| true,
        |_| true,
        |_| false, // bg_live fail-closed true on the unreadable roster
        |_| false,
        |_| false,
        |_| RowLiveness::Unknown,
        false, // the roster read itself FAILED
    );
    assert_eq!(changes[0].new_status, None);
}

#[test]
fn reconcile_ends_the_status_constant_for_a_roster_stale_silent_row() {
    // The roster hit used to hold a claude row `live` forever: presence
    // in a possibly-stale roster proves nothing about RUNNING. A silent
    // ladder (no socket, no advancing heartbeat, no working truth state)
    // means no positive running-marker, so the ask-bucket row goes
    // Orphaned - the REVERSIBLE transition, not Exited - and gc takes it
    // from the terminal set.
    let entries = vec![bg_claude_row("zombie", "zomb0001")];
    let (changes, out) = plan_reconcile(
        &entries,
        |_| Ok(true),
        || false,
        |_| true,
        |_| true,
        |_| false, // roster: entry present
        |_| false,
        |_| false,
        |_| RowLiveness::Unknown,
        true, // roster readable
    );
    assert_eq!(changes[0].new_status, Some(AgentStatus::Orphaned));
    assert_eq!(out.orphans, vec!["zombie".to_string()]);
}

/// Adapt a per-handle answer into the BATCH seam `handle_list_with_truth`
/// takes, for the rendering tests below - they assert what a row renders,
/// never how many processes paid for it.
///
/// Deliberately NOT used by
/// `list_pays_exactly_one_batch_call_for_the_whole_page`: an adapter that
/// fans a batch back out per handle would render identically whether the
/// handler called it once or once per row, so the call SHAPE needs its own
/// test against the raw seam.
fn per_handle(
    f: impl Fn(&str) -> Option<crate::truth_probe::TruthProbe>,
) -> impl Fn(
    &[String],
) -> (
    std::collections::HashMap<String, crate::truth_probe::TruthProbe>,
    crate::truth_probe::BatchOutcome,
) {
    move |handles: &[String]| {
        let map: std::collections::HashMap<String, crate::truth_probe::TruthProbe> = handles
            .iter()
            .filter_map(|h| Some((h.clone(), f(h)?)))
            .collect();
        (map, crate::truth_probe::BatchOutcome::Measured)
    }
}

/// A probe carrying the shared verdict, as a current `fno` emits it.
pub(super) fn probe_with_verdict(
    state: &str,
    reachability: &str,
) -> Option<crate::truth_probe::TruthProbe> {
    Some(crate::truth_probe::TruthProbe {
        state: state.into(),
        reachability: Some(reachability.into()),
        basis: Some("transcript".into()),
        last_activity_age_s: Some(12.0),
        last_activity_basis: None,
        last_event_at: Some("2026-08-15T17:00:00+00:00".into()),
        last_message: Some("Still growing (101 lines, 26 percent through the pytest run)".into()),
        observed_model: Value::Null,
        provider_refusal: None,
        harness_title: None,
    })
}

pub(super) fn test_ctx(home: AgentsHome, worker_bin: PathBuf) -> Ctx {
    // The emitter commits to the store beside the home journal: `/dev/null`
    // was a sink under the journal regime, but the store needs a real path.
    let events_path = home.events_jsonl();
    Ctx {
        emitter: EventEmitter::new(events_path, "daemon"),
        home,
        opts: DaemonOptions {
            idle_exit: Duration::from_secs(1800),
            worker_bin,
            reconcile_on_start: true,
            agents_config_cwd: PathBuf::from("/dev/null"),
            // Off in tests: a unit test must never spawn a real `fno inbox notify`.
            notify_on_blocked: false,
            notify_on_done: false,
        },
        started_at: std::time::Instant::now(),
        exe_fingerprint: crate::drift::ExeFingerprint::current(),
        pid_start_time: process_start_time(std::process::id()),
        pending_inside_leg: std::sync::Mutex::new(std::collections::HashMap::new()),
        codex_threads: Arc::new(tokio::sync::Mutex::new(std::collections::HashMap::new())),
    }
}

// ---- Group 2, Task 3.1: switchboard tests --------------------------
//
// A fake stream-json emitter (NEVER a real `claude -p`): for each user turn
// it reads on stdin it emits the canonical sequence (user-echo receipt, a
// partial, the assistant reply, a result). Mirrors the stream_worker harness.

const FAKE_STREAM_EMITTER: &str = r#"
printf '%s\n' '{"type":"system","subtype":"init","session_id":"s1"}'
while IFS= read -r line; do
  printf '%s\n' '{"type":"user","message":{"role":"user"}}'
  printf '%s\n' '{"type":"stream_event","event":{"type":"content_block_delta","delta":{"type":"text_delta","text":"par"}}}'
  printf '%s\n' '{"type":"assistant","message":{"content":[{"type":"text","text":"reply-text"}]}}'
  printf '%s\n' '{"type":"result","subtype":"success","is_error":false,"result":"reply-text"}'
done
"#;

/// A SHORT-path agents home under `/tmp`: a worker's `<root>/<short_id>/worker.sock`
/// must fit in SUN_LEN (~104 chars on macOS) so switchboard tests bind real sockets.
pub(super) fn short_home(tag: &str) -> AgentsHome {
    use std::sync::atomic::{AtomicU32, Ordering};
    static C: AtomicU32 = AtomicU32::new(0);
    let n = C.fetch_add(1, Ordering::Relaxed);
    let p = PathBuf::from(format!("/tmp/fnosb{tag}{}_{n}", std::process::id()));
    let home = AgentsHome::at(&p);
    home.ensure_root().unwrap();
    crate::paths::pin_test_claims_root(&p.join("claims-root"));
    home
}

/// Seed a held-stream-thread registry row (claude + full UUID + Live).
pub(super) fn seed_stream_row(home: &AgentsHome, name: &str, short_id: &str) {
    state::update_registry(&home.registry_json(), |r| {
        r.entries.push(RegistryEntry {
            substrate: None,
            node: None,
            spawned_by_session: None,
            spawned_by_harness: None,
            spawned_by_cwd: None,
            launch_account: None,
            related_session_id: None,
            origin: None,
            name: name.into(),
            short_id: short_id.into(),
            legacy_provider: "claude".into(),
            provider: None,
            model: None,
            model_basis: None,
            effort: None,
            harness: None,
            harness_session_id: None,
            predecessor_session_ids: Vec::new(),
            forked_from_session_id: None,
            route_provider_id: None,
            model_name: None,
            account_record_id: None,
            cwd: "/tmp".into(),
            project_root: "/tmp".into(),
            session_id: None,
            spawn_trigger: None,
            legacy_claude_short_id: None,
            claude_session_uuid: Some(format!("uuid-{short_id}")),
            messaging_socket_path: None,
            codex_session_id: None,
            gemini_session_id: None,
            mcp_channel_id: None,
            cc_session_id: None,
            host_mode: None,
            status: AgentStatus::Live,
            last_message_at: None,
            created_at: "2026-06-09T00:00:00Z".into(),
            pid: None,
            pid_start_time: None,
            keeper_child_pid: None,
            log_path: None,
            last_reconciled_at: None,
            inside_leg: None,
            exited_at: None,
            mux: None,
            screen_state: None,
            role_level: None,
            role_scope: None,
            role_grantor: None,
            route_settings_path: None,
            fno_id: None,
            delivery_policy: None,
            sandbox_posture: None,
            ..Default::default()
        });
    })
    .unwrap();
}

#[test]
fn watch_serves_on_connect_and_only_on_change() {
    // The subscription contract: connect serves
    // the full document; the same (mtime, len) version answers "unchanged"
    // without a document; any write moves the stamp and the SAME `since`
    // then serves fresh rows. One stat per idle tick is the whole cost.
    let home = short_home("watch-connect");
    seed_stream_row(&home, "w1", "abc12345");
    let ctx = test_ctx(home.clone(), PathBuf::from("fno-agents-worker"));

    let resp = handle_watch(&ctx, &Request::new(1, "agent.watch", json!({})));
    let first = resp.result().unwrap();
    assert_eq!(first["doc"]["agents"].as_array().unwrap().len(), 1);
    let version = first["version"].clone();

    let resp = handle_watch(
        &ctx,
        &Request::new(2, "agent.watch", json!({"since": version})),
    );
    let again = resp.result().unwrap();
    assert!(
        again["doc"].is_null(),
        "unchanged version serves no document"
    );
    assert_eq!(again["version"], version);

    state::update_registry(&home.registry_json(), |r| {
        r.entries[0].status = AgentStatus::Exited;
    })
    .unwrap();
    let resp = handle_watch(
        &ctx,
        &Request::new(3, "agent.watch", json!({"since": version})),
    );
    let after = resp.result().unwrap();
    assert_ne!(after["version"], version, "a write moves the stamp");
    let doc = after["doc"]["agents"].as_array().unwrap();
    assert_eq!(doc.len(), 1);
    assert_eq!(
        doc[0]["status"],
        json!("exited"),
        "rows are the fresh write"
    );
    std::fs::remove_dir_all(home.root()).ok();
}

#[test]
fn list_row_key_set_matches_shared_contract() {
    const CONTRACT: &str = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../schemas/agents-list-row.json"
    ));
    let contract: Value = serde_json::from_str(CONTRACT).expect("contract is valid JSON");
    assert_eq!(
        contract["projection_omissions"],
        json!([]),
        "model and model_basis are projected with their basis, never omitted"
    );
    let mut expected: std::collections::BTreeSet<String> = contract["required"]
        .as_array()
        .expect("required is an array")
        .iter()
        .map(|k| k.as_str().unwrap().to_string())
        .collect();
    expected.extend(
        contract["rust_only"]["keys"]
            .as_array()
            .expect("rust_only.keys is an array")
            .iter()
            .map(|k| k.as_str().unwrap().to_string()),
    );

    let home = short_home("listcontract");
    seed_stream_row(&home, "worker-contract", "abc12345");
    state::update_registry(&home.registry_json(), |r| {
        let e = &mut r.entries[0];
        // A pane-hosted row holds the mux ref INSTEAD of a transport key
        // (mux XOR worker XOR bg), so short_id is empty -- which is why
        // `session_id` resolves to null for exactly these rows and
        // `harness_session_id` is the only identity they carry.
        e.short_id = String::new();
        e.harness = Some("claude".into());
        e.harness_session_id = Some("e6f78b98-e594-47ed-ad81-84f8a78b8bb7".into());
        e.claude_session_uuid = Some("e6f78b98-e594-47ed-ad81-84f8a78b8bb7".into());
        e.mux = Some(crate::state::MuxRef {
            session: "main".into(),
            pane_id: 10,
        });
        e.role_level = Some(1);
        e.role_scope = Some("epic-x".into());
        e.role_grantor = Some("lead".into());
        // A vendor stamp on a claude-hosted row: the exact shape the
        // provider axis exists to describe, and the one the pre-split
        // alias lied about by carrying "claude" here.
        e.provider = Some("zai".into());
        e.effort = Some("xhigh".into());
        e.node = Some("x-cafe".into());
        e.context_used_pct = Some(26);
        e.context_used_tokens = Some(258_687);
        e.context_window_tokens = Some(1_000_000);
        e.context_measured_at = Some("2026-09-30T12:00:00Z".into());
        e.mail_unread = Some(1);
        // (x-7955) AC9-HP: the recorded lane rides verbatim, so a reader
        // can tell a paneless pane row from a thread row.
        e.substrate = Some("thread".into());
    })
    .unwrap();
    let ctx = test_ctx(home.clone(), PathBuf::from("fno-agents-worker"));
    let req = Request::new(1, "agent.list", json!({}));

    let response = handle_list_with_truth(&ctx, &req, per_handle(|_handle| probe("working")));
    let result = response.result().unwrap();
    let row = &result["agents"][0];
    assert_eq!(row["context_used_pct"], 26);
    assert_eq!(row["context_used_tokens"], 258_687);
    assert_eq!(row["context_window_tokens"], 1_000_000);
    assert_eq!(row["context_measured_at"], "2026-09-30T12:00:00Z");
    assert_eq!(row["mail_unread"], 1);

    let actual: std::collections::BTreeSet<String> =
        row.as_object().unwrap().keys().cloned().collect();
    assert_eq!(actual, expected, "list row key set drifted from contract");
    assert_eq!(
        result["fields_omitted"], contract["projection_omissions"],
        "list envelope omissions drifted from contract"
    );
    // (x-7955) The recorded lane, by VALUE: the projection reads the
    // registry's record, never an inference from mux or thread_id.
    assert_eq!(
        row["substrate"], "thread",
        "the list row carries the recorded substrate"
    );

    // By VALUE: an unknown request renders null, never a fabricated match.
    assert_eq!(row["requested_model"], Value::Null);
    assert_eq!(row["model_substituted"], Value::Null);
    state::update_registry(&home.registry_json(), |r| {
        r.entries[0].requested_model = Some("glm-5.3[1m]".into());
        r.entries[0].model = Some("glm-5.3-flash[1m]".into());
        r.entries[0].model_basis = Some("requested".into());
    })
    .unwrap();
    let mut contradicting = probe("working").unwrap();
    contradicting.observed_model = json!({"kind": "observed", "model": "glm-5.3-flash"});
    let response = handle_list_with_truth(
        &ctx,
        &req,
        per_handle(|_handle| Some(contradicting.clone())),
    );
    let row = &response.result().unwrap()["agents"][0];
    assert_eq!(row["requested_model"], "glm-5.3[1m]");
    assert_eq!(
        (&row["model"], &row["model_basis"]),
        (&json!("glm-5.3-flash[1m]"), &json!("requested"))
    );
    assert_eq!(
        row["model_substituted"],
        json!({"requested": "glm-5.3[1m]", "observed": "glm-5.3-flash"})
    );
    // Suffix-only difference is a MATCH: the marker stays null.
    let mut agreeing = probe("working").unwrap();
    agreeing.observed_model = json!({"kind": "observed", "model": "glm-5.3"});
    let response = handle_list_with_truth(&ctx, &req, per_handle(|_handle| Some(agreeing.clone())));
    let row = &response.result().unwrap()["agents"][0];
    assert_eq!(row["requested_model"], "glm-5.3[1m]");
    assert_eq!(row["model_substituted"], Value::Null);

    // An always-null key is the same lie as a missing one: assert values.
    assert_eq!(row["harness"], "claude");
    // `provider` carries the stored vendor axis, never a harness (AC8,
    // post x-f273). Emitting it from one serializer only would be worse
    // than emitting it from both: the field would then be present or
    // absent depending on which reader answered.
    assert_eq!(row["provider"], "zai");
    assert_ne!(row["provider"], row["harness"]);
    assert!(row.get("effort").is_some(), "effort key must be emitted");
    assert_eq!(row["effort"], "xhigh");
    assert_eq!(row["node"], "x-cafe");
    assert!(["no-pr", "graph-unreadable"].contains(&row["pr_basis"].as_str().unwrap()));
    assert_eq!(
        row["harness_session_id"],
        "e6f78b98-e594-47ed-ad81-84f8a78b8bb7"
    );
    // The mailbox address, asserted by VALUE and not merely by presence.
    // This row is the exact shape the address column exists for: a pane
    // worker with no transport key, whose only copyable identifier before
    // this key was `name` -- and a name-lane durable write is the largest
    // still-growing category of stranded mail on the bus. The value must
    // equal what `mail drain-self` computes for itself, which is the first
    // eight; the retired `<harness>-<short>` form is refused by the
    // resolver, so emitting it here would advertise an unreachable mailbox.
    assert_eq!(row["address"], "e6f78b98");
    // The pre-fix surface reported this row as having no identity at all:
    // session_id is legitimately null for a pane row (no transport key), so
    // harness_session_id is what has to carry it.
    assert!(row["session_id"].is_null());
    assert_eq!(row["mux"]["session"], "main");
    assert_eq!(row["mux"]["pane_id"], 10);
    assert_eq!(
        row["role"], "L1 epic-x",
        "same formatter as Python team_label"
    );
    // The raw team fields need value assertions too, not just presence:
    // hardcoding either to null passes a key-set check and the bare-row
    // null check, which is the "present but always null" lie again.
    assert_eq!(row["role_level"], 1);
    assert_eq!(row["role_scope"], "epic-x");
    assert_eq!(row["role_grantor"], "lead");

    std::fs::remove_dir_all(home.root()).ok();
}

/// The row reports the model the worker is ACTUALLY answering as, taken
/// from the same family-1 probe that produced `status`.
///
/// The projection orders rows by evidence of neglect, pinned to the
/// shared fixture: the same file the mux ranker (crates/fno) and the
/// Python serializer (cli) assert against. The three cannot share code -
/// the crates do not link and the CLI is Python - so this file is the
/// contract that keeps the three orders identical. `include_str!` is
/// compile-time, so deleting or moving the fixture breaks the build
/// rather than silently disarming the check.
#[test]
fn list_rows_sort_in_the_shared_attention_order() {
    const FIXTURE: &str = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../schemas/agents-attention-order.json"
    ));
    let fixture: Value = serde_json::from_str(FIXTURE).expect("fixture is valid JSON");
    let mut rows: Vec<Value> = fixture["rows"]
        .as_array()
        .expect("rows is an array")
        .clone();
    rows.sort_by(|a, b| attention_sort_key(a).cmp(&attention_sort_key(b)));
    let got: Vec<&str> = rows
        .iter()
        .map(|r| r["name"].as_str().expect("row has a name"))
        .collect();
    let expected: Vec<&str> = fixture["expected_order"]
        .as_array()
        .expect("expected_order is an array")
        .iter()
        .map(|v| v.as_str().expect("order entry is a string"))
        .collect();
    assert_eq!(got, expected);
}

fn stream_identity(short_id: &str) -> Value {
    json!({
        "harness": "claude",
        "session_id": format!("uuid-{short_id}"),
        "short_id": short_id,
        "created_at": "2026-06-09T00:00:00Z",
    })
}

fn switchboard_params(
    to: &str,
    to_short: &str,
    from: &str,
    from_short: Option<&str>,
    body: &str,
) -> Value {
    let mut params = json!({
        "to": to,
        "from": from,
        "body": body,
        "mirror": from_short.is_some(),
        "recipient_identity": stream_identity(to_short),
    });
    if let Some(short_id) = from_short {
        params["from_identity"] = stream_identity(short_id);
    }
    params
}

#[test]
fn list_renders_family1_truth_instead_of_stored_registry_status() {
    let home = short_home("listtruth");
    seed_stream_row(&home, "worker-list", "abc12345");
    state::update_registry(&home.registry_json(), |registry| {
        registry.entries[0].status = AgentStatus::Orphaned;
    })
    .unwrap();
    let ctx = test_ctx(home.clone(), PathBuf::from("fno-agents-worker"));
    let req = Request::new(1, "agent.list", json!({"status": "writing"}));

    let response = handle_list_with_truth(
        &ctx,
        &req,
        per_handle(|_handle| probe_with_verdict("working", "reachable")),
    );
    let result = response.result().unwrap();
    let agents = result["agents"].as_array().unwrap();
    assert_eq!(agents.len(), 1);
    assert_eq!(agents[0]["status"], "writing");

    std::fs::remove_dir_all(home.root()).ok();
}

#[test]
fn list_applies_cheap_filters_before_family1_subprocesses() {
    let home = short_home("listprefilter");
    seed_stream_row(&home, "claude-worker", "aaaaaaaa");
    seed_stream_row(&home, "codex-worker", "bbbbbbbb");
    state::update_registry(&home.registry_json(), |registry| {
        registry.entries[1].harness = Some("codex".into());
        registry.entries[1].harness_session_id =
            Some("bbbbbbbb-1111-2222-3333-444444444444".into());
        // The provider filter reads the v15+ vendor axis: stamp the row
        // so it matches below. entries[0] stays unstamped, so a null
        // provider contributes to no vendor filter.
        registry.entries[1].provider = Some("openai".into());
    })
    .unwrap();
    let ctx = test_ctx(home.clone(), PathBuf::from("fno-agents-worker"));
    let req = Request::new(1, "agent.list", json!({"provider": "openai"}));
    let seen = std::cell::RefCell::new(Vec::new());

    let response = handle_list_with_truth(
        &ctx,
        &req,
        per_handle(|handle| {
            seen.borrow_mut().push(handle.to_string());
            probe("working")
        }),
    );

    assert!(response.result().is_some());
    assert_eq!(
        seen.into_inner(),
        vec!["bbbbbbbb-1111-2222-3333-444444444444"]
    );
    std::fs::remove_dir_all(home.root()).ok();
}

#[test]
fn list_pays_exactly_one_batch_call_for_the_whole_page() {
    // The x-0d93 fix, asserted where it is spent: N rows used to cost N
    // Python interpreter cold starts (780 ms each) on every `fno agents
    // list`. Asserted against the RAW seam, never `per_handle` -- an
    // adapter that fans a batch back out renders identically whether the
    // handler called it once or once per row, so only a direct call count
    // can tell the two apart.
    let home = short_home("listbatchonce");
    let rows = 24;
    for i in 0..rows {
        seed_stream_row(&home, &format!("worker-{i:02}"), &format!("{i:02}aaaaaa"));
    }
    let ctx = test_ctx(home.clone(), PathBuf::from("fno-agents-worker"));
    let req = Request::new(1, "agent.list", json!({"all": true}));
    let calls = std::cell::RefCell::new(Vec::new());

    let response = handle_list_with_truth(&ctx, &req, |handles: &[String]| {
        calls.borrow_mut().push(handles.to_vec());
        let map = handles
            .iter()
            .map(|h| (h.clone(), probe("working").unwrap()))
            .collect();
        (map, crate::truth_probe::BatchOutcome::Measured)
    });

    let calls = calls.into_inner();
    assert_eq!(calls.len(), 1, "one page, one batch");
    assert_eq!(calls[0].len(), rows, "every filtered row rides that batch");
    let entries = response.result().unwrap()["agents"]
        .as_array()
        .unwrap()
        .len();
    assert_eq!(entries, rows);
    std::fs::remove_dir_all(home.root()).ok();
}

#[test]
fn list_row_the_batch_did_not_answer_renders_exactly_as_an_unanswered_row() {
    // A handle absent from the batch map must be indistinguishable from the
    // per-row path's `None`: null triple, no invented model. Anything else
    // and the batch would be reporting a reading it never took.
    let home = short_home("listbatchpartial");
    seed_stream_row(&home, "answered", "aaaaaaaa");
    seed_stream_row(&home, "unanswered", "bbbbbbbb");
    let ctx = test_ctx(home.clone(), PathBuf::from("fno-agents-worker"));
    let req = Request::new(1, "agent.list", json!({"all": true}));

    let response = handle_list_with_truth(&ctx, &req, |handles: &[String]| {
        // Answers for the first handle only; the second is simply missing.
        let map = handles
            .iter()
            .take(1)
            .map(|h| (h.clone(), probe("working").unwrap()))
            .collect();
        (map, crate::truth_probe::BatchOutcome::Measured)
    });

    let agents = response.result().unwrap()["agents"].clone();
    let rows = agents.as_array().unwrap();
    let missing = rows
        .iter()
        .find(|r| r["name"] == "unanswered")
        .expect("the unanswered row still renders");
    assert!(missing["reachability"].is_null());
    assert!(missing["basis"].is_null());
    assert!(missing["last_activity_age_s"].is_null());
    assert_eq!(missing["observed_model"]["kind"], "no-transcript");
    std::fs::remove_dir_all(home.root()).ok();
}

/// Start a real stream worker (fake emitter child) on `home.worker_sock(id)`
/// via the PUBLIC `stream_worker::run`; wait for its socket to appear.
async fn start_stream_worker(home: &AgentsHome, short_id: &str, script: &str) -> PathBuf {
    let cfg = crate::stream_worker::StreamWorkerConfig::new(
        short_id,
        home.root().to_path_buf(),
        std::env::temp_dir(),
        vec!["bash".into(), "-c".into(), script.into()],
    );
    let short_id_dbg = short_id.to_string();
    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            if let Err(e) = crate::stream_worker::run(cfg).await {
                eprintln!("STREAM WORKER RUN ERROR ({short_id_dbg}): {e}");
            }
        });
    });
    let sock = home.worker_sock(short_id);
    // Phase 1: the socket file appears (the worker has bound).
    let bind_start = std::time::Instant::now();
    while !sock.exists() && bind_start.elapsed() < Duration::from_secs(20) {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(
        sock.exists(),
        "stream worker socket never appeared for {short_id}"
    );
    // Phase 2: the worker actually accepts and answers a ping. sock.exists()
    // is bind, not readiness - a bound-but-starved worker (its own OS thread
    // + bash subprocess compete for cores under --test-threads=32) makes the
    // caller's 2s liveness probe time out and the test flake as
    // delivered:false ("not-a-live-stream-thread"). Wait for a ping before
    // handing the socket back, so the caller always sees a warm worker.
    // Capture the successful probe rather than re-probing in the assert: a
    // second independent 2s probe can itself time out under the same
    // CPU-starvation that made us wait, flaking the helper after readiness
    // was already established.
    let live_start = std::time::Instant::now();
    let mut live = false;
    while live_start.elapsed() < Duration::from_secs(30) {
        if is_live_stream_thread(&sock).await {
            live = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(
        live,
        "stream worker socket appeared but never answered a ping for {short_id}"
    );
    sock
}

/// Locate the cargo-built `fno-agents-worker` via this crate's manifest
/// dir (final binaries stay in the checkout's target/ under
/// build.build-dir, and the manifest path is worktree-local). `None` if
/// it is not built, so the e2e adopt test SKIPS rather than failing.
fn built_worker_bin() -> Option<PathBuf> {
    let cand = Path::new(env!("CARGO_MANIFEST_DIR")).join("target/debug/fno-agents-worker");
    cand.exists().then_some(cand)
}

// ---- Group 3 (ab-734fcd6c): claude stream-json front door --------------

#[test]
fn acquire_session_claim_maps_native_outcomes() {
    let td = tempfile::tempdir().unwrap();
    let _guard = crate::claims::test_env_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    std::env::set_var("FNO_CLAIMS_ROOT", td.path());
    // Fresh acquire -> Acquired.
    assert!(matches!(
        acquire_session_claim("U-1", "stream:sw1"),
        ClaimOutcome::Acquired
    ));
    // Same holder re-acquire -> still Acquired (idempotent).
    assert!(matches!(
        acquire_session_claim("U-1", "stream:sw1"),
        ClaimOutcome::Acquired
    ));
    // A different holder against a LIVE claim -> HeldByOther naming the
    // incumbent (the claim is pinned to this live test process).
    match acquire_session_claim("U-1", "stream:other") {
        ClaimOutcome::HeldByOther(who) => assert_eq!(who, "stream:sw1"),
        other => panic!("expected HeldByOther, got {other:?}"),
    }
    std::env::remove_var("FNO_CLAIMS_ROOT");
}

/// AC1-ERR / front-door routing: a fresh `host --provider claude` with no
/// `--from` has nothing to resume; it is rejected (before any claim/spawn)
/// with a pointer to the adopt verb, proving claude routed to the stream lane
/// (not the codex/gemini PTY "only codex or gemini" gate).
#[tokio::test(flavor = "current_thread")]
async fn host_claude_without_from_rejected_with_adopt_pointer() {
    let home = short_home("clnofrom");
    let ctx = test_ctx(home.clone(), PathBuf::from("/nonexistent-worker"));
    let req = Request::new(
        1,
        "agent.spawn",
        json!({"name": "cl", "provider": "claude", "host_mode": "interactive"}),
    );
    let resp = handle_spawn(&ctx, &req).await;
    match &resp.payload {
        crate::protocol::ResponsePayload::Err(e) => {
            assert_eq!(e.code, ErrorCode::InvalidParams);
            assert!(
                e.message.contains("promote") && e.message.contains("--from"),
                "claude host without --from must point at the adopt verb; got: {}",
                e.message
            );
        }
        _ => panic!("expected error for claude host without --from"),
    }
    std::fs::remove_dir_all(home.root()).ok();
}

/// AC1-EDGE single-writer: a second adopt of a session already held by a live
/// claude thread is refused (one writer per session), before any spawn. Uses
/// the lock-free one-host pre-check so it is hermetic (no worker, no claim).
#[tokio::test(flavor = "current_thread")]
async fn promote_claude_duplicate_session_refused() {
    let home = short_home("cldup");
    seed_stream_row(&home, "first", "swDup"); // claude_session_uuid = uuid-swDup, Live
    let ctx = test_ctx(home.clone(), PathBuf::from("/nonexistent-worker"));
    let req = Request::new(
        1,
        "agent.spawn",
        json!({
            "name": "second", "provider": "claude", "host_mode": "interactive",
            "resume_id": "uuid-swDup"
        }),
    );
    let resp = handle_spawn(&ctx, &req).await;
    match &resp.payload {
        crate::protocol::ResponsePayload::Err(e) => {
            assert_eq!(e.code, ErrorCode::InvalidParams);
            assert!(
                e.message.contains("already hosted") && e.message.contains("first"),
                "duplicate adopt must name the existing host; got: {}",
                e.message
            );
        }
        _ => panic!("expected single-writer refusal for duplicate adopt"),
    }
    std::fs::remove_dir_all(home.root()).ok();
}

/// AC1-HP end-to-end: `promote --provider claude --from <uuid>` adopts an idle
/// session by spawning the real `--stream` worker (with a FAKE emitter child,
/// never a real `claude -p`) and registering it `live`. The row carries
/// provider=claude + host_mode=interactive + the FULL uuid, and the worker
/// serves the stream protocol. Skips when the worker binary is not built.
#[tokio::test(flavor = "current_thread")]
async fn promote_claude_spawns_live_stream_thread() {
    let Some(worker_bin) = built_worker_bin() else {
        eprintln!("skip promote_claude_spawns_live_stream_thread: worker bin not built");
        return;
    };
    let _guard = crate::claims::test_env_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let home = short_home("cle2e");
    // Hermetic claims: point `fno agents claim` at the test home so the real
    // acquire (daemon, this process) AND the worker child (inherits this env)
    // write `session:uuid-e2e` under /tmp, never the canonical, shared
    // ~/.fno/claims. A panic before teardown then leaks at worst into a
    // throwaway /tmp dir. Only this test exercises claims, so the process-wide
    // env set does not race the claim-free tests. (Edition 2021: set_var safe.)
    std::env::set_var("FNO_CLAIMS_ROOT", home.root());
    let ctx = test_ctx(home.clone(), worker_bin);
    let req = Request::new(
        1,
        "agent.spawn",
        json!({
            "name": "cl", "provider": "claude", "host_mode": "interactive",
            "resume_id": "uuid-e2e", "cwd": "/tmp",
            // Test escape hatch: a fake stream emitter stands in for `claude -p`.
            "argv": ["bash", "-c", FAKE_STREAM_EMITTER]
        }),
    );
    let resp = handle_spawn(&ctx, &req).await;
    let res = resp.result().expect("claude adopt errored");
    assert_eq!(res["harness"], "claude");
    assert_eq!(res["status"], "live");
    assert_eq!(res["lane"], "stream");

    let reg = load_registry_offloaded(home.registry_json())
        .await
        .expect("registry readable");
    let row = reg.find("cl").expect("adopted row missing");
    assert_eq!(row.harness_name(), "claude");
    assert_eq!(row.host_mode.as_deref(), Some("interactive"));
    assert_eq!(row.claude_session_uuid.as_deref(), Some("uuid-e2e"));
    assert_eq!(row.status, AgentStatus::Live);

    let sock = home.worker_sock(&row.short_id);
    assert!(
        is_live_stream_thread(&sock).await,
        "adopted thread must serve the stream protocol"
    );

    // Teardown: shut the worker down (its RAII guard releases the claim), then
    // drop the test home (which holds the redirected claims dir) and clear the
    // env override so later tests see the default claims root.
    best_effort_worker_shutdown(&sock).await;
    std::fs::remove_dir_all(home.root()).ok();
    std::env::remove_var("FNO_CLAIMS_ROOT");
}

/// AC1-ERR (codex review P2): a dead-on-arrival `claude -p --resume` (bad uuid
/// / auth fail, here a child that exits immediately) must NOT register live.
/// The worker still binds + answers stream.ping, but stream.status.child_alive
/// is false, so adopt is rejected and no row is created.
#[tokio::test(flavor = "current_thread")]
async fn promote_claude_dead_on_arrival_resume_rejected() {
    let Some(worker_bin) = built_worker_bin() else {
        eprintln!("skip promote_claude_dead_on_arrival_resume_rejected: worker bin not built");
        return;
    };
    let _guard = crate::claims::test_env_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let home = short_home("cldoa");
    std::env::set_var("FNO_CLAIMS_ROOT", home.root());
    let ctx = test_ctx(home.clone(), worker_bin);
    let req = Request::new(
        1,
        "agent.spawn",
        json!({
            "name": "cl", "provider": "claude", "host_mode": "interactive",
            "resume_id": "uuid-doa", "cwd": "/tmp",
            // Child exits immediately -> stands in for a bad/expired --resume id.
            "argv": ["bash", "-c", "exit 1"]
        }),
    );
    let resp = handle_spawn(&ctx, &req).await;
    assert!(
        resp.is_err(),
        "DOA resume child must be rejected, not registered"
    );
    assert_eq!(resp.error().unwrap().code, ErrorCode::SpawnFailed);
    let reg = load_registry_offloaded(home.registry_json())
        .await
        .expect("registry readable");
    assert!(
        reg.find("cl").is_none(),
        "no row may be registered for a DOA adopt"
    );
    std::fs::remove_dir_all(home.root()).ok();
    std::env::remove_var("FNO_CLAIMS_ROOT");
}

/// AC2-HP: `send A->B` between two held stream threads drives B, discriminates
/// the user-echo receipt from the reply, and mirrors B's reply into A.
#[tokio::test(flavor = "current_thread")]
async fn switchboard_drives_b_and_mirrors_into_a() {
    let home = short_home("hp");
    seed_stream_row(&home, "A", "swA");
    seed_stream_row(&home, "B", "swB");
    let _a = start_stream_worker(&home, "swA", FAKE_STREAM_EMITTER).await;
    let _b = start_stream_worker(&home, "swB", FAKE_STREAM_EMITTER).await;
    let ctx = test_ctx(home.clone(), PathBuf::from("/nonexistent-worker"));

    let req = Request::new(
        1,
        "agent.switchboard",
        switchboard_params("B", "swB", "A", Some("swA"), "hello"),
    );
    let resp = handle_switchboard(&ctx, &req).await;
    let res = resp.result().expect("switchboard errored");
    assert_eq!(res["delivered"], true, "not delivered: {res:?}");
    assert_eq!(res["reply"], "reply-text");
    assert_eq!(res["is_error"], false);
    assert_eq!(res["receipt"], true, "user-echo receipt not observed");
    assert_eq!(res["mirrored"], true, "B's reply was not mirrored into A");
    assert_eq!(res["identity_verified"], true);

    // The injected-event reuse carries the switchboard transport discriminator.
    let events = read_events(&home);
    assert!(
        events.iter().any(|e| e["type"] == "agent_deliver_injected"
            && e["data"]["transport"] == "switchboard"
            && e["data"]["mirrored"] == true),
        "switchboard injected event missing: {events:?}"
    );
    std::fs::remove_dir_all(home.root()).ok();
}

#[tokio::test(flavor = "current_thread")]
async fn switchboard_refuses_replaced_recipient_identity() {
    let home = short_home("replaced");
    seed_stream_row(&home, "victim", "swB");
    let ctx = test_ctx(home.clone(), PathBuf::from("/nonexistent-worker"));
    let mut params = switchboard_params("victim", "swB", "ghost", None, "secret");
    params["recipient_identity"]["session_id"] = json!("uuid-swA");

    let response = handle_switchboard(&ctx, &Request::new(1, "agent.switchboard", params)).await;
    let result = response.result().expect("identity mismatch is a demotion");
    assert_eq!(result["delivered"], false);
    assert_eq!(result["reason"], "recipient-identity-changed");
    std::fs::remove_dir_all(home.root()).ok();
}

#[tokio::test(flavor = "current_thread")]
async fn switchboard_failed_drive_does_not_orphan_restamped_recipient() {
    let home = short_home("failedrestamp");
    seed_stream_row(&home, "B", "swB");
    let turn_started = home.root().join("turn-started");
    let restamp_done = home.root().join("restamp-done");
    let script = format!(
        r#"
printf '%s\n' '{{"ts":"2026-01-01T00:00:00Z","source":"test","type":"system","subtype":"init","session_id":"s1"}}'
while IFS= read -r line; do
  touch '{}'
  while [ ! -f '{}' ]; do sleep 0.01; done
  exit 1
done
"#,
        turn_started.display(),
        restamp_done.display()
    );
    let _b = start_stream_worker(&home, "swB", &script).await;
    let ctx = test_ctx(home.clone(), PathBuf::from("/nonexistent-worker"));

    let registry_path = home.registry_json();
    let restamp_signal = turn_started.clone();
    let restamp_complete = restamp_done.clone();
    let restamp = tokio::spawn(async move {
        let start = Instant::now();
        while !restamp_signal.exists() && start.elapsed() < Duration::from_secs(5) {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        assert!(restamp_signal.exists(), "drive never reached the worker");
        state::update_registry(&registry_path, |registry| {
            let row = registry.find_mut("B").expect("recipient row missing");
            row.short_id = "swC".into();
            row.harness_session_id = Some("uuid-replacement".into());
            row.claude_session_uuid = Some("uuid-replacement".into());
            row.created_at = "2026-06-09T00:00:01Z".into();
            row.status = AgentStatus::Live;
        })
        .unwrap();
        std::fs::write(restamp_complete, b"done\n").unwrap();
    });

    let response = handle_switchboard(
        &ctx,
        &Request::new(
            1,
            "agent.switchboard",
            switchboard_params("B", "swB", "ghost", None, "fail after receipt"),
        ),
    )
    .await;
    restamp.await.unwrap();
    let result = response.result().expect("failed drive is a demotion");
    assert_eq!(result["delivered"], false);

    let registry = state::load_registry(&home.registry_json()).unwrap();
    let replacement = registry.find("B").expect("replacement row missing");
    assert_eq!(replacement.status, AgentStatus::Live);
    assert_eq!(
        replacement.harness_session_id.as_deref(),
        Some("uuid-replacement")
    );
    let events = read_events(&home);
    assert!(
        events.iter().any(|event| {
            event["type"] == "agent_deliver_status_write_failed"
                && event["data"]["name"] == "B"
                && event["data"]["reason"] == "recipient-identity-changed"
        }),
        "identity-CAS failure event missing: {events:?}"
    );
    std::fs::remove_dir_all(home.root()).ok();
}

#[tokio::test(flavor = "current_thread")]
async fn switchboard_v2_routes_to_identity_guard() {
    let home = short_home("v2route");
    let ctx = Arc::new(test_ctx(home.clone(), PathBuf::from("/nonexistent-worker")));
    let response = dispatch_agent(
        &ctx,
        &Request::new(
            1,
            "agent.switchboard_v2",
            json!({"to": "victim", "from": "ghost", "body": "secret"}),
        ),
    )
    .await;
    let error = response.error().expect("missing identity must fail");
    assert_eq!(error.code, ErrorCode::InvalidParams);
    assert!(error.message.contains("recipient_identity"));
    std::fs::remove_dir_all(home.root()).ok();
}

// --- codex thread lane: actor-driven ask / stop (the x-de10 probes) ---
//
// These three are the make-it-fail probes for the concurrency rewrite:
// each FAILS against the old Arc<Mutex<CodexThread>> shape (verified by
// running them on the pre-rewrite daemon) and passes against the actor.

// PATH + ask-wait env serialization across these tests AND every other
// PATH-mutating test in the lib (provider.rs, client_verbs.rs): one shared
// mutex, never nested. The fakes install `codex` on PATH and one test
// retunes FNO_CODEX_ASK_WAIT_MS; a concurrent `set_var` from an unrelated
// test would make the child inherit a broken PATH (exit 127).

/// Point `CODEX_HOME` at a fake shared app-server daemon for the duration
/// of `body`. The fake lives in [`crate::codex_fake_daemon`]: one
/// implementation of the protocol, shared with the integration tests.
async fn with_fake_codex_daemon(
    behavior: crate::codex_fake_daemon::Behavior,
    body: impl std::future::Future<Output = ()>,
) {
    let _guard = crate::path_test_guard();
    let _daemon = crate::codex_fake_daemon::FakeDaemon::start(behavior);
    body.await;
}

/// Block until the worker's actor reports a DRIVING turn, or fail.
///
/// The tests below used a fixed 300ms sleep, which is a bet that the seed
/// turn started by then. Under a loaded parallel suite it does not always
/// hold, and losing it does not make a test slower, it makes it WRONG: an
/// interrupt arriving before the turn starts reads `NoTurnInFlight`, so
/// `stop` correctly reports `no-turn` and the assertion for `interrupted`
/// fails. Waiting on the actor's own turn id removes the bet.
async fn await_driving_turn(ctx: &Ctx, name: &str) -> String {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        let handle = ctx.codex_threads.lock().await.get(name).cloned();
        if let Some(turn) = handle.and_then(|handle| handle.current_turn_id()) {
            return turn;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "no turn ever started driving for {name}"
        );
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
}

/// Block until an `agent_ask_done` is on disk, then return every event.
///
/// The two callers slept a fixed 400ms or 1300ms and then counted. That is
/// a bet that the fake's turn finished AND its emit reached the file inside
/// the window. CI lost it: `switchboard_to_codex_thread_delivers_on_steering_ack_mid_turn`
/// read ZERO done events on a loaded runner while passing on every local
/// run. A completion is an observable marker, so wait for the marker.
///
/// Counting `== 1` right after the first one lands is still sound: both
/// callers make exactly two submits and have already asserted upstream
/// that the two shared one turn id, so nothing can start a second turn
/// after this returns.
async fn await_ask_done(home: &AgentsHome) -> Vec<Value> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    loop {
        let events = read_events(home);
        if events.iter().any(|e| e["type"] == "agent_ask_done") {
            return events;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "no agent_ask_done ever landed: {events:?}"
        );
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
}

// ── gate record tests (Task 2.3) ─────────────────────────────────────────

// ---- inside-leg report (E3.2) ------------------------------------------

/// AC-X2 store: a report for a registered claude session lands on the row's
/// `inside_leg` field with the daemon-stamped `received_at`, and emits
/// `inside_leg_report`.
#[test]
fn handle_report_stores_on_matching_row() {
    let home = tmp_home("report-store");
    seed_stream_row(&home, "worker-A", "repA"); // claude_session_uuid = uuid-repA
    let ctx = test_ctx(home.clone(), PathBuf::from("fno-agents-worker"));
    let req = Request::new(
        1,
        "agent.report",
        json!({"session_id": "uuid-repA", "seq": 3, "state": "working", "reason": "running tests"}),
    );
    let resp = handle_report(&ctx, &req);
    assert!(!resp.is_err(), "report must return Ok: {resp:?}");
    assert_eq!(resp.result().unwrap()["stored"], true);

    let reg = state::load_registry(&home.registry_json()).unwrap();
    let rep = reg.entries[0]
        .inside_leg
        .as_ref()
        .expect("inside_leg stored");
    assert_eq!(rep.state, state::InsideLegState::Working);
    assert_eq!(rep.seq, 3);
    assert_eq!(rep.reason.as_deref(), Some("running tests"));
    assert!(!rep.received_at.is_empty(), "daemon stamps received_at");

    // A same-state repeat stores its newer seq and journals nothing.
    let repeat = Request::new(
        2,
        "agent.report",
        json!({"session_id": "uuid-repA", "seq": 4, "state": "working"}),
    );
    assert_eq!(
        handle_report(&ctx, &repeat).result().unwrap()["stored"],
        true
    );
    let events = read_events(&home);
    assert_eq!(
        events
            .iter()
            .filter(|e| e["type"] == "inside_leg_report")
            .count(),
        1,
        "one inside_leg_report per state change: {events:?}"
    );
    std::fs::remove_dir_all(home.root()).ok();
}

/// AC-X2-1 seq: a `seq <= last_seq` is dropped (the newer report wins) and
/// emits `inside_leg_report_dropped`.
#[test]
fn handle_report_drops_stale_seq() {
    let home = tmp_home("report-stale");
    seed_stream_row(&home, "worker-A", "repB");
    let ctx = test_ctx(home.clone(), PathBuf::from("fno-agents-worker"));
    // seq=2 stored, then a reordered seq=1 arrives.
    let _ = handle_report(
        &ctx,
        &Request::new(
            1,
            "agent.report",
            json!({"session_id": "uuid-repB", "seq": 2, "state": "working"}),
        ),
    );
    let resp = handle_report(
        &ctx,
        &Request::new(
            2,
            "agent.report",
            json!({"session_id": "uuid-repB", "seq": 1, "state": "done"}),
        ),
    );
    assert!(!resp.is_err());
    assert_eq!(resp.result().unwrap()["stored"], false);
    assert_eq!(resp.result().unwrap()["dropped"], "stale_seq");

    // The badge still reflects seq=2/working, not the late seq=1/done.
    let reg = state::load_registry(&home.registry_json()).unwrap();
    let rep = reg.entries[0].inside_leg.as_ref().unwrap();
    assert_eq!(rep.seq, 2);
    assert_eq!(rep.state, state::InsideLegState::Working);

    let events = read_events(&home);
    assert!(events
        .iter()
        .any(|e| e["type"] == "inside_leg_report_dropped" && e["data"]["reason"] == "stale_seq"));
    std::fs::remove_dir_all(home.root()).ok();
}

/// Missing/invalid params fail closed with InvalidParams (no registry write).
#[test]
fn handle_report_rejects_bad_params() {
    let home = tmp_home("report-bad");
    let ctx = test_ctx(home.clone(), PathBuf::from("fno-agents-worker"));
    for params in [
        json!({"seq": 1, "state": "working"}),          // no session_id
        json!({"session_id": "x", "state": "working"}), // no seq
        json!({"session_id": "x", "seq": 1}),           // no state
        json!({"session_id": "x", "seq": 1, "state": "idle"}), // bad state
    ] {
        let resp = handle_report(&ctx, &Request::new(1, "agent.report", params.clone()));
        assert!(resp.is_err(), "expected InvalidParams for {params}");
    }
    std::fs::remove_dir_all(home.root()).ok();
}
