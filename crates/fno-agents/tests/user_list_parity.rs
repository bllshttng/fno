//! parity-stage: characterization
//! parity-oracle: fno.inbox.operator_turns.cmd_list (the deleted Python
//! body, goldens captured from it through `fno inbox user list` before the
//! forward swap)
//!
//! Characterization tests for `compaction operator-turns --render list`, the
//! native answer behind `fno inbox user list`. The contract is the Python
//! verb's stdout, stderr and exit code: one tab-separated row per pending
//! turn with the `[stand-down]` marker, the empty-queue line, the stderr skip
//! report, `--limit`, and the `--json` array (escaped the way `json.dumps`
//! writes it). The fixture rows carry no timestamp, so every age reads
//! `age-unknown` and stays out of the clock.

use common::{assert_golden, Golden};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

mod common;

fn tmp_dir(tag: &str) -> PathBuf {
    let mut p = std::env::temp_dir();
    p.push(format!(
        "fno-agents-user-list-parity-{}-{tag}",
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

/// Run the list render with the env cleared to the operator pins.
fn list(dir: &Path, flags: &[&str]) -> Golden {
    let transcript = dir.join("transcript.jsonl");
    let capture = dir.join("capture");
    let _ = fs::remove_file(capture.join("s-golden.scan.json"));
    let out = Command::new(env!("CARGO_BIN_EXE_fno-agents"))
        .env_clear()
        .env("PATH", std::env::var("PATH").unwrap_or_default())
        .env("HOME", dir)
        .env("FNO_AGENTS_HOME", dir.join("agents-home"))
        .env("FNO_TEST_HERMETIC", "1")
        .envs(fno_agents::test_run::self_owner_env())
        .env("FNO_OPERATOR_SESSION_ID", "s-golden")
        .env("FNO_OPERATOR_HARNESS", "claude")
        .env("FNO_OPERATOR_TRANSCRIPT", &transcript)
        .env("FNO_OPERATOR_CAPTURE_DIR", &capture)
        .current_dir(dir)
        .args(["compaction", "operator-turns", "--render", "list"])
        .args(flags)
        .output()
        .unwrap();
    Golden {
        exit: out.status.code(),
        streams: vec![
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        ],
    }
}

#[test]
fn list_matches_the_python_verb() {
    let assistant = r#"{"type": "assistant", "message": {"role": "assistant", "content": "hi"}}"#;
    let fixtures: [(&str, Vec<String>); 3] = [
        ("empty", vec![assistant.to_string()]),
        (
            "turns",
            vec![
                row("u-1", "Perhaps our lead has overstayed its welcome"),
                row("u-2", "widen the review gate"),
            ],
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
        ),
    ];
    for (label, rows) in fixtures {
        let dir = tmp_dir(label);
        fs::write(dir.join("transcript.jsonl"), rows.join("\n") + "\n").unwrap();
        fs::create_dir_all(dir.join("capture")).unwrap();
        for (mode, flags) in [("text", &[][..]), ("json", &["--json"][..])] {
            assert_golden(
                "user_list",
                &format!("{label}_{mode}"),
                &list(&dir, flags),
                None,
            );
        }
        if label == "turns" {
            assert_golden(
                "user_list",
                "limit_text",
                &list(&dir, &["--limit", "1"]),
                None,
            );
            assert_golden(
                "user_list",
                "limit_json",
                &list(&dir, &["-L", "1", "-J"]),
                None,
            );
        }
    }
}
