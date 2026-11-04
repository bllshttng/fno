use super::*;
use crate::plan_doc::doc::{load_plan_text, PlanDocument};

const SAMPLE_PLAN: &str = r####"---
claims: ab-test-001
created: 2026-05-18
status: ready
type: think
feature: sample plan for brief generation tests
---

# Sample Plan

## Overview

This is the first paragraph of the overview section. It gives workers project context.

Second paragraph with more detail.

## Failure Modes

**Boundaries**
- The system must reject missing plan files with exit code 1
- The system must reject unknown task-ids with exit code 2

**Errors**
- The system must surface YAML parse failures with exit code 3

## Acceptance Criteria

**AC2-HP:** Given a plan with task 2.1, when I call brief, then I get a markdown brief.

**AC2-ERR:** Given an unknown task-id, when I call brief, then exit code is 2.

**AC2-UI:** With --format json, output matches the fixed schema.

**AC2-EDGE:** Given no tagged entries, all Locked Decisions are included.

**AC2-FR:** Given malformed Execution Strategy YAML, exit code is 3.

## Locked Decisions (DO NOT revisit)

1. **Use stdlib json.** Use the stdlib json module. *Why:* zero extra dependency. *How to apply:* import json.

2. **Fail-open on untagged entries.** Entries without surface tags are included. *Why:* avoids silent brief shrinkage. *How to apply:* include the entry.

## Patterns to Reuse

| Pattern | Source | Why reuse |
|---|---|---|
| `resolve_repo_root()` | fno.paths | path anchoring from any cwd |
| TyperRunner | cli/src/fno/plan/brief.py | consistent CliRunner pattern |

## Execution Strategy

> Bootstrapped by hand for testing.

```yaml
execution_mode: mixed
waves:
  - wave: 1
    mode: sequential
    name: "Foundation"
    tasks: [1.1]
  - wave: 2
    mode: sequential
    name: "Surface"
    tasks: [2.1]

tasks:
  - id: "1.1"
    title: "Foundation module"
    surface:
      - cli/src/fno/sample/core.py
      - cli/tests/unit/sample/test_core.py
    verify: "uv run pytest cli/tests/unit/sample/test_core.py -v"
    acceptance: [AC2-HP]
    notes: |
      Build the core module.

  - id: "2.1"
    title: "CLI entry point"
    surface:
      - cli/src/fno/plan/brief.py
      - cli/tests/unit/plan/test_brief.py
    verify: "uv run pytest cli/tests/unit/plan/test_brief.py -v"
    acceptance: [AC2-HP, AC2-ERR, AC2-UI, AC2-EDGE, AC2-FR]
    notes: |
      Build the CLI command for brief generation.
```
"####;

fn sample() -> PlanDocument {
    load_plan_text(SAMPLE_PLAN).unwrap()
}

fn fenced(yaml: &str) -> String {
    format!("```yaml\n{yaml}```")
}

#[test]
fn execution_strategy_structural_errors_are_named() {
    let cases = [
        ("tasks:\n  - not-a-mapping\n", "task 1 must be a mapping"),
        ("tasks: nope\n", "'tasks' must be a list"),
        ("waves: nope\n", "'waves' must be a list"),
        ("- a\n- b\n", "must be a YAML mapping"),
        (
            "tasks:\n  - id: '1.1'\n    surface: src/a.py\n",
            "surface must be a list",
        ),
        (
            "tasks:\n  - id: '1.1'\n    surface: ['']\n",
            "surface item 1 must be a non-empty path string",
        ),
        (
            "tasks:\n  - id: '1.1'\n    blocked_by: '1.2'\n",
            "blocked_by must be a list",
        ),
        (
            "tasks:\n  - id: [unclosed list\n    title: bad\n",
            "malformed",
        ),
    ];
    for (yaml, needle) in cases {
        let err = parse_execution_strategy(&fenced(yaml)).unwrap_err();
        assert!(err.0.contains(needle), "{yaml:?} -> {}", err.0);
    }
}

#[test]
fn a_declared_blocked_by_survives_normalization_and_an_absent_one_is_marked_undeclared() {
    let parsed = parse_execution_strategy(&fenced(
        "tasks:\n  - id: '1.1'\n    title: Setup\n  - id: '2.1'\n    title: Follow-up\n    blocked_by: ['1.1']\n  - id: '3.1'\n    title: Head\n    blocked_by: []\nwaves:\n  - wave: 1\n    tasks: ['1.1']\n",
    ))
    .unwrap();
    let tasks = &parsed["tasks"];
    assert_eq!(tasks[0]["blocked_by"], json!([]));
    assert_eq!(tasks[0]["blocked_by_declared"], json!(false));
    assert_eq!(tasks[1]["blocked_by"], json!(["1.1"]));
    assert_eq!(tasks[1]["blocked_by_declared"], json!(true));
    assert_eq!(tasks[2]["blocked_by"], json!([]));
    assert_eq!(tasks[2]["blocked_by_declared"], json!(true));
}

