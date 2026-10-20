//! Characterization over the claude-route goldens
//! (`tests/golden/claude_route/`): the route env, notices and provider
//! registry the Python leg (`_route_for_target`, `effective_providers`,
//! `key_source`) produced at the merge base, before the port deleted it.

//! parity-stage: characterization
//! parity-oracle: fno.agents.model_routing._route_for_target

use fno_agents::provider_route::{claude_route, providers_json, RouteCtx};
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, OnceLock};

fn env_lock() -> MutexGuard<'static, ()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|e| e.into_inner())
}

fn golden_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/golden/claude_route")
}

fn ctx_from(case: &Value) -> RouteCtx {
    let ctx = case.get("ctx");
    let name = ctx
        .and_then(|c| c.get("name"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    match ctx.and_then(|c| c.get("kind")).and_then(Value::as_str) {
        Some("peer") => RouteCtx::Peer(name.to_string()),
        _ => RouteCtx::Role(name.to_string()),
    }
}

/// Pin the case's config layers: `HOME` at the temp dir, the config at
/// `FNO_CONFIG` or the project layer, the keys file. The `{keys}`
/// placeholder in the config template becomes the temp keys.env path.
fn prepare(case: &Value, tmp: &Path) {
    std::env::set_var("HOME", tmp);
    match case.get("config") {
        Some(spec) => {
            let keys = tmp.join("keys.env");
            let text = spec["toml"]
                .as_str()
                .unwrap_or_default()
                .replace("{keys}", keys.to_str().unwrap_or_default());
            match spec["layer"].as_str() {
                Some("fno_config") => {
                    let config = tmp.join("config.toml");
                    std::fs::write(&config, text).unwrap();
                    std::env::set_var("FNO_CONFIG", &config);
                }
                _ => {
                    let proj = tmp.join(".fno");
                    std::fs::create_dir_all(&proj).unwrap();
                    std::fs::write(proj.join("config.toml"), text).unwrap();
                    std::env::remove_var("FNO_CONFIG");
                }
            }
            if let Some(body) = spec["keys"].as_str() {
                std::fs::write(&keys, body).unwrap();
            }
        }
        None => {
            std::env::remove_var("FNO_CONFIG");
        }
    }
}

#[test]
fn claude_route_matches_the_python_leg() {
    let _guard = env_lock();
    let mut cases: Vec<PathBuf> = std::fs::read_dir(golden_dir())
        .unwrap()
        .filter_map(|entry| entry.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|e| e == "json"))
        .collect();
    cases.sort();
    let mut failures: Vec<String> = Vec::new();
    for path in &cases {
        let case: Value = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
        let tmp = std::env::temp_dir().join(format!(
            "fno-claude-route-parity-{}-{}",
            std::process::id(),
            case["name"].as_str().unwrap_or_default()
        ));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        prepare(&case, &tmp);
        let env_map: BTreeMap<String, String> = case["env"]
            .as_object()
            .map(|m| {
                m.iter()
                    .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
                    .collect()
            })
            .unwrap_or_default();
        let env_get = |k: &str| env_map.get(k).cloned();
        let route = claude_route(
            &tmp,
            case["provider"].as_str().unwrap_or_default(),
            case["model"].as_str().unwrap_or_default(),
            &ctx_from(&case),
            &env_get,
        );
        let expected = case["expected"].clone();
        let expected_text = serde_json::to_string(&expected).unwrap_or_default();
        let expected: Value =
            serde_json::from_str(&expected_text.replace("{tmp}", tmp.to_str().unwrap_or_default()))
                .unwrap_or(expected.clone());
        let actual = route.to_json();
        if actual != expected {
            failures.push(format!(
                "{}:\n  expected: {expected}\n  actual:   {actual}",
                case["name"].as_str().unwrap_or_default()
            ));
        }
        if let Some(expected_providers) = case.get("expected_providers") {
            let all = providers_json(&tmp, &env_get);
            for (name, expected_record) in expected_providers
                .as_object()
                .unwrap_or(&Default::default())
            {
                let actual_record = all
                    .get("providers")
                    .and_then(|p| p.get(name))
                    .cloned()
                    .unwrap_or(Value::Null);
                let expected_text = serde_json::to_string(expected_record).unwrap_or_default();
                let expected_record: Value = serde_json::from_str(
                    &expected_text.replace("{tmp}", tmp.to_str().unwrap_or_default()),
                )
                .unwrap_or_else(|_| expected_record.clone());
                if actual_record != expected_record {
                    failures.push(format!(
                        "{} (provider {name}):\n  expected: {expected_record}\n  actual:   {actual_record}",
                        case["name"].as_str().unwrap_or_default()
                    ));
                }
            }
        }
        let _ = std::fs::remove_dir_all(&tmp);
    }
    assert!(
        failures.is_empty(),
        "claude-route parity mismatches:\n{}",
        failures.join("\n")
    );
}
