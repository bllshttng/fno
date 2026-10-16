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

fn golden(key: &str, ext: &str) -> Option<String> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/golden/post_merge")
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
        let want = golden(slug, "out").unwrap_or_else(|| panic!("[{slug}] no frozen golden"));
        assert_eq!(got, want, "[{slug}] stdout");
    }
}

/// The mint subject: run the `pr-watch merge-cleanup-mint` arm per case and
/// compare the journal row's `data` against the frozen Python mint. `ts` and
/// the now-stamped `merged_at` mask to `<STAMP>`; keys re-sort alphabetically
/// to match the Python capture's `sort_keys` golden.
#[test]
fn mint_matches_python_goldens() {
    if common::capture_mode() {
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
    let cases: Vec<Value> = map["cases"].as_array().expect("cases list").to_vec();
    assert!(!cases.is_empty(), "the corpus built no cases");

    // The mint subject's cases, keyed by slug: the driver labels every case;
    // the mint's four carry payloads + agents_home.
    let mint_cases: Vec<&Value> = cases
        .iter()
        .filter(|entry| entry["slug"].as_str().unwrap_or("").starts_with("mint_"))
        .collect();
    assert_eq!(mint_cases.len(), 4, "the mint corpus is four cases");

    for entry in mint_cases {
        let slug = entry["slug"].as_str().expect("case slug");
        let case_dir = PathBuf::from(entry["dir"].as_str().expect("case dir"));
        let payload = fs::read_to_string(case_dir.join("payload.json")).expect("payload");
        let home = case_dir.join("agents-home");
        let mut child = Command::new(env!("CARGO_BIN_EXE_fno-agents"))
            .args(["pr-watch", "merge-cleanup-mint"])
            .env("FNO_AGENTS_HOME", &home)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .expect("pr-watch merge-cleanup-mint runs");
        {
            use std::io::Write;
            child
                .stdin
                .as_mut()
                .expect("stdin piped")
                .write_all(payload.as_bytes())
                .expect("write payload");
        }
        let out = child.wait_with_output().expect("mint arm finishes");
        assert_eq!(
            out.status.code(),
            Some(0),
            "[{slug}] exit: {}",
            String::from_utf8_lossy(&out.stderr)
        );

        // The journal row: the last merge_cleanup_requested line under the
        // agents home. Keys re-sort alphabetically (BTreeMap) to match the
        // Python capture's sort_keys golden; the volatile merged_at masks.
        let journal = fs::read_to_string(home.join("events.jsonl")).expect("journal");
        let row: Value = journal
            .lines()
            .filter_map(|ln| serde_json::from_str(ln).ok())
            .filter(|v: &Value| v["type"] == "merge_cleanup_requested")
            .last()
            .expect("the mint wrote one journal row");
        let mut data = row["data"].clone();
        if slug == "mint_merged_at_missing" {
            data["merged_at"] = Value::String("<STAMP>".into());
        }
        let sorted: std::collections::BTreeMap<String, Value> = data
            .as_object()
            .expect("data object")
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        let got = serde_json::to_string(&sorted).expect("serialize sorted data");
        let want = golden(slug, "out").unwrap_or_else(|| panic!("[{slug}] no frozen golden"));
        assert_eq!(got + "\n", want, "[{slug}] mint data");
    }
}

/// The ritual subject: drive the `pr-watch ritual` arm per case against the
/// frozen Python receipts. The Python oracle (the pre-port `_ritual.py`) is
/// restored only by the capture driver's pin; this test never re-freezes. The
/// mutex-held case seeds a REAL claim through the production claim CLI so the
/// arm's acquire reads HeldByOther the way the capture's stubbed exit 1 read.
#[test]
fn ritual_matches_python_goldens() {
    if common::capture_mode() {
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
    let mut ritual_cases: Vec<Value> = map["cases"]
        .as_array()
        .expect("cases list")
        .iter()
        .filter(|entry| entry["slug"].as_str().unwrap_or("").starts_with("ritual_"))
        .cloned()
        .collect();
    ritual_cases.sort_by_key(|entry| entry["slug"].as_str().unwrap_or("").to_string());
    assert_eq!(ritual_cases.len(), 8, "the ritual corpus is eight cases");

    for entry in ritual_cases {
        let slug = entry["slug"].as_str().expect("case slug").to_string();
        let case_dir = PathBuf::from(entry["dir"].as_str().expect("case dir"));
        let case: Value =
            serde_json::from_str(&fs::read_to_string(case_dir.join("case.json")).expect("case"))
                .expect("case.json parses");
        let env = &case["env"];
        // Hermetic on purpose: clear the ambient session, then apply exactly
        // the pins the capture child applied.
        let base = |cmd: &mut Command| {
            cmd.env_clear();
            for (k, v) in env.as_object().expect("case env object") {
                cmd.env(k, v.as_str().expect("env string value"));
            }
        };
        let slug_for_seed = slug.clone();
        if slug_for_seed == "ritual_mutex_held" {
            // A live foreign holder: pid 1 outlives the test, so the claim
            // classifies LIVE and is not stealable.
            let key = format!("reconcile:pr-{}", case["pr"].as_i64().expect("pr"));
            let mut seed = Command::new(env!("CARGO_BIN_EXE_fno-agents"));
            base(&mut seed);
            let st = seed
                .args([
                    "claim",
                    "acquire",
                    &key,
                    "--holder",
                    "parity-other",
                    "--ttl",
                    "15m",
                    "--pid",
                    "1",
                ])
                .current_dir(&case_dir)
                .output()
                .expect("claim seed runs");
            assert!(st.status.success(), "[{slug}] claim seed failed");
        }
        let pr = case["pr"].as_i64().expect("pr");
        let mut argv = vec!["pr-watch".to_string(), "ritual".to_string(), pr.to_string()];
        if case["autonomous"].as_bool().unwrap_or(false) {
            argv.push("--autonomous".to_string());
        }
        let mut run = Command::new(env!("CARGO_BIN_EXE_fno-agents"));
        base(&mut run);
        let out = run
            .args(&argv)
            .current_dir(&case_dir)
            .output()
            .expect("pr-watch ritual runs");
        let got_out = mask(&String::from_utf8_lossy(&out.stdout), &case_dir);
        let want_out = golden(&slug, "out").unwrap_or_else(|| panic!("[{slug}] no frozen golden"));
        assert_eq!(got_out, want_out, "[{slug}] stdout");
        let got_err = mask(&String::from_utf8_lossy(&out.stderr), &case_dir);
        let want_err = golden(&slug, "err").unwrap_or_default();
        assert_eq!(got_err, want_err, "[{slug}] stderr");
        let want_exit: i32 = golden(&slug, "exit")
            .unwrap_or_else(|| panic!("[{slug}] no frozen exit"))
            .trim()
            .parse()
            .expect("exit golden parses");
        assert_eq!(out.status.code(), Some(want_exit), "[{slug}] exit");
    }
}
