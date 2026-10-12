//! parity-stage: characterization
//! parity-oracle: cli/src/fno/verify_advise.py (read_plan_acs, build_prompt,
//! parse_verdict)
//!
//! Characterization tests for the `verify_advise` Rust port, frozen against
//! the live Python module before its deletion. Golden encoding: a `Some(v)`
//! case freezes `<key>.out` = v + "\n" plus `<key>.exit` = "0"; a `None`
//! case freezes only an empty `<key>.out` (the oracle exits 1 with no
//! output, and normal mode always reads `.out`). No case output embeds a
//! fixture path, so no path normalization is needed.
//!
//! The goldens were frozen directly from the Python module by a driver, not
//! through cargo, so capture mode is a replay of that freeze: the oracle
//! here runs the same three Python functions on the same fixtures.

use common::{assert_golden as assert_golden_common, capture_mode, Golden};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

mod common;

/// The Python oracle body. argv: family, then the family's args. A `Some(v)`
/// result prints v (plus newline) and exits 0; `None` exits 1 silently.
const ORACLE: &str = r#"
import sys
from fno import verify_advise as va
from pathlib import Path
family = sys.argv[1]
a = sys.argv[2:]
if family == "read_plan_acs":
    r = va.read_plan_acs(Path(a[0]))
elif family == "build_prompt":
    r = va.build_prompt(a[0], a[1], a[2])
elif family == "parse_verdict":
    r = va.parse_verdict(a[0])
else:
    sys.exit(2)
if r is None:
    sys.exit(1)
print(r)
"#;

/// Run the Python leg on one case. Only invoked in capture mode.
fn run_oracle(family: &str, args: &[&str]) -> Golden {
    let cli_src = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("cli")
        .join("src");
    let out = Command::new("python3")
        .arg("-c")
        .arg(ORACLE)
        .arg(family)
        .args(args)
        .env("PYTHONPATH", &cli_src)
        .output()
        .expect("python oracle runs");
    if out.status.code() == Some(0) {
        Golden {
            exit: Some(0),
            streams: vec![String::from_utf8_lossy(&out.stdout).into_owned()],
        }
    } else {
        Golden {
            exit: None,
            streams: vec![String::new()],
        }
    }
}

/// Assert one case: `value` is the Rust mirror as (exit, stdout) and `args`
/// replays the same fixture through the Python leg in capture mode.
fn assert_parity(family: &str, args: &[&str], value: (Option<i32>, String), label: &str) {
    let rust = Golden {
        exit: value.0,
        streams: vec![value.1],
    };
    let oracle = capture_mode().then(|| run_oracle(family, args));
    assert_golden_common("verify_advise", label, &rust, oracle);
}

fn write(dir: &Path, name: &str, content: &str) -> PathBuf {
    let p = dir.join(name);
    fs::write(&p, content).unwrap();
    p
}

/// Rust mirror of the Python `read_plan_acs` arms: `Ok(Some(v))` freezes as
/// a 0-exit case, `Ok(None)` as a None case. An `Err` has no golden arm and
/// fails loudly rather than silently matching a `None` golden.
fn mirror_read(p: &Path) -> (Option<i32>, String) {
    match fno_agents::verify_advise::read_plan_acs(p) {
        Ok(Some(v)) => (Some(0), format!("{v}\n")),
        Ok(None) => (None, String::new()),
        Err(e) => panic!("read_plan_acs errored: {e}"),
    }
}

#[test]
fn acs_section_ends_at_next_heading() {
    let tmp = tempfile::TempDir::new().unwrap();
    let plan = write(
        tmp.path(),
        "plan.md",
        "## Acceptance Criteria\n\nAC1-HP: it works\nAC2: also this\n\n## Next Steps\n\nlater\n",
    );
    assert_parity(
        "read_plan_acs",
        &[plan.to_str().unwrap()],
        mirror_read(&plan),
        "acs section ends at next heading",
    );
}

