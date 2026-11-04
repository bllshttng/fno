use super::*;
use tempfile::TempDir;

const MINI_PLAN: &str = "---\nstatus: ready\nkind: plan\ncreated: 2026-07-25\n---\n\n# Plan\n\n## Overview\n\nExecutable plan.\n\n## Failure Modes\n\n**Boundaries**\n- Reject a missing plan file\n\n## Execution Strategy\n\n```yaml\nexecution_mode: sequential\nwaves:\n  - wave: 1\n    mode: sequential\n    name: Build\n    tasks: [\"1.1\"]\ntasks:\n  - id: \"1.1\"\n    title: Build it\n    surface: [src/a.rs]\n    verify: cargo check\n    acceptance: [AC1-HP]\n```\n";

fn write(dir: &TempDir, name: &str, text: &str) -> String {
    let path = dir.path().join(name);
    std::fs::write(&path, text).unwrap();
    path.display().to_string()
}

fn args(parts: &[&str]) -> Vec<String> {
    parts.iter().map(|s| s.to_string()).collect()
}

#[test]
fn brief_exits_zero_one_two_or_three_by_failure_kind() {
    let dir = TempDir::new().unwrap();
    let plan = write(&dir, "plan.md", MINI_PLAN);
    let malformed = write(
        &dir,
        "malformed.md",
        "---\nstatus: ready\n---\n\n## Execution Strategy\n\n```yaml\ntasks:\n  - id: [unclosed list\n    title: broken\n```\n",
    );
    let no_strategy = write(
        &dir,
        "none.md",
        "---\nstatus: ready\n---\n\n## Overview\n\nNo strategy.\n",
    );
    let missing = dir.path().join("absent.md").display().to_string();

    let cases: Vec<(Vec<String>, i32)> = vec![
        (args(&["brief", &plan, "--task", "1.1"]), 0),
        (
            args(&["brief", &plan, "--task", "1.1", "--format", "json"]),
            0,
        ),
        (
            args(&[
                "brief",
                &plan,
                "--task",
                "1.1",
                "--include-failure-modes",
                "none",
            ]),
            0,
        ),
        (args(&["brief", &plan, "--task", "9.9"]), 2),
        (args(&["brief", &no_strategy, "--task", "1.1"]), 2),
        (args(&["brief", &plan]), 2),
        (
            args(&["brief", &plan, "--task", "1.1", "--format", "yaml"]),
            2,
        ),
        (
            args(&[
                "brief",
                &plan,
                "--task",
                "1.1",
                "--include-locked-decisions",
                "some",
            ]),
            2,
        ),
        (args(&["brief", &missing, "--task", "1.1"]), 1),
        (args(&["brief", &malformed, "--task", "1.1"]), 3),
    ];
    for (argv, want) in cases {
        assert_eq!(run(&argv), want, "{argv:?}");
    }
}

#[test]
fn the_door_refuses_an_unknown_leaf_and_a_missing_one_with_a_usage_exit() {
    assert_eq!(run(&[]), 2);
    assert_eq!(run(&args(&["bogus"])), 2);
    assert_eq!(run(&args(&["rung"])), 2);
    assert_eq!(run(&args(&["--help"])), 0);
}

#[test]
fn parse_exits_zero_for_a_plan_one_for_an_unreadable_file_and_three_for_bad_frontmatter() {
    let dir = TempDir::new().unwrap();
    let plan = write(&dir, "plan.md", MINI_PLAN);
    let bad = write(&dir, "bad.md", "---\nstatus: [unclosed\n---\nbody\n");
    let missing = dir.path().join("absent.md").display().to_string();

    assert_eq!(run(&args(&["parse", &plan])), 0);
    assert_eq!(run(&args(&["parse", &missing])), 1);
    assert_eq!(run(&args(&["parse", &bad])), 3);
    assert_eq!(run(&args(&["parse"])), 2);
}

