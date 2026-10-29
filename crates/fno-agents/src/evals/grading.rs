//! Mechanical grading: the four check kinds against a graded worktree
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
            "tool-budget" => results.push(grade_tool_budget(
                check,
                &crate::claude_drive::claude_projects_dir()
                    .join(crate::claude_ask::claude_cwd_slug(workdir)),
            )),
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
            let ok = i64::from(code) == check.expect;
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

/// `tool-budget`: the worker's recorded tool calls must not exceed
/// `max_tool_calls`. The evidence is the claude transcript the trial left in
/// its own projects dir (every trial is a fresh cwd, so the dir holds this
/// attempt alone). A missing or unreadable transcript FAILS the check: an
/// unmeasured route is never a pass. Codex and opencode workers write no
/// claude transcript, so they fail here as unmeasured.
fn grade_tool_budget(
    check: &super::bank::GradeCheck,
    transcript_dir: &std::path::Path,
) -> CheckResult {
    let max = check.max_tool_calls.unwrap_or(0);
    let fail = |why: String| CheckResult {
        kind: "tool-budget".into(),
        passed: false,
        detail: format!("tool-budget: unmeasured, {why}"),
    };
    let files: Vec<std::path::PathBuf> = match std::fs::read_dir(transcript_dir) {
        Ok(rd) => rd
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|e| e == "jsonl"))
            .collect(),
        Err(e) => {
            return fail(format!(
                "no transcript dir {}: {e}",
                transcript_dir.display()
            ))
        }
    };
    if files.is_empty() {
        return fail(format!("no transcript in {}", transcript_dir.display()));
    }
    let mut calls: i64 = 0;
    for f in &files {
        match crate::interrupt_classify::read_transcript(f) {
            Ok(entries) => {
                calls += entries
                    .iter()
                    .filter(|e| {
                        matches!(
                            e,
                            crate::interrupt_classify::TranscriptEntry::AssistantToolUse { .. }
                        )
                    })
                    .count() as i64;
            }
            Err(e) => return fail(format!("cannot read {}: {e}", f.display())),
        }
    }
    CheckResult {
        kind: "tool-budget".into(),
        passed: calls <= max,
        detail: format!("tool-budget: {calls} tool calls (max {max})"),
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

#[cfg(test)]
mod tests {
    use super::*;

    fn budget(max: i64) -> super::super::bank::GradeCheck {
        super::super::bank::GradeCheck {
            kind: "tool-budget".into(),
            command: None,
            expect: 0,
            path: None,
            pattern: None,
            max_tool_calls: Some(max),
        }
    }

    fn tool_line(ids: &[&str]) -> String {
        let blocks: Vec<String> = ids
            .iter()
            .map(|i| format!(r#"{{"type":"tool_use","id":"{i}","name":"Bash","input":{{}}}}"#))
            .collect();
        format!(r#"{{"message":{{"content":[{}]}}}}"#, blocks.join(","))
    }

    fn dir_with(lines: &[String]) -> tempfile::TempDir {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("s.jsonl"), lines.join("\n") + "\n").unwrap();
        d
    }

    #[test]
    fn under_budget_passes_and_names_both_numbers() {
        let lines: Vec<String> = (0..12).map(|i| tool_line(&[&format!("t{i}")])).collect();
        let r = grade_tool_budget(&budget(40), dir_with(&lines).path());
        assert!(r.passed, "{}", r.detail);
        assert_eq!(r.detail, "tool-budget: 12 tool calls (max 40)");
    }

    #[test]
    fn over_budget_fails_naming_count_and_budget() {
        let lines: Vec<String> = (0..91).map(|i| tool_line(&[&format!("t{i}")])).collect();
        let r = grade_tool_budget(&budget(40), dir_with(&lines).path());
        assert!(!r.passed);
        assert_eq!(r.detail, "tool-budget: 91 tool calls (max 40)");
    }

    #[test]
    fn three_blocks_in_one_message_count_three() {
        let r = grade_tool_budget(&budget(2), dir_with(&[tool_line(&["a", "b", "c"])]).path());
        assert!(!r.passed);
        assert!(r.detail.contains("3 tool calls"), "{}", r.detail);
    }

    #[test]
    fn a_missing_or_empty_transcript_fails_never_passes() {
        let gone = tempfile::tempdir().unwrap().path().join("nope");
        let r = grade_tool_budget(&budget(40), &gone);
        assert!(!r.passed && r.detail.contains("unmeasured"), "{}", r.detail);
        let empty = tempfile::tempdir().unwrap();
        let r = grade_tool_budget(&budget(40), empty.path());
        assert!(
            !r.passed && r.detail.contains("no transcript"),
            "{}",
            r.detail
        );
    }
}
