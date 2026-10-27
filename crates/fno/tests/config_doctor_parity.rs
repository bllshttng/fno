//! parity-stage: characterization
//! parity-oracle: cli/src/fno/setup/config_doctor.py
//!
//! `fno config doctor`, frozen against goldens captured from the Python leg
//! (cli/src/fno/setup/doctor.py and config_doctor.py, deleted in the same
//! change) through `config_doctor_driver.py`. Each case rebuilds its fixture
//! through the driver's `--fixture` mode (the case table there is the one
//! source), runs the native door on it, and compares the normalized
//! (stdout, stderr, exit) triple with the golden.
//!
//! The run mirrors the capture: FNO_AGENTS_BIN names a missing file, PATH is
//! the system dirs, and the binary runs from a copy in an empty dir, so no
//! fno-agents sibling or dev build answers the route-slot, roster or review
//! reads. Fixtures live under the cargo tmp dir rather than the system temp
//! dir: a `/tmp` root trips the suspicious-path check on Linux, which the
//! macOS capture never saw. GIT_CEILING_DIRECTORIES keeps git from finding
//! the checkout around that dir, as the capture's temp root had none.
//!
//! Folds: the fixture root in its space-slug, resolved, raw and `/private`
//! spellings, and the TOML parser's own wording in a post-merge `cause`.

use std::path::{Path, PathBuf};
use std::process::Command;

const DRIVER: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/config_doctor_driver.py");
const GOLDEN_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/golden/config_doctor");

struct Case {
    name: String,
    argv: Vec<String>,
    env: Vec<(String, String)>,
}

/// The driver's case table, read through `--list`.
fn cases() -> Vec<Case> {
    let out = Command::new("python3")
        .arg(DRIVER)
        .arg("--list")
        .output()
        .expect("python3 must exist to read the parity case table");
    assert!(out.status.success(), "driver --list failed");
    let rows: serde_json::Value = serde_json::from_slice(&out.stdout).expect("case table is JSON");
    rows.as_array()
        .expect("case table is a list")
        .iter()
        .map(|row| Case {
            name: row["name"].as_str().unwrap_or_default().to_string(),
            argv: row["argv"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|v| v.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default(),
            env: row["env"]
                .as_object()
                .map(|m| {
                    m.iter()
                        .map(|(k, v)| (k.clone(), v.as_str().unwrap_or_default().to_string()))
                        .collect()
                })
                .unwrap_or_default(),
        })
        .collect()
}

fn normalize(text: &str, root: &Path) -> String {
    let resolved = root
        .canonicalize()
        .unwrap_or_else(|_| root.to_path_buf())
        .to_string_lossy()
        .to_string();
    let raw = root.to_string_lossy().to_string();
    let mut folded = text.to_string();
    for token in [resolved.replace('/', "-"), raw.replace('/', "-")] {
        folded = folded.replace(&token, "<FIXTURE_SLUG>");
    }
    for token in [format!("/private{resolved}"), resolved, raw] {
        folded = folded.replace(&token, "<FIXTURE>");
    }
    let cause = regex::Regex::new(r#""cause": "TOMLDecodeError: [^"]*""#).expect("valid pattern");
    cause
        .replace_all(&folded, "\"cause\": \"TOMLDecodeError: <PARSER>\"")
        .into_owned()
}

fn run_native(case: &Case, root: &Path) -> (String, String, i32) {
    let built = Command::new("python3")
        .arg(DRIVER)
        .args(["--fixture", &case.name])
        .arg(root)
        .output()
        .expect("python3 must exist to rebuild parity fixtures");
    assert!(
        built.status.success(),
        "fixture build failed for {}: {}",
        case.name,
        String::from_utf8_lossy(&built.stderr)
    );
    let bin_dir = root.join("bin");
    std::fs::create_dir_all(&bin_dir).unwrap();
    let copied = bin_dir.join("fno");
    std::fs::copy(env!("CARGO_BIN_EXE_fno"), &copied).unwrap();

    let home = root.join("fake_home");
    let mut command = Command::new(&copied);
    command
        .args(&case.argv)
        .current_dir(root.join("project"))
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("GIT_CEILING_DIRECTORIES", root)
        .env("HOME", &home)
        .env("FNO_GLOBAL_SETTINGS_PATH", home.join(".fno/settings.yaml"))
        .env("FNO_NO_CANONICAL_CONFIG", "1")
        .env("FNO_STATE_DIR", root.join("state"))
        .env("FNO_AGENTS_HOME", home.join(".fno/agents"))
        .env("FNO_AGENTS_BIN", "/nonexistent/fno-agents");
    for (key, value) in &case.env {
        command.env(key, value);
    }
    let out = command.output().expect("the fno binary must run");
    (
        normalize(&String::from_utf8_lossy(&out.stdout), root),
        normalize(&String::from_utf8_lossy(&out.stderr), root),
        out.status.code().unwrap_or(-1),
    )
}

fn golden(name: &str, suffix: &str, root: &Path) -> String {
    let text = std::fs::read_to_string(format!("{GOLDEN_DIR}/{name}.{suffix}"))
        .unwrap_or_else(|_| panic!("golden {name}.{suffix} missing"));
    normalize(&text, root)
}

/// A long root on purpose: the state-roots table pads its root column to 48,
/// and the capture's temp paths all overflowed it.
fn case_root(name: &str) -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!(
        "config-doctor-parity-fixture-root-{}-{name}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn config_doctor_matches_the_python_goldens() {
    for case in cases() {
        let root = case_root(&case.name);
        let (out, err, exit) = run_native(&case, &root);
        let golden_exit: i32 = golden(&case.name, "exit", &root)
            .trim()
            .parse()
            .expect("golden exit parses");
        assert_eq!(exit, golden_exit, "{}: exit code drifted", case.name);
        assert_eq!(
            out,
            golden(&case.name, "out", &root),
            "{}: stdout drifted",
            case.name
        );
        assert_eq!(
            err,
            golden(&case.name, "err", &root),
            "{}: stderr drifted",
            case.name
        );
        let _ = std::fs::remove_dir_all(&root);
    }
}
