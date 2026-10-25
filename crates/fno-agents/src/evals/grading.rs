//! Mechanical grading: the three check kinds against a graded worktree
//! (the deleted Python leg's grading.py, ported). Deterministic by
//! construction: no model in the pass/fail path. The same worktree
//! end-state always grades the same.

pub struct CheckResult {
    pub kind: String,
    pub passed: bool,
    pub detail: String,
}

pub struct GradeOutcome {
    pub passed: bool,
    pub results: Vec<CheckResult>,
}

impl GradeOutcome {
    /// One-line reason: the first failing check's detail, or empty.
    pub fn reason(&self) -> String {
        for r in &self.results {
            if !r.passed {
                return r.detail.clone();
            }
        }
        String::new()
    }
}

/// The per-check timeout derives from the task's timeout_minutes: the same
/// budget that bounds the worker step.
pub fn grade(
    task: &super::bank::TaskSpec,
    workdir: &std::path::Path,
    base_ref: Option<&str>,
) -> GradeOutcome {
    let timeout_s = std::cmp::max(1, task.timeout_minutes.saturating_mul(60));
    let mut results: Vec<CheckResult> = Vec::new();
    for check in &task.grade {
        match check.kind.as_str() {
            "exit" => results.push(grade_exit(check, workdir, timeout_s, base_ref)),
            "file-exists" => results.push(grade_file_exists(check, workdir)),
            _ => results.push(grade_grep(check, workdir)),
        }
    }
    GradeOutcome {
        passed: results.iter().all(|r| r.passed),
        results,
    }
}

/// `exit`: a shell command's exit code must equal `expect`. Graders scope
/// commit scans to the run's own commits through FNO_EVALS_BASE_REF; a
/// grader that needs it fail-closes on its absence, never a moving remote
/// ref.
fn grade_exit(
    check: &super::bank::GradeCheck,
    workdir: &std::path::Path,
    timeout_s: i64,
    base_ref: Option<&str>,
) -> CheckResult {
    let command = check.command.clone().unwrap_or_default();
    let mut cmd = std::process::Command::new("sh");
    cmd.arg("-c").arg(&command).current_dir(workdir);
    if let Some(r) = base_ref {
        cmd.env("FNO_EVALS_BASE_REF", r);
    }
    match spawn_wait_timeout(&mut cmd, timeout_s) {
        WaitOutcome::Done(code) => {
            let ok = code == check.expect;
            let want = check.expect;
            CheckResult {
                kind: "exit".into(),
                passed: ok,
                detail: format!("exit: `{command}` -> {code} (want {want})"),
            }
        }
        WaitOutcome::TimedOut => CheckResult {
            kind: "exit".into(),
            passed: false,
            detail: format!("exit: `{command}` timed out after {timeout_s}s"),
        },
        WaitOutcome::Failed(e) => CheckResult {
            kind: "exit".into(),
            passed: false,
            detail: format!("exit: `{command}` failed to run: {e}"),
        },
    }
}

/// `file-exists`: a workdir-relative path must exist.
fn grade_file_exists(check: &super::bank::GradeCheck, workdir: &std::path::Path) -> CheckResult {
    let rel = check.path.clone().unwrap_or_default();
    let ok = workdir.join(&rel).exists();
    let verdict = if ok { "present" } else { "missing" };
    CheckResult {
        kind: "file-exists".into(),
        passed: ok,
        detail: format!("file-exists: {rel} -> {verdict}"),
    }
}

/// `grep`: a workdir-relative file must contain a substring.
fn grade_grep(check: &super::bank::GradeCheck, workdir: &std::path::Path) -> CheckResult {
    let rel = check.path.clone().unwrap_or_default();
    let pattern = check.pattern.clone().unwrap_or_default();
    let target = workdir.join(&rel);
    if !target.is_file() {
        return CheckResult {
            kind: "grep".into(),
            passed: false,
            detail: format!("grep: {rel} not a file"),
        };
    }
    match std::fs::read(&target) {
        Err(e) => CheckResult {
            kind: "grep".into(),
            passed: false,
            detail: format!("grep: cannot read {rel}: {e}"),
        },
        Ok(bytes) => {
            let text = String::from_utf8_lossy(&bytes);
            let ok = text.contains(&pattern);
            let verdict = if ok { "found" } else { "absent" };
            CheckResult {
                kind: "grep".into(),
                passed: ok,
                detail: format!("grep: '{pattern}' in {rel} -> {verdict}"),
            }
        }
    }
}

enum WaitOutcome {
    Done(i32),
    TimedOut,
    Failed(String),
}

/// Spawn the command and wait up to *timeout_s*; a timed-out child is
/// killed (the Python subprocess timeout semantics).
fn spawn_wait_timeout(cmd: &mut std::process::Command, timeout_s: i64) -> WaitOutcome {
    match cmd.spawn() {
        Err(e) => WaitOutcome::Failed(e.to_string()),
        Ok(mut child) => {
            let deadline =
                std::time::Instant::now() + std::time::Duration::from_secs(timeout_s.max(1) as u64);
            loop {
                match child.try_wait() {
                    Ok(Some(status)) => return WaitOutcome::Done(status.code().unwrap_or(-1)),
                    Ok(None) => {
                        if std::time::Instant::now() >= deadline {
                            let _ = child.kill();
                            let _ = child.wait();
                            return WaitOutcome::TimedOut;
                        }
                        std::thread::sleep(std::time::Duration::from_millis(50));
                    }
                    Err(e) => return WaitOutcome::Failed(e.to_string()),
                }
            }
        }
    }
}
