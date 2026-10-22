//! parity-stage: differential
//! parity-oracle: fno.adapters.providers.binding.resolve_account_binding
//!
//! Differential parity for `accounts::binding`, the port of `binding.py` and
//! the `managed.py` reads it stands on. Each
//! `tests/golden/accounts_binding/*.json` case lays down an accounts config,
//! Keychain items keyed by role, credential files, store principals and a
//! proof cache; `expect` holds the binding and the proof cache the Python leg
//! left behind, captured by `accounts_binding_golden_driver.py` (the case
//! table lives there). This test answers the same Keychain and profile reads
//! from the same file through a fake probe and asserts equal JSON.

use fno_agents::accounts::binding::{self, BindingRequest, SlotProbe, PRINCIPAL_TTL_S};
use fno_agents::accounts::records;
use fno_agents::claude_vault::{ExternalFailure, Principal};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

struct FakeProbe {
    /// Keychain service -> the case's value for that role.
    keychain: BTreeMap<String, Value>,
    profiles: serde_json::Map<String, Value>,
}

impl SlotProbe for FakeProbe {
    fn keychain(&self, service: &str) -> Result<Option<String>, ExternalFailure> {
        match self.keychain.get(service) {
            Some(Value::String(blob)) => Ok(Some(blob.clone())),
            Some(Value::Object(_)) => Err(ExternalFailure::Unavailable),
            _ => Ok(None),
        }
    }
    fn profile(&self, bearer: &str) -> Result<Principal, ExternalFailure> {
        match self.profiles.get(bearer) {
            Some(Value::Object(p)) => Ok(Principal {
                account_uuid: p["account_uuid"].as_str().unwrap().to_string(),
                organization_uuid: p["organization_uuid"].as_str().unwrap().to_string(),
                email: p.get("email").and_then(Value::as_str).map(str::to_string),
            }),
            Some(Value::String(s)) if s == "rejected" => Err(ExternalFailure::Rejected),
            Some(Value::String(s)) if s == "malformed" => Err(ExternalFailure::Malformed),
            _ => Err(ExternalFailure::Unavailable),
        }
    }
}

fn golden_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/golden/accounts_binding")
}

fn run_case(case: &Value, tmp: &Path) -> Value {
    let home = tmp.join("home");
    let store = tmp.join("state/providers");
    let alt = tmp.join("alt");
    for dir in [&home, &store, &alt, &home.join(".claude")] {
        std::fs::create_dir_all(dir).unwrap();
    }
    let config = tmp.join("config.toml");
    let text = case["config"].as_str().unwrap_or_default();
    std::fs::write(&config, text.replace("{TMP}", tmp.to_str().unwrap())).unwrap();
    let scrub: Vec<String> = std::env::vars()
        .map(|(k, _)| k)
        .filter(|k| k.starts_with("FNO_") || k.starts_with("CLAUDE_"))
        .collect();
    for key in scrub {
        std::env::remove_var(key);
    }
    std::env::set_var("HOME", &home);
    std::env::set_var("FNO_CONFIG", &config);
    std::env::set_var("FNO_STATE_DIR", tmp.join("state"));
    std::env::set_var("USER", "tester");

    let dirs = [("slot", home.join(".claude")), ("alt", alt)];
    if let Some(files) = case.get("files").and_then(Value::as_object) {
        for (label, dir) in &dirs {
            if let Some(text) = files.get(*label).and_then(Value::as_str) {
                std::fs::write(dir.join(".credentials.json"), text).unwrap();
            }
        }
    }
    if let Some(meta) = case.get("meta").and_then(Value::as_object) {
        for (rid, principal) in meta {
            let dir = store.join(rid);
            std::fs::create_dir_all(&dir).unwrap();
            let body = json!({ "principal": principal });
            std::fs::write(dir.join("meta.json"), body.to_string()).unwrap();
        }
    }
    let now = case["now"].as_f64().unwrap();
    if let Some(cache) = case.get("cache") {
        let body = json!({
            "account_uuid": cache["identity"],
            "credential": cache["digest"],
            "at": now - cache["age"].as_f64().unwrap(),
        });
        std::fs::write(store.join(".active-claude.principal"), body.to_string()).unwrap();
    }
    let mut services = BTreeMap::new();
    services.insert(
        "Claude Code-credentials".to_string(),
        "unscoped".to_string(),
    );
    for (label, dir) in &dirs {
        services.insert(
            binding::keychain_service(dir).unwrap(),
            format!("scoped:{label}"),
        );
    }
    let roles = case.get("keychain").cloned().unwrap_or(json!({}));
    let probe = FakeProbe {
        keychain: services
            .into_iter()
            .filter_map(|(service, role)| roles.get(&role).map(|v| (service, v.clone())))
            .collect(),
        profiles: case
            .get("profiles")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default(),
    };

    let accounts = records::load(None, tmp).expect("case config loads");
    let request = case.get("request").cloned().unwrap_or(json!({}));
    let record = request["record"].as_str().and_then(|id| accounts.by_id(id));
    let got = binding::resolve(
        &BindingRequest {
            record,
            harness: "claude",
            bearer: request["bearer"].as_str(),
            root: &store,
            records: &accounts.records,
            now,
            ttl: PRINCIPAL_TTL_S,
        },
        &probe,
    );
    let cache = std::fs::read_to_string(store.join(".active-claude.principal"))
        .ok()
        .and_then(|t| serde_json::from_str::<Value>(&t).ok());
    json!({ "binding": got.to_json(), "cache_after": cache })
}

#[test]
fn account_binding_matches_the_python_leg() {
    let mut cases: Vec<PathBuf> = std::fs::read_dir(golden_dir())
        .unwrap()
        .filter_map(|entry| entry.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|e| e == "json"))
        .collect();
    cases.sort();
    assert!(cases.len() >= 30, "golden cases missing: {}", cases.len());
    let saved: Vec<(String, String)> = std::env::vars().collect();
    let mut failures = Vec::new();
    for path in &cases {
        let case: Value = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(tmp.path()).unwrap();
        let got = run_case(&case, &root);
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
