//! The done close path accepts `--note` beside the close flags. A forced
//! close of a plan-bound PR-less epic (every ship merged on a child) then
//! lands with completion evidence instead of refusing at the evidence seam,
//! and the other completion flags keep their own path.

use fno_agents::graph_store::{read_rows, seed_rows};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::process::Command;
use tempfile::TempDir;

const EPIC: &str = "ab-1e2d3c4b";

fn spawn_backlog(env: &[(String, String)], argv: &[String], cwd: &Path) -> (i32, String, String) {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_fno-agents"));
    cmd.arg("backlog").arg("done").args(argv);
    cmd.current_dir(cwd);
    cmd.env_clear();
    for (k, v) in fno_agents::test_run::self_owner_env() {
        cmd.env(k, v);
    }
    for (k, v) in env {
        cmd.env(k, v.clone());
    }
    let out = cmd.output().expect("binary runs");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// A fixture whose graph holds one plan-bound epic with no PR refs and no
/// completion facts: the specimen shape whose ships all merged on children.
fn fixture() -> (TempDir, PathBuf, Vec<(String, String)>) {
    let dir = TempDir::new().expect("tempdir");
    let fx = dir.path().join("fx");
    std::fs::create_dir_all(fx.join("plans")).expect("fixture dirs");
    std::fs::write(
        fx.join("config.toml"),
        format!("state_dir = \"{}\"\n", fx.display()),
    )
    .expect("config.toml");
    std::fs::write(
        fx.join("plans/epic.md"),
        "---\nclaims: ab-1e2d3c4b\ntitle: scratch epic\nstatus: ready\nkind: quick-plan\nexpected_url_count: 2\n---\n\n## Execution Strategy\n\nShip two PRs.\n",
    )
    .expect("plan");
    let epic = json!({
        "id": EPIC,
        "slug": "scratch-epic",
        "title": "scratch epic",
        "type": "feature",
        "status": "in_progress",
        "priority": "p2",
        "domain": "code",
        "plan_path": fx.join("plans/epic.md").to_string_lossy(),
    });
    seed_rows(&fx.join("graph.json"), &[epic]).expect("seed");
    let fx_str = fx.to_string_lossy().to_string();
    let env = vec![
        ("PATH".to_string(), "/usr/bin:/bin".to_string()),
        ("HOME".to_string(), fx_str.clone()),
        (
            "FNO_CONFIG".to_string(),
            fx.join("config.toml").to_string_lossy().to_string(),
        ),
        ("FNO_TRACKER_BACKEND".to_string(), "graph".to_string()),
        (
            "FNO_GLOBAL_SETTINGS_PATH".to_string(),
            "/dev/null".to_string(),
        ),
        ("FNO_SKIP_MIGRATION".to_string(), "1".to_string()),
    ];
    (dir, fx, env)
}

fn row_at(fx: &Path) -> Value {
    let rows = read_rows(&fx.join("graph.json")).expect("read rows");
    rows.into_iter()
        .find(|r| r["id"] == json!(EPIC))
        .expect("epic row survives")
}

#[test]
fn a_forced_close_with_a_note_carries_completion_evidence() {
    let (_dir, fx, env) = fixture();
    let (code, _, stderr) = spawn_backlog(
        &env,
        &[
            EPIC.to_string(),
            "--force".into(),
            "--reason".into(),
            "every ship merged on the children".into(),
            "--note".into(),
            "all ships merged on children".into(),
        ],
        &fx,
    );
    assert_eq!(code, 0, "close refused: {stderr}");
    let row = row_at(&fx);
    assert_eq!(row["status"], json!("done"), "{stderr}");
    assert!(
        row["completed_at"].as_str().is_some_and(|s| !s.is_empty()),
        "{stderr}"
    );
    assert_eq!(
        row["completion_note"],
        json!("all ships merged on children")
    );
}

#[test]
fn non_note_completion_flags_still_refuse_beside_close_flags() {
    let (_dir, fx, env) = fixture();
    let base = [
        EPIC.to_string(),
        "--force".to_string(),
        "--reason".to_string(),
        "r".to_string(),
    ];
    for flag in [
        vec!["--pr".to_string(), "7".to_string()],
        vec!["--link".to_string(), "https://example.test/a".to_string()],
        vec!["--backfill".to_string()],
        vec!["--force-overwrite".to_string()],
    ] {
        let argv: Vec<String> = base.iter().cloned().chain(flag.clone()).collect();
        let (code, _, stderr) = spawn_backlog(&env, &argv, &fx);
        assert_eq!(code, 2, "[{:?}] close flags must refuse: {stderr}", flag);
        assert!(
            stderr.contains("separate paths"),
            "[{:?}] must name the separation: {stderr}",
            flag
        );
        assert_eq!(row_at(&fx)["status"], json!("in_progress"));
    }
}

#[test]
fn note_alone_still_routes_the_completion_path_and_stays_open() {
    let (_dir, fx, env) = fixture();
    let (code, _, stderr) = spawn_backlog(
        &env,
        &[EPIC.to_string(), "--note".into(), "solo note".into()],
        &fx,
    );
    assert_ne!(
        code, 0,
        "the promise gate holds the plan-bound epic: {stderr}"
    );
    let row = row_at(&fx);
    assert_eq!(row["status"], json!("in_progress"));
    assert!(
        row["completion_note"].is_null(),
        "a refused close writes nothing"
    );
}