#[test]
fn task_edges_refuse_unknown_ids_and_cycles_through_inherited_wave_blockers() {
    let unknown = json!({"tasks": [
        {"id": "1.1", "blocked_by": ["9.9"], "blocked_by_declared": true},
    ]});
    assert_eq!(
        validate_task_edges(&unknown),
        ["task 1.1 blocked_by unknown task 9.9"]
    );

    // 2.1 declares nothing, so it inherits wave 1 (1.1) while 1.1 waits on 2.1.
    let cyclic = json!({
        "tasks": [
            {"id": "1.1", "blocked_by": ["2.1"], "blocked_by_declared": true},
            {"id": "2.1", "blocked_by": [], "blocked_by_declared": false},
        ],
        "waves": [
            {"wave": 1, "tasks": ["1.1"]},
            {"wave": 2, "tasks": ["2.1"]},
        ],
    });
    let errors = validate_task_edges(&cyclic);
    assert_eq!(errors.len(), 1, "{errors:?}");
    assert!(errors[0].contains("cycle"), "{errors:?}");
    assert!(errors[0].contains("1.1") && errors[0].contains("2.1"));
}

#[test]
fn section_parsers_read_overview_decisions_failure_modes_and_pattern_tables() {
    let doc = sample();
    let overview = extract_overview_paragraph(doc.get_section("Overview").unwrap());
    assert!(overview.starts_with("This is the first paragraph"));
    assert!(!overview.contains("Second paragraph"));

    let decisions = parse_locked_decisions(
        doc.get_section("Locked Decisions (DO NOT revisit)")
            .unwrap(),
    );
    assert_eq!(decisions.len(), 2);
    assert_eq!(decisions[0]["number"], json!(1));
    assert_eq!(decisions[0]["title"], json!("Use stdlib json"));
    assert_eq!(decisions[0]["rationale"], json!("zero extra dependency"));
    assert_eq!(decisions[0]["application"], json!("import json"));

    let modes = parse_failure_modes(doc.get_section("Failure Modes").unwrap());
    assert_eq!(modes.len(), 3);
    assert_eq!(modes[0]["category"], json!("Boundaries"));
    assert_eq!(modes[2]["category"], json!("Errors"));

    let patterns = parse_patterns(doc.get_section("Patterns to Reuse").unwrap());
    assert_eq!(patterns.len(), 2);
    assert_eq!(patterns[0]["pattern"], json!("resolve_repo_root()"));
    assert_eq!(patterns[1]["source"], json!("cli/src/fno/plan/brief.py"));
}

#[test]
fn acceptance_adapter_reads_numbered_shapes_and_swallows_a_malformed_section() {
    let entries = parse_acceptance_criteria(
        "1. Given a numbered criterion, when compiled, then it is visible.\n2. A second numbered criterion.\n",
    );
    let codes: Vec<&str> = entries
        .iter()
        .map(|e| e["code"].as_str().unwrap())
        .collect();
    assert_eq!(codes, ["AC1", "AC2"]);
    assert!(entries.iter().all(|e| e["ac_type"] == json!("GENERAL")));

    assert!(
        parse_acceptance_criteria("Just prose with no criterion shape at all.\nMore prose.\n")
            .is_empty()
    );
}

#[test]
fn fail_open_filters_keep_untagged_entries_and_other_tasks_codes_are_dropped() {
    let decisions = parse_locked_decisions(
        sample()
            .get_section("Locked Decisions (DO NOT revisit)")
            .unwrap(),
    );
    assert_eq!(
        filter_entries(decisions.clone(), "relevant", &["some/other/file.py"]).len(),
        2
    );
    assert!(filter_entries(decisions.clone(), "none", &[]).is_empty());
    assert_eq!(filter_entries(decisions, "all", &[]).len(), 2);

    let entry = |code: &str| json!({"code": code, "ac_type": "HP", "text": "t", "tags": []});
    let entries = vec![entry("AC1-HP"), entry("AC2-HP"), entry("AC4-EDGE")];
    let tagged: std::collections::HashSet<String> =
        ["AC1-HP", "AC2-HP"].iter().map(|s| s.to_string()).collect();
    let result = filter_acs_for_task(entries, &["AC1-HP".to_string()], Some(&tagged));
    let codes: Vec<&str> = result.iter().map(|e| e["code"].as_str().unwrap()).collect();
    // AC2-HP belongs to another task; AC4-EDGE is tagged by no task.
    assert_eq!(codes, ["AC1-HP", "AC4-EDGE"]);
}

