//! parity-stage: characterization
//! parity-oracle: fno.adapters.providers.cli.list_providers
//!
//! Characterization for `accounts::list`, the port of the
//! `fno config accounts list` command, whose Python body is gone. Each
//! `tests/golden/accounts_list/*.json` case carries the records, quota table,
//! runtime state, store files, Keychain items keyed by role and a profile
//! endpoint keyed by bearer, and the text and `-J` stdout the Python command
//! printed at `now` before deletion. Scoped Keychain service names read as
//! `<scoped:slot>`, `<scoped:a>` and `<scoped:b>`, the temp root as `<TMP>`.
//!
//! One field was corrected after capture. The Python snapshot age read the
//! wall clock, which the capture did not pin, so a snapshot stamped
//! 2027-01-01 read as a future stamp and clamped to `0m`. At the pinned
//! `now` (2027-01-15) the same Python code prints `14d`, which the goldens
//! now hold.

use fno_agents::accounts::binding::{self, SlotProbe};
use fno_agents::accounts::list;
use fno_agents::claude_vault::{ExternalFailure, Principal};
use serde_json::{Map, Value};
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
            _ => Err(ExternalFailure::Unavailable),
        }
    }
}

fn toml_value(v: &Value) -> String {
    match v {
        Value::Object(m) => format!(
            "{{ {} }}",
            m.iter()
                .map(|(k, x)| format!("{k} = {}", toml_value(x)))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        other => other.to_string(),
    }
}

fn config_text(case: &Value, tmp: &str) -> String {
    let mut lines = vec!["[accounts]".to_string()];
    if let Some(active) = case["active"].as_str() {
        lines.push(format!("active = \"{active}\""));
    }
    for r in case["records"].as_array().unwrap() {
        lines.push(String::new());
        lines.push("[[accounts.records]]".to_string());
        for (k, v) in r.as_object().unwrap() {
            let v = match (k.as_str(), v) {
                ("config_dir", Value::String(d)) => Value::String(d.replace("{TMP}", tmp)),
                _ => v.clone(),
            };
            lines.push(format!("{k} = {}", toml_value(&v)));
        }
    }
    if let Some(quota) = case["quota"].as_object() {
        lines.push(String::new());
        lines.push("[accounts.quota]".to_string());
        for (k, v) in quota {
            lines.push(format!("{k} = {}", toml_value(v)));
        }
    }
    lines.join("\n") + "\n"
}

fn run_case(case: &Value, tmp: &Path) -> Value {
    let tmp_s = tmp.to_str().unwrap();
    let (home, work, state) = (tmp.join("home"), tmp.join("work"), tmp.join("state"));
    let store = state.join("providers");
    for dir in [
        home.join(".claude"),
        home.join(".fno"),
        work.clone(),
        store.clone(),
        tmp.join("a"),
        tmp.join("b"),
    ] {
        std::fs::create_dir_all(dir).unwrap();
    }
    std::fs::write(home.join(".fno/config.toml"), config_text(case, tmp_s)).unwrap();
    let state_path = state.join("provider-runtime-state.json");
    if case["state"].is_object() {
        std::fs::write(&state_path, case["state"].to_string()).unwrap();
    }
    if let Some(files) = case["store"].as_object() {
        for (rel, text) in files {
            let path = store.join(rel);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, text.as_str().unwrap()).unwrap();
        }
    }
    let stale: Vec<String> = std::env::vars()
        .map(|(k, _)| k)
        .filter(|k| {
            ["FNO_", "CLAUDE_", "CODEX_"]
                .iter()
                .any(|p| k.starts_with(p))
        })
        .collect();
    for key in stale {
        std::env::remove_var(key);
    }
    std::env::set_var("HOME", &home);
    std::env::set_var("FNO_STATE_DIR", &state);
    std::env::set_var("FNO_RUNTIME_STATE_PATH", &state_path);
    let mut roles = vec![(
        "Claude Code-credentials".to_string(),
        "unscoped".to_string(),
    )];
    for (dir, role) in [
        (home.join(".claude"), "scoped:slot"),
        (tmp.join("a"), "scoped:a"),
        (tmp.join("b"), "scoped:b"),
    ] {
        roles.push((binding::keychain_service(&dir).unwrap(), role.to_string()));
    }
    let field = |key: &str| case[key].as_object().cloned().unwrap_or_default();
    let probe = FakeProbe {
        roles: roles.clone(),
        keychain: field("keychain"),
        profiles: field("profile"),
    };
    let now = case["now"].as_f64().unwrap();
    let mut out = Map::new();
    for (name, as_json) in [("text", false), ("json", true)] {
        let mut stdout = list::report(&work, as_json, &probe, now).unwrap() + "\n";
        // Longest first: the unscoped name prefixes every scoped one.
        roles.sort_by_key(|(s, _)| std::cmp::Reverse(s.len()));
        for (service, role) in &roles {
            stdout = stdout.replace(service, &format!("<{role}>"));
        }
        out.insert(
            name.to_string(),
            serde_json::json!({ "stdout": stdout.replace(tmp_s, "<TMP>"), "exit": 0 }),
        );
    }
    Value::Object(out)
}

#[test]
fn accounts_list_matches_the_python_command() {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/golden/accounts_list");
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
