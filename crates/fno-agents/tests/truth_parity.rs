//! parity-stage: characterization
//! parity-oracle: fno.agents.cli._truth_line
//!
//! The truth CLI's human line and `--json` bytes, frozen against goldens
//! captured from the Python leg (tests/golden/truth/capture.py) before it was
//! deleted. Each golden renders one session_truth fixture payload with no
//! falsifier, or with each falsifier and with and without a session id. The
//! reader itself is held by session_truth_parity.rs.

use fno_agents::truth_cli::{cli_payload, truth_line};
use fno_agents::wake_read::python_json_default;
use serde_json::Value;
use std::path::PathBuf;

fn read(path: PathBuf) -> Value {
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    serde_json::from_str(&text).expect("golden parses")
}

#[test]
fn every_render_matches_its_python_golden() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests");
    let payloads = read(root.join("fixtures/session_truth/goldens.json"));
    let goldens = read(root.join("golden/truth/goldens.json"));
    let goldens = goldens.as_object().expect("goldens object");
    assert!(goldens.len() >= 40, "the golden set is present");
    for (key, golden) in goldens {
        let mut parts = key.split('|');
        let (case, falsifier, sid) = (
            parts.next().unwrap(),
            parts.next().unwrap(),
            parts.next().unwrap(),
        );
        let mut payload = payloads["cases"][case].clone();
        if sid == "nosid" {
            payload["session_id"] = Value::Null;
        }
        if falsifier != "-" {
            payload["reachability"] = "unreachable".into();
            payload["basis"] = falsifier.into();
        }
        assert_eq!(truth_line(&payload), golden["line"], "{key}: human line");
        assert_eq!(
            python_json_default(&cli_payload(&payload)),
            golden["json"],
            "{key}: --json bytes"
        );
    }
}

#[test]
fn the_single_form_exits_13_on_an_unknown_handle_and_the_batch_exits_0() {
    let home = std::env::temp_dir().join(format!("truth-cli-exit-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(&home).unwrap();
    let run = |argv: &[&str]| {
        std::process::Command::new(env!("CARGO_BIN_EXE_fno-agents"))
            .args(argv)
            .env("FNO_AGENTS_HOME", &home)
            .env("FNO_CLAUDE_PROJECTS_DIR", home.join("projects"))
            .env("FNO_CODEX_SESSIONS_DIR", home.join("codex"))
            .env("OPENCODE_DB", home.join("opencode.db"))
            .output()
            .unwrap()
    };
    let single = run(&["status", "--truth", "nobody-here", "--json"]);
    assert_eq!(single.status.code(), Some(13));
    let payload: Value = serde_json::from_slice(&single.stdout).unwrap();
    assert_eq!(payload["reason"], "not-found");

    let batch = run(&[
        "status",
        "--truth",
        "--handles",
        "nobody-here,nor-here",
        "--json",
    ]);
    assert_eq!(batch.status.code(), Some(0));
    let map: Value = serde_json::from_slice(&batch.stdout).unwrap();
    assert_eq!(map["nor-here"]["state"], "unknown");

    let both = run(&["status", "--truth", "a", "--handles", "b"]);
    assert_eq!(both.status.code(), Some(2));
    let _ = std::fs::remove_dir_all(&home);
}
