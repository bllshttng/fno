//! parity-stage: differential
//! parity-oracle: fno.adapters.providers.runtime_state
//!
//! Differential parity for `accounts::state`, the port of the read side of
//! `runtime_state.py`. Each `tests/golden/accounts_state_read/*.json` case
//! carries the state file it was captured against, the op and its inputs,
//! and the Python leg's answer under `expect`, frozen at `NOW` by
//! `accounts_state_read_golden_driver.py` (the case table lives there;
//! re-run it to add a case or re-freeze). This test writes the same file,
//! runs the Rust op at `now = NOW` and asserts equal JSON.

use fno_agents::accounts::state;
use fno_agents::fallback_chain::Verdict;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

const NOW: f64 = 1_789_400_000.0;

fn run_op(case: &Value, path: &Path) -> Value {
    let id = case["id"].as_str().unwrap_or_default();
    match case["op"].as_str().unwrap() {
        "read_state" => state::read_state(path, NOW).to_json(),
        "is_in_cooldown" => json!(state::is_in_cooldown(path, id, case["model"].as_str(), NOW)),
        "read_usage" => state::read_usage(path, id, case["ttl"].as_f64().unwrap(), NOW)
            .map_or(Value::Null, |s| s.to_json()),
        "headroom" => {
            let h = state::headroom(
                path,
                id,
                NOW,
                case["ttl"].as_f64().unwrap(),
                case["threshold"].as_f64().unwrap(),
            );
            let verdict = match h.verdict {
                Verdict::Exhausted => "exhausted",
                Verdict::Low => "low",
                Verdict::Ok => "ok",
                Verdict::Unknown => "unknown",
            };
            json!({
                "state": verdict,
                "resets_at": h.resets_at,
                "source": h.source,
                "observed_at": h.observed_at,
            })
        }
        "project_window" => {
            let span = case["window_minutes"].as_f64().map(|m| m * 60.0);
            let p = state::project_window(path, id, span, NOW);
            json!({
                "opened_at": p.opened_at,
                "closes_at": p.closes_at,
                "closes_in_s": p.closes_in_s,
                "warned": p.warned,
            })
        }
        "read_cursor" => state::read_cursor(
            path,
            case["combo"].as_str().unwrap(),
            case["hash"].as_str().unwrap(),
            NOW,
        )
        .map_or(Value::Null, |c| c.to_json()),
        other => panic!("unknown op {other}"),
    }
}

#[test]
fn accounts_state_read_matches_the_python_leg() {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/golden/accounts_state_read");
    let mut cases: Vec<PathBuf> = std::fs::read_dir(dir)
        .unwrap()
        .filter_map(|entry| entry.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|e| e == "json"))
        .collect();
    cases.sort();
    assert!(cases.len() >= 42, "golden cases missing: {}", cases.len());
    let tmp = tempfile::tempdir().unwrap();
    let failures: Vec<String> = cases
        .iter()
        .enumerate()
        .filter_map(|(i, golden)| {
            let case: Value =
                serde_json::from_str(&std::fs::read_to_string(golden).unwrap()).unwrap();
            let path = tmp.path().join(format!("state-{i}.json"));
            match &case["state"] {
                Value::Null => {}
                Value::String(text) => std::fs::write(&path, text).unwrap(),
                other => std::fs::write(&path, other.to_string()).unwrap(),
            }
            let got = run_op(&case, &path);
            (got != case["expect"]).then(|| {
                format!(
                    "{}\n  expect: {}\n  rust:   {}",
                    golden.file_name().unwrap().to_string_lossy(),
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
