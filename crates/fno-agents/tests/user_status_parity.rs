//! parity-stage: characterization
//! parity-oracle: fno.inbox.operator_turns.cmd_status (the deleted Python
//! body, goldens captured from it through `fno inbox user status` before the
//! forward swap)
//!
//! Characterization tests for `compaction operator-turns --render status`,
//! the native answer behind `fno inbox user status`. The contract is the
//! Python command's stdout, stderr and exit code: the depth line and the oldest
//! excerpt, the stderr skip report, the `--json` payload without its turns
//! (non-ASCII escaped the way `json.dumps` writes it), and the refusal when
//! no session identity resolves. The fixture rows carry no timestamp, so the
//! age stays out of the frozen bytes.

use common::{assert_golden, Golden};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

mod common;

fn tmp_dir(tag: &str) -> PathBuf {
    let mut p = std::env::temp_dir();
    p.push(format!(
        "fno-agents-user-status-parity-{}-{tag}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&p);
    fs::create_dir_all(&p).unwrap();
    p
}

fn row(uuid: &str, text: &str) -> String {
    serde_json::json!({"type": "user", "uuid": uuid,
                       "message": {"role": "user", "content": text}})
    .to_string()
}

/// Run the status render with the env cleared to the pins the case names.
fn status(dir: &Path, pins: &[(&str, String)], json: bool) -> Golden {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_fno-agents"));
    cmd.env_clear()
        .env("PATH", std::env::var("PATH").unwrap_or_default())
        .env("HOME", dir)
        .env("FNO_AGENTS_HOME", dir.join("agents-home"))
        .env("FNO_TEST_HERMETIC", "1")
        .envs(fno_agents::test_run::self_owner_env())
        .current_dir(dir)
        .args(["compaction", "operator-turns", "--render", "status"]);
    if json {
        cmd.arg("--json");
    }
    for (key, value) in pins {
        cmd.env(key, value);
    }
    let out = cmd.output().unwrap();
    Golden {
        exit: out.status.code(),
        streams: vec![
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        ],
    }
}

#[test]
fn status_matches_the_python_command() {
    let assistant = r#"{"type": "assistant", "message": {"role": "assistant", "content": "hi"}}"#;
    let cases: [(&str, Vec<String>, Option<&str>); 5] = [
        ("empty", vec![assistant.to_string()], None),
        (
            "oldest",
            vec![row("u-1", "first ask"), row("u-2", "second ask")],
            None,
        ),
        (
            "skipped",
            vec![
                row(
                    "u-tn",
                    "<task-notification><task-id>t9</task-id></task-notification>",
                ),
                row("u-real", "status on your nodes?"),
            ],
            None,
        ),
        (
            "acked",
            vec![row("u-acked", "already handled")],
            Some("u-acked"),
        ),
        (
            "unicode",
            vec![row("u-uni", "caf\u{e9} ask \u{2192} done")],
            None,
        ),
    ];
    for (label, rows, acked) in cases {
        let dir = tmp_dir(label);
        let transcript = dir.join("transcript.jsonl");
        fs::write(&transcript, rows.join("\n") + "\n").unwrap();
        let capture = dir.join("capture");
        fs::create_dir_all(&capture).unwrap();
        if let Some(turn) = acked {
            fs::write(
                capture.join("s-golden.jsonl"),
                format!("{{\"turn_id\": \"{turn}\"}}\n"),
            )
            .unwrap();
        }
        let pins = [
            ("FNO_OPERATOR_SESSION_ID", "s-golden".to_string()),
            ("FNO_OPERATOR_HARNESS", "claude".to_string()),
            ("FNO_OPERATOR_TRANSCRIPT", transcript.display().to_string()),
            ("FNO_OPERATOR_CAPTURE_DIR", capture.display().to_string()),
        ];
        for (mode, json) in [("text", false), ("json", true)] {
            let _ = fs::remove_file(capture.join("s-golden.scan.json"));
            let rust = status(&dir, &pins, json);
            assert_golden("user_status", &format!("{label}_{mode}"), &rust, None);
        }
    }

    // AC2-ERR: no pin and no ambient identity refuses by name, never depth 0.
    let dir = tmp_dir("no-identity");
    let rust = status(&dir, &[], false);
    assert_golden("user_status", "no_identity", &rust, None);
}
