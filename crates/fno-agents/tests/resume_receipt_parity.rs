//! The resume receipt port's characterization: the door's bytes ARE the
//! deleted Python leg's contract (`fno.resume.receipt`), frozen in the
//! goldens under `tests/golden/resume_receipt/`. The capture script
//! (`tests/fixtures/resume_receipt/capture_golden.sh`) re-derives every
//! fixture and freezes fresh outputs; this test asserts the door still
//! answers the frozen bytes.
//!
//! parity-stage: characterization
//! parity-oracle: fno.resume.receipt

use regex::Regex;
use std::path::PathBuf;
use std::process::Command;

/// Machine-varying bytes mask before comparison. Longest first, so a 64-char
/// digest never splits into fake 40/12 matches. The git commit shas depend on
/// the capturing machine's git object encoding, and content_sha digests the
/// fixture path, so the mask keeps the comparison cross-machine; the tokens'
/// positions and shapes stay pinned. (Fixture paths themselves are already
/// masked to FIXTURE by the capture script at freeze time.)
fn mask(text: &str) -> String {
    let re64 = Regex::new(r"[0-9a-f]{64}").expect("compiles");
    let re40 = Regex::new(r"[0-9a-f]{40}").expect("compiles");
    let re12 = Regex::new(r"[0-9a-f]{12}").expect("compiles");
    let masked64 = re64.replace_all(text, "<SHA64>");
    let masked40 = re40.replace_all(&masked64, "<SHA40>");
    re12.replace_all(&masked40, "<CAND12>").into_owned()
}

fn read_file(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_default()
}

/// Re-derive every fixture through the capture script and assert the door
/// answers the frozen bytes: exit codes, stdout, stderr - all 19 cases.
#[test]
fn the_door_answers_the_frozen_python_goldens() {
    // crates/fno-agents -> crates -> repo root
    let repo_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("manifest sits two levels under the repo root")
        .to_path_buf();
    let golden_dir = repo_root
        .join("crates/fno-agents/tests/golden/resume_receipt")
        .canonicalize()
        .expect("the committed goldens exist");
    let fresh = tempfile::tempdir().expect("scratch dir");

    let status = Command::new("bash")
        .arg(repo_root.join("tests/fixtures/resume_receipt/capture_golden.sh"))
        .env("FNO_CAPTURE_BIN", env!("CARGO_BIN_EXE_fno-agents"))
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
            // The exit file carries only the number: mask it and it must stay
            // a bare number, so assert it unmasked as well.
            if suffix == "exit" {
                assert_eq!(fresh_text.trim(), frozen.trim(), "{case}: exit code");
            }
        }
    }
}

/// A v1 receipt written before task_context existed must still validate: the
/// canonical payload ignores the absent (null) optional field, so a legacy
/// receipt's digest equals a modern one's. (The deleted leg pinned this in
/// Python; the contract moves here with the leg.)
#[test]
fn the_legacy_digest_ignores_the_absent_optional_field() {
    let receipt = fno_agents::resume_receipt::model::build_receipt(
        "x-59b0",
        "sess-worker",
        "execute",
        1,
        "footnote",
        "/wt/x-59b0",
        "feature/x-59b0",
        "b48ba4b8cfff",
        "/fno:target",
        Some("x-59b0"),
        "2026-09-12T00:00:00Z",
        Vec::new(),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        None,
    )
    .expect("a valid receipt");

    let mut legacy = fno_agents::resume_receipt::model::receipt_to_value(&receipt);
    legacy
        .as_object_mut()
        .expect("receipt shape")
        .remove("task_context");
    legacy
        .as_object_mut()
        .expect("receipt shape")
        .remove("content_sha");
    assert_eq!(
        fno_agents::resume_receipt::model::content_sha_of(&legacy),
        receipt.content_sha
    );
}
