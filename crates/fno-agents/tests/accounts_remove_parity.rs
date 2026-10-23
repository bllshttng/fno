//! parity-stage: characterization
//! parity-oracle: fno.adapters.providers.cli.remove_provider
//!
//! Characterization for `fno-agents spawn-overlay accounts remove`, the port
//! of the Python `fno config accounts remove` body, which is gone. Each
//! `tests/golden/accounts_remove/*.json` case holds the global config (as
//! config.toml or a settings.yaml sibling) and the project config before the
//! run, the command's args, and what the Python command left: exit code,
//! stdout, stderr, and each scope's config.toml parsed after the write. The
//! files compare parsed, because the Rust writer keeps comments the Python
//! writer dropped.

use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::process::Command;

fn golden_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/golden/accounts_remove")
}

fn parsed(path: &Path) -> Value {
    match std::fs::read_to_string(path) {
        Ok(text) => toml::from_str(&text).expect("the written config parses"),
        Err(_) => Value::Null,
    }
}

fn run_case(case: &Value, tmp: &Path) -> Value {
    let home = tmp.join("home");
    let repo = tmp.join("repo");
    for dir in [home.join(".fno"), repo.join(".fno"), tmp.join("creds")] {
        std::fs::create_dir_all(dir).unwrap();
    }
    let fill = |s: &str| s.replace("{TMP}", tmp.to_str().unwrap());
    for (key, path) in [
        ("global", home.join(".fno/config.toml")),
        ("yaml", home.join(".fno/settings.yaml")),
        ("project", repo.join(".fno/config.toml")),
    ] {
        if let Some(text) = case[key].as_str() {
            std::fs::write(path, fill(text)).unwrap();
        }
    }
    let args: Vec<String> = case["args"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| fill(a.as_str().unwrap()))
        .collect();
    let out = Command::new(env!("CARGO_BIN_EXE_fno-agents"))
        .args(["spawn-overlay", "accounts", "remove"])
        .args(&args)
        .current_dir(&repo)
        .env_clear()
        .envs(fno_agents::test_run::self_owner_env())
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", &home)
        .env("PWD", &repo)
        .env("FNO_REPO_ROOT", &repo)
        .env("FNO_STATE_DIR", tmp.join("state"))
        .env("USER", "tester")
        .output()
        .expect("fno-agents starts");
    json!({
        "exit": out.status.code(),
        "stdout": String::from_utf8_lossy(&out.stdout),
        "stderr": String::from_utf8_lossy(&out.stderr),
        "global": parsed(&home.join(".fno/config.toml")),
        "project": parsed(&repo.join(".fno/config.toml")),
    })
}

#[test]
fn accounts_remove_matches_the_python_command() {
    let mut cases: Vec<PathBuf> = std::fs::read_dir(golden_dir())
        .unwrap()
        .filter_map(|entry| entry.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|e| e == "json"))
        .collect();
    cases.sort();
    assert!(cases.len() >= 6, "golden cases missing: {}", cases.len());
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
