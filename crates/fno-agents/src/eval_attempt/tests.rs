use super::{classify, classify_rows, run_evals_attempt, validate_cohorts};
use serde_json::json;

fn decl() -> serde_json::Value {
    json!({
        "train": ["t1", "t2"],
        "validation": ["v1"],
        "qualification": ["q1", "q2"],
    })
}

#[test]
fn cohorts_valid_split_passes() {
    let (ok, errors, roles) = validate_cohorts(&decl(), None, None);
    assert!(ok, "unexpected errors: {errors:?}");
    assert!(errors.is_empty());
    assert!(roles.is_empty());
}

#[test]
fn cohorts_unknown_id_is_refused() {
    let known = json!(["t1", "v1", "q1"]);
    let (ok, errors, _) = validate_cohorts(&decl(), Some(&known), None);
    assert!(!ok);
    assert!(errors
        .iter()
        .any(|e| e.contains("t2") && e.contains("unknown")));
    assert!(errors
        .iter()
        .any(|e| e.contains("q2") && e.contains("unknown")));
}

#[test]
fn cohorts_overlap_is_refused() {
    let mut bad = decl();
    bad["qualification"] = json!(["q1", "t1"]);
    let (ok, errors, _) = validate_cohorts(&bad, None, None);
    assert!(!ok);
    assert!(errors
        .iter()
        .any(|e| e.contains("'t1'") && e.contains("both")));
}

// --- the cohort door: aggregate fold, train export, yaml load --------------

use super::{export_train, load_cohorts_yaml, qualify_aggregate};

fn rows_text() -> String {
    let graded = json!({
        "task_id": "q1", "tier": "regression", "pass": true,
        "bank_rev": "rev123",
        "obs": {"fixture_prepared": true, "worker_required": false,
                 "grader_ran": true, "grader_passed": true},
    });
    let infra = json!({
        "task_id": "q1", "tier": "regression", "pass": false,
        "obs": {"fixture_prepared": false},
    });
    let legacy = json!({"task_id": "q1", "tier": "regression", "pass": false});
    let wrong_rev = json!({
        "task_id": "q1", "tier": "regression", "pass": true,
        "bank_rev": "other",
        "obs": {"fixture_prepared": true, "worker_required": false,
                 "grader_ran": true, "grader_passed": true},
    });
    format!("{graded}\n{infra}\n{legacy}\n{wrong_rev}\n")
}

fn qual_decl() -> serde_json::Value {
    json!({
        "bank_rev": "rev123",
        "train": [],
        "validation": [],
        "qualification": ["q1"],
    })
}

#[test]
fn aggregate_folds_valid_grades_and_excludes_wrong_rev_and_legacy() {
    let out = qualify_aggregate(&rows_text(), &qual_decl(), None, None, None);
    assert_eq!(out["qualified"], true);
    assert_eq!(out["valid_grades"], 1);
    assert_eq!(out["passes"], 1);
    assert_eq!(out["attempts"]["graded"], 1);
    assert_eq!(out["attempts"]["infrastructure"], 1);
    assert_eq!(out["excluded"]["wrong_rev"], 1);
    assert_eq!(out["excluded"]["legacy"], 1);
    assert_eq!(out["missing_tasks"], 0);
}

#[test]
fn aggregate_refuses_when_the_bank_changed_since_the_pin() {
    // A repo dir with no git metadata: the rev check cannot certify, so it
    // refuses (the same fail-closed shape as an unreadable pin).
    let tmp = tempfile::TempDir::new().unwrap();
    let out = qualify_aggregate(
        &rows_text(),
        &qual_decl(),
        None,
        Some(tmp.path().to_str().unwrap()),
        None,
    );
    assert_eq!(out["qualified"], false);
    assert!(out["reason"].as_str().unwrap().contains("bank changed"));
}

#[test]
fn export_train_writes_only_train_rows() {
    let tmp = tempfile::TempDir::new().unwrap();
    let out_path = tmp.path().join("tuning.jsonl");
    let decl = json!({
        "bank_rev": "rev123",
        "train": ["q1", "t9"],
        "validation": [],
        "qualification": [],
    });
    let out = export_train(&rows_text(), &decl, None, None, out_path.to_str().unwrap()).unwrap();
    assert_eq!(out["exported"], 4); // every q1 row, including infra + legacy
    let text = std::fs::read_to_string(&out_path).unwrap();
    let lines: Vec<serde_json::Value> = text
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(lines.len(), 4);
    assert!(lines.iter().all(|r| r["role"] == "train"));
    assert!(lines.iter().all(|r| r["task_id"] == "q1"));
}

#[test]
fn cohorts_yaml_loads_shape_and_defaults_missing_roles() {
    let tmp = tempfile::TempDir::new().unwrap();
    let path = tmp.path().join("cohorts.yaml");
    std::fs::write(&path, "bank_rev: abc123\ntrain: [t1]\n").unwrap();
    let decl = load_cohorts_yaml(path.to_str().unwrap()).unwrap();
    assert_eq!(decl["bank_rev"], "abc123");
    assert_eq!(decl["train"], json!(["t1"]));
    assert_eq!(decl["qualification"], json!([]));
}

fn obs(extra: serde_json::Value) -> serde_json::Value {
    json!({ "obs": extra })
}

#[test]
fn fixture_failure_is_infrastructure_and_retryable() {
    let v = classify(
        &obs(json!({ "fixture_prepared": false, "worker_required": true })),
        None,
    );
    assert_eq!(v.status, "infrastructure");
    assert!(v.retryable);
    assert_eq!(v.graded, None);
}

