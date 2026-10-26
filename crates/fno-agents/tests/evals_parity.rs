//! The evals port's characterization: the native dispatch's bytes ARE the
//! deleted Python leg's contract (`fno.evals.cli`), frozen in the goldens
//! under `tests/golden/evals/`. The capture script
//! (`tests/fixtures/evals/capture_golden.sh`) re-derives every fixture and
//! freezes fresh outputs through the forwarding leaf; this test asserts the
//! leaf + door still answer the frozen bytes.
//!
//! parity-stage: characterization
//! parity-oracle: fno.evals.cli

use std::path::{Path, PathBuf};
use std::process::Command;

/// Machine-varying bytes mask before comparison: the macOS realpath prefix
/// the capture host's Path.resolve answers (/private/tmp), and the fixture
/// repo's git sha (the bank_rev echo), whose bytes depend on the capturing
/// machine's git object encoding. The tokens' positions and shapes stay
/// pinned. (Fixture paths and wall-clock stamps are already masked to
/// FIXTURE and <TS> by the capture script at freeze time.)
fn mask(text: &str) -> String {
    let normalized = text.replace("/privateFIXTURE", "FIXTURE");
    let re40 = regex::Regex::new(r"[0-9a-f]{40}").expect("compiles");
    re40.replace_all(&normalized, "<SHA40>").into_owned()
}

fn read_file(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_default()
}

/// Re-derive every fixture through the capture script and assert the leaf
/// answers the frozen bytes: exit codes, stdout, stderr - all 28 cases.
#[test]
fn the_leaf_answers_the_frozen_python_goldens() {
    // crates/fno-agents -> crates -> repo root
    let repo_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("manifest sits two levels under the repo root")
        .to_path_buf();
    let golden_dir = repo_root
        .join("crates/fno-agents/tests/golden/evals")
        .canonicalize()
        .expect("the committed goldens exist");
    let fresh = tempfile::tempdir().expect("scratch dir");

    let status = Command::new("bash")
        .arg(repo_root.join("tests/fixtures/evals/capture_golden.sh"))
        .envs(fno_agents::test_run::self_owner_env())
        .env("FNO_CAPTURE_AGENTS_BIN", env!("CARGO_BIN_EXE_fno-agents"))
        .env("FNO_CAPTURE_GOLDEN_OUT", fresh.path())
        .env_remove("FNO_TASK_CONTEXT_FILE")
        .env_remove("FNO_CLAIMS_ROOT")
        .status()
        .expect("runs the capture script");
    assert!(status.success(), "the capture run must succeed");

    let mut cases: Vec<String> = std::fs::read_dir(&golden_dir)
        .expect("golden dir reads")
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().into_string().ok()?;
            name.strip_suffix(".exit").map(str::to_string)
        })
        .collect();
    cases.sort();
    assert!(!cases.is_empty(), "the golden set is not empty");

    for case in &cases {
        for (suffix, kind) in [("exit", "exit code"), ("out", "stdout"), ("err", "stderr")] {
            let frozen = read_file(&golden_dir.join(format!("{case}.{suffix}")));
            let fresh_text = read_file(&fresh.path().join(format!("{case}.{suffix}")));
            assert_eq!(
                mask(&fresh_text),
                mask(&frozen),
                "{case}: {kind} diverged from the frozen golden"
            );
            if suffix == "exit" {
                assert_eq!(fresh_text.trim(), frozen.trim(), "{case}: exit code");
            }
        }
    }
}
