//! parity-stage: characterization
//! parity-oracle: fno.adapters.providers.managed.register_slot_snapshot
//!
//! Characterization for `accounts::register`, the port of the Python
//! `fno config accounts register` command and `managed.register_slot_snapshot`,
//! which are gone. Each `tests/golden/accounts_register/*.json` case lays down
//! the global config, a store tree, Keychain items keyed by role, a profile
//! endpoint keyed by bearer, a codex auth.json and the taint writers' start
//! times; `expect` holds what the Python command left: exit code, stdout,
//! stderr, each scope's config.toml parsed, and the store tree. Scoped
//! Keychain service names were replaced by their role at capture, because the
//! name hashes the temp path. `flip` rewrites Keychain items while the profile
//! endpoint answers, or after `flip_after_calls` Keychain reads. The cases run
//! in one test because the command reads HOME and the state dir from the
//! environment.

use fno_agents::accounts::binding::{self, SlotProbe};
use fno_agents::accounts::register;
use fno_agents::accounts::store::{CodexRun, Host, ProcInfo, SecurityRun, StoreError};
use fno_agents::claude_vault::{ExternalFailure, Principal};
use serde_json::{json, Map, Value};
use std::cell::{Cell, RefCell};
use std::os::unix::fs::PermissionsExt;
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
            "register reads the slot through the probe".into(),
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

/// Keychain items by role and a profile endpoint by bearer, flipped the way
/// the Python fake flips them.
struct FakeProbe {
    roles: Vec<(String, String)>,
    keychain: RefCell<Map<String, Value>>,
    profiles: Map<String, Value>,
    flip: Map<String, Value>,
    flip_at_profile: bool,
    flip_after_calls: Option<u64>,
    calls: Cell<u64>,
}

impl FakeProbe {
    fn apply_flip(&self) {
        for (role, blob) in &self.flip {
            self.keychain
                .borrow_mut()
                .insert(role.clone(), blob.clone());
        }
    }
}

impl SlotProbe for FakeProbe {
    fn keychain(&self, service: &str) -> Result<Option<String>, ExternalFailure> {
        self.calls.set(self.calls.get() + 1);
        let role = self
            .roles
            .iter()
            .find(|(s, _)| s == service)
            .map(|(_, r)| r.as_str())
            .unwrap_or("other");
        let value = self
            .keychain
            .borrow()
            .get(role)
            .and_then(Value::as_str)
            .map(str::to_string);
        if self.flip_after_calls == Some(self.calls.get()) {
            self.apply_flip();
        }
        Ok(value)
    }
    fn profile(&self, bearer: &str) -> Result<Principal, ExternalFailure> {
        if self.flip_at_profile {
            self.apply_flip();
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

fn parsed(path: &Path) -> Value {
    match std::fs::read_to_string(path) {
        Ok(text) => toml::from_str(&text).expect("the written config parses"),
        Err(_) => Value::Null,
    }
}

fn run_case(case: &Value, tmp: &Path) -> Value {
    let tmp_s = tmp.to_str().unwrap();
    let home = tmp.join("home");
    let repo = tmp.join("repo");
    let store_root = tmp.join("state/providers");
    for dir in [
        home.join(".claude"),
        home.join(".fno"),
        home.join(".codex"),
        repo.join(".fno"),
        store_root.clone(),
    ] {
        std::fs::create_dir_all(dir).unwrap();
    }
    for rel in case["mkdirs"].as_array().into_iter().flatten() {
        std::fs::create_dir_all(tmp.join(rel.as_str().unwrap())).unwrap();
    }
    for (key, _) in std::env::vars() {
        if key.starts_with("FNO_") || key.starts_with("CLAUDE_") || key.starts_with("CODEX_") {
            std::env::remove_var(key);
        }
    }
    std::env::set_var("HOME", &home);
    std::env::set_var("FNO_STATE_DIR", tmp.join("state"));
    std::env::set_var("FNO_REPO_ROOT", &repo);
    std::env::set_var("PWD", &repo);
    std::env::set_var("USER", "tester");
    if let Some(text) = case["global_cfg"].as_str() {
        std::fs::write(home.join(".fno/config.toml"), text).unwrap();
    }
    if let Some(text) = case["codex"].as_str() {
        std::fs::write(home.join(".codex/auth.json"), text).unwrap();
    }
    if let Some(files) = case["store"].as_object() {
        for (rel, text) in files {
            let path = store_root.join(rel);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, text.as_str().unwrap()).unwrap();
        }
    }
    let scoped = binding::keychain_service(&home.join(".claude")).unwrap();
    let field = |key: &str| case[key].as_object().cloned().unwrap_or_default();
    let probe = FakeProbe {
        roles: vec![
            (
                "Claude Code-credentials".to_string(),
                "unscoped".to_string(),
            ),
            (scoped.clone(), "scoped:slot".to_string()),
        ],
        keychain: RefCell::new(field("keychain")),
        profiles: field("profile"),
        flip: field("flip"),
        flip_at_profile: case["flip_at_profile"].as_bool().unwrap_or(true),
        flip_after_calls: case["flip_after_calls"].as_u64(),
        calls: Cell::new(0),
    };
    let host = FakeHost {
        started: field("started"),
    };
    let fno_dir = home.join(".fno");
    let readonly = case["readonly_global"].as_bool() == Some(true);
    // SAFETY: geteuid has no preconditions.
    if readonly && unsafe { libc::geteuid() } == 0 {
        // Root ignores mode bits, so the refused write cannot be staged.
        return case["expect"].clone();
    }
    if readonly {
        std::fs::set_permissions(&fno_dir, std::fs::Permissions::from_mode(0o500)).unwrap();
    }
    let args: Vec<String> = case["args"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a.as_str().unwrap().replace("{TMP}", tmp_s))
        .collect();
    let got = register::run_with(
        &args,
        &repo,
        &probe,
        &host,
        Duration::from_millis(200),
        case["now"].as_f64().unwrap(),
    );
    std::fs::set_permissions(&fno_dir, std::fs::Permissions::from_mode(0o700)).unwrap();
    // Longest first: the unscoped name prefixes every scoped one.
    let roles = |s: &str| {
        s.replace(&scoped, "<scoped:slot>")
            .replace("Claude Code-credentials", "<unscoped>")
    };
    let mut stderr = roles(&got.stderr);
    if let Some(prefix) = case["stderr_prefix"].as_str() {
        assert!(stderr.starts_with(prefix), "stderr: {stderr}");
        stderr = prefix.to_string();
    }
    json!({
        "code": got.code,
        "stdout": roles(&got.stdout),
        "stderr": stderr,
        "global": parsed(&home.join(".fno/config.toml")),
        "project": parsed(&repo.join(".fno/config.toml")),
        "store": dump_tree(&store_root),
    })
}

#[test]
fn accounts_register_matches_the_python_command() {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/golden/accounts_register");
    let mut cases: Vec<PathBuf> = std::fs::read_dir(&dir)
        .unwrap()
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|e| e == "json"))
        .collect();
    cases.sort();
    assert!(cases.len() >= 12, "golden cases missing: {}", cases.len());
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