#[test]
fn the_status_ladder_and_blueprint_allowlist_are_pinned_by_membership() {
    assert_eq!(
        STATUS_PROGRESSION,
        ["idea", "design", "ready", "in_progress", "in_review"]
    );
    let mut allowed: Vec<&str> = BLUEPRINT_WRITE_ALLOWLIST.to_vec();
    allowed.sort();
    let mut want = vec![
        "Execution Strategy",
        "File Ownership Map",
        "Five questions",
        "Patterns to Reuse",
        "kill_criteria",
        "execution_mode",
        "waves",
        "acceptance_contract",
        "consolidation",
        "difficulty",
        "join",
        "carveouts",
    ];
    want.sort();
    assert_eq!(allowed, want);
}

#[test]
fn validate_execution_scope_reports_text_json_and_exit_codes() {
    let dir = TempDir::new().unwrap();
    let plan = write(&dir, "plan.md", MINI_PLAN);
    let path = std::path::Path::new(&plan);

    let text = super::super::exec_validate::op_validate_execution(path, false, None).unwrap();
    assert_eq!(text["valid"], json!(true));
    assert_eq!(text["text"], json!(format!("valid execution: {plan}")));

    let as_json = super::super::exec_validate::op_validate_execution(path, true, None).unwrap();
    assert_eq!(
        as_json,
        json!({
            "valid": true,
            "path": plan,
            "scope": "execution",
            "violations": [],
            "warnings": [],
        })
    );

    assert_eq!(run(&args(&["validate", &plan, "--execution"])), 0);
    assert_eq!(run(&args(&["validate", &plan, "--execution", "--json"])), 0);

    let thin = write(
        &dir,
        "thin.md",
        "---\nstatus: ready\nkind: quick-plan\ncreated: 2026-07-25\n---\n\n## Context\n\nOnly context.\n",
    );
    let report = super::super::exec_validate::op_validate_execution(
        std::path::Path::new(&thin),
        false,
        None,
    )
    .unwrap();
    let rendered = report["text"].as_str().unwrap();
    assert!(rendered.starts_with("invalid execution:"), "{rendered}");
    assert!(rendered.contains("  Changes:"), "{rendered}");
    assert_eq!(run(&args(&["validate", &thin, "--execution"])), 1);
}

#[test]
fn default_validate_checks_frontmatter_only_and_refuses_an_unloadable_plan() {
    let dir = TempDir::new().unwrap();
    let body_invalid = write(
        &dir,
        "plan.md",
        "---\nnode: x-test\nstatus: ready\ncreated: 2026-07-25\n---\n\n## Context\n\nExecution-invalid but irrelevant here.\n",
    );
    let missing = dir.path().join("absent.md").display().to_string();

    assert_eq!(run(&args(&["validate", &body_invalid])), 0);
    assert_eq!(run(&args(&["validate", &missing])), 1);
    assert_eq!(run(&args(&["validate"])), 2);
}

#[test]
fn migrate_keys_is_a_dry_run_until_apply_is_passed() {
    let dir = TempDir::new().unwrap();
    let plans = dir.path().join("plans");
    std::fs::create_dir_all(&plans).unwrap();
    let doc = plans.join("a.md");
    let original = "---\ngraph_node_id: x-1\nkind: feature\n---\n\n# Title\n";
    std::fs::write(&doc, original).unwrap();
    let plans_arg = plans.display().to_string();

    assert_eq!(run(&args(&["migrate-keys", "--plans-dir", &plans_arg])), 0);
    assert_eq!(std::fs::read_to_string(&doc).unwrap(), original);

    assert_eq!(
        run(&args(&[
            "migrate-keys",
            "--plans-dir",
            &plans_arg,
            "--apply"
        ])),
        0
    );
    let migrated = std::fs::read_to_string(&doc).unwrap();
    assert!(migrated.contains("\nnode: x-1\n") && migrated.contains("\ntype: feature\n"));
}
