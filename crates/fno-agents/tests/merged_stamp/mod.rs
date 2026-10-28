//! The done-node merged-stamp early allow and its fail-open arms.
//!
//! A finished worker must retire its stop-gate fire on the graph store's
//! own stamp; these cases pin the allow, the gates it evaluates first
//! (open findings, shipped work, done probes), and the arms that keep the
//! REST read.

use super::*;

/// Shared rig for the merged-stamp tests: a manifest bound to `node_id`, a
/// promise transcript, and the node row seeded under a temp FNO_HOME. The
/// caller owns the FNO_HOME restore after its fire.
fn stamped_rig(
    node_id: &str,
    session: &str,
    row: serde_json::Value,
) -> (TempDir, PathBuf, PathBuf) {
    let tmp = TempDir::new().unwrap();
    let cwd = tmp.path();
    fs::create_dir_all(cwd.join(".fno")).unwrap();
    isolate_settings(cwd);
    let manifest = format!(
        "{}graph_node_id: {node_id}\n",
        new_manifest(session, "2026-06-05T00:00:00Z", true)
    );
    let manifest_path = cwd.join("target-state.md");
    fs::write(&manifest_path, &manifest).unwrap();
    fs::write(cwd.join("transcript.jsonl"), transcript_with_promise()).unwrap();
    std::env::set_var("FNO_HOME", tmp.path());
    fno_agents::graph_store::seed_rows(&tmp.path().join("graph.json"), std::slice::from_ref(&row))
        .unwrap();
    (tmp, manifest_path, cwd.join("transcript.jsonl"))
}

/// A done node whose recorded PR reads merged retires the fire on
/// the graph store's own stamp, before the REST read. Under a loaded
/// machine the read is the one thing that can still time out, and each
/// timeout re-blocked a finished worker that never went idle, so its queued
/// mail never landed. The gh mock fails on EVERYTHING including --version:
/// if the gate reaches gh at all, the test fails loudly.
#[test]
fn x0d87_done_node_merged_stamp_allows_without_rest_read() {
    let restore_home = std::env::var_os("FNO_HOME").unwrap_or_default();
    let (tmp, manifest_path, transcript_path) = stamped_rig(
        "x-stamp",
        "sess-stamp",
        serde_json::json!({
            "id": "x-stamp", "slug": "x-stamp", "title": "n", "type": "feature",
            "status": "done", "priority": "p1",
            "pr_number": 42, "pr_url": "https://github.com/bllshttng/footnote/pull/42",
            "merge_status": "merged"
        }),
    );
    let cwd = tmp.path();

    let gh = make_script(
        tmp.path(),
        "gh",
        r#"echo "unexpected gh invocation: $*" >&2
exit 1"#,
    );
    // The gate's shipping check reads local git state: HEAD equals its push
    // target and the tree is clean.
    let git = make_script(
        tmp.path(),
        "git",
        r#"case "$*" in
  *"@{push}"*) echo sha1111111111111111111111111111111111111111 ;;
  *"rev-parse"*) echo sha1111111111111111111111111111111111111111 ;;
  *"porcelain"*) exit 0 ;;
  *) echo deadbeefdeadbeefdeadbeefdeadbeef00000001 ;;
esac"#,
    );

    let (code, d) = fire(&[
        "loop-check",
        "--state",
        manifest_path.to_str().unwrap(),
        "--transcript",
        transcript_path.to_str().unwrap(),
        "--cwd",
        cwd.to_str().unwrap(),
        "--now",
        "2026-06-05T00:30:00Z",
        &format!("--gh-bin={}", gh.display()),
        &format!("--git-bin={}", git.display()),
    ]);
    std::env::set_var("FNO_HOME", restore_home);

    assert_eq!(code, 0, "exit code must be 0 for allow");
    assert_eq!(
        d.decision, "allow",
        "the merged stamp must retire the fire: {}",
        d.message
    );
    assert_eq!(
        d.termination_reason.as_deref(),
        Some("DonePRGreen"),
        "the early allow carries the same terminal the REST read would produce; got {:?} ({})",
        d.termination_reason,
        d.message
    );
    let events = event_text(&project_events(&cwd));
    assert!(
        events.contains("loop_check_merged_stamp"),
        "the stamp row must name its evidence; events: {events}"
    );
    assert!(
        !events.contains("loop_check_gh_error"),
        "no gh read may run on this path; events: {events}"
    );
}

