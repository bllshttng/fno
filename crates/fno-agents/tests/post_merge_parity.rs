//! parity-stage: characterization
//! parity-oracle: fno.ledger_join.resolve_pr_sessions (deleted with its module)
//!
//! The goldens under tests/golden/post_merge/ were captured from the Python
//! legs by fixtures/post_merge/capture_driver.py while those legs lived. This
//! test rebuilds the identical fixture tree with `--build-only` (import-free,
//! no oracle run), drives the pr-watch subverbs through the production argv
//! door, and compares masked streams against the frozen goldens. A golden can
//! only be re-frozen by running the driver against the Python leg, never from
//! this binary.

use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::process::Command;

use serde_json::Value;

mod common;

const SUBJECT: &str = "pr-sessions";

/// Paths that vary per run mask to <CASE>, matching the capture driver's
/// mask(); the unreadable-ledger exception text is machine-specific, so it
/// masks the same way the freeze did.
fn mask(text: &str, case_dir: &PathBuf) -> String {
    let mut text = text.to_string();
    for spelling in [case_dir.to_string_lossy().to_string()] {
        text = text.replace(&spelling, "<CASE>");
    }
    let real = fs::canonicalize(case_dir)
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_default();
    if !real.is_empty() {
        text = text.replace(&real, "<CASE>");
    }
    text
}

fn golden(subject: &str, key: &str, ext: &str) -> Option<String> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/golden")
        .join(subject)
        .join(format!("{key}.{ext}"));
    fs::read_to_string(path).ok()
}

#[test]
fn pr_sessions_matches_python_goldens() {
    if common::capture_mode() {
        // The driver is the capture path; a golden can only be re-frozen
        // against the Python leg.
        return;
    }
    let root = tempfile::tempdir().expect("fixture tempdir");
    let driver = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/post_merge/capture_driver.py");
    let built = Command::new(common::python_executable())
        .arg(&driver)
        .arg("--build-only")
        .arg(root.path())
        .output()
        .expect("capture_driver --build-only runs");
    assert!(
        built.status.success(),
        "--build-only failed:\n{}",
        String::from_utf8_lossy(&built.stderr)
    );
    let start = built
        .stdout
        .iter()
        .position(|&b| b == b'{')
        .expect("--build-only prints a JSON map");
    let map: Value = serde_json::from_slice(&built.stdout[start..]).expect("build map parses");
    let cases: HashMap<String, Value> = map["cases"]
        .as_array()
        .expect("cases list")
        .iter()
        .map(|entry| {
            (
                entry["slug"].as_str().expect("case slug").to_string(),
                entry.clone(),
            )
        })
        .collect();
    assert!(!cases.is_empty(), "the corpus built no cases");

    for (slug, entry) in &cases {
        let case_dir = PathBuf::from(entry["dir"].as_str().expect("case dir"));
        let payload = fs::read_to_string(case_dir.join("payload.json")).expect("payload");
        let out = Command::new(env!("CARGO_BIN_EXE_fno-agents"))
            .args(["pr-watch", SUBJECT])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .and_then(|mut child| {
                use std::io::Write;
                child
                    .stdin
                    .as_mut()
                    .expect("stdin piped")
                    .write_all(payload.as_bytes())?;
                let out = child.wait_with_output()?;
                Ok(out)
            })
            .expect("pr-watch pr-sessions runs");
        assert_eq!(
            out.status.code(),
            Some(0),
            "[{slug}] exit: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let got = mask(&String::from_utf8_lossy(&out.stdout), &case_dir);
        let want = golden("pr-sessions", slug, "out")
            .unwrap_or_else(|| panic!("[{slug}] no frozen golden"));
        assert_eq!(got, want, "[{slug}] stdout");
    }
}
