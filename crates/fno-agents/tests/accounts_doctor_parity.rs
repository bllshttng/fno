//! parity-stage: differential
//! parity-oracle: fno.adapters.providers.cli._doctor_findings
//!
//! Differential parity for `accounts::doctor`, the port of the
//! `fno config accounts doctor` findings. Each
//! `tests/golden/accounts_doctor/*.json` case lays down a store tree, config
//! dir files, Keychain items keyed by role and a profile endpoint keyed by
//! bearer; `expect` holds the findings and the store tree the Python leg left,
//! captured by `accounts_doctor_golden_driver.py` (the case table lives
//! there). This test runs the same findings through a fake probe and asserts
//! equal JSON.

use fno_agents::accounts::binding::{self, SlotProbe};
use fno_agents::accounts::doctor;
use fno_agents::accounts::records::Record;
use fno_agents::claude_vault::{ExternalFailure, Principal};
use serde_json::{json, Map, Value};
use std::path::{Path, PathBuf};

/// Keychain items by role and a profile endpoint by bearer.
struct FakeProbe {
    roles: Vec<(String, String)>,
    keychain: Map<String, Value>,
    profiles: Map<String, Value>,
}

impl SlotProbe for FakeProbe {
    fn keychain(&self, service: &str) -> Result<Option<String>, ExternalFailure> {
        let role = self
            .roles
            .iter()
            .find(|(s, _)| s == service)
            .map(|(_, r)| r.as_str())
            .unwrap_or("other");
        Ok(self
            .keychain
            .get(role)
            .and_then(Value::as_str)
            .map(str::to_string))
    }
    fn profile(&self, bearer: &str) -> Result<Principal, ExternalFailure> {
        match self.profiles.get(bearer) {
            Some(Value::Object(p)) => Ok(Principal {
                account_uuid: p["account_uuid"].as_str().unwrap().to_string(),
                organization_uuid: p["organization_uuid"].as_str().unwrap().to_string(),
                email: p.get("email").and_then(Value::as_str).map(str::to_string),
            }),
            Some(Value::String(s)) if s == "credential-rejected" => Err(ExternalFailure::Rejected),
            Some(Value::String(s)) if s == "malformed-profile" => Err(ExternalFailure::Malformed),
            _ => Err(ExternalFailure::Unavailable),
        }
    }
}

fn blob(token: &str) -> String {
    format!(r#"{{"claudeAiOauth": {{"accessToken": "{token}", "refreshToken": "r"}}}}"#)
}

fn record(r: &Value, tmp: &str) -> Record {
    let id = r["id"].as_str().unwrap().to_string();
    let auth = r["auth"].as_str().unwrap().to_string();
    Record {
        name: id.clone(),
        harness: r["harness"].as_str().unwrap().to_string(),
        priority: 100,
        route_provider_id: None,
        model_name: None,
        credentials_source: None,
        env: None,
        route: None,
        reset_timezone: None,
        window_minutes: None,
        account_id: id.clone(),
        tags: Vec::new(),
        description: None,
        config_dir: r["config_dir"]
            .as_str()
            .map(|d| PathBuf::from(d.replace("{TMP}", tmp))),
        pricing: None,
        cost_cap_usd_per_session: None,
        extra: Map::new(),
        id,
        auth,
    }
}

fn dump_tree(root: &Path) -> Map<String, Value> {
    let mut out = Map::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).unwrap().flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            let rel = path
                .strip_prefix(root)
                .unwrap()
                .to_string_lossy()
                .into_owned();
            let text = std::fs::read_to_string(&path).unwrap();
            let value = serde_json::from_str(&text).unwrap_or(Value::String(text));
            out.insert(rel, value);
        }
    }
    out
}

fn run_case(case: &Value, tmp: &Path) -> Value {
    let tmp_s = tmp.to_str().unwrap();
    let home = tmp.join("home");
    let store_root = tmp.join("state/providers");
    let alt = tmp.join("alt");
    for dir in [home.join(".claude"), store_root.clone(), alt.clone()] {
        std::fs::create_dir_all(dir).unwrap();
    }
    std::env::set_var("HOME", &home);
    let now = case["now"].as_f64().unwrap();
    if let Some(files) = case["store"].as_object() {
        for (rel, text) in files {
            let mut text = text.as_str().unwrap().to_string();
            if let Some(spec) = text.strip_prefix("CACHE:") {
                let (token, identity) = spec.split_once(':').unwrap();
                let digest = binding::credential_digest(&blob(token)).unwrap();
                text = json!({ "account_uuid": identity, "credential": digest, "at": now - 60.0 })
                    .to_string();
            }
            let path = store_root.join(rel);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, text).unwrap();
        }
    }
    if let Some(files) = case["files"].as_object() {
        for (rel, text) in files {
            std::fs::write(tmp.join(rel), text.as_str().unwrap()).unwrap();
        }
    }
    let scoped_slot = binding::keychain_service(&home.join(".claude")).unwrap();
    let scoped_alt = binding::keychain_service(&alt).unwrap();
    let roles = vec![
        (
            "Claude Code-credentials".to_string(),
            "unscoped".to_string(),
        ),
        (scoped_slot.clone(), "scoped:slot".to_string()),
        (scoped_alt.clone(), "scoped:alt".to_string()),
    ];
    let field = |key: &str| case[key].as_object().cloned().unwrap_or_default();
    let probe = FakeProbe {
        roles,
        keychain: field("keychain"),
        profiles: field("profile"),
    };
    let records: Vec<Record> = case["records"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| record(r, tmp_s))
        .collect();
    let found: Vec<Value> = doctor::findings(&records, &store_root, &probe, now)
        .iter()
        .map(|f| {
            let mut row = f.to_json();
            // Longest first: the unscoped name prefixes every scoped one.
            let detail = f
                .detail
                .replace(&scoped_slot, "<scoped:slot>")
                .replace(&scoped_alt, "<scoped:alt>")
                .replace("Claude Code-credentials", "<unscoped>");
            row["detail"] = json!(detail);
            row
        })
        .collect();
    json!({ "findings": found, "store": dump_tree(&store_root) })
}

#[test]
fn accounts_doctor_matches_the_python_leg() {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/golden/accounts_doctor");
    let mut cases: Vec<PathBuf> = std::fs::read_dir(&dir)
        .unwrap()
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|e| e == "json"))
        .collect();
    cases.sort();
    assert!(cases.len() >= 10, "golden cases missing: {}", cases.len());
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
    assert!(
        failures.is_empty(),
        "{} case(s) differ:\n{}",
        failures.len(),
        failures.join("\n")
    );
}