/// The early allow must not outrank the shipped-work check: a local commit
/// the push target does not have falls through to the full gate, which on a
/// gh outage blocks exactly as before.
#[test]
fn x0d87_unshipped_local_commit_falls_through() {
    let restore_home = std::env::var_os("FNO_HOME").unwrap_or_default();
    let (tmp, manifest_path, transcript_path) = stamped_rig(
        "x-stamp4",
        "sess-stamp4",
        serde_json::json!({
            "id": "x-stamp4", "slug": "x-stamp4", "title": "n", "type": "feature",
            "status": "done", "priority": "p1",
            "pr_number": 42, "merge_status": "merged"
        }),
    );
    let cwd = tmp.path();
    let git = make_script(
        tmp.path(),
        "git",
        r#"case "$*" in
  *"@{push}"*) echo sha2222222222222222222222222222222222222222 ;;
  *"rev-parse"*) echo sha1111111111111111111111111111111111111111 ;;
  *"porcelain"*) exit 0 ;;
  *) echo deadbeefdeadbeefdeadbeefdeadbeef00000001 ;;
esac"#,
    );

    let mock = MockBins::failing_gh();
    let (code, d) = fire(&[
        "loop-check",
        "--state",
        manifest_path.to_str().unwrap(),
        "--transcript",
        transcript_path.to_str().unwrap(),
        "--cwd",
        cwd.to_str().unwrap(),
        "--now",
        "2026-06-05T00:30:00Z",
        &format!("--gh-bin={}", mock.gh.display()),
        &format!("--git-bin={}", git.display()),
    ]);
    std::env::set_var("FNO_HOME", restore_home);

    assert_eq!(
        d.decision, "block",
        "unshipped local work must fall through to the full gate: {}",
        d.message
    );
    assert_eq!(code, 0, "a block decision still exits 0");
    let events = event_text(&project_events(&cwd));
    assert!(
        !events.contains("loop_check_merged_stamp"),
        "no stamp row while local work is unshipped; events: {events}"
    );
}

/// The early allow must not outrank the operator-finding hold: an open
/// review_finding on the node falls through to the full gate.
#[test]
fn x0d87_open_operator_finding_falls_through() {
    let restore_home = std::env::var_os("FNO_HOME").unwrap_or_default();
    let (tmp, manifest_path, transcript_path) = stamped_rig(
        "x-stamp5",
        "sess-stamp5",
        serde_json::json!({
            "id": "x-stamp5", "slug": "x-stamp5", "title": "n", "type": "feature",
            "status": "done", "priority": "p1",
            "pr_number": 42, "merge_status": "merged"
        }),
    );
    let cwd = tmp.path();
    let git = make_script(
        tmp.path(),
        "git",
        r#"case "$*" in
  *"@{push}"*) echo sha1111111111111111111111111111111111111111 ;;
  *"rev-parse"*) echo sha1111111111111111111111111111111111111111 ;;
  *"porcelain"*) exit 0 ;;
  *) echo deadbeefdeadbeefdeadbeefdeadbeef00000001 ;;
