//! The worked-nodes join contracts for the claim status leaf. The Python
//! pins monkeypatched the roster read, so the binary forward silently
//! bypassed them; these drive the real legs end to end.

use std::fs::Permissions;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::{json, Value};

const JOIN_SESSION_ID: &str = "c3d4e5f6-1111-2222-3333-444455556666";

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn identity_env() -> Vec<(&'static str, &'static str)> {
    vec![
        ("FNO_HARNESS_NAME", "claude"),
        ("FNO_HARNESS_SESSION_ID", JOIN_SESSION_ID),
        ("CODEX_THREAD_ID", ""),
        ("CLAUDE_CODE_SESSION_ID", ""),
        ("CODEX_SESSION_ID", ""),
        ("GEMINI_SESSION_ID", ""),
        ("OPENCODE_SESSION_ID", ""),
        ("CLAUDE_SESSION_ID", ""),
    ]
}

fn run_status(root: &Path, roster_dir: &Path) -> (i32, String, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_fno-agents"))
        .args(["claim", "status", "node:x-join1"])
        .env("FNO_GLOBAL_SETTINGS_PATH", "/dev/null")
        .current_dir(repo_root())
        .env("FNO_CLAIMS_ROOT", root)
        .env("FNO_STATE_DIR", root)
        .env("FNO_AGENTS_HOME", root.join("agents-home"))
        .env("FNO_EVENTS_PATH", root.join("events.jsonl"))
        .envs(fno_agents::test_run::self_owner_env())
        .envs(identity_env())
        .env("PATH", roster_dir)
        .output()
        .expect("run claim status");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8(out.stdout).unwrap(),
        String::from_utf8(out.stderr).unwrap(),
    )
}

fn seed_graph(root: &Path, entry: Value) {
    let db = root.join("db/graph.db");
    std::fs::create_dir_all(db.parent().unwrap()).unwrap();
    fno_agents::graph_store::seed_rows(&db, &[entry]).expect("seed graph");
}

fn graph_entry(sessions: Value) -> Value {
    json!({
        "id": "x-join1",
        "title": "join pin",
        "status": "design",
        "project": "fno",
        "slug": "join-pin",
        "sessions": sessions,
    })
}

fn open_session(sid: &str) -> Value {
    json!({
        "phase": "execute",
        "harness": "claude",
        "session_id": sid,
        "started_at": "2026-09-11T00:00:00Z",
        "ended_at": Value::Null,
    })
}

fn closed_session(sid: &str) -> Value {
    let mut row = open_session(sid);
    row["ended_at"] = json!("2026-09-11T01:00:00Z");
    row
}

// A cwd the fleet fold reads as a linked worktree: a .git file and a
// target-state naming the graph node.
fn fake_worktree(root: &Path, name: &str, node_id: &str) -> PathBuf {
    let wt = root.join(name);
    std::fs::create_dir_all(wt.join(".fno")).unwrap();
    std::fs::write(wt.join(".git"), "gitdir: /dev/null\n").unwrap();
    std::fs::write(
        wt.join(".fno").join("target-state.md"),
        format!("graph_node_id: {node_id}\n"),
    )
    .unwrap();
    wt
}

