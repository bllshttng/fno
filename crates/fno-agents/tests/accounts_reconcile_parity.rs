//! parity-stage: differential
//! parity-oracle: fno.adapters.providers.managed.reconcile_slot
//!
//! Differential parity for `accounts::reconcile`, the port of
//! `managed.reconcile_slot`. Each `tests/golden/accounts_reconcile/*.json`
//! case lays down a store tree, Keychain items keyed by role, a profile
//! endpoint keyed by bearer and the taint writers' start times; `expect`
//! holds the outcome and the store tree the Python leg left, captured by
//! `accounts_reconcile_golden_driver.py` (the case table lives there). This
//! test runs the same reconcile through a fake host and probe and asserts
//! equal JSON.

use fno_agents::accounts::binding::{self, SlotProbe};
use fno_agents::accounts::reconcile;
use fno_agents::accounts::records::Record;
use fno_agents::accounts::store::{self, CodexRun, Host, ProcInfo, SecurityRun, StoreError};
use fno_agents::claude_vault::{ExternalFailure, Principal};
use serde_json::{json, Map, Value};
use std::cell::RefCell;
use std::fs::OpenOptions;
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// The process table the taint gate reads: only start times and liveness.
struct FakeHost {
    started: Map<String, Value>,
}

impl Host for FakeHost {
    fn keychain_slot(&self) -> bool {
        true
    }
    fn security(&self, _args: &[String]) -> Result<SecurityRun, StoreError> {
        Err(StoreError::Keychain(
            "reconcile reads the slot through the probe".into(),
        ))
    }
    fn processes(&self) -> Vec<ProcInfo> {
        Vec::new()
    }
    fn env_value(&self, _pid: u32, _key: &str) -> Result<Option<String>, ()> {
        Ok(None)
    }
    fn started_at(&self, pid: u32) -> Option<f64> {
        self.started.get(&pid.to_string()).and_then(Value::as_f64)
    }
    fn pid_exists(&self, pid: u32) -> bool {
        self.started.contains_key(&pid.to_string())
    }
    fn self_pid(&self) -> u32 {
        0
    }
    fn codex_login_status(&self, _codex_home: &Path) -> CodexRun {
        CodexRun::Missing
    }
}

/// Keychain items by role and a profile endpoint by bearer. `flip` rewrites
/// items while the endpoint answers, as the Python fake does.
struct FakeProbe {
    roles: Vec<(String, String)>,
    keychain: RefCell<Map<String, Value>>,
    profiles: Map<String, Value>,
    flip: Map<String, Value>,
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
            .borrow()
            .get(role)
            .and_then(Value::as_str)
            .map(str::to_string))
    }
    fn profile(&self, bearer: &str) -> Result<Principal, ExternalFailure> {
        for (role, blob) in &self.flip {
            self.keychain
                .borrow_mut()
                .insert(role.clone(), blob.clone());
        }
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
            if rel == ".switch.lock" {
                continue;
            }
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
    std::fs::create_dir_all(home.join(".claude")).unwrap();
    std::fs::create_dir_all(tmp.join("alt")).unwrap();
    if case["no_store"].as_bool() != Some(true) {
        std::fs::create_dir_all(&store_root).unwrap();
    }
    std::env::set_var("HOME", &home);
    if let Some(files) = case["store"].as_object() {
        for (rel, text) in files {
            let path = store_root.join(rel);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, text.as_str().unwrap()).unwrap();
        }
    }
    let scoped = binding::keychain_service(&home.join(".claude")).unwrap();
    let roles = vec![
        (
            "Claude Code-credentials".to_string(),
            "unscoped".to_string(),
        ),
        (scoped.clone(), "scoped:slot".to_string()),
    ];
    let field = |key: &str| case[key].as_object().cloned().unwrap_or_default();
    let probe = FakeProbe {
        roles,
        keychain: RefCell::new(field("keychain")),
        profiles: field("profile"),
        flip: field("flip"),
    };
    let host = FakeHost {
        started: field("started"),
    };
    let records: Vec<Record> = case["records"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| record(r, tmp_s))
        .collect();
    let held = case["hold_lock"].as_bool().unwrap_or(false).then(|| {
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(store::switch_lock_path(&store_root))
            .unwrap();
        // SAFETY: flock on a descriptor this test owns; dropping it releases.
        assert_eq!(unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) }, 0);
        file
    });
    let got = reconcile::reconcile_slot(
        case["cli"].as_str().unwrap_or("claude"),
        &records,
        &store_root,
        &probe,
        &host,
        Duration::from_millis(200),
        case["now"].as_f64().unwrap(),
    );
    drop(held);
    // Longest first: the unscoped name prefixes every scoped one.
    let detail = got
        .detail
        .replace(&scoped, "<scoped:slot>")
        .replace("Claude Code-credentials", "<unscoped>");
    let exists = store_root.is_dir();
    json!({
        "result": { "outcome": got.outcome, "record_id": got.record_id, "detail": detail },
        "store_exists": exists,
        "store": if exists { dump_tree(&store_root) } else { Map::new() },
    })
}

#[test]
fn accounts_reconcile_matches_the_python_leg() {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/golden/accounts_reconcile");
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
