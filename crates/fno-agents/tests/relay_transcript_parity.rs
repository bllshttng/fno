//! parity-stage: characterization
//! parity-oracle: fno.relay.registry.transcript_path_for
//!
//! Characterization harness for the claude transcript resolver door. The
//! goldens (tests/golden/relay_transcript/<case>.{exit,out}) were captured
//! from the Python leg (`fno.relay.registry.transcript_path_for`) before its
//! family deleted: the exact-file hit and three misses (empty store, a
//! dotted-stem sibling, another session's file). Each case plants the same
//! fixture store the capture driver built
//! (tests/fixtures/relay_transcript/capture_driver.py) and drives the real
//! `fno-agents provider-cap claude-transcript` door, so the frozen Python
//! verdict is the contract for the native read recovery makes.
//!
//! One case is Rust-only and carries no golden: a prefix-shaped id resolves
//! where the exact-name Python leg answered None. The deleted leg never held
//! that behavior, so no golden exists for it (the claims-root corpus keeps
//! the same rule for its Rust-only taxonomy).

use std::path::PathBuf;
use std::process::Command;

/// The planted store constants, shared with the capture driver.
const CASE_UUID: &str = "d8996f9b-8854-4f22-8c28-c7819c6d0316";
const OTHER_UUID: &str = "11111111-2222-3333-4444-555555555555";
const PROJ_DIR: &str = "-Users-bb16--claude-jobs-x";

/// One frozen case: the label, the files to plant, the id the door runs.
struct Case {
    label: &'static str,
    files: Vec<(String, String)>,
    id: &'static str,
}

fn run_door(projects: &std::path::Path, id: &str) -> (i32, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_fno-agents"))
        .args([
            "provider-cap",
            "claude-transcript",
            "--projects",
            projects.to_str().unwrap(),
            id,
        ])
        .output()
        .expect("run fno-agents provider-cap claude-transcript");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
    )
}

fn plant(root: &std::path::Path, files: &[(String, String)]) -> PathBuf {
    let projects = root.join("projects");
    for (dir, name) in files {
        let target = projects.join(dir);
        std::fs::create_dir_all(&target).unwrap();
        std::fs::write(target.join(name), "{}").unwrap();
    }
    projects
}

#[test]
fn the_transcript_door_matches_the_captured_leg_and_keeps_its_extra_strength() {
    let cases = [
        Case {
            label: "hit one project dir",
            files: vec![(PROJ_DIR.to_string(), format!("{CASE_UUID}.jsonl"))],
            id: CASE_UUID,
        },
        Case {
            label: "miss empty store",
            files: vec![],
            id: CASE_UUID,
        },
        Case {
            label: "miss dotted sibling only",
            files: vec![(
                PROJ_DIR.to_string(),
                format!("{CASE_UUID}.orphaned-x.jsonl"),
            )],
            id: CASE_UUID,
        },
        Case {
            label: "miss other session",
            files: vec![(PROJ_DIR.to_string(), format!("{OTHER_UUID}.jsonl"))],
            id: CASE_UUID,
        },
    ];
    let golden_dir =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/golden/relay_transcript");
    let root = tempfile::tempdir().unwrap();
    for (index, case) in cases.iter().enumerate() {
        let case_root = root.path().join(format!("case-{index:02}"));
        std::fs::create_dir_all(&case_root).unwrap();
        let projects = plant(&case_root, &case.files);
        let (code, stdout) = run_door(&projects, case.id);
        let key = format!(
            "{}.out",
            case.label
                .to_ascii_lowercase()
                .split(|c: char| !c.is_ascii_alphanumeric())
                .filter(|s| !s.is_empty())
                .collect::<Vec<_>>()
                .join("_")
        );
        let golden = std::fs::read_to_string(golden_dir.join(&key))
            .unwrap_or_else(|e| panic!("missing golden {key}: {e}"));
        let masked = stdout.replace(case_root.to_str().unwrap(), "<CASE>");
        assert_eq!(code, 0, "[{label}] exit", label = case.label);
        assert_eq!(masked, golden, "[{label}] stdout", label = case.label);
    }

    // Rust-only, no golden: a prefix-shaped id resolves against the store.
    // The deleted Python leg matched the exact `<uuid>.jsonl` name only, so
    // the same query answered None there. Recovery passes full uuids, so the
    // extra strength never changes its verdict; the door keeps it because the
    // resolver is the one native answer.
    let case_root = root.path().join("case-prefix");
    std::fs::create_dir_all(&case_root).unwrap();
    let projects = plant(
        &case_root,
        &[(PROJ_DIR.to_string(), format!("{CASE_UUID}.jsonl"))],
    );
    let (code, stdout) = run_door(&projects, "d8996f9b");
    assert_eq!(code, 0);
    let masked = stdout.replace(case_root.to_str().unwrap(), "<CASE>");
    assert_eq!(
        masked,
        format!("{{\"path\": \"<CASE>/projects/{PROJ_DIR}/{CASE_UUID}.jsonl\"}}\n")
    );
}
