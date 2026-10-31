//! parity-stage: characterization
//! parity-oracle: fno.agents.peek.peek
//!
//! The peek command, frozen against goldens captured from its Python leg
//! before that leg was deleted. Each case
//! copies the fixture world (registry, one claude transcript, the shared
//! events file) to a temp root and compares exit code, stdout and stderr
//! byte for byte.

use std::path::{Path, PathBuf};
use std::process::Command;

fn golden_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/golden/peek")
}

fn copy_tree(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap().flatten() {
        let dest = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_tree(&entry.path(), &dest);
        } else {
            std::fs::copy(entry.path(), dest).unwrap();
        }
    }
}

#[test]
fn every_case_matches_its_python_golden() {
    let text = std::fs::read_to_string(golden_dir().join("goldens.json")).unwrap();
    let goldens: serde_json::Value = serde_json::from_str(&text).unwrap();
    let cases = goldens.as_object().expect("cases");
    assert!(cases.len() >= 10, "the golden set is present");
    for (case, golden) in cases {
        let root = std::env::temp_dir().join(format!("peek-parity-{}-{case}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        copy_tree(&golden_dir().join("world"), &root);
        for dir in ["sessions", "codex", "opencode"] {
            std::fs::create_dir_all(root.join(dir)).unwrap();
        }
        let argv: Vec<String> = golden["argv"]
            .as_array()
            .unwrap()
            .iter()
            .map(|a| a.as_str().unwrap().to_string())
            .collect();
        let out = Command::new(env!("CARGO_BIN_EXE_fno-agents"))
            .args(["status", "--peek"])
            .args(&argv)
            .current_dir(&root)
            .envs(fno_agents::test_run::self_owner_env())
            .env("HOME", &root)
            .env("FNO_AGENTS_HOME", root.join("agents"))
            .env("FNO_HOME", root.join("state"))
            .env("FNO_STATE_DIR", root.join("state"))
            .env("FNO_CLAUDE_PROJECTS_DIR", root.join(".claude/projects"))
            .env("FNO_CODEX_SESSIONS_DIR", root.join("codex"))
            .env("OPENCODE_DB", root.join("opencode/opencode.db"))
            .env_remove("CLAUDE_CONFIG_DIR")
            .output()
            .unwrap();
        assert_eq!(
            out.status.code().map(i64::from),
            golden["exit"].as_i64(),
            "{case}: exit code; stderr {}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert_eq!(
            String::from_utf8_lossy(&out.stdout),
            golden["out"].as_str().unwrap(),
            "{case}: stdout"
        );
        assert_eq!(
            String::from_utf8_lossy(&out.stderr),
            golden["err"].as_str().unwrap(),
            "{case}: stderr"
        );
        let _ = std::fs::remove_dir_all(&root);
    }
}
