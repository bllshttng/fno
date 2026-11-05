//! Mechanical grading: the check kinds against a graded worktree
//! (the deleted Python leg's grading.py, ported). Deterministic by
//! construction, with one fenced exception: the `rubric` kind is the one
//! model path, and it stays fenced by the load-time refusal on
//! `tier: regression`, the verdict cache, and the recorded-verdicts proof
//! rule (see rubric.rs). Every other kind: the same worktree end-state
//! always grades the same.

/// The directory holding a build of the graded ref's `fno-agents` pair. The
/// gate sets it; a grader's `fno` then runs that build and not the deployed one.
pub const HEAD_BIN_ENV: &str = "FNO_EVALS_HEAD_BIN";

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
    let has_rubric = task.grade.iter().any(|c| c.rubric.is_some());
    let judge = if has_rubric {
        super::rubric::SpawnJudge::from_config(
            &std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from(".")),
            &crate::evals::history_path()
                .parent()
                .map(std::path::Path::to_path_buf)
                .unwrap_or_else(|| std::path::PathBuf::from(".")),
        )
    } else {
        Ok(None)
    };
    let judge = match judge {
        Ok(Some(judge)) => Some(judge),
        Ok(None) => None,
        Err(e) => {
            eprintln!("WARNING: rubric judges refused: {e}; rubric checks read missing results");
            None
        }
    };
    grade_checks(
        &task.grade,
        workdir,
        timeout_s,
        base_ref,
        None,
        task.prompt.as_deref(),
        judge.as_ref().map(|j| j as &dyn super::rubric::Judge),
    )
}

