//! parity-stage: differential
//! parity-oracle: fno.adapters.providers.loader.load_providers
//!
//! Differential parity for `accounts::records`, the port of `model.py` and
//! the read side of `loader.py`. Each `tests/golden/accounts_records/*.json`
//! case names an op, the config layers and slot stamps to lay down, and the
//! Python leg's answer under `expect`, captured by
//! `accounts_records_golden_driver.py` (the case table lives there; re-run it
//! to add a case or re-freeze). This test builds the same fixture from the
//! same file, runs the Rust op, and asserts equal JSON. A refusal compares
//! the Python headline, which the Rust `LoadError::headline` reproduces.

use fno_agents::accounts::records::{self, Scope};
use fno_agents::backlog::quota_route::QuotaConfig;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

const LAYERS: [(&str, &str); 4] = [
    ("global", "home/.fno/config.toml"),
    ("global_yaml", "home/.fno/settings.yaml"),
    ("project", "repo/.fno/config.toml"),
    ("project_yaml", "repo/.fno/settings.yaml"),
];

fn golden_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/golden/accounts_records")
}

fn lay_down(case: &Value, tmp: &Path) {
    for (layer, rel) in LAYERS {
        if let Some(text) = case["files"].get(layer).and_then(Value::as_str) {
            let path = tmp.join(rel);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, text).unwrap();
        }
    }
    std::fs::create_dir_all(tmp.join("repo")).unwrap();
    std::fs::create_dir_all(tmp.join("home")).unwrap();
    if let Some(stamps) = case.get("stamps").and_then(Value::as_object) {
        let dir = tmp.join("state/providers");
        std::fs::create_dir_all(&dir).unwrap();
        for (harness, text) in stamps {
            std::fs::write(
                dir.join(format!(".active-{harness}")),
                text.as_str().unwrap(),
            )
            .unwrap();
        }
    }
    let fno_vars: Vec<String> = std::env::vars()
        .map(|(k, _)| k)
        .filter(|k| k.starts_with("FNO_"))
        .collect();
    for key in fno_vars {
        std::env::remove_var(key);
    }
    std::env::set_var("HOME", tmp.join("home"));
    std::env::set_var("PWD", tmp.join("repo"));
    std::env::set_var("FNO_REPO_ROOT", tmp.join("repo"));
    std::env::set_var("FNO_STATE_DIR", tmp.join("state"));
}

fn accounts(r: Result<records::Accounts, records::LoadError>) -> Value {
    match r {
        Ok(a) => json!({ "ok": a.to_json() }),
        Err(e) => json!({ "error": e.headline }),
    }
}

fn run_op(case: &Value, repo: &Path) -> Value {
    match case["op"].as_str().unwrap() {
        "load" => accounts(records::load(None, repo)),
        "combos" => match records::load_combos(None, repo) {
            Ok(combos) => {
                let map: serde_json::Map<String, Value> = combos
                    .iter()
                    .map(|c| (c.name.clone(), c.to_json()))
                    .collect();
                json!({ "ok": map })
            }
            Err(e) => json!({ "error": e.headline }),
        },
        "active_combo" => json!({ "ok": records::load_active_combo(None, repo) }),
        "quota" => {
            let q = QuotaConfig::from_block(&records::quota_block(None, repo));
            json!({ "ok": {
                "observe": q.observe,
                "defer_dispatch": q.defer_dispatch,
                "defer_threshold_pct": q.defer_threshold_pct,
                "probe_ttl_seconds": q.probe_ttl_seconds as i64,
                "defer_horizon_minutes": q.defer_horizon_minutes as i64,
                "pick_on_launch": q.pick_on_launch,
            }})
        }
        "known_ids" => json!({ "ok": records::known_account_ids(None, repo) }),
        "effective_active" => match records::load(None, repo) {
            Ok(a) => json!({ "ok": records::effective_active(&a, repo) }),
            Err(e) => json!({ "error": e.headline }),
        },
        "scope_project" => accounts(records::load_scope(Scope::Project, repo)),
        "scope_global" => accounts(records::load_scope(Scope::Global, repo)),
        "record_harness" => {
            let id = case["id"].as_str().unwrap();
            json!({ "ok": records::record_harness(id, None, repo) })
        }
        other => panic!("unknown op {other}"),
    }
}

#[test]
fn accounts_records_match_the_python_leg() {
    let mut cases: Vec<PathBuf> = std::fs::read_dir(golden_dir())
        .unwrap()
        .filter_map(|entry| entry.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|e| e == "json"))
        .collect();
    cases.sort();
    assert!(cases.len() >= 60, "golden cases missing: {}", cases.len());
    let saved: Vec<(String, String)> = std::env::vars().collect();
    let mut failures = Vec::new();
    for path in &cases {
        let case: Value = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(tmp.path()).unwrap();
        lay_down(&case, &root);
        let got = run_op(&case, &root.join("repo"));
        let text = serde_json::to_string(&got)
            .unwrap()
            .replace(root.to_str().unwrap(), "<TMP>");
        let got: Value = serde_json::from_str(&text).unwrap();
        if got != case["expect"] {
            failures.push(format!(
                "{}\n  expect: {}\n  rust:   {}",
                path.file_name().unwrap().to_string_lossy(),
                case["expect"],
                got
            ));
        }
    }
    for (k, _) in std::env::vars() {
        std::env::remove_var(k);
    }
    for (k, v) in saved {
        std::env::set_var(k, v);
    }
    assert!(
        failures.is_empty(),
        "{} case(s) differ:\n{}",
        failures.len(),
        failures.join("\n")
    );
}
