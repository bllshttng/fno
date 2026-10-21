//! Climb verdict tests: one per verdict family the plan names (propose,
//! overfit, underpowered). Rows carry `bank_rev` equal to the split pin, so
//! the per-rev content check short-circuits and no test needs a git repo.

use super::{climb_payload, ClimbArgs};
use serde_json::{json, Value};
use std::fs;
use tempfile::TempDir;

const PIN: &str = "1111111111111111111111111111111111111111";

fn row(
    task: &str,
    pass: bool,
    cohort: &str,
    model: &str,
    lane_status: &str,
    usage: Value,
) -> String {
    let mut v = json!({
        "ts": "2026-10-20T12:00:00Z", "task_id": task, "tier": "capability",
        "pass": pass, "status": "graded", "lane_status": lane_status,
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

/// Held-out rows over the declared qualification tasks, `passes` of
/// `total` passing, every row `ok` (scored).
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
                "ok",
                usage.clone(),
            )
        })
        .collect()
}

/// Trial-shaped rows for the power gate: `ok_total` scored rows, then
/// `subst_total` graded-but-substituted rows the rates never read.
fn substituted_rows(
    cohort: &str,
    model: &str,
    ok_passes: usize,
    ok_total: usize,
    subst_total: usize,
    usage: Value,
) -> Vec<String> {
    let tasks = ["h1", "h2", "h3", "h4"];
    let mut out: Vec<String> = (0..ok_total)
        .map(|i| {
            row(
                tasks[i % tasks.len()],
                i < ok_passes,
                cohort,
                model,
                "ok",
                usage.clone(),
            )
        })
        .collect();
    out.extend((0..subst_total).map(|i| {
        row(
            tasks[i % tasks.len()],
            true,
            cohort,
            model,
            "substituted",
            usage.clone(),
        )
    }));
    out
}

fn train_rows(cohort: &str, model: &str, passes: usize, total: usize) -> Vec<String> {
    (0..total)
        .map(|i| {
            row(
                if i % 2 == 0 { "t1" } else { "t2" },
                i < passes,
                cohort,
                model,
                "ok",
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
            // Quoted: a bare 40-digit pin parses as a YAML number, and the
            // loader demands a string.
            "bank_rev: '{PIN}'\ntrain: [t1, t2]\nvalidation: []\nqualification: [h1, h2, h3, h4]\n"
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
    // Trial 17 of 20 puts its Wilson lower bound (about 0.64) above the
    // baseline rate minus the noise floor (0.75 - 0.15), so the tie arm's
    // interval condition holds; the intervals never need to separate here.
    let prices = json!({
        "m-base": {"input_per_m": 2.0, "output_per_m": 0.0},
        "m-trial": {"input_per_m": 1.0, "output_per_m": 0.0},
    });
    let mut lines = heldout_rows("base", "m-base", 15, 20, priced_usage());
    lines.extend(heldout_rows("trial", "m-trial", 17, 20, priced_usage()));
    lines.extend(train_rows("base", "m-base", 2, 4));
    lines.extend(train_rows("trial", "m-trial", 2, 4));
    let v = climb_of(&tmp, &lines, Some(prices));
    assert_eq!(v["verdict"], "propose", "reason: {}", v["reason"]);
    assert_eq!(v["heldout"]["baseline"]["scored"], 20);
    assert_eq!(v["heldout"]["trial"]["scored"], 20);
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
    // Train +20 points on 20 scored train rows per cohort, held-out flat at
    // the same cost: the revert shape, decided before any cost rule.
    let mut lines = heldout_rows("base", "m-base", 15, 20, Value::Null);
    lines.extend(heldout_rows("trial", "m-trial", 15, 20, Value::Null));
    lines.extend(train_rows("base", "m-base", 10, 20));
    lines.extend(train_rows("trial", "m-trial", 14, 20));
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
    // The run-lane-dries-up shape: 20 graded trial rows, 16 of them
    // substituted. Only 4 rows score, so the floor reads underpowered. On
    // the old graded-count floor this run reached the cost rules and read
    // hold, so the assertion discriminates the two floors.
    let prices = json!({
        "m-base": {"input_per_m": 2.0, "output_per_m": 0.0},
        "m-trial": {"input_per_m": 1.0, "output_per_m": 0.0},
    });
    let mut lines = heldout_rows("base", "m-base", 15, 20, priced_usage());
    lines.extend(substituted_rows(
        "trial",
        "m-trial",
        4,
        4,
        16,
        priced_usage(),
    ));
    lines.extend(train_rows("base", "m-base", 2, 4));
    lines.extend(train_rows("trial", "m-trial", 2, 2));
    let v = climb_of(&tmp, &lines, Some(prices));
    assert_eq!(v["verdict"], "underpowered", "reason: {}", v["reason"]);
    assert_eq!(v["heldout"]["trial"]["scored"], 4);
    assert!(v.get("accept").is_none());
}