/// Grade a check list against a directory. A metric case calls this with the
/// golden's own checks, so the proof covers the grader the golden runs.
/// `bin_dir` leads the grader's PATH: a proof case puts a stub binary there.
/// The rubric arm consults `judge` (the panel over one criterion) and reads
/// the task prompt; both arrive from `grade()`, or a recorded-verdicts judge
/// from the metric proof. A rubric check with `judge: None` reads a missing
/// result, never a pass.
pub fn grade_checks(
    checks: &[super::bank::GradeCheck],
    workdir: &std::path::Path,
    timeout_s: i64,
    base_ref: Option<&str>,
    bin_dir: Option<&std::path::Path>,
    task_prompt: Option<&str>,
    judge: Option<&dyn super::rubric::Judge>,
) -> GradeOutcome {
    let mut results: Vec<CheckResult> = Vec::new();
    for check in checks {
        match check.kind.as_str() {
            "exit" => results.push(grade_exit(check, workdir, timeout_s, base_ref, bin_dir)),
            "file-exists" => results.push(grade_file_exists(check, workdir)),
            "tool-budget" => results.push(grade_tool_budget(
                check,
                &crate::claude_drive::claude_projects_dir()
                    .join(crate::claude_ask::claude_cwd_slug(workdir)),
            )),
            "tests" => results.push(super::tests_grade::grade(check, workdir, timeout_s)),
            "rubric" => results.push(grade_rubric(check, workdir, base_ref, task_prompt, judge)),
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
    bin_dir: Option<&std::path::Path>,
) -> CheckResult {
    let command = check.command.clone().unwrap_or_default();
    let mut cmd = std::process::Command::new("sh");
    // The verdict is the exit code; a grader's output must not reach a report.
    cmd.arg("-c")
        .arg(&command)
        .current_dir(workdir)
        .env("FNO_EVALS_GRADER", "1")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    if let Some(r) = base_ref {
        cmd.env("FNO_EVALS_BASE_REF", r);
    }
    // The gate builds the ref's binaries and names their directory here. Only
    // the grader's `fno` follows it: the runner and a worker keep the deployed
    // pair, so the run itself is never head code.
    if let Some(dir) = std::env::var_os(HEAD_BIN_ENV) {
        let dir = std::path::Path::new(&dir);
        cmd.env("FNO_AGENTS_BIN", dir.join("fno-agents"));
        cmd.env("FNO_AGENTS_WORKER", dir.join("fno-agents-worker"));
    }
    if let Some(dir) = bin_dir {
        let mut dirs = vec![dir.to_path_buf()];
        dirs.extend(std::env::split_paths(
            &std::env::var_os("PATH").unwrap_or_default(),
        ));
        if let Ok(joined) = std::env::join_paths(dirs) {
            cmd.env("PATH", joined);
        }
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

/// `rubric`: judge-graded. One work product (`path` file, or the git diff
/// against the grade base for `diff: true`), one panel per criterion. The
/// check fails when a hurdle fails under any judge, or any judge cell
/// faults (a missing result, never a pass); the detail names rates and the
/// disagreement count.
fn grade_rubric(
    check: &super::bank::GradeCheck,
    workdir: &std::path::Path,
    base_ref: Option<&str>,
    task_prompt: Option<&str>,
    judge: Option<&dyn super::rubric::Judge>,
) -> CheckResult {
    use super::bank::RubricTag;
    use super::rubric::RubricAsk;
    let missing = |why: String| CheckResult {
        kind: "rubric".into(),
        passed: false,
        detail: format!("rubric: missing result, {why}"),
    };
    let Some(spec) = check.rubric.as_ref() else {
        return missing("check carries no criteria".into());
    };
    let Some(judge) = judge else {
        return missing("no judge configured (set config.evals.rubric.judges to two lanes from different vendors)".into());
    };
    let Some(prompt) = task_prompt else {
        return missing("no task prompt".into());
    };
    let work_product = match (&spec.path, spec.diff) {
        (Some(rel), _) => std::fs::read_to_string(workdir.join(rel))
            .map_err(|e| format!("cannot read work product {rel}: {e}")),
        (None, true) => match base_ref {
            Some(base) => std::process::Command::new("/usr/bin/git")
                .args(["diff", base])
                .current_dir(workdir)
                .output()
                .map(|out| String::from_utf8_lossy(&out.stdout).into_owned())
                .map_err(|e| format!("git diff failed: {e}")),
            None => Err("a diff work product needs a grade base".to_string()),
        },
        (None, false) => Err("no work-product source (path or diff) on the check".into()),
    };
    let work_product = match work_product {
        Ok(text) => text,
        Err(why) => return missing(why),
    };
    let mut faults: Vec<String> = Vec::new();
    let mut passed_cells = 0u32;
    let mut cells = 0u32;
    let mut disagreements = 0u32;
    let mut hurdle_failure: Option<(String, String)> = None;
    for criterion in &spec.criteria {
        let ask = RubricAsk {
            task_prompt: prompt,
            criterion,
            work_product: &work_product,
        };
        let panel = judge.panel(&ask);
        if panel.is_empty() {
            faults.push(format!(
                "criterion '{}': the judge panel returned no verdicts",
                criterion.id
            ));
        }
        let mut cell_verdicts: Vec<bool> = Vec::new();
        for v in &panel {
            match &v.fault {
                Some(why) => faults.push(format!(
                    "criterion '{}': judge '{}' fault: {why}",
                    criterion.id, v.lane
                )),
                None => {
                    cells += 1;
                    cell_verdicts.push(v.passed);
                    if v.passed {
                        passed_cells += 1;
                    }
                    if criterion.tag == RubricTag::Hurdle && !v.passed {
                        hurdle_failure = Some((criterion.id.clone(), v.lane.clone()));
                    }
                }
            }
        }
        cell_verdicts.dedup();
        if cell_verdicts.len() > 1 {
            disagreements += 1;
        }
    }
    if let Some((cid, lane)) = hurdle_failure {
        return CheckResult {
            kind: "rubric".into(),
            passed: false,
            detail: format!(
                "rubric: hurdle '{cid}' failed under judge '{lane}'; cells {passed_cells}/{cells}, disagreements {disagreements}"
            ),
        };
    }
    if !faults.is_empty() {
        return CheckResult {
            kind: "rubric".into(),
            passed: false,
            detail: format!("rubric: missing result, {}", faults.join("; ")),
        };
    }
    CheckResult {
        kind: "rubric".into(),
        passed: true,
        detail: format!(
            "rubric: all hurdles passed; cells {passed_cells}/{cells}, disagreements {disagreements}"
        ),
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
    if let Err(e) = std::fs::read_dir(transcript_dir) {
        return fail(format!(
            "no transcript dir {}: {e}",
            transcript_dir.display()
        ));
    }
    let mut files: Vec<std::path::PathBuf> = Vec::new();
    collect_jsonl(transcript_dir, &mut files);
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

/// Every `.jsonl` under the dir, subagent transcripts
/// (`<session>/subagents/agent-*.jsonl`) included.
fn collect_jsonl(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for p in rd.flatten().map(|e| e.path()) {
        if p.is_dir() {
            collect_jsonl(&p, out);
        } else if p.extension().is_some_and(|e| e == "jsonl") {
            out.push(p);
        }
    }
}

pub(super) enum WaitOutcome {
    Done(i32),
    TimedOut,
    Failed(String),
}

/// Spawn the command and wait up to *timeout_s*; a timed-out child is
/// killed (the Python subprocess timeout semantics).
pub(super) fn spawn_wait_timeout(cmd: &mut std::process::Command, timeout_s: i64) -> WaitOutcome {
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
            tests: None,
            rubric: None,
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
    fn subagent_transcripts_count_toward_the_budget() {
        let d = dir_with(&[tool_line(&["a"])]);
        let sub = d.path().join("sess").join("subagents");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::write(sub.join("agent-1.jsonl"), tool_line(&["b", "c"]) + "\n").unwrap();
        let r = grade_tool_budget(&budget(40), d.path());
        assert_eq!(r.detail, "tool-budget: 3 tool calls (max 40)");
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

    fn rubric_check(hurdle_first: bool) -> super::super::bank::GradeCheck {
        let tag = |hurdle: bool| {
            if hurdle {
                super::super::bank::RubricTag::Hurdle
            } else {
                super::super::bank::RubricTag::Principle
            }
        };
        super::super::bank::GradeCheck {
            kind: "rubric".into(),
            command: None,
            expect: 0,
            path: Some("w.md".into()),
            pattern: None,
            max_tool_calls: None,
            tests: None,
            rubric: Some(super::super::bank::RubricSpec {
                path: Some("w.md".into()),
                diff: false,
                criteria: vec![
                    super::super::bank::RubricCriterion {
                        id: "stays-rust".into(),
                        text: "names a crates/ path".into(),
                        tag: tag(hurdle_first),
                    },
                    super::super::bank::RubricCriterion {
                        id: "concise".into(),
                        text: "one line of reason".into(),
                        tag: tag(!hurdle_first),
                    },
                ],
            }),
        }
    }

    struct FakeJudge {
        hurdle_passes: bool,
        principle_passes: bool,
        fault: Option<String>,
    }

    impl super::super::rubric::Judge for FakeJudge {
        fn panel(
            &self,
            ask: &super::super::rubric::RubricAsk,
        ) -> Vec<super::super::rubric::LaneVerdict> {
            let cell = |lane: &str, passed: bool| super::super::rubric::LaneVerdict {
                lane: lane.into(),
                passed,
                fault: None,
            };
            if let Some(why) = &self.fault {
                return vec![
                    cell("zai", true),
                    super::super::rubric::LaneVerdict {
                        lane: "codex".into(),
                        passed: false,
                        fault: Some(why.clone()),
                    },
                ];
            }
            let zai_passes = match ask.criterion.tag {
                super::super::bank::RubricTag::Hurdle => self.hurdle_passes,
                super::super::bank::RubricTag::Principle => self.principle_passes,
            };
            vec![cell("zai", zai_passes), cell("codex", true)]
        }
    }

    #[test]
    fn a_failed_hurdle_fails_the_check_and_names_hurdle_and_judge() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("w.md"), "action: crates/x\n").unwrap();
        let judge = FakeJudge {
            hurdle_passes: false,
            principle_passes: true,
            fault: None,
        };
        let r = grade_rubric(
            &rubric_check(true),
            dir.path(),
            None,
            Some("prompt"),
            Some(&judge),
        );
        assert!(!r.passed, "{}", r.detail);
        assert!(
            r.detail.contains("stays-rust") && r.detail.contains("zai"),
            "{}",
            r.detail
        );
    }

    #[test]
    fn principles_only_lower_the_rate_and_a_judge_fault_reads_missing_never_pass() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("w.md"), "action: crates/x\n").unwrap();
        // A failing principle drops one cell: 3 of 4 clean cells pass.
        let judge = FakeJudge {
            hurdle_passes: true,
            principle_passes: false,
            fault: None,
        };
        let r = grade_rubric(
            &rubric_check(false),
            dir.path(),
            None,
            Some("prompt"),
            Some(&judge),
        );
        assert!(r.passed, "{}", r.detail);
        assert!(r.detail.contains("cells 3/4"), "{}", r.detail);
        // AC2-EDGE: a faulting judge is a missing result, never a pass.
        let judge = FakeJudge {
            hurdle_passes: true,
            principle_passes: true,
            fault: Some("timed out".into()),
        };
        let r = grade_rubric(
            &rubric_check(false),
            dir.path(),
            None,
            Some("prompt"),
            Some(&judge),
        );
        assert!(
            !r.passed && r.detail.contains("missing result"),
            "{}",
            r.detail
        );
    }
}