#[test]
fn a_brief_carries_the_task_spec_scoped_acs_surface_files_and_matching_patterns() {
    let doc = sample();
    let brief = build_brief(&doc, "2.1", "relevant", "relevant")
        .ok()
        .unwrap();
    assert_eq!(brief.task_spec["id"], json!("2.1"));
    assert_eq!(brief.task_spec["title"], json!("CLI entry point"));
    assert_eq!(
        brief.verify_command,
        "uv run pytest cli/tests/unit/plan/test_brief.py -v"
    );
    assert!(brief
        .project_context
        .starts_with("This is the first paragraph"));
    assert_eq!(brief.files.len(), 2);
    assert_eq!(brief.acceptance_criteria.len(), 5);
    assert_eq!(brief.locked_decisions.len(), 2);
    let sources: Vec<&str> = brief
        .patterns
        .iter()
        .map(|p| p["source"].as_str().unwrap())
        .collect();
    assert_eq!(sources, ["cli/src/fno/plan/brief.py"]);

    // Task 1.1 asks for AC2-HP only; the other four codes are tagged for task 2.1.
    let first = build_brief(&doc, "1.1", "relevant", "relevant")
        .ok()
        .unwrap();
    let codes: Vec<&str> = first
        .acceptance_criteria
        .iter()
        .map(|a| a["code"].as_str().unwrap())
        .collect();
    assert_eq!(codes, ["AC2-HP"]);

    let quiet = build_brief(&doc, "2.1", "none", "all").ok().unwrap();
    assert!(quiet.failure_modes.is_empty());
}

#[test]
fn a_brief_renders_the_fixed_json_schema_and_the_markdown_sections() {
    let brief = build_brief(&sample(), "2.1", "relevant", "relevant")
        .ok()
        .unwrap();

    let value = brief.to_json();
    let mut keys: Vec<&str> = value
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort();
    assert_eq!(
        keys,
        [
            "acceptance_criteria",
            "failure_modes",
            "files",
            "locked_decisions",
            "patterns",
            "project_context",
            "task_spec",
            "verify_command",
        ]
    );
    let mut spec_keys: Vec<&str> = value["task_spec"]
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    spec_keys.sort();
    assert_eq!(
        spec_keys,
        ["acceptance", "id", "notes", "surface", "title", "verify"]
    );

    let md = brief.to_markdown();
    assert!(md.starts_with("## Project Context\n"), "{md}");
    assert!(md.contains("## Task 2.1: CLI entry point"));
    assert!(md.contains("- `cli/src/fno/plan/brief.py`"));
    assert!(md.contains("**AC2-HP:** "));
    assert!(md.contains("1. **Use stdlib json**"));
    assert!(md.contains("   *How to apply:* import json"));
    assert!(md.contains("**Boundaries**"));
    assert!(md.contains("### Verify Command\n\n```\nuv run pytest"));
}

#[test]
fn a_brief_refuses_an_unknown_task_a_missing_strategy_and_a_malformed_strategy() {
    let unknown = build_brief(&sample(), "9.9", "relevant", "relevant")
        .err()
        .unwrap();
    assert!(unknown.0.contains("'9.9' not found"), "{unknown}");
    assert!(unknown.0.contains("'1.1', '2.1'"), "{unknown}");

    let no_strategy =
        load_plan_text("---\nstatus: ready\n---\n\n## Overview\n\nOnly this.\n").unwrap();
    let err = build_brief(&no_strategy, "1.1", "relevant", "relevant")
        .err()
        .unwrap();
    assert!(err.0.contains("missing ## Execution Strategy"), "{err}");

    let malformed = load_plan_text(
        "---\nstatus: ready\n---\n\n## Execution Strategy\n\n```yaml\ntasks:\n  - id: [unclosed list\n    title: bad\n```\n",
    )
    .unwrap();
    let err = build_brief(&malformed, "1.1", "relevant", "relevant")
        .err()
        .unwrap();
    assert!(err.0.starts_with("exit3:"), "{err}");
}
