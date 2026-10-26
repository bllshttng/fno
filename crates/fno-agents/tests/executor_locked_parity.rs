//! parity-stage: characterization
//! parity-oracle: fno.executor._locked
//!
//! Characterization harness for the locked-decision parser door. The goldens
//! (tests/golden/executor_locked/<case>.{exit,out,err}) were captured from
//! the Python leg (`fno.executor._locked`) before its family deleted; each
//! case pipes the same fixture the capture driver built and drives the real
//! `fno-agents executor-route locked` door, so the frozen Python verdict is
//! the contract the native parser answers.

use std::path::PathBuf;
use std::process::{Command, Stdio};

/// One frozen case: the golden slug, the mode flag, the stdin fixture the
/// capture driver piped. `input` bytes must match the capture exactly.
struct Case {
    label: &'static str,
    flag: &'static str,
    input: String,
}

fn doc(body: &str) -> String {
    format!(
        "# Some design\n\n## Architecture\n\nThe operator resolves an executor: impeccable \
         for frontend surfaces.\n\n## Locked Decisions\n\n{body}\n\n## Open Questions\n\n1. \
         Anything?\n"
    )
}

fn run_door(flag: &str, input: &str) -> (i32, String, String) {
    let mut child = Command::new(env!("CARGO_BIN_EXE_fno-agents"))
        .args(["executor-route", "locked"])
        .args(flag.split_whitespace())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .envs(fno_agents::test_run::self_owner_env())
        .spawn()
        .expect("run fno-agents executor-route locked");
    use std::io::Write as _;
    child
        .stdin
        .take()
        .expect("stdin")
        .write_all(input.as_bytes())
        .expect("pipe stdin");
    let out = child.wait_with_output().expect("collect door output");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

#[test]
fn the_locked_door_matches_the_captured_leg_on_every_golden() {
    let cases = [
        Case { label: "canonical-plan-level", flag: "", input: doc("5. **Executor routing**: plan-level `executor: impeccable` (auto-detected).") },
        Case { label: "canonical-bullet-tdd", flag: "", input: doc("- **Executor routing**: plan-level `executor: tdd` (cli-flag).") },
        Case { label: "do-alias-normalizes", flag: "", input: doc("- **Executor routing**: plan-level `executor: do` (cli-flag).") },
        Case {
            label: "mixed-override-shape",
            flag: "",
            input: doc(
                "5. **Executor routing**: plan-level `executor: tdd` with per-task overrides\n   \
                 `executor: impeccable` on tasks touching `**/*.tsx`, `components/**` \
                 (auto-detected).\n   Rationale: design has a frontend page and a backend \
                 migration.",
            ),
        },
        Case { label: "bare-executor-entry", flag: "", input: doc("5. **Executor: `tdd` (archer / TDD).** Backend-only Python + config, no UI surface.") },
        Case { label: "bare-executor-bold-key", flag: "", input: doc("- **Executor:** impeccable") },
        Case { label: "prose-mention-not-lock", flag: "", input: doc("1. We rejected the idea that the executor: impeccable resolver should be per-record.") },
        Case { label: "outside-section-not-lock", flag: "", input: doc("1. Nothing about routing here.") },
        Case { label: "unknown-value-emits-empty", flag: "", input: doc("5. **Executor: archer**") },
        Case { label: "last-entry-wins", flag: "", input: doc("1. **Executor: impeccable**\n\n2. **Executor: `tdd`**") },
        Case { label: "no-locked-section", flag: "", input: "# Doc\n\n## Architecture\n\nexecutor: do\n".to_string() },
        Case { label: "empty-input", flag: "", input: String::new() },
        Case { label: "rejected-alternative-prose", flag: "", input: "## Locked Decisions\n\n5. **Executor routing**: `executor: do` (archer / TDD), not the rejected `executor: impeccable` option.\n".to_string() },
        Case { label: "decision-history-prose", flag: "", input: "## Locked Decisions\n\n5. **Executor routing**: changed from `executor: impeccable` to `executor: tdd` after review.\n".to_string() },
        Case { label: "documented-override-shape", flag: "", input: "## Locked Decisions\n\n5. **Executor routing**: plan-level `executor: tdd` with per-task overrides `executor: impeccable` on tasks touching `**/*.tsx`.\n".to_string() },
        Case { label: "explicit-mixed", flag: "", input: "## Locked Decisions\n\n5. **Executor**: `executor: mixed`\n".to_string() },
        Case { label: "model-provenance-suffix", flag: "--key model", input: "## Locked Decisions\n\n5. **Model**: `fable` (user-confirmed)\n".to_string() },
        Case { label: "model-multi-token-rejected", flag: "--key model", input: "## Locked Decisions\n\n5. **Model**: `opus 4.8`\n".to_string() },
        Case { label: "model-bold-key-colon-inside", flag: "--key model", input: "## Locked Decisions\n\n5. **Model**: `sonnet`\n".to_string() },
        Case { label: "model-crlf-input", flag: "--key model", input: "## Locked Decisions\r\n\r\n5. **Model**: `fable`\r\n".to_string() },
        Case { label: "model-backticked-value", flag: "--key model", input: "## Locked Decisions\n\n5. **Model**: `glm-5.3-flash[1m]`\n".to_string() },
        Case { label: "model-numbered-entry", flag: "--key model", input: "## Locked Decisions\n\n3. **Model**: opus-5.5 (auto-detected)\n".to_string() },
        Case { label: "model-prose-mention", flag: "--key model", input: "## Locked Decisions\n\n1. We discussed whether the model: choice should be per-task.\n".to_string() },
        Case { label: "model-outside-section", flag: "--key model", input: "# Doc\n\n## Architecture\n\nModel: fable\n".to_string() },
    ];
    assert_cases_against_goldens(&cases, "executor_locked");
}

#[test]
fn the_locked_door_answers_usage_on_a_missing_or_unknown_mode() {
    let missing = run_door_raw(&[]);
    assert_eq!(missing.0, 2, "no mode is a usage error");
    assert!(missing.2.contains("usage: fno-agents executor-route"));
    let unknown = run_door_raw(&["bogus"]);
    assert_eq!(unknown.0, 2, "an unknown mode is a usage error");
}

fn run_door_raw(args: &[&str]) -> (i32, String, String) {
    let output = Command::new(env!("CARGO_BIN_EXE_fno-agents"))
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .envs(fno_agents::test_run::self_owner_env())
        .output()
        .expect("run the door with no stdin");
    (
        output.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

fn assert_cases_against_goldens(cases: &[Case], subject: &str) {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/golden")
        .join(subject);
    for case in cases {
        let expected_exit = std::fs::read_to_string(dir.join(format!("{}.exit", case.label)))
            .unwrap_or_else(|e| panic!("{subject}/{}: golden exit missing: {e}", case.label))
            .trim()
            .parse::<i32>()
            .expect("golden exit parses");
        let expected_out = std::fs::read_to_string(dir.join(format!("{}.out", case.label)))
            .unwrap_or_else(|e| panic!("{subject}/{}: golden out missing: {e}", case.label));
        let expected_err = std::fs::read_to_string(dir.join(format!("{}.err", case.label)))
            .unwrap_or_else(|e| panic!("{subject}/{}: golden err missing: {e}", case.label));
        let (code, stdout, stderr) = run_door(case.flag, &case.input);
        assert_eq!(code, expected_exit, "{subject}/{}: exit", case.label);
        assert_eq!(stdout, expected_out, "{subject}/{}: stdout", case.label);
        assert_eq!(stderr, expected_err, "{subject}/{}: stderr", case.label);
    }
}
