use super::*;
use crate::plan_doc::doc::load_plan_text;
use std::collections::BTreeSet;

const FULL_FM: &str = "status: ready\nkind: plan\ncreated: 2026-07-25\n";
const QUICK_FM: &str = "status: ready\nkind: quick-plan\ncreated: 2026-07-25\n";

const VALID_STRATEGY: &str = "execution_mode: sequential\nwaves:\n  - wave: 1\n    mode: sequential\n    name: Build\n    tasks: [\"1.1\"]\ntasks:\n  - id: \"1.1\"\n    title: Build the validator\n    surface: [cli/src/plan/cli.py]\n    verify: fno doctor test tests/test_a.py\n    acceptance: [AC1-HP]\n";

fn doc_of(frontmatter: &str, body: &str) -> PlanDocument {
    load_plan_text(&format!("---\n{frontmatter}---\n\n# Plan\n\n{body}")).unwrap()
}

/// The fixture states the kind its body actually is: only a full plan opens
/// with an Overview.
fn validate_body(body: &str) -> ExecutionValidationResult {
    let fm = if body.contains("## Overview") {
        FULL_FM
    } else {
        QUICK_FM
    };
    validate_execution(&doc_of(fm, body), None)
}

fn full_plan(strategy: &str) -> String {
    format!("## Overview\n\nExecutable plan.\n\n## Execution Strategy\n\n```yaml\n{strategy}```\n")
}

fn quick_plan(files: &str, verification: &str) -> String {
    format!("## Context\n\nWhy this is needed.\n\n## Changes\n\n### 1. Do the work\n\nMake the change.\n\n## Files to Modify\n\n{files}\n\n## Verification\n\n{verification}\n")
}

const QUICK_FILES: &str = "| File | Action |\n|---|---|\n| `cli/src/plan/exec.py` | modify |";
const QUICK_VERIFY: &str = "1. `fno doctor test tests/test_a.py`";

fn strategy_with_verify(verify: &str) -> String {
    VALID_STRATEGY.replace(
        "verify: fno doctor test tests/test_a.py",
        &format!("verify: {}", serde_json::to_string(verify).unwrap()),
    )
}

fn fields(result: &ExecutionValidationResult) -> BTreeSet<String> {
    result.violations.iter().map(|v| v.field.clone()).collect()
}

