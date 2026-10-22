//! parity-stage: characterization
//! parity-oracle: fno.adapters.providers.cli.show_provider
//!
//! Characterization for `fno-agents accounts show`, the port of the Python
//! `fno config accounts show` verb, which is gone. Each
//! `tests/golden/accounts_show/*.json` case holds an accounts config, the
//! verb's args, and the exit code, stdout and stderr the Python verb printed
//! before deletion. This test runs the binary over the same layout and
//! asserts the same three.

use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::process::Command;

fn golden_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/golden/accounts_show")
}

fn run_case(case: &Value, tmp: &Path) -> Value {
    let home = tmp.join("home");
    let repo = tmp.join("repo");
    for dir in [
        home.join(".fno"),
        home.join(".claude"),
        repo.clone(),
        tmp.join("creds"),
        tmp.join("alt"),
        tmp.join("state/providers"),
    ] {
        std::fs::create_dir_all(dir).unwrap();
    }
    let config = case["config"].as_str().unwrap();
    std::fs::write(
        home.join(".fno/config.toml"),
        config.replace("{TMP}", tmp.to_str().unwrap()),
    )
    .unwrap();
    std::fs::write(tmp.join("alt/.credentials.json"), "{}").unwrap();
    std::fs::write(tmp.join("state/providers/.active-claude"), "makers").unwrap();
    let args: Vec<&str> = case["args"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a.as_str().unwrap())
        .collect();
    let out = Command::new(env!("CARGO_BIN_EXE_fno-agents"))
        .arg("accounts")
        .arg("show")
        .args(&args)
        .current_dir(&repo)
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", &home)
        .env("PWD", &repo)
        .env("FNO_REPO_ROOT", &repo)
        .env("FNO_STATE_DIR", tmp.join("state"))
        .env("USER", "tester")
        .env("SHOW_TOKEN", "tok")
        .output()
        .expect("fno-agents starts");
    json!({
        "exit": out.status.code(),
        "stdout": String::from_utf8_lossy(&out.stdout),
        "stderr": String::from_utf8_lossy(&out.stderr),
    })
}

#[test]
fn accounts_show_matches_the_python_verb() {
    let mut cases: Vec<PathBuf> = std::fs::read_dir(golden_dir())
        .unwrap()
        .filter_map(|entry| entry.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|e| e == "json"))
        .collect();
    cases.sort();
    assert!(cases.len() >= 10, "golden cases missing: {}", cases.len());
    let mut failures = Vec::new();
    for path in &cases {
        let case: Value = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(tmp.path()).unwrap();
        let got = run_case(&case, &root);
        let text = serde_json::to_string(&got)
            .unwrap()
            .replace(root.to_str().unwrap(), "<TMP>");
        let got: Value = serde_json::from_str(&text).unwrap();
        if got != case["expect"] {
            failures.push(format!(
                "{}\n  expect: {}\n  rust:   {}",
                path.file_name().unwrap().to_string_lossy(),
                case["expect"],
                got
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "{} case(s) differ:\n{}",
        failures.len(),
        failures.join("\n")
    );
}
