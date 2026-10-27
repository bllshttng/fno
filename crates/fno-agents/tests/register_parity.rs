//! parity-stage: differential
//! parity-oracle: fno.agents.registry.register_existing_session
//!
//! The `fno agents register` command, pinned against goldens captured
//! from the Python leg (tests/golden/register/capture.py). The oracle leg is
//! deliberately LIVE: the SessionStart pane restamp still calls
//! register_existing_session, so the provenance gate reads this file as a
//! differential pin and asserts the oracle resolves. When a later wave
//! deletes the leg, this stage flips to characterization in the same PR.

use serde_json::{json, Value};
use std::path::PathBuf;
use std::process::Command;

const SESSION_ID: &str = "3f2b8c1a-9d4e-4f6a-b7c8-2e5d1a0b9c8d";
const OTHER_ID: &str = "7c1d4e9b-2a6f-4b8c-9d0e-5f1a2b3c4d5e";

fn golden_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/golden/register")
}

/// Run the register command with the fixture env. The identity markers are
/// stripped first so each case names exactly the identity it wants seen.
fn run_register(
    home: &PathBuf,
    root: &PathBuf,
    env: &[(&str, &str)],
    args: &[&str],
) -> (i32, String, String) {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_fno-agents"));
    cmd.env("FNO_AGENTS_HOME", home)
        .env("FNO_HOME", root.join("state"))
        .env("FNO_STATE_DIR", root.join("state"))
        .env(
            "FNO_CLAUDE_PROJECTS_DIR",
            root.join("empty-claude-projects"),
        )
        .env("FNO_CODEX_SESSIONS_DIR", root.join("empty-codex-sessions"))
        .env(
            "FNO_OPENCODE_STORAGE_DIR",
            root.join("empty-opencode-storage"),
        )
        .env("FNO_MUX_DIR", root.join("empty-mux"))
        .envs(fno_agents::test_run::self_owner_env())
        .current_dir(root);
    for (key, value) in env {
        cmd.env(key, value);
    }
    for key in [
        "CLAUDE_CODE_SESSION_ID",
        "CODEX_THREAD_ID",
        "FNO_AGENT_SELF",
        "FNO_AGENT_ROW_PENDING",
    ] {
        cmd.env_remove(key);
    }
    cmd.args(["status", "--register"]);
    for a in args {
        cmd.arg(a);
    }
    let out = cmd.output().expect("register ran");
    (
        out.status.code().unwrap_or(-1),
        scrub(&out.stdout, root),
        scrub(&out.stderr, root),
    )
}

fn scrub(bytes: &[u8], root: &PathBuf) -> String {
    String::from_utf8_lossy(bytes)
        .replace(root.to_string_lossy().as_ref(), "<ROOT>")
        .replace(
            root.canonicalize()
                .unwrap_or_else(|_| root.clone())
                .to_string_lossy()
                .as_ref(),
            "<ROOT>",
        )
}

fn seed_registry(home: &PathBuf, row: Value) {
    std::fs::create_dir_all(home).unwrap();
    let doc = json!({"schema_version": 43, "agents": [row]});
    std::fs::write(home.join("registry.json"), doc.to_string()).unwrap();
}

fn otter(name: &str, session: &str, status: &str, origin: &str) -> Value {
    json!({
        "name": name,
        "cwd": "",
        "log_path": "",
        "harness": "claude",
        "harness_session_id": session,
        "status": status,
        "origin": origin,
    })
}

fn assert_golden(label: &str, exit: i32, out: &str, err: &str) {
    let dir = golden_dir();
    let exit_text = std::fs::read_to_string(dir.join(format!("{label}.exit"))).unwrap();
    let expected_exit: i32 = exit_text.trim().parse().unwrap();
    let expected_out =
        std::fs::read_to_string(dir.join(format!("{label}.out"))).unwrap_or_default();
    let expected_err =
        std::fs::read_to_string(dir.join(format!("{label}.err"))).unwrap_or_default();
    assert_eq!(exit, expected_exit, "{label}: exit code");
    assert_eq!(out, expected_out, "{label}: stdout bytes");
    assert_eq!(err, expected_err, "{label}: stderr bytes");
}

fn read_rows(home: &PathBuf) -> Vec<Value> {
    let text = std::fs::read_to_string(home.join("registry.json")).unwrap();
    let parsed: Value = serde_json::from_str(&text).unwrap();
    parsed
        .get("agents")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}