esac"#,
    );
    let store = fno_agents::backlog::api::Store::new(&cwd.join("graph.json"));
    let input = fno_agents::backlog::api::FindingInput {
        body: "operator says hold".into(),
        ..Default::default()
    };
    let _ = fno_agents::backlog::api::finding_create(&store, "x-stamp5", input).unwrap();

    let mock = MockBins::failing_gh();
    let (code, d) = fire(&[
        "loop-check",
        "--state",
        manifest_path.to_str().unwrap(),
        "--transcript",
        transcript_path.to_str().unwrap(),
        "--cwd",
        cwd.to_str().unwrap(),
        "--now",
        "2026-06-05T00:30:00Z",
        &format!("--gh-bin={}", mock.gh.display()),
        &format!("--git-bin={}", git.display()),
    ]);
    std::env::set_var("FNO_HOME", restore_home);

    assert_eq!(
        d.decision, "block",
        "an open operator finding must fall through to the full gate: {}",
        d.message
    );
    assert_eq!(code, 0, "a block decision still exits 0");
    let events = event_text(&project_events(&cwd));
    assert!(
        !events.contains("loop_check_merged_stamp"),
        "no stamp row while a finding is open; events: {events}"
    );
}

/// Fail-open: a replacement PR still in flight (an unstamped row above
/// every terminal stamp) holds the early allow, and the fire keeps today's
/// outage behavior.
#[test]
fn x0d87_live_replacement_pr_holds_the_early_allow() {
    let restore_home = std::env::var_os("FNO_HOME").unwrap_or_default();
    let (tmp, manifest_path, transcript_path) = stamped_rig(
        "x-stamp2",
        "sess-stamp2",
        serde_json::json!({
            "id": "x-stamp2", "slug": "x-stamp2", "title": "n", "type": "feature",
            "status": "done", "priority": "p1",
            "pr_number": 42, "merge_status": "merged",
            "additional_prs": [{"number": 43, "url": "https://github.com/bllshttng/footnote/pull/43"}]
        }),
    );
    let cwd = tmp.path();

    let mock = MockBins::failing_gh();
    let (code, d) = fire(&[
        "loop-check",
        "--state",
        manifest_path.to_str().unwrap(),
        "--transcript",
        transcript_path.to_str().unwrap(),
        "--cwd",
        cwd.to_str().unwrap(),
        "--now",
        "2026-06-05T00:30:00Z",
        &format!("--gh-bin={}", mock.gh.display()),
        &format!("--git-bin={}", mock.git.display()),
    ]);
    std::env::set_var("FNO_HOME", restore_home);

    assert_eq!(code, 0, "a block decision still exits 0");
    assert_eq!(
        d.decision, "block",
        "a live replacement PR must hold the gate: {}",
        d.message
    );
    assert!(
        d.termination_reason.is_none(),
        "no termination while work is in flight"
    );
    let events = event_text(&project_events(&cwd));
    assert!(
        !events.contains("loop_check_merged_stamp"),
        "no stamp row while a live PR row is open; events: {events}"
    );
}

/// Fail-open: the done-check binds - an open node with a merged stamp
/// does not take the early allow.
#[test]
fn x0d87_open_node_keeps_rest_read() {
    let restore_home = std::env::var_os("FNO_HOME").unwrap_or_default();
    let (tmp, manifest_path, transcript_path) = stamped_rig(
        "x-stamp3",
        "sess-stamp3",
        serde_json::json!({
            "id": "x-stamp3", "slug": "x-stamp3", "title": "n", "type": "feature",
            "status": "ready", "priority": "p1",
            "pr_number": 42, "merge_status": "merged"
        }),
    );
    let cwd = tmp.path();

    let mock = MockBins::failing_gh();
    let (code, d) = fire(&[
        "loop-check",
        "--state",
        manifest_path.to_str().unwrap(),
        "--transcript",
        transcript_path.to_str().unwrap(),
        "--cwd",
        cwd.to_str().unwrap(),
        "--now",
        "2026-06-05T00:30:00Z",
        &format!("--gh-bin={}", mock.gh.display()),
        &format!("--git-bin={}", mock.git.display()),
    ]);
    std::env::set_var("FNO_HOME", restore_home);

    assert_eq!(code, 0, "a block decision still exits 0");
    assert_eq!(
        d.decision, "block",
        "an open node must keep the REST read: {}",
        d.message
    );
    assert!(d.termination_reason.is_none());
    let events = event_text(&project_events(&cwd));
    assert!(
        !events.contains("loop_check_merged_stamp"),
        "no stamp row for an open node; events: {events}"
    );
}
