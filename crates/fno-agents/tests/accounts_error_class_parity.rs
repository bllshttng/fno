//! parity-stage: differential
//! parity-oracle: fno.adapters.providers.error_taxonomy
//!
//! Differential parity for `accounts::error_class`, the port of
//! `error_taxonomy.py`. Each `tests/golden/accounts_error_class/*.json` case
//! names an op, its inputs and the Python leg's answer under `expect`,
//! captured with the clock pinned to `NOW` by
//! `accounts_error_class_golden_driver.py` (the case table lives there;
//! re-run it to add a case or re-freeze). This test runs the Rust op with
//! `now = NOW` and asserts equal JSON.

use fno_agents::accounts::error_class::{self, Outcome, ERROR_RULES};
use serde_json::{json, Value};
use std::path::PathBuf;

const NOW: f64 = 1_789_400_000.0;

fn run_op(case: &Value) -> Value {
    let body = case["body"].as_str();
    match case["op"].as_str().unwrap() {
        "normalize" => {
            let outcome = Outcome {
                http_status: case["status"].as_i64(),
                exit_code: case.get("exit_code").and_then(Value::as_i64),
                body: body.unwrap(),
                parser_failed: case
                    .get("parser_failed")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
                model: case.get("model").and_then(Value::as_str),
                reset_timezone: case.get("tz").and_then(Value::as_str),
                now: NOW,
            };
            match error_class::normalize(&outcome) {
                Ok(got) => got.to_json(),
                Err(e) => json!({ "error": e }),
            }
        }
        "classify_error" => error_class::classify_error(case["status"].as_i64(), body)
            .map_or(Value::Null, |rule| rule.to_json()),
        "rules" => Value::Array(ERROR_RULES.iter().map(|r| r.to_json()).collect()),
        "parse_reset_stamp" => {
            let (epoch, unparsed) =
                error_class::parse_reset_stamp(body.unwrap(), case["tz"].as_str(), NOW);
            json!([epoch, unparsed])
        }
        "window" => json!(error_class::reset_window_seconds_from(body.unwrap())),
        other => panic!("unknown op {other}"),
    }
}

#[test]
fn accounts_error_class_matches_the_python_leg() {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/golden/accounts_error_class");
    let mut cases: Vec<PathBuf> = std::fs::read_dir(dir)
        .unwrap()
        .filter_map(|entry| entry.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|e| e == "json"))
        .collect();
    cases.sort();
    assert!(cases.len() >= 60, "golden cases missing: {}", cases.len());
    let failures: Vec<String> = cases
        .iter()
        .filter_map(|path| {
            let case: Value =
                serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
            let got = run_op(&case);
            (got != case["expect"]).then(|| {
                format!(
                    "{}\n  expect: {}\n  rust:   {}",
                    path.file_name().unwrap().to_string_lossy(),
                    case["expect"],
                    got
                )
            })
        })
        .collect();
    assert!(
        failures.is_empty(),
        "{} case(s) differ:\n{}",
        failures.len(),
        failures.join("\n")
    );
}
