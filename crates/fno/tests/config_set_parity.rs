//! parity-stage: characterization
//! parity-oracle: fno.config.writer.set_config_values
//!
//! The config-writer port (`fno config set`, `unset`, `rm`), frozen against
//! goldens captured from the Python writer through `config_set_driver.py
//! --capture` (the committed oracle; it loads the writer legs from git, so
//! a re-capture works while an ancestor of HEAD still holds them). The case
//! table lives once, in the driver: this test reads it through `--list`,
//! rebuilds each fixture through `--fixture`, runs the freshly built `fno`
//! on it with the env the capture ran under, and compares the normalized
//! (stdout, stderr, exit) triple plus the after-state `--state` reads: each
//! config file parsed, the claims left, the fake fno-agents door calls,
//! and the `config_write` receipts.
//!
//! Recorded deltas, each a fold here:
//! - toml_edit keeps comments and key order where tomli_w rewrote the
//!   file, so files compare parsed, never byte for byte.
//! - a malformed file's parse message is tomllib's wording on the Python
//!   leg and toml_edit's on the native one; the text after `is malformed: `
//!   folds.
//! - `FNO_CONFIG` now names the global file a set writes (it used to be
//!   ignored); no golden pins it, because the old leg wrote the wrong file.
//! - the legacy settings.yaml convert is ported, not refused (ruling
//!   d-f3485d21); `set_yaml_convert` freezes it.

use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::Command;

const DRIVER: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/config_set_driver.py");
const GOLDEN_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/golden/config_set");
const MALFORMED: &str = "is malformed: ";

fn driver(args: &[&str], root: Option<&Path>, env: &[(&str, String)]) -> std::process::Output {
    let mut command = Command::new("python3");
    command.arg(DRIVER).args(args);
    if let Some(root) = root {
        command.arg(root);
    }
    for (key, value) in env {
        command.env(key, value);
    }
    command
        .output()
        .expect("python3 must exist to drive the parity fixtures")
}

/// Fold what the host owns: the fixture root (resolved, raw, `/private`)
/// and the parse-error wording after `is malformed: `.
fn normalize(text: &str, root: &Path) -> String {
    let resolved = root
        .canonicalize()
        .unwrap_or_else(|_| root.to_path_buf())
        .to_string_lossy()
        .to_string();
    let mut folded = text.to_string();
    for token in [
        format!("/private{resolved}"),
        resolved,
        root.to_string_lossy().to_string(),
    ] {
        if !token.is_empty() {
            folded = folded.replace(&token, "<FIXTURE>");
        }
    }
    folded
        .lines()
        .map(|line| match line.find(MALFORMED) {
            Some(at) => format!("{}<PARSE-ERROR>", &line[..at + MALFORMED.len()]),
            None => line.to_string(),
        })
        .map(|line| line + "\n")
        .collect()
}

/// The env `scrubbed_env` in the driver floors every case to, then the
/// case's stamps.
fn case_env(root: &Path, stamps: &Value) -> Vec<(String, String)> {
    let at = |rel: &str| root.join(rel).to_string_lossy().to_string();
    let mut env = vec![
        (
            "PATH".to_string(),
            std::env::var("PATH").unwrap_or_default(),
        ),
        ("HOME".to_string(), at("fake_home")),
        (
            "FNO_GLOBAL_SETTINGS_PATH".to_string(),
            at("fake_home/.fno/settings.yaml"),
        ),
        ("FNO_NO_CANONICAL_CONFIG".to_string(), "1".to_string()),
        ("FNO_STATE_DIR".to_string(), at("state")),
        ("FNO_AGENTS_HOME".to_string(), at("fake_home/.fno/agents")),
        (
            "FNO_EVENTS_PATH".to_string(),
            at("state/project-events.jsonl"),
        ),
        ("FNO_AGENTS_BIN".to_string(), at("fake_bin/fno-agents")),
    ];
    for name in ["TMPDIR", "TEMPDIR"] {
        if let Some(value) = std::env::var_os(name) {
            env.push((name.to_string(), value.to_string_lossy().to_string()));
        }
    }
    for (key, value) in stamps.as_object().into_iter().flatten() {
        env.push((key.clone(), value.as_str().unwrap_or_default().to_string()));
    }
    env
}

fn golden(name: &str, suffix: &str) -> String {
    std::fs::read_to_string(format!("{GOLDEN_DIR}/{name}.{suffix}"))
        .unwrap_or_else(|_| panic!("golden {name}.{suffix} missing"))
}

fn unique_root(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "fno-config-set-parity-{}-{name}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("case root");
    dir
}

/// One case's divergences, empty when the native writer matches.
fn diff_case(case: &Value) -> Vec<String> {
    let name = case["name"].as_str().unwrap_or_default();
    let root = unique_root(name);
    let built = driver(&["--fixture", name], Some(&root), &[]);
    assert!(
        built.status.success(),
        "fixture build failed for {name}: {}",
        String::from_utf8_lossy(&built.stderr)
    );
    let argv: Vec<String> = case["argv"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|a| a.as_str().map(str::to_string))
        .collect();
    let out = Command::new(env!("CARGO_BIN_EXE_fno"))
        .args(&argv)
        .current_dir(root.join("project"))
        .env_clear()
        .envs(case_env(&root, &case["env"]))
        .output()
        .expect("the fno binary must run");
    let state = driver(
        &["--state"],
        Some(&root),
        &[("FNO_BIN", env!("CARGO_BIN_EXE_fno").to_string())],
    );
    let mut diffs = Vec::new();
    let got = [
        (
            "out",
            normalize(&String::from_utf8_lossy(&out.stdout), &root),
        ),
        (
            "err",
            normalize(&String::from_utf8_lossy(&out.stderr), &root),
        ),
        ("exit", format!("{}\n", out.status.code().unwrap_or(-1))),
    ];
    for (suffix, text) in got {
        let want = normalize(&golden(name, suffix), Path::new("/nonexistent-root"));
        if text != want {
            diffs.push(format!("{name}.{suffix}\n  want {want:?}\n  got  {text:?}"));
        }
    }
    let state_got: Value =
        serde_json::from_str(&normalize(&String::from_utf8_lossy(&state.stdout), &root))
            .unwrap_or(Value::Null);
    let state_want: Value =
        serde_json::from_str(&golden(name, "state.json")).expect("golden state parses");
    if state_got != state_want {
        diffs.push(format!(
            "{name}.state.json\n  want {state_want}\n  got  {state_got}"
        ));
    }
    let _ = std::fs::remove_dir_all(&root);
    diffs
}

#[test]
fn native_writer_answers_every_golden_case() {
    let listed = driver(&["--list"], None, &[]);
    let cases: Vec<Value> =
        serde_json::from_slice(&listed.stdout).expect("the driver lists its cases");
    assert!(
        cases.len() >= 50,
        "{} cases, the capture had 58",
        cases.len()
    );
    let diffs: Vec<String> = cases.iter().flat_map(diff_case).collect();
    assert!(
        diffs.is_empty(),
        "{} divergence(s):\n{}",
        diffs.len(),
        diffs.join("\n")
    );
}