fn messages(result: &ExecutionValidationResult) -> String {
    result
        .violations
        .iter()
        .map(|v| v.message.as_str())
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn runnable_verify_commands_pass_in_quick_and_full_plans() {
    for command in [
        "mix test",
        "[ -f out.txt ]",
        "awk '... ' file",
        "\"$VIRTUAL_ENV/bin/pytest\" -q",
    ] {
        let quick = validate_body(&quick_plan(QUICK_FILES, &format!("1. `{command}`")));
        assert!(
            quick.violations.is_empty(),
            "{command}: {:?}",
            quick.violations
        );
        let wrapped = format!("env FNO_TEST=1 {command}");
        let full = validate_body(&full_plan(&strategy_with_verify(&wrapped)));
        assert!(
            full.violations.is_empty(),
            "{command}: {:?}",
            full.violations
        );
    }
}

#[test]
fn prose_and_non_commands_fail_verify_in_both_plan_shapes() {
    for verification in [
        "Manually inspect the result",
        "make sure",
        "timeout 30",
        "README.md",
        "pytest tests/ && echo \"unclosed",
    ] {
        let quick = validate_body(&quick_plan(QUICK_FILES, &format!("1. {verification}")));
        assert_eq!(
            fields(&quick),
            BTreeSet::from(["Verification".to_string()]),
            "{verification}"
        );
        let full = validate_body(&full_plan(&strategy_with_verify(verification)));
        assert_eq!(
            fields(&full),
            BTreeSet::from(["tasks.1.1.verify".to_string()]),
            "{verification}"
        );
    }
}

#[test]
fn quick_verification_tolerates_list_shapes_wrappers_and_ambiguous_command_words() {
    for verification in [
        "1. `fno doctor test tests/test_a.py`",
        "- fno doctor test tests/test_a.py",
        "Run the check first.\n\n1. env FNO_DEBUG=1 fno doctor test tests/test_a.py",
        "1. `./scripts/ci/preflight.sh`",
        "1. `timeout 30 pytest tests/test_a.py`",
        "1. `timeout -k 5 1m fno doctor test tests/test_a.py`",
        "1. `gtimeout 30 pytest tests/test_a.py`",
        "1. `poetry run pytest -q`",
        "1. `tsc --noEmit`",
        "1. `cargo check`",
        "1. `make test`",
        "1. `fno worktree ensure`",
        "1. `npm run check`",
        "1. `test -f out.txt`",
        "1. `make all`",
        // A quoted pipe is not a segment separator.
        "1. `rg -n 'foo|bar' src/`",
    ] {
        let result = validate_body(&quick_plan(QUICK_FILES, verification));
        assert!(
            result.violations.is_empty(),
            "{verification}: {:?}",
            result.violations
        );
    }
}

#[test]
fn quick_files_accept_lexical_paths_and_refuse_non_path_tokens() {
    for token in [
        "cli/src/plan/does_not_exist_yet.py",
        "/etc/hosts",
        "~/.fno/config.toml",
        "app/[slug]/page.tsx",
        "AGENTS.md",
        "Dockerfile",
        ".gitignore",
        "gradle.properties",
        "site.webmanifest",
        "bin/mycli",
        "cli/src",
    ] {
        let result = validate_body(&quick_plan(&format!("- `{token}`"), QUICK_VERIFY));
        assert!(
            result.violations.is_empty(),
            "{token}: {:?}",
            result.violations
        );
    }
    for token in [
        "https://example.com/a.py",
        "--dry-run",
        "cli/**/*.py",
        "backend service",
        "cli/src/",
        "/",
        "~/.fno/",
        "..",
        "N/A",
    ] {
        let result = validate_body(&quick_plan(&format!("- `{token}`"), QUICK_VERIFY));
        assert_eq!(
            fields(&result),
            BTreeSet::from(["Files to Modify".to_string()]),
            "{token}"
        );
    }
}

#[test]
fn quick_body_checks_run_whether_or_not_a_strategy_is_present() {
    let strategy = "## Execution Strategy\n\n```yaml\n".to_string() + VALID_STRATEGY + "```\n";

    let clean = validate_body(&(quick_plan(QUICK_FILES, QUICK_VERIFY) + "\n" + &strategy));
    assert!(clean.violations.is_empty(), "{:?}", clean.violations);

    let scaffold = "## Context\n\nWhy this is needed.\n\n## Changes\n\n### 1. [change name]\n\n[describe the change here]\n\n## Files to Modify\n\n| File | Action |\n|---|---|\n| `path/to/file.ts` | Modify |\n\n## Verification\n\n1. Manually inspect the result\n\n";
    let result = validate_body(&format!("{scaffold}{strategy}"));
    let got = fields(&result);
    for field in ["Changes", "Files to Modify", "Verification"] {
        assert!(got.contains(field), "{field}: {:?}", result.violations);
    }

    let only_context = validate_body("## Context\n\nOnly context.\n");
    assert!(fields(&only_context).contains("Changes"));
}

#[test]
fn placeholder_task_fields_fail_with_exact_field_paths() {
    let strategy = "execution_mode: sequential\nwaves:\n  - wave: 1\n    mode: sequential\n    tasks: [\"1.1\"]\ntasks:\n  - id: \"1.1\"\n    title: Implement feature\n    surface: []\n    verify: \"# fill in verify command\"\n    acceptance: []\n";
    let got = fields(&validate_body(&full_plan(strategy)));
    for field in [
        "tasks.1.1.surface",
        "tasks.1.1.verify",
        "tasks.1.1.acceptance",
    ] {
        assert!(got.contains(field), "{field}: {got:?}");
    }
}

#[test]
fn task_and_wave_structure_errors_are_each_reported() {
    let no_tasks = "execution_mode: sequential\nwaves:\n  - wave: 1\n    mode: sequential\n    tasks: [\"1.1\"]\n";
    assert!(messages(&validate_body(&full_plan(no_tasks)))
        .contains("Execution Strategy must declare at least one task"));

    let duplicate = "execution_mode: sequential\nwaves:\n  - wave: 1\n    mode: sequential\n    tasks: [\"1.1\", \"9.9\"]\ntasks:\n  - id: \"1.1\"\n    title: First\n    surface: [src/a.py]\n    verify: fno doctor test tests/test_a.py\n    acceptance: [AC1]\n  - id: \"1.1\"\n    title: Duplicate\n    surface: [src/b.py]\n    verify: fno doctor test tests/test_b.py\n    acceptance: [AC2]\n";
    let text = messages(&validate_body(&full_plan(duplicate)));
    assert!(text.contains("duplicate task id"), "{text}");
    assert!(text.contains("unknown task '9.9'"), "{text}");

    let cycle = "execution_mode: sequential\nwaves:\n  - wave: 1\n    mode: sequential\n    depends_on: 2\n    tasks: [\"1.1\"]\n  - wave: 2\n    mode: sequential\n    depends_on: 1\n    tasks: [\"2.1\"]\ntasks:\n  - id: \"1.1\"\n    title: First\n    surface: [src/a.py]\n    verify: fno doctor test tests/test_a.py\n    acceptance: [AC1]\n  - id: \"2.1\"\n    title: Second\n    surface: [src/b.py]\n    verify: fno doctor test tests/test_b.py\n    acceptance: [AC2]\n";
    assert!(messages(&validate_body(&full_plan(cycle))).contains("dependency cycle"));

    let bad_wave = VALID_STRATEGY.replace("wave: 1", "wave: nope").replace(
        "mode: sequential\n    name: Build",
        "mode: mixed\n    name: Build",
    );
    let got = fields(&validate_body(&full_plan(&bad_wave)));
    assert!(got.contains("waves.nope.wave"), "{got:?}");
    assert!(got.contains("waves.nope.mode"), "{got:?}");

    let dup_key = format!("{VALID_STRATEGY}tasks: []\n");
    let text = messages(&validate_body(&full_plan(&dup_key)));
    assert!(
        text.contains("duplicate") && text.contains("tasks"),
        "{text}"
    );
}

#[test]
fn a_task_blocked_by_an_unknown_task_is_refused_on_the_edge_field() {
    let strategy = VALID_STRATEGY.replace(
        "acceptance: [AC1-HP]",
        "acceptance: [AC1-HP]\n    blocked_by: [\"9.9\"]",
    );
    let result = validate_body(&full_plan(&strategy));
    assert!(result
        .violations
        .iter()
        .any(|v| v.field == "tasks.blocked_by" && v.message.contains("unknown task 9.9")));
}

#[test]
fn a_total_single_blocker_chain_warns_but_still_validates() {
    let chain = "execution_mode: sequential\nwaves:\n  - wave: 1\n    mode: sequential\n    name: Build\n    difficulty: medium\n    tasks: [\"1.1\", \"1.2\", \"1.3\"]\ntasks:\n  - id: \"1.1\"\n    title: First\n    surface: [a.py]\n    verify: fno doctor test tests/t.py\n    acceptance: [AC1-HP]\n    blocked_by: []\n  - id: \"1.2\"\n    title: Second\n    surface: [b.py]\n    verify: fno doctor test tests/t.py\n    acceptance: [AC1-HP]\n    blocked_by: [\"1.1\"]\n  - id: \"1.3\"\n    title: Third\n    surface: [c.py]\n    verify: fno doctor test tests/t.py\n    acceptance: [AC1-HP]\n    blocked_by: [\"1.2\"]\n";
    let result = validate_body(&full_plan(chain));
    assert!(result.violations.is_empty(), "{:?}", result.violations);
    assert_eq!(result.warnings.len(), 1, "{:?}", result.warnings);
    assert_eq!(result.warnings[0].field, "tasks.blocked_by");
    assert!(result.warnings[0].message.contains("1.1 -> 1.2 -> 1.3"));
}

#[test]
fn acceptance_references_resolve_only_for_compiled_v1_plans_or_an_override() {
    let strategy = VALID_STRATEGY.replace("AC1-HP", "AC9");
    let body = format!(
        "## Overview\n\nExecutable plan.\n\n## Acceptance Criteria\n\n**AC1-HP:** Given valid input, the module returns the result.\n\n## Execution Strategy\n\n```yaml\n{strategy}```\n"
    );

    let stamped = validate_execution(
        &doc_of(
            "node: x-test\nstatus: ready\ncreated: 2026-07-25\nacceptance_contract: compiled-v1\n",
            &body,
        ),
        None,
    );
    assert!(stamped
        .violations
        .iter()
        .any(|v| v.field == "tasks.1.1.acceptance" && v.message.contains("AC9")));

    let unstamped_fm = "node: x-test\nstatus: ready\ncreated: 2026-07-25\n";
    let unstamped = validate_execution(&doc_of(unstamped_fm, &body), None);
    assert!(!messages(&unstamped).contains("AC9"));

    let proposed = validate_execution(&doc_of(unstamped_fm, &body), Some("compiled-v1"));
    assert!(messages(&proposed).contains("AC9"));
}

#[test]
fn acceptance_evidence_bindings_are_checked_against_criteria_and_probes() {
    const STRATEGY: &str = "execution_mode: sequential\nwaves:\n  - wave: 1\n    mode: parallel\n    name: w\n    difficulty: high\n    tasks: ['1.1']\ntasks:\n  - id: '1.1'\n    title: t\n    surface: [a.py]\n    verify: pytest -q\n    acceptance: [AC1-HP]\n";
    let body = format!(
        "## Acceptance Criteria\n\n**AC1-HP:** Given a probe, when it runs, then it passes.\n\n## Execution Strategy\n\n```yaml\n{STRATEGY}```\n"
    );
    let violations = |fm_body: &str| -> String {
        let fm = format!("acceptance_contract: compiled-v1\n{fm_body}");
        messages(&validate_execution(&doc_of(&fm, &body), None))
    };

    assert_eq!(
        violations("done_probes:\n  - 'echo m1'\nacceptance_evidence:\n  required: true\n  bindings:\n    AC1-HP: done_probes[0]\n"),
        ""
    );
    assert!(violations("done_probes:\n  - 'echo m1'\nacceptance_evidence:\n  bindings:\n    AC9-HP: done_probes[0]\n")
        .contains("AC9-HP"));
    assert!(violations(
        "done_probes:\n  - 'echo m1'\nacceptance_evidence:\n  bindings:\n    AC1-HP: probes[0]\n"
    )
    .contains("expected `done_probes[n]`"));
    assert!(violations(
        "done_probes: 'echo m1'\nacceptance_evidence:\n  bindings:\n    AC1-HP: done_probes[1]\n"
    )
    .contains("the bound probe is missing"));
    let required = violations("close_probes:\n  - 'echo m1'\nacceptance_evidence:\n  required: true\n  bindings:\n    AC1-HP: close_probes[0]\n");
    assert!(
        required.contains("asserts required evidence") && required.contains("session terminal")
    );
}

#[test]
fn a_plan_dated_after_the_difficulty_gate_needs_a_band_on_every_wave() {
    let doc = doc_of(
        "status: ready\ncreated: 2026-08-27\ndifficulty: medium\n",
        &full_plan(VALID_STRATEGY),
    );
    let result = validate_execution(&doc, None);
    assert!(
        result
            .violations
            .iter()
            .any(|v| v.field == "waves.1.difficulty"
                && v.message.contains("one of low, medium, high"))
    );
}