#[test]
fn acs_section_runs_through_subheading_to_eof() {
    let tmp = tempfile::TempDir::new().unwrap();
    // A `###` heading is NOT a section end (`^#{1,2}\s` needs whitespace on
    // the third char), so the section runs through it to end of file.
    let plan = write(
        tmp.path(),
        "plan.md",
        "### Acceptance Criteria\n\nAC1-X: first\n\n### Subsection\n\nAC2-Y: second\n",
    );
    assert_parity(
        "read_plan_acs",
        &[plan.to_str().unwrap()],
        mirror_read(&plan),
        "acs section runs through subheading to eof",
    );
}

#[test]
fn acs_label_fallback_without_section() {
    let tmp = tempfile::TempDir::new().unwrap();
    let plan = write(
        tmp.path(),
        "plan.md",
        "# Title\n\nAC1-HP: do the thing\nsome prose\n\n- AC2-B: other thing\n",
    );
    assert_parity(
        "read_plan_acs",
        &[plan.to_str().unwrap()],
        mirror_read(&plan),
        "acs label fallback without section",
    );
}

#[test]
fn acs_absent_none() {
    let tmp = tempfile::TempDir::new().unwrap();
    let missing = tmp.path().join("nope.md");
    assert_eq!(
        fno_agents::verify_advise::read_plan_acs(&missing),
        Ok(None),
        "a missing plan is Ok(None), never an error"
    );
    assert_parity(
        "read_plan_acs",
        &[missing.to_str().unwrap()],
        (None, String::new()),
        "acs absent -> none",
    );
}

#[test]
fn empty_section_falls_back_to_labels() {
    let tmp = tempfile::TempDir::new().unwrap();
    // The section closes at the next heading with nothing captured, so the
    // fallback scans AC-labelled lines across the FULL text, including lines
    // before the heading.
    let plan = write(
        tmp.path(),
        "plan.md",
        "AC0-Z: pre-heading label\n\n## Acceptance Criteria\n\n## Next\n\nAC9-Q: after next\n",
    );
    assert_parity(
        "read_plan_acs",
        &[plan.to_str().unwrap()],
        mirror_read(&plan),
        "empty section falls back to labels",
    );
}

#[test]
fn build_prompt_exact_bytes() {
    let acs = "AC1: x";
    let diffstat = " file | 2 +-\n";
    let commits = "abc1234 do it";
    let v = fno_agents::verify_advise::build_prompt(acs, diffstat, commits);
    assert_parity(
        "build_prompt",
        &[acs, diffstat, commits],
        (Some(0), format!("{v}\n")),
        "build prompt exact bytes",
    );
}

#[test]
fn verdict_pass_uppercase() {
    let text = "looks good\nVERDICT: PASS";
    let v = fno_agents::verify_advise::parse_verdict(text).expect("uppercase PASS parses");
    assert_eq!(v, "pass");
    assert_parity(
        "parse_verdict",
        &[text],
        (Some(0), format!("{v}\n")),
        "verdict pass uppercase",
    );
}

#[test]
fn verdict_concerns_lowercase() {
    let text = "verdict: concerns";
    let v = fno_agents::verify_advise::parse_verdict(text).expect("lowercase form parses");
    assert_eq!(v, "concerns");
    assert_parity(
        "parse_verdict",
        &[text],
        (Some(0), format!("{v}\n")),
        "verdict concerns lowercase",
    );
}

#[test]
fn verdict_nonmatch_none() {
    let text = "VERDICT: maybe";
    assert_eq!(
        fno_agents::verify_advise::parse_verdict(text),
        None,
        "a word outside the pass|concerns|fail set does not parse"
    );
    assert_parity(
        "parse_verdict",
        &[text],
        (None, String::new()),
        "verdict nonmatch -> none",
    );
}

#[test]
fn verdict_empty_none() {
    assert_eq!(fno_agents::verify_advise::parse_verdict(""), None);
    assert_parity(
        "parse_verdict",
        &[""],
        (None, String::new()),
        "verdict empty -> none",
    );
}