#[test]
fn register_matches_python_goldens() {
    // no-identity: no marker in the env, nothing addressable to register.
    let dir = tempfile::TempDir::new().unwrap();
    let root = dir.path().to_path_buf();
    let (exit, out, err) = run_register(&root.join("home"), &root, &[], &[]);
    assert_golden("no-identity", exit, &out, &err);

    // bad-policy: the flag validates before identity resolution.
    let dir = tempfile::TempDir::new().unwrap();
    let root = dir.path().to_path_buf();
    let (exit, out, err) = run_register(
        &root.join("home"),
        &root,
        &[("CLAUDE_CODE_SESSION_ID", SESSION_ID)],
        &["--delivery-policy", "always"],
    );
    assert_golden("bad-policy", exit, &out, &err);

    // mint: fresh home, one claude marker; JSON and plain forms agree.
    for label in ["mint-json", "mint-plain"] {
        let dir = tempfile::TempDir::new().unwrap();
        let root = dir.path().to_path_buf();
        let args: Vec<&str> = if label == "mint-json" {
            vec!["--json"]
        } else {
            vec![]
        };
        let (exit, out, err) = run_register(
            &root.join("home"),
            &root,
            &[("CLAUDE_CODE_SESSION_ID", SESSION_ID)],
            &args,
        );
        assert_golden(label, exit, &out, &err);
    }

    // refresh: the same session id registered twice into one home. The
    // second run refreshes in place (same name), never a duplicate row.
    let dir = tempfile::TempDir::new().unwrap();
    let root = dir.path().to_path_buf();
    let home = root.join("home-refresh");
    let env = [("CLAUDE_CODE_SESSION_ID", SESSION_ID)];
    let (exit, out, err) = run_register(&home, &root, &env, &["--json"]);
    assert_golden("refresh-first", exit, &out, &err);
    let (exit, out, err) = run_register(&home, &root, &env, &["--json"]);
    assert_golden("refresh", exit, &out, &err);
    let rows = read_rows(&home);
    assert_eq!(rows.len(), 1, "refresh mints no duplicate row");

    // bus-only: the clock writes ride the same binary's mail-hold door.
    let dir = tempfile::TempDir::new().unwrap();
    let root = dir.path().to_path_buf();
    let (exit, out, err) = run_register(
        &root.join("home-bus"),
        &root,
        &[("CLAUDE_CODE_SESSION_ID", SESSION_ID)],
        &["--json", "--delivery-policy", "bus-only"],
    );
    assert_golden("bus-only", exit, &out, &err);

    // policy-off: the seeded bus-only row flips back to injectable.
    let dir = tempfile::TempDir::new().unwrap();
    let root = dir.path().to_path_buf();
    let home = root.join("home-off");
    let mut row = otter("calm-otter", SESSION_ID, "idle", "operator");
    row["delivery_policy"] = json!("bus-only");
    seed_registry(&home, row);
    let (exit, out, err) = run_register(
        &home,
        &root,
        &[("CLAUDE_CODE_SESSION_ID", SESSION_ID)],
        &["--json", "--delivery-policy", "off"],
    );
    assert_golden("policy-off", exit, &out, &err);

    // self-refusal: FNO_AGENT_SELF naming an existing row refuses the mint.
    let dir = tempfile::TempDir::new().unwrap();
    let root = dir.path().to_path_buf();
    let home = root.join("home-self");
    seed_registry(&home, otter("busy-otter", OTHER_ID, "live", "spawn"));
    let (exit, out, err) = run_register(
        &home,
        &root,
        &[
            ("CLAUDE_CODE_SESSION_ID", SESSION_ID),
            ("FNO_AGENT_SELF", "busy-otter"),
        ],
        &["--json"],
    );
    assert_golden("self-refusal", exit, &out, &err);

    // pending-window: the spawner's row-pending marker refuses the same way,
    // before the row it names exists.
    let dir = tempfile::TempDir::new().unwrap();
    let root = dir.path().to_path_buf();
    let (exit, out, err) = run_register(
        &root.join("home-pending"),
        &root,
        &[
            ("CLAUDE_CODE_SESSION_ID", SESSION_ID),
            ("FNO_AGENT_SELF", "t-pending-worker"),
            ("FNO_AGENT_ROW_PENDING", "t-pending-worker"),
        ],
        &["--json"],
    );
    assert_golden("pending-window", exit, &out, &err);

    // origin-note: a refresh never rewrites a birth fact; it says so.
    let dir = tempfile::TempDir::new().unwrap();
    let root = dir.path().to_path_buf();
    let home = root.join("home-origin");
    seed_registry(&home, otter("swift-otter", SESSION_ID, "idle", "spawn"));
    let (exit, out, err) = run_register(
        &home,
        &root,
        &[("CLAUDE_CODE_SESSION_ID", SESSION_ID)],
        &["--json"],
    );
    assert_golden("origin-note", exit, &out, &err);
}

#[test]
fn minted_row_shape_matches_the_python_contract() {
    let dir = tempfile::TempDir::new().unwrap();
    let root = dir.path().to_path_buf();
    let home = root.join("home");
    let (exit, _out, _err) = run_register(
        &home,
        &root,
        &[("CLAUDE_CODE_SESSION_ID", SESSION_ID)],
        &["--json"],
    );
    assert_eq!(exit, 0);
    let rows = read_rows(&home);
    assert_eq!(rows.len(), 1, "one row minted");
    let row = &rows[0];
    assert_eq!(row["name"], json!("3f2b8c1a"));
    assert_eq!(row["harness"], json!("claude"));
    assert_eq!(row["harness_session_id"], json!(SESSION_ID));
    assert_eq!(row["short_id"], json!("3f2b8c1a"));
    assert_eq!(row["status"], json!("idle"));
    assert_eq!(row["origin"], json!("operator"));
    assert_eq!(row.get("delivery_policy"), None);
    assert_eq!(row["cwd"], json!(root.to_string_lossy().as_ref()));
}