fn plain_dir(root: &Path, name: &str) -> PathBuf {
    let dir = root.join(name);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn roster_dir_with(root: &Path, rows: &[Value]) -> PathBuf {
    let dir = root.join("roster-bin");
    std::fs::create_dir_all(&dir).unwrap();
    let payload = json!({"agents": rows}).to_string();
    let script = dir.join("claude");
    std::fs::write(&script, format!("#!/bin/sh\necho '{payload}'\n")).unwrap();
    std::fs::set_permissions(&script, Permissions::from_mode(0o755)).unwrap();
    dir
}

fn plant_registry(root: &Path, entries: &[Value]) {
    let home = root.join("agents-home");
    std::fs::create_dir_all(&home).unwrap();
    let doc = json!({
        "schema_version": fno_agents::state::REGISTRY_SCHEMA_VERSION,
        "agents": entries,
    });
    std::fs::write(home.join("registry.json"), doc.to_string()).unwrap();
}

#[test]
fn a_positively_live_worker_flips_an_unheld_node_to_unknown() {
    let root = tempfile::tempdir().unwrap();
    seed_graph(root.path(), graph_entry(json!([open_session("sess-x1")])));
    let wt = fake_worktree(root.path(), "wt-join", "x-join1");
    let roster = roster_dir_with(
        root.path(),
        &[json!({
            "session_id": "sess-x1",
            "name": "bp-worker",
            "state": "working",
            "cwd": wt.display().to_string(),
        })],
    );
    let (code, stdout, stderr) = run_status(root.path(), &roster);
    assert_eq!(code, 0, "stderr: {stderr}");
    let info: Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(info["state"], "unknown");
    assert_eq!(info["worked_by"], json!(["bp-worker"]));
    assert_eq!(info["basis"], "live-worker");
}

#[test]
fn the_graph_join_resolves_an_unresolved_row() {
    let root = tempfile::tempdir().unwrap();
    seed_graph(root.path(), graph_entry(json!([open_session("s-1")])));
    let plain = |n: &str| plain_dir(root.path(), n).display().to_string();
    let roster = roster_dir_with(
        root.path(),
        &[
            json!({
                "session_id": "s-1",
                "name": "lead-a792-control",
                "state": "working",
                "cwd": plain("plain-a"),
            }),
            json!({
                "session_id": "s-o1",
                "name": "other-1",
                "state": "working",
                "cwd": plain("plain-b"),
            }),
            json!({
                "session_id": "s-o2",
                "name": "other-2",
                "state": "working",
                "cwd": plain("plain-c"),
            }),
        ],
    );
    let (code, stdout, stderr) = run_status(root.path(), &roster);
    assert_eq!(code, 0, "stderr: {stderr}");
    let info: Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(info["state"], "unknown");
    assert_eq!(info["worked_by"], json!(["lead-a792-control"]));
    assert_eq!(info["basis"], "live-worker-degraded-coverage");
}

#[test]
fn a_registry_only_probe_still_answers_the_join() {
    let root = tempfile::tempdir().unwrap();
    seed_graph(root.path(), graph_entry(json!([open_session("s-1")])));
    let wt_reg = plain_dir(root.path(), "wt-reg");
    plant_registry(
        root.path(),
        &[json!({
            "name": "reg-worker",
            "cwd": wt_reg.display().to_string(),
            "status": "busy",
            "harness": "claude",
            "harness_session_id": "s-1",
        })],
    );
    // No claude stub on PATH: the vendor read degrades to registry-only.
    let empty = plain_dir(root.path(), "no-vendor-bin");
    let (code, stdout, stderr) = run_status(root.path(), &empty);
    assert_eq!(code, 0, "stderr: {stderr}");
    let info: Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(info["roster_consulted"], true);
    assert!(info["roster_probe"]
        .as_str()
        .unwrap_or_default()
        .contains("registry-only"));
    assert_eq!(info["state"], "unknown");
    assert_eq!(info["worked_by"], json!(["reg-worker"]));
}

#[test]
fn a_closed_phase_row_never_renders_as_unmeasured_noise() {
    let root = tempfile::tempdir().unwrap();
    seed_graph(root.path(), graph_entry(json!([closed_session("sess-x1")])));
    let wt = fake_worktree(root.path(), "wt-join", "x-join1");
    let roster = roster_dir_with(
        root.path(),
        &[json!({
            "session_id": "sess-x1",
            "name": "bp-worker",
            "state": "working",
            "cwd": wt.display().to_string(),
        })],
    );
    let (code, stdout, stderr) = run_status(root.path(), &roster);
    assert_eq!(code, 0, "stderr: {stderr}");
    assert_eq!(info_field(&stdout, "roster_consulted"), "true");
    let info: Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(info["roster_workers"], json!([]));
    assert!(info.get("worked_by").is_none());
    let combined = format!("{stdout}{stderr}");
    assert!(!combined.contains("unmeasured, never live"));
}

#[test]
fn unrelated_unresolved_rows_still_answer_free() {
    let root = tempfile::tempdir().unwrap();
    seed_graph(root.path(), graph_entry(json!([open_session("sess-mine")])));
    let plain = plain_dir(root.path(), "plain-b");
    let roster = roster_dir_with(
        root.path(),
        &[json!({
            "session_id": "other-session",
            "name": "other-worker",
            "state": "working",
            "cwd": plain.display().to_string(),
        })],
    );
    let (code, stdout, stderr) = run_status(root.path(), &roster);
    assert_eq!(code, 0, "stderr: {stderr}");
    let info: Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(info["state"], "free");
    assert_eq!(info["roster_coverage"], "degraded");
}

// The human-readable leaf prints pretty JSON on stdout with no verdict
// fields inside it; read one field without guessing the exact layout.
fn info_field(stdout: &str, key: &str) -> String {
    let info: Value = serde_json::from_str(stdout).unwrap();
    info[key].to_string()
}
