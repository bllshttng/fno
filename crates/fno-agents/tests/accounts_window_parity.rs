//! parity-stage: characterization
//! parity-oracle: fno.adapters.providers.cli.window_providers
//!
//! Characterization for `accounts::window`, the port of the `window` command,
//! whose Python body is gone. Each `tests/golden/accounts_window/*.json` case
//! carries the files it set up, the warn writer's answer per record, and what
//! the Python command left at `NOW` under `expect`: exit code, stdout, stderr,
//! the records it asked to warn, and the events it emitted.
//! `accounts_window_golden_driver.py` holds the case table. The event source
//! is not compared: the Python command logged `cli`, and the Rust command
//! logs `daemon`, the kind's one source now.

use fno_agents::accounts::records::{self, Record};
use fno_agents::accounts::window::{self, Effects};
use serde_json::{json, Map, Value};
use std::cell::RefCell;
use std::path::{Path, PathBuf};

const NOW: f64 = 1_789_400_000.0;

fn run_case(case: &Value, dir: &Path) -> Value {
    let at = |text: &str| text.replace("{dir}", &dir.display().to_string());
    for f in case["files"].as_array().unwrap() {
        let path = dir.join(f["rel"].as_str().unwrap());
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, at(f["text"].as_str().unwrap())).unwrap();
    }
    let args: Vec<String> = case["args"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a.as_str().unwrap().to_string())
        .collect();
    let (as_json, threshold) = window::parse(&args).unwrap().unwrap();
    let config = std::fs::read_to_string(dir.join(".fno/config.toml")).unwrap();
    let table: toml::Table = config.parse().unwrap();
    let records: Vec<Record> = match records::scope_accounts(&table) {
        Ok(accounts) => accounts.records,
        Err(e) => {
            let stderr = format!("error: {e}\n");
            let stderr = if case["stderr_prefix"].as_bool() == Some(true) {
                let want = case["expect"]["stderr"].as_str().unwrap();
                if stderr.starts_with(want) {
                    want.to_string()
                } else {
                    stderr
                }
            } else {
                stderr
            };
            return json!({"exit": 1, "stdout": "", "stderr": stderr,
                          "warn_calls": [], "events": []});
        }
    };
    let calls = RefCell::new(Vec::<String>::new());
    let events = RefCell::new(Vec::<Value>::new());
    let warn = |id: &str| {
        calls.borrow_mut().push(id.to_string());
        case["warned"][id].as_bool().unwrap_or(false)
    };
    let emit = |data: Map<String, Value>| events.borrow_mut().push(Value::Object(data));
    let rows = window::collect(
        &records,
        &dir.join("runtime-state.json"),
        threshold,
        NOW,
        &Effects {
            warn: &warn,
            emit: &emit,
        },
    );
    let stdout = format!("{}\n", window::render(&records, &rows, as_json, NOW));
    json!({
        "exit": 0,
        "stdout": stdout.replace(&dir.display().to_string(), "{dir}"),
        "stderr": "",
        "warn_calls": calls.into_inner(),
        "events": events.into_inner(),
    })
}

#[test]
fn accounts_window_matches_the_python_command() {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/golden/accounts_window");
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
                    assert_eq!(e["type"], "provider_window_closing");
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
