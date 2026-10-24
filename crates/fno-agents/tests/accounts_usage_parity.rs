//! parity-stage: differential
//! parity-oracle: fno.adapters.providers.cli
//!
//! Differential parity for `accounts::usage`, the port of the `usage`
//! command. Each `tests/golden/accounts_usage/*.json` case carries the files
//! it set up, the probe answer per record, the cache-write result and the
//! slot principal, and the Python command's stdout under `expect`, frozen at
//! `NOW` by `accounts_usage_golden_driver.py` (the case table lives there).

use fno_agents::accounts::binding::SlotProbe;
use fno_agents::accounts::records::{self, Record};
use fno_agents::accounts::state::{self, Snapshot};
use fno_agents::accounts::usage::{self, Inputs};
use fno_agents::claude_vault::{ExternalFailure, Principal};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

const NOW: f64 = 1_789_400_000.0;

struct FakeSlot(Option<Principal>);

impl SlotProbe for FakeSlot {
    fn keychain(&self, _service: &str) -> Result<Option<String>, ExternalFailure> {
        Ok(self.0.as_ref().map(|_| {
            json!({"claudeAiOauth": {"accessToken": "tok", "refreshToken": "r"}}).to_string()
        }))
    }
    fn profile(&self, _bearer: &str) -> Result<Principal, ExternalFailure> {
        self.0.clone().ok_or(ExternalFailure::Unavailable)
    }
}

fn principal(v: &Value) -> Option<Principal> {
    Some(Principal {
        account_uuid: v.get("account_uuid")?.as_str()?.to_string(),
        organization_uuid: v.get("organization_uuid")?.as_str()?.to_string(),
        email: v.get("email").and_then(Value::as_str).map(str::to_string),
    })
}

fn snapshot(v: &Value) -> Option<Snapshot> {
    let windows: Vec<Value> = v["windows"]
        .as_array()?
        .iter()
        .map(|w| json!({"label": w[0], "used_pct": w[1], "resets_at": w[2]}))
        .collect();
    let mut entry = v.clone();
    entry["windows"] = Value::Array(windows);
    // The driver builds the probe's snapshot with the dataclass default.
    if entry.get("confidence").is_none() {
        entry["confidence"] = json!("exact");
    }
    state::parse_snapshot(&entry)
}

fn run_case(case: &Value, dir: &Path) -> String {
    let at = |text: &str| text.replace("{dir}", &dir.display().to_string());
    for f in case["files"].as_array().unwrap() {
        let path = dir.join(f["rel"].as_str().unwrap());
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, at(f["text"].as_str().unwrap())).unwrap();
    }
    let config = std::fs::read_to_string(dir.join(".fno/config.toml")).unwrap();
    let table: toml::Table = config.parse().unwrap();
    let records: Vec<Record> = records::scope_accounts(&table).unwrap().records;
    let probes = &case["probe"];
    let probe = |r: &Record| -> (Option<Snapshot>, Option<String>) {
        match probes.get(&r.id) {
            None => (None, Some("probe-failed".to_string())),
            Some(Value::Null) => (None, None),
            Some(Value::String(reason)) => (None, Some(reason.clone())),
            Some(v) => (snapshot(v), None),
        }
    };
    let persisted = case["persist"].as_bool().unwrap();
    let persist = |_: &Record, _: &Snapshot| persisted;
    let slot = FakeSlot(principal(&case["slot"]));
    let args: Vec<&str> = case["args"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a.as_str().unwrap())
        .collect();
    let state_path: PathBuf = dir.join("runtime-state.json");
    let store = dir.join(".fno").join("providers");
    let rows = usage::collect(&Inputs {
        records: &records,
        state: &state_path,
        store: Some(&store),
        slot: &slot,
        probe: &probe,
        persist: &persist,
        refresh: args.contains(&"--refresh"),
        ttl: case["ttl"].as_f64().unwrap(),
        now: NOW,
    });
    let out = usage::render(
        &records,
        &rows,
        args.contains(&"--json"),
        case["threshold"].as_f64().unwrap(),
        NOW,
        "{remedy}",
    );
    format!("{out}\n").replace(&dir.display().to_string(), "{dir}")
}

#[test]
fn accounts_usage_matches_the_python_command() {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/golden/accounts_usage");
    let mut cases: Vec<PathBuf> = std::fs::read_dir(dir)
        .unwrap()
        .filter_map(|entry| entry.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|e| e == "json"))
        .collect();
    cases.sort();
    assert!(cases.len() >= 20, "golden cases missing: {}", cases.len());
    let failures: Vec<String> = cases
        .iter()
        .filter_map(|golden| {
            let case: Value =
                serde_json::from_str(&std::fs::read_to_string(golden).unwrap()).unwrap();
            let tmp = tempfile::tempdir().unwrap();
            let got = run_case(&case, tmp.path());
            let want = case["expect"].as_str().unwrap();
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
