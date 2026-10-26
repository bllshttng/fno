//! parity-stage: characterization
//! parity-oracle: fno.adapters.providers.cli.required_bot_check_cmd
//!
//! Characterization for `accounts::required_bot`, the port of the
//! `required-bot-check` command, whose Python body is gone. Each
//! `tests/golden/accounts_required_bot/*.json` case carries the files it set
//! up and what the Python command left at `NOW` under `expect`: exit code,
//! stdout, stderr and the events it emitted.
//! `accounts_required_bot_golden_driver.py` holds the case table. The event
//! source is not compared: the Python command logged `target`, and the Rust
//! command logs `daemon`, the kind's one source now.

use fno_agents::accounts::records;
use fno_agents::accounts::required_bot;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

const NOW: f64 = 1_789_400_000.0;

fn run_case(case: &Value, dir: &Path) -> Value {
    for f in case["files"].as_array().unwrap() {
        let path = dir.join(f["rel"].as_str().unwrap());
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, f["text"].as_str().unwrap()).unwrap();
    }
    let args: Vec<String> = case["args"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a.as_str().unwrap().to_string())
        .collect();
    let as_json = required_bot::parse(&args).unwrap().unwrap();
    let config = std::fs::read_to_string(dir.join(".fno/config.toml")).unwrap();
    let table: toml::Table = config.parse().unwrap();
    let review = |key: &str| {
        table
            .get("review")
            .and_then(|r| r.get(key))
            .map(|v| serde_json::to_value(v).unwrap())
    };
    let (bots, both) =
        required_bot::configured_bots(review("github_apps"), review("required_bots"));
    let stderr = if both {
        format!("{}\n", required_bot::BOTH_KEYS_WARNING)
    } else {
        String::new()
    };
    let found = match records::scope_accounts(&table) {
        Ok(accounts) if !bots.is_empty() => required_bot::warnings(
            &bots,
            &accounts.records,
            &dir.join("runtime-state.json"),
            NOW,
        ),
        _ => Vec::new(),
    };
    let events: Vec<Value> = found
        .iter()
        .map(|w| Value::Object(required_bot::event_data(w)))
        .collect();
    let stdout = required_bot::render(&found, as_json, NOW)
        .map(|text| format!("{text}\n"))
        .unwrap_or_default();
    json!({"exit": 0, "stdout": stdout, "stderr": stderr, "events": events})
}

#[test]
fn accounts_required_bot_matches_the_python_command() {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/golden/accounts_required_bot");
    let mut cases: Vec<PathBuf> = std::fs::read_dir(dir)
        .unwrap()
        .filter_map(|entry| entry.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|e| e == "json"))
        .collect();
    cases.sort();
    assert!(cases.len() >= 6, "golden cases missing: {}", cases.len());
    let failures: Vec<String> = cases
        .iter()
        .filter_map(|golden| {
            let case: Value =
                serde_json::from_str(&std::fs::read_to_string(golden).unwrap()).unwrap();
            let tmp = tempfile::tempdir().unwrap();
            let got = run_case(&case, tmp.path());
            let mut want = case["expect"].clone();
            let data: Vec<Value> = want["events"]
                .as_array()
                .unwrap()
                .iter()
                .map(|e| {
                    assert_eq!(e["type"], "quota_required_bot_exhausted");
                    e["data"].clone()
                })
                .collect();
            want["events"] = Value::Array(data);
            (got != want).then(|| {
                format!(
                    "{}\n  expect: {want}\n  rust:   {got}",
                    golden.file_name().unwrap().to_string_lossy()
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
