//! parity-stage: differential
//! parity-oracle: fno.adapters.providers.dispatch.dispatch_env
//!
//! Differential parity for `accounts::env`, the port of `dispatch.py`
//! (`dispatch_env`, `resolve_env_value`), `staging.verify_staged` and
//! `account_env.resolve_account_overlay`. Each
//! `tests/golden/accounts_env/*.json` case lays down an accounts config,
//! staged links, credential files, store principals, Keychain items and a
//! slot stamp; `expect` holds what the Python leg answered, captured by
//! `accounts_env_golden_driver.py` (the case table lives there). This test
//! answers the same reads from the same file through fake sources and
//! asserts equal JSON.

use fno_agents::accounts::binding::{self, SlotProbe};
use fno_agents::accounts::env::{self, EnvErrorKind, Sources};
use fno_agents::claude_vault::{ExternalFailure, Principal};
use serde_json::{json, Map, Value};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

struct FakeProbe {
    /// Keychain service -> the case's blob for that role.
    keychain: BTreeMap<String, String>,
    profiles: Map<String, Value>,
}

impl SlotProbe for FakeProbe {
    fn keychain(&self, service: &str) -> Result<Option<String>, ExternalFailure> {
        Ok(self.keychain.get(service).cloned())
    }
    fn profile(&self, bearer: &str) -> Result<Principal, ExternalFailure> {
        let p = self
            .profiles
            .get(bearer)
            .and_then(Value::as_object)
            .ok_or(ExternalFailure::Unavailable)?;
        Ok(Principal {
            account_uuid: p["account_uuid"].as_str().unwrap().to_string(),
            organization_uuid: p["organization_uuid"].as_str().unwrap().to_string(),
            email: p.get("email").and_then(Value::as_str).map(str::to_string),
        })
    }
}

fn golden_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/golden/accounts_env")
}

fn strings(case: &Value, field: &str) -> BTreeMap<String, String> {
    case.get(field)
        .and_then(Value::as_object)
        .map(|m| {
            m.iter()
                .map(|(k, v)| (k.clone(), v.as_str().unwrap().to_string()))
                .collect()
        })
        .unwrap_or_default()
}

fn lay_down(case: &Value, tmp: &Path) -> (PathBuf, [(&'static str, PathBuf); 2]) {
    let home = tmp.join("home");
    let store = tmp.join("state/providers");
    for dir in [&home.join(".claude"), &store] {
        std::fs::create_dir_all(dir).unwrap();
    }
    for rel in case
        .get("dirs")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        std::fs::create_dir_all(tmp.join(rel.as_str().unwrap())).unwrap();
    }
    for (rel, text) in strings(case, "files") {
        let path = tmp.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }
    for (rel, target) in strings(case, "links") {
        let link = tmp.join(rel);
        std::fs::create_dir_all(link.parent().unwrap()).unwrap();
        std::os::unix::fs::symlink(tmp.join(target), link).unwrap();
    }
    if let Some(stamp) = case.get("stamp").and_then(Value::as_str) {
        std::fs::write(store.join(".active-claude"), stamp).unwrap();
    }
    let config = case["config"].as_str().unwrap();
    std::fs::write(
        tmp.join("config.toml"),
        config.replace("{TMP}", tmp.to_str().unwrap()),
    )
    .unwrap();
    if let Some(meta) = case.get("meta").and_then(Value::as_object) {
        for (rid, principal) in meta {
            let dir = store.join(rid);
            std::fs::create_dir_all(&dir).unwrap();
            let body = json!({ "principal": principal });
            std::fs::write(dir.join("meta.json"), body.to_string()).unwrap();
        }
    }
    (
        store,
        [("slot", home.join(".claude")), ("alt", tmp.join("alt"))],
    )
}

fn kind_name(kind: EnvErrorKind) -> &'static str {
    match kind {
        EnvErrorKind::NotFound => "ProviderNotFoundError",
        EnvErrorKind::Unavailable => "ProviderUnavailableError",
        EnvErrorKind::Config => "ProviderConfigError",
    }
}

fn run_case(case: &Value, tmp: &Path) -> Value {
    let (store, dirs) = lay_down(case, tmp);
    let scrub: Vec<String> = std::env::vars()
        .map(|(k, _)| k)
        .filter(|k| k.starts_with("FNO_") || k.starts_with("CLAUDE_") || k.starts_with("ZAI_"))
        .collect();
    for key in scrub {
        std::env::remove_var(key);
    }
    std::env::set_var("HOME", tmp.join("home"));
    std::env::set_var("FNO_CONFIG", tmp.join("config.toml"));
    std::env::set_var("FNO_STATE_DIR", tmp.join("state"));
    std::env::set_var("USER", "tester");

    let roles = strings(case, "keychain");
    let mut services = BTreeMap::from([(
        "Claude Code-credentials".to_string(),
        "unscoped".to_string(),
    )]);
    for (label, dir) in &dirs {
        services.insert(
            binding::keychain_service(dir).unwrap(),
            format!("scoped:{label}"),
        );
    }
    let probe = FakeProbe {
        keychain: services
            .into_iter()
            .filter_map(|(service, role)| roles.get(&role).map(|blob| (service, blob.clone())))
            .collect(),
        profiles: case
            .get("profiles")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default(),
    };
    let secrets = strings(case, "secrets");
    let process_env = strings(case, "env");
    let secret = |item: &str| secrets.get(item).cloned();
    let env_get = |key: &str| process_env.get(key).cloned();
    let src = Sources {
        slot: &probe,
        secret: &secret,
        env_get: &env_get,
    };

    let id = case["id"].as_str().unwrap();
    if case["op"] == "launch_env" {
        return match env::launch_env(id, None, Some(&store), tmp, &src) {
            Ok(env) => json!({ "env": env }),
            Err(e) => json!({ "error": kind_name(e.kind), "message": e.message }),
        };
    }
    match env::overlay(id, None, Some(&store), tmp, &src) {
        Ok(got) => json!({
            "overlay": { "account_id": got.account_id, "env": got.env, "lane": got.lane }
        }),
        Err(receipt) => json!({ "error": receipt }),
    }
}

#[test]
fn launch_env_and_overlay_match_the_python_leg() {
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