#[test]
fn graded_pass_is_final_and_not_retryable() {
    let v = classify(
        &obs(json!({
            "fixture_prepared": true, "worker_required": true,
            "worker_started": true, "grader_ran": true, "grader_passed": true,
        })),
        None,
    );
    assert_eq!(v.status, "graded");
    assert_eq!(v.graded, Some(true));
    assert!(!v.retryable);
}

#[test]
fn graded_task_failure_is_distinct_from_infrastructure() {
    // AC1-EDGE: the worker did the task wrong - a valid grade of false, never
    // an infrastructure/unavailable retryable row.
    let v = classify(
        &obs(json!({
            "fixture_prepared": true, "worker_required": true,
            "worker_started": true, "grader_ran": true, "grader_passed": false,
        })),
        None,
    );
    assert_eq!(v.status, "graded");
    assert_eq!(v.graded, Some(false));
    assert!(!v.retryable);
}

#[test]
fn worker_ran_without_grader_result_is_ungraded() {
    let v = classify(
        &obs(json!({
            "fixture_prepared": true, "worker_required": true,
            "worker_started": true, "grader_ran": true,
        })),
        None,
    );
    assert_eq!(v.status, "ungraded");
    assert!(v.retryable);
}

#[test]
fn legacy_row_without_obs_is_never_guessed() {
    // AC1-EDGE: a pre-attempt row carries pass:false, but there is no
    // structured evidence - the classifier must not reinterpret the boolean.
    let v = classify(
        &json!({ "task_id": "t", "pass": false, "reason": "spawn exit 1" }),
        None,
    );
    assert_eq!(v.status, "legacy");
    assert_eq!(v.graded, None);
    assert!(!v.retryable);
}

#[test]
fn expected_rev_drives_rev_match() {
    let row = obs(
        json!({ "fixture_prepared": true, "worker_required": false, "grader_ran": true, "grader_passed": true }),
    );
    let mut with_rev = row.clone();
    with_rev["bank_rev"] = json!("abc123");
    assert_eq!(classify(&with_rev, Some("abc123")).rev_match, Some(true));
    assert_eq!(classify(&with_rev, Some("def456")).rev_match, Some(false));
    // No bank_rev on the row: provenance unknown, not a proven mismatch.
    assert_eq!(classify(&row, Some("abc123")).rev_match, None);
    // No expectation supplied: the field stays null, not false.
    assert_eq!(classify(&with_rev, None).rev_match, None);
}

#[test]
fn classify_rows_numbers_lines_and_tolerates_corrupt_rows() {
    let text = concat!(
        "{\"obs\":{\"fixture_prepared\":true,\"worker_required\":false,\"grader_ran\":true,\"grader_passed\":true}}\n",
        "{broken json\n",
        "{\"pass\": false}\n",
    );
    let out = classify_rows(text, None);
    let arr = out.as_array().unwrap();
    assert_eq!(arr.len(), 3);
    assert_eq!(arr[0]["line"], 1);
    assert_eq!(arr[0]["status"], "graded");
    assert_eq!(arr[1]["line"], 2);
    assert_eq!(arr[1]["status"], "legacy");
    assert_eq!(arr[2]["line"], 3);
    assert_eq!(arr[2]["status"], "legacy");
}

#[test]
fn cli_row_json_prints_one_verdict() {
    let args: Vec<String> = vec![
        "--row-json".into(),
        obs(json!({ "fixture_prepared": false })).to_string(),
    ];
    // Cannot capture println here; assert the exit code and rely on the
    // classify tests for the payload.
    assert_eq!(run_evals_attempt(&args), 0);
}

// --- the split pin excludes the declaration file it names ------------------

use super::bank_unchanged;
use std::process::Command;

/// One commit inside a throwaway git repo; returns the sha.
fn commit(dir: &std::path::Path, message: &str) -> String {
    let run = |args: &[&str]| {
        let out = Command::new("git")
            .args(["-c", "user.email=t@t", "-c", "user.name=t"])
            .args(args)
            .current_dir(dir)
            .output()
            .expect("git runs");
        assert!(
            out.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        out
    };
    run(&["add", "-A"]);
    let out = run(&["commit", "-m", message]);
    let sha = String::from_utf8_lossy(&out.stdout).trim().to_string();
    assert!(!sha.is_empty(), "commit produced no sha");
    sha
}

#[test]
fn pin_ignores_cohorts_yaml_but_not_task_files() {
    let tmp = tempfile::TempDir::new().unwrap();
    let dir = tmp.path();
    let bank = dir.join("evals/bank");
    std::fs::create_dir_all(&bank).unwrap();
    let init = Command::new("git")
        .args(["init", "-q"])
        .current_dir(dir)
        .output()
        .unwrap();
    assert!(init.status.success(), "git init failed");

    std::fs::write(bank.join("capability-lane-blueprint.yaml"), "id: t1\n").unwrap();
    let pinned = commit(dir, "the bank task the split pins");

    // The declaration lands after the pin and names it: the pin must hold.
    std::fs::write(
        bank.join("cohorts.yaml"),
        format!("bank_rev: {pinned}\ntrain: [t1]\nvalidation: []\nqualification: []\n"),
    )
    .unwrap();
    commit(dir, "declare the split against the pinned bank");
    assert!(
        bank_unchanged(dir.to_str().unwrap(), &pinned),
        "a cohorts.yaml commit alone must not void the pin"
    );

    // A task change is what the pin exists to catch.
    std::fs::write(
        bank.join("capability-lane-blueprint.yaml"),
        "id: t1\ngrade: changed\n",
    )
    .unwrap();
    commit(dir, "change a task after the pin");
    assert!(
        !bank_unchanged(dir.to_str().unwrap(), &pinned),
        "a task change after the pin must void it"
    );
}
