//! parity-stage: characterization
//! parity-oracle: fno.verify_advise.decide_verdict
//!
//! Characterization tests for the `verify_advise` Rust port, frozen against
//! the live Python module before its deletion. Golden encoding: a `Some(v)`
//! case freezes `<key>.out` = v + "\n" plus `<key>.exit` = "0"; a `None`
//! case freezes only an empty `<key>.out` (the oracle exits 1 with no
//! output, and normal mode always reads `.out`). No case output embeds a
//! fixture path, so no path normalization is needed.
//!
//! The goldens were captured from the Python module by a driver, not
//! through cargo. The module and that capture driver are both gone; the
//! committed goldens are now the only oracle.

use common::{assert_golden as assert_golden_common, Golden};
use std::fs;
use std::path::{Path, PathBuf};

mod common;

/// Assert one case against its frozen golden. `value` is the Rust mirror as
/// (exit, stdout); capture mode is gone with the Python oracle, so the
/// oracle slot is always `None` and the golden file decides.
fn assert_parity(value: (Option<i32>, String), label: &str) {
    let rust = Golden {
        exit: value.0,
        streams: vec![value.1],
    };
    assert_golden_common("verify_advise", label, &rust, None);
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
    assert_parity(mirror_read(&plan), "acs section ends at next heading");
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
    assert_parity(mirror_read(&plan), "acs label fallback without section");
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
    assert_parity((None, String::new()), "acs absent -> none");
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
    assert_parity(mirror_read(&plan), "empty section falls back to labels");
}

#[test]
fn build_prompt_exact_bytes() {
    let acs = "AC1: x";
    let diffstat = " file | 2 +-\n";
    let commits = "abc1234 do it";
    let v = fno_agents::verify_advise::build_prompt(acs, diffstat, commits);
    assert_parity((Some(0), format!("{v}\n")), "build prompt exact bytes");
}

#[test]
fn verdict_pass_uppercase() {
    let text = "looks good\nVERDICT: PASS";
    let v = fno_agents::verify_advise::parse_verdict(text).expect("uppercase PASS parses");
    assert_eq!(v, "pass");
    assert_parity((Some(0), format!("{v}\n")), "verdict pass uppercase");
}

#[test]
fn verdict_concerns_lowercase() {
    let text = "verdict: concerns";
    let v = fno_agents::verify_advise::parse_verdict(text).expect("lowercase form parses");
    assert_eq!(v, "concerns");
    assert_parity((Some(0), format!("{v}\n")), "verdict concerns lowercase");
}

#[test]
fn verdict_nonmatch_none() {
    let text = "VERDICT: maybe";
    assert_eq!(
        fno_agents::verify_advise::parse_verdict(text),
        None,
        "a word outside the pass|concerns|fail set does not parse"
    );
    assert_parity((None, String::new()), "verdict nonmatch -> none");
}

#[test]
fn verdict_empty_none() {
    assert_eq!(fno_agents::verify_advise::parse_verdict(""), None);
    assert_parity((None, String::new()), "verdict empty -> none");
}
