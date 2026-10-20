//! Climb verdict tests: one per verdict family the plan names (propose,
//! overfit, underpowered). Rows carry `bank_rev` equal to the split pin, so
//! the per-rev content check short-circuits and no test needs a git repo.

use super::{climb_payload, ClimbArgs};
use serde_json::{json, Value};
use std::fs;
use tempfile::TempDir;

const PIN: &str = "1111111111111111111111111111111111111111";

fn row(task: &str, pass: bool, cohort: &str, model: &str, usage: Value) -> String {
    let mut v = json!({
        "ts": "2026-10-20T12:00:00Z", "task_id": task, "tier": "capability",
        "pass": pass, "status": "graded", "lane_status": "ok",
        "experiment_id": cohort, "bank_rev": PIN, "observed_model": model,
    });
    if !usage.is_null() {
        v["usage"] = usage;
    }
    v.to_string()
}

fn priced_usage() -> Value {
    json!({"input": 1_000_000, "output": 0})
}

/// Held-out rows spread over the declared qualification tasks, `passes` of
/// `total` passing, one row per list entry.
fn heldout_rows(
    cohort: &str,
    model: &str,
    passes: usize,
    total: usize,
    usage: Value,
) -> Vec<String> {
    let tasks = ["h1", "h2", "h3", "h4"];
    (0..total)
        .map(|i| {
            row(
                tasks[i % tasks.len()],
                i < passes,
                cohort,
                model,
                usage.clone(),
            )
        })
        .collect()
}

fn train_rows(cohort: &str, model: &str, passes: usize, total: usize) -> Vec<String> {
    (0..total)
        .map(|i| {
            row(
                if i % 2 == 0 { "t1" } else { "t2" },
                i < passes,
                cohort,
                model,
                Value::Null,
            )
        })
        .collect()
}

fn climb_of(tmp: &TempDir, lines: &[String], prices: Option<Value>) -> Value {
    let history = tmp.path().join("history.jsonl");
    fs::write(&history, lines.join("\n") + "\n").unwrap();
    let yaml = tmp.path().join("cohorts.yaml");
    fs::write(
        &yaml,
        format!(
            "bank_rev: {PIN}\ntrain: [t1, t2]\nvalidation: []\nqualification: [h1, h2, h3, h4]\n"
        ),
    )
    .unwrap();
    let payload = climb_payload(&ClimbArgs {
        history: history.to_string_lossy().as_ref(),
        baseline: "base",
        trial: "trial",
        cohorts_yaml: Some(yaml.to_string_lossy().as_ref()),
        stage: Some("review"),
        trial_lane: Some("glm-flash"),
        prices: prices.as_ref(),
        json_out: false,
    });
    assert!(payload.is_ok(), "climb refused: {:?}", payload.err());
    payload.unwrap()
}

#[test]
fn propose_within_noise_and_cheaper_prints_the_accept_line() {
    let tmp = TempDir::new().unwrap();
    let prices = json!({
        "m-base": {"input_per_m": 2.0},
        "m-trial": {"input_per_m": 1.0},
    });
    let mut lines = heldout_rows("base", "m-base", 15, 20, priced_usage());
    lines.extend(heldout_rows("trial", "m-trial", 16, 20, priced_usage()));
    lines.extend(train_rows("base", "m-base", 2, 4));
    lines.extend(train_rows("trial", "m-trial", 2, 4));
    let v = climb_of(&tmp, &lines, Some(prices));
    assert_eq!(v["verdict"], "propose", "reason: {}", v["reason"]);
    assert_eq!(v["heldout"]["baseline"]["graded"], 20);
    assert_eq!(v["heldout"]["trial"]["graded"], 20);
    assert_eq!(v["wrong_rev"], 0);
    assert_eq!(v["unsplit"], 0);
    let accept = v["accept"].as_str().unwrap_or_default();
    assert!(
        accept.contains("agents.profiles.review.lanes '[\"glm-flash\""),
        "accept must name the trial lane first: {accept}"
    );
    assert!(
        accept.contains("on_exhausted degrade"),
        "accept must set the exhausted-lane policy: {accept}"
    );
}

#[test]
fn train_up_heldout_flat_reads_overfit_without_an_accept_line() {
    let tmp = TempDir::new().unwrap();
    // Train +20 points while held-out is flat is the article's revert shape;
    // it fires before the cost rules, so no prices are needed.
    let mut lines = heldout_rows("base", "m-base", 15, 20, Value::Null);
    lines.extend(heldout_rows("trial", "m-trial", 15, 20, Value::Null));
    lines.extend(train_rows("base", "m-base", 5, 10));
    lines.extend(train_rows("trial", "m-trial", 7, 10));
    let v = climb_of(&tmp, &lines, None);
    assert_eq!(v["verdict"], "overfit", "reason: {}", v["reason"]);
    assert!(
        v.get("accept").is_none(),
        "overfit never prints an accept line"
    );
}

#[test]
fn few_graded_heldout_rows_read_underpowered_never_propose() {
    let tmp = TempDir::new().unwrap();
    // A gate-blocked run shape: the trial cohort mostly never graded, and
    // the rows that did are perfect. The verdict stays underpowered.
    let prices = json!({"m-base": {"input_per_m": 2.0}, "m-trial": {"input_per_m": 0.001}});
    let mut lines = heldout_rows("base", "m-base", 15, 20, priced_usage());
    lines.extend(heldout_rows("trial", "m-trial", 12, 12, priced_usage()));
    lines.extend(train_rows("base", "m-base", 2, 4));
    lines.extend(train_rows("trial", "m-trial", 2, 2));
    let v = climb_of(&tmp, &lines, Some(prices));
    assert_eq!(v["verdict"], "underpowered", "reason: {}", v["reason"]);
    assert!(v.get("accept").is_none());
}
