//! parity-stage: characterization
//! parity-oracle: fno.agent.cli.whoami_command
//!
//! Characterization for the whoami family, frozen against goldens captured
//! from the Python render (`cli/src/fno/agent/cli.py`, deleted in the same
//! change) through tests/whoami_driver.py before that leg died. The case
//! table lives in the driver; this file asks it for the table (`--list`),
//! the fixtures (`--fixture`), and, under FNO_CAPTURE_GOLDEN=1 only, the
//! old leg's bytes (`--case`) so a re-capture asserts Rust==old at freeze
//! time. The old leg is gone now, so capture mode refuses (a golden can
//! only be captured while the oracle leg runs).
//!
//! Two folds apply on both sides of a comparison. The fixture root folds to
//! `<FIXTURE>` in all its spellings (raw, canonicalized, macOS's /private
//! prefix). The malformed-state refusal's parser detail folds to `<REASON>`:
//! that text is the YAML layer's wording, never a contract. Everything else
//! is byte-exact, including the `False`/`True` gate values and the rc=4
//! ambiguity refusal.

use common::{assert_golden, capture_mode, Golden};
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::Command;

mod common;

/// Repo root: crates/fno-agents -> repo.
fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
}

/// (name, argv, env stamps) per case, straight from the driver's table.
struct Case {
    name: String,
    argv: Vec<String>,
    env: Vec<(String, String)>,
}

fn cases() -> Vec<Case> {
    let interpreter = std::env::var("FNO_PYTHON").unwrap_or_else(|_| "python3".to_string());
    let out = Command::new(interpreter)
        .arg(repo_root().join("crates/fno-agents/tests/whoami_driver.py"))
        .arg("--list")
        .output()
        .expect("the case table comes from the driver");
    let rows: Value = serde_json::from_slice(&out.stdout).expect("the table is JSON");
    rows.as_array()
        .expect("the table is a list")
        .iter()
        .map(|row| Case {
            name: row["name"].as_str().expect("name").to_string(),
            argv: row["argv"]
                .as_array()
                .expect("argv")
                .iter()
                .map(|v| v.as_str().expect("argv words").to_string())
                .collect(),
            env: row["env"]
                .as_object()
                .expect("env")
                .iter()
                .map(|(k, v)| (k.clone(), v.as_str().expect("env values").to_string()))
                .collect(),
        })
        .collect()
}

/// Build the case's fixture through the driver, so the fixture bytes live
/// in one place for both legs.
fn build_fixture(case_name: &str, root: &Path) {
    let interpreter = std::env::var("FNO_PYTHON").unwrap_or_else(|_| "python3".to_string());
    let out = Command::new(interpreter)
        .arg(repo_root().join("crates/fno-agents/tests/whoami_driver.py"))
        .args(["--fixture", case_name])
        .arg(root)
        .output()
        .expect("the fixture builds through the driver");
    assert!(
        out.status.success(),
        "fixture build failed for {case_name}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Fold what the host owns: the fixture root in all its spellings, and the
/// malformed-state refusal's parser detail. The rule mirrors the capture
/// driver's normalize().
fn normalize(text: &str, root: &Path) -> String {
    let resolved = root
        .canonicalize()
        .unwrap_or_else(|_| root.to_path_buf())
        .to_string_lossy()
        .into_owned();
    let raw = root.to_string_lossy().into_owned();
    text.replace(&format!("/private{resolved}"), "<FIXTURE>")
        .replace(&resolved, "<FIXTURE>")
        .replace(&raw, "<FIXTURE>")
}

/// The malformed refusal keeps the path and folds the parser detail: the
/// wording belongs to the YAML layer, not the command.
fn fold_malformed_reason(text: &str) -> String {
    let marker = "error: malformed state file: ";
    let Some(start) = text.find(marker) else {
        return text.to_string();
    };
    let Some(anchor) = text[start..].find("<FIXTURE>").map(|i| i + start) else {
        return text.to_string();
    };
    let Some(tail) = text[anchor..].find(": ").map(|i| i + anchor) else {
        return text.to_string();
    };
    format!("{}: <REASON>\n", &text[..tail])
}

/// The scrubbed child env: only HOME, PATH and the case's stamps reach the
/// binary, so identity floors to absent except what a case stamps.
fn child_env(cmd: &mut Command, root: &Path, case: &Case) {
    cmd.env_clear();
    cmd.env("HOME", root.join("fake_home"));
    if let Some(path) = std::env::var_os("PATH") {
        cmd.env("PATH", path);
    }
    for (k, v) in &case.env {
        cmd.env(k, v);
    }
}

#[test]
fn whoami_matches_the_python_goldens() {
    let rows = cases();
    assert!(!rows.is_empty(), "the case table is non-empty");
    for case in &rows {
        let tmp = tempfile::TempDir::new().unwrap();
        let root = tmp.path();
        build_fixture(&case.name, root);
        // The door folds a root -J into the group's own spelling before the
        // exec, and the exec prefixes the group name, so the binary sees
        // `spawn-overlay whoami <argv after `fno whoami`>`.
        let rest: Vec<String> = if case.argv.first().map(String::as_str) == Some("-J") {
            vec!["--json".to_string()]
        } else {
            case.argv[1..].to_vec()
        };
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_fno-agents"));
        cmd.args(["spawn-overlay", "whoami"]);
        cmd.args(&rest);
        child_env(&mut cmd, root, case);
        cmd.current_dir(root.join("project"));
        let out = cmd.output().expect("the native binary runs");
        let exit = out.status.code();
        let mut stdout = normalize(&String::from_utf8_lossy(&out.stdout), root);
        let mut stderr = normalize(&String::from_utf8_lossy(&out.stderr), root);
        stdout = fold_malformed_reason(&stdout);
        stderr = fold_malformed_reason(&stderr);
        let rust = Golden {
            exit,
            streams: vec![stdout, stderr],
        };
        let oracle = if capture_mode() {
            run_oracle(root, case)
        } else {
            None
        };
        if capture_mode() && oracle.is_none() {
            continue;
        }
        assert_golden("whoami", &case.name, &rust, oracle);
    }
}

/// The old leg's bytes for one case, through the driver. None when the
/// interpreter is absent (capture skips, never fails).
fn run_oracle(root: &Path, case: &Case) -> Option<Golden> {
    let interpreter = std::env::var("FNO_PYTHON").unwrap_or_else(|_| "python3".to_string());
    let mut cmd = Command::new(interpreter);
    cmd.arg(repo_root().join("crates/fno-agents/tests/whoami_driver.py"))
        .args(["--case", &case.name])
        .arg(root)
        .env_clear();
    cmd.env("HOME", root.join("fake_home"));
    if let Some(path) = std::env::var_os("PATH") {
        cmd.env("PATH", path);
    }
    for (k, v) in &case.env {
        cmd.env(k, v);
    }
    let out = cmd.output().ok()?;
    Some(Golden {
        exit: out.status.code(),
        streams: vec![
            fold_malformed_reason(&normalize(&String::from_utf8_lossy(&out.stdout), root)),
            fold_malformed_reason(&normalize(&String::from_utf8_lossy(&out.stderr), root)),
        ],
    })
}
