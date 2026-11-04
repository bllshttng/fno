//! The `tests` grade kind: grade a fix by the tests the real fix added.
//! The check re-applies the fix commit's test files over the worker's tree
//! (so the worker cannot rewrite them), runs the named test ids, and passes
//! only when every fail-to-pass (f2p) and pass-to-pass (p2p) id reads ok. An
//! id with no result line is a failure, never a pass. No model is involved.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use super::bank::{GradeCheck, TestsSpec};
use super::grading::{spawn_wait_timeout, CheckResult, WaitOutcome};

/// One shared target dir, so a cold sealed tree reuses compiled dependencies.
fn target_dir() -> PathBuf {
    std::env::temp_dir().join("fno-evals-cargo-target")
}

fn fail(detail: String) -> CheckResult {
    CheckResult {
        kind: "tests".into(),
        passed: false,
        detail: format!("tests: {detail}"),
    }
}

/// Grade the `tests` check of a task against its worker's tree.
pub fn grade(check: &GradeCheck, workdir: &Path, timeout_s: i64) -> CheckResult {
    let Some(spec) = &check.tests else {
        return fail("check carries no tests spec".into());
    };
    let repo = super::canonical_repo_root();
    let has_ref = std::process::Command::new("git")
        .args(["cat-file", "-e", &format!("{}^{{commit}}", spec.tests_ref)])
        .current_dir(&repo)
        .output()
        .is_ok_and(|o| o.status.success());
    if !has_ref {
        return fail("repo for tests_ref unavailable".into());
    }
    grade_in(spec, workdir, &repo, timeout_s)
}

pub(super) fn grade_in(
    spec: &TestsSpec,
    workdir: &Path,
    repo: &Path,
    timeout_s: i64,
) -> CheckResult {
    let files: Vec<&str> = spec.test_files.iter().map(String::as_str).collect();
    if let Err(e) = super::runner::extract_ref(repo, &spec.tests_ref, &files, workdir) {
        return fail(format!("cannot re-apply test files: {e}"));
    }
    let ids: Vec<String> = spec.f2p.iter().chain(&spec.p2p).cloned().collect();
    match run_cargo(workdir, &spec.package, Some(&ids), timeout_s) {
        Err(e) => fail(e),
        Ok(out) => verdict(spec, &out),
    }
}

/// `cargo test` for a package: the named ids exactly, or every test. Returns
/// the combined output; a timeout or a runner that will not start is an Err.
pub(super) fn run_cargo(
    workdir: &Path,
    package: &str,
    ids: Option<&[String]>,
    timeout_s: i64,
) -> Result<String, String> {
    let out = tempfile::NamedTempFile::new().map_err(|e| e.to_string())?;
    let err = out.reopen().map_err(|e| e.to_string())?;
    let mut cmd = std::process::Command::new("cargo");
    cmd.args(["test", "-p", package, "--lib", "--"])
        .current_dir(workdir)
        .env("CARGO_TARGET_DIR", target_dir())
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .stdout(std::process::Stdio::from(
            out.reopen().map_err(|e| e.to_string())?,
        ))
        .stderr(std::process::Stdio::from(err));
    if let Some(ids) = ids {
        cmd.arg("--exact").args(ids);
    }
    match spawn_wait_timeout(&mut cmd, timeout_s) {
        WaitOutcome::Done(_) => {}
        WaitOutcome::TimedOut => return Err(format!("cargo test timed out after {timeout_s}s")),
        WaitOutcome::Failed(e) => return Err(format!("cargo test failed to run: {e}")),
    }
    std::fs::read_to_string(out.path()).map_err(|e| e.to_string())
}

/// libtest `test <id> ... ok|FAILED|ignored` lines, by id.
pub(super) fn parse_libtest(output: &str) -> BTreeMap<String, String> {
    output
        .lines()
        .filter_map(|l| l.strip_prefix("test "))
        .filter_map(|l| l.rsplit_once(" ... "))
        .map(|(id, status)| (id.to_string(), status.trim().to_string()))
        .collect()
}

fn verdict(spec: &TestsSpec, output: &str) -> CheckResult {
    let seen = parse_libtest(output);
    let total = spec.f2p.len() + spec.p2p.len();
    let bad: Vec<String> = spec
        .f2p
        .iter()
        .chain(&spec.p2p)
        .filter_map(|id| match seen.get(id).map(String::as_str) {
            Some("ok") => None,
            Some(other) => Some(format!("{id} ({other})")),
            None => Some(format!("{id} (missing: no result line)")),
        })
        .collect();
    if bad.is_empty() {
        CheckResult {
            kind: "tests".into(),
            passed: true,
            detail: format!(
                "tests: {total} ids ok (f2p {}, p2p {})",
                spec.f2p.len(),
                spec.p2p.len()
            ),
        }
    } else {
        fail(format!("{}/{total} not ok: {}", bad.len(), bad.join(", ")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_failed_or_ignored_id_fails_and_is_named() {
        let spec = TestsSpec {
            tests_ref: "abc1234".into(),
            test_files: vec!["a/tests.rs".into()],
            package: "p".into(),
            f2p: vec!["m::new_one".into(), "m::lost".into()],
            p2p: vec!["m::kept".into(), "m::skipped".into()],
        };
        let out = "running 3 tests\ntest m::new_one ... FAILED\ntest m::kept ... ok\ntest m::skipped ... ignored\n";
        let r = verdict(&spec, out);
        assert!(!r.passed);
        for want in [
            "m::new_one (FAILED)",
            "m::lost (missing",
            "m::skipped (ignored)",
        ] {
            assert!(r.detail.contains(want), "{}", r.detail);
        }
        assert!(!r.detail.contains("m::kept"), "{}", r.detail);
        let all_ok = "test m::new_one ... ok\ntest m::lost ... ok\ntest m::kept ... ok\ntest m::skipped ... ok\n";
        assert!(verdict(&spec, all_ok).passed);
    }
}
