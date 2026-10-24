//! parity-stage: differential
//! parity-oracle: fno.adapters.providers.usage
//!
//! Differential parity for `accounts::usage_probe`, the port of `usage.py`.
//! Each `tests/golden/accounts_usage_probe/*.json` case carries one parser
//! input, or one record plus the Keychain blobs, HTTP answers, route env and
//! session files its probe reads, and the Python leg's answer under `expect`,
//! frozen at `NOW` by `accounts_usage_probe_golden_driver.py` (the case table
//! lives there). A case with `delta` records a behavior change: `expect` is
//! the fixed answer and `python_expect` the old one.

use fno_agents::accounts::binding::SlotProbe;
use fno_agents::accounts::records;
use fno_agents::accounts::state::Snapshot;
use fno_agents::accounts::store::SystemHost;
use fno_agents::accounts::usage_probe::{self as up, Ctx};
use fno_agents::claude_vault::{ExternalFailure, Principal};
use serde_json::{json, Value};
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::fs::{File, FileTimes};
use std::path::{Path, PathBuf};
use std::time::{Duration, UNIX_EPOCH};

const NOW: f64 = 1_789_400_000.0;

struct FakeSlot(Option<String>);

impl SlotProbe for FakeSlot {
    fn keychain(&self, _service: &str) -> Result<Option<String>, ExternalFailure> {
        Ok(self.0.clone())
    }
    fn profile(&self, _bearer: &str) -> Result<Principal, ExternalFailure> {
        Err(ExternalFailure::Unavailable)
    }
}

fn windows(ws: &[fno_agents::accounts::state::Window]) -> Value {
    Value::Array(
        ws.iter()
            .map(|w| json!({"label": w.label, "used_pct": w.used_pct, "resets_at": w.resets_at}))
            .collect(),
    )
}

fn snapshot(s: Option<Snapshot>) -> Value {
    s.map_or(Value::Null, |s| s.to_json())
}

fn sub(value: &Value, dir: &Path) -> Value {
    serde_json::from_str(
        &value
            .to_string()
            .replace("{dir}", &dir.display().to_string()),
    )
    .unwrap()
}

fn detail(case: &Value, dir: &Path) -> Value {
    for f in case["files"].as_array().into_iter().flatten() {
        let path = dir.join(f["rel"].as_str().unwrap());
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, f["text"].as_str().unwrap()).unwrap();
        let at = UNIX_EPOCH + Duration::from_secs_f64(NOW - f["age"].as_f64().unwrap());
        File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_times(FileTimes::new().set_modified(at).set_accessed(at))
            .unwrap();
    }
    let rec = sub(&case["record"], dir);
    let table: toml::Table = toml::Value::try_from(json!({"accounts": {"records": [rec]}}))
        .unwrap()
        .try_into()
        .unwrap();
    let accounts = records::scope_accounts(&table).unwrap();
    let record = accounts.records[0].clone();
    let blobs: Vec<String> = case["blobs"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|b| b.as_str().unwrap().to_string())
        .collect();
    if let (Some(root), Some(file)) = (record.credentials_source.as_ref(), blobs.get(1)) {
        std::fs::create_dir_all(root).unwrap();
        std::fs::write(root.join(".credentials.json"), file).unwrap();
    }
    let slot = FakeSlot(blobs.first().cloned());
    let responses = RefCell::new(
        case["http"]
            .as_array()
            .into_iter()
            .flatten()
            .cloned()
            .collect::<Vec<_>>(),
    );
    let http = |_config: &str| -> Result<(u16, String), ()> {
        let r = responses.borrow_mut().remove(0);
        Ok((
            r["status"].as_u64().unwrap() as u16,
            r["body"].as_str().unwrap().to_string(),
        ))
    };
    let env = case.get("route_env").cloned().unwrap_or_else(|| json!({}));
    let route_env = |_id: &str| -> Option<BTreeMap<String, String>> {
        env.as_object().map(|m| {
            m.iter()
                .map(|(k, v)| (k.clone(), v.as_str().unwrap().to_string()))
                .collect()
        })
    };
    let ctx = Ctx {
        cwd: dir,
        repo_root: None,
        store: Some(dir.join("state").join("providers")),
        records: &accounts.records,
        slot: &slot,
        host: &SystemHost,
        http: &http,
        route_env: &route_env,
        codex_home: dir.join("home").join(".codex"),
    };
    let (snap, reason) = up::probe_detail(&ctx, &record, NOW);
    json!({"snapshot": snapshot(snap), "reason": reason})
}

fn run_op(case: &Value, dir: &Path) -> Value {
    let payload = &case["payload"];
    match case["op"].as_str().unwrap() {
        "claude_windows" => windows(&up::parse_claude_windows(payload)),
        "codex_windows" => windows(&up::parse_codex_windows(payload)),
        "zai_windows" => {
            let (ws, partial) = up::parse_zai_windows(payload, case["now"].as_f64().unwrap_or(NOW));
            json!({"windows": windows(&ws), "partial": partial})
        }
        "zai_confidence" => json!(up::zai_confidence(payload)),
        "labels" => Value::Array(
            case["minutes"]
                .as_array()
                .unwrap()
                .iter()
                .map(|m| json!(up::label_for_minutes(m.as_i64())))
                .collect(),
        ),
        "iso" => Value::Array(
            case["values"]
                .as_array()
                .unwrap()
                .iter()
                .map(|v| json!(up::iso_to_epoch(v)))
                .collect(),
        ),
        "detail" => detail(case, dir),
        other => panic!("unknown op {other}"),
    }
}

#[test]
fn accounts_usage_probe_matches_the_python_leg() {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/golden/accounts_usage_probe");
    let mut cases: Vec<PathBuf> = std::fs::read_dir(dir)
        .unwrap()
        .filter_map(|entry| entry.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|e| e == "json"))
        .collect();
    cases.sort();
    assert!(cases.len() >= 40, "golden cases missing: {}", cases.len());
    let failures: Vec<String> = cases
        .iter()
        .filter_map(|golden| {
            let case: Value =
                serde_json::from_str(&std::fs::read_to_string(golden).unwrap()).unwrap();
            let tmp = tempfile::tempdir().unwrap();
            let got = run_op(&case, tmp.path());
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
