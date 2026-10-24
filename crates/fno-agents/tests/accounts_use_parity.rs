//! parity-stage: characterization
//! parity-oracle: fno.adapters.providers.managed.switch
//!
//! Characterization for `accounts::use_verb`, the port of the Python
//! `fno config accounts use` body and `managed.switch`, which are gone. Each
//! `tests/golden/accounts_use/*.json` case lays down the global and project
//! config, a store tree, Keychain items keyed by role, a codex auth.json, the
//! pin scans in call order, the codex probe answer, the vault verdicts and
//! whether a terminal is attached; `expect` holds what the Python leg left:
//! exit code, stdout, stderr, each scope's config.toml parsed, the store tree,
//! the Keychain by role, the codex auth.json, the `account_switched` events
//! and the outside calls in order. Scoped Keychain service names were replaced
//! by their role at capture, because the name hashes the temp path. A `switch`
//! case drives the JSON op recovery transports, against `managed.switch` with
//! the defer policy. The cases run in one test because the verb reads HOME and
//! the state dir from the environment.

use fno_agents::accounts::binding;
use fno_agents::accounts::store::{self, CodexRun, Host, ProcInfo, SecurityRun, StoreError};
use fno_agents::accounts::use_verb::{self, Outside};
use serde_json::{json, Map, Value};
use std::cell::RefCell;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// The Keychain, the process table, the codex probe and the vault, as the
/// Python capture faked them, with every outside call logged in order.
struct FakeWorld {
    scoped: String,
    keychain: RefCell<Map<String, Value>>,
    drop_writes: Vec<String>,
    pins: RefCell<Vec<Value>>,
    codex_login: String,
    vault: Map<String, Value>,
    terminal: bool,
    login_code: i32,
    now: f64,
    calls: RefCell<Vec<String>>,
    events: RefCell<Vec<Value>>,
}

impl FakeWorld {
    fn role(&self, service: &str) -> String {
        if service == store::UNSCOPED_SERVICE {
            "unscoped".to_string()
        } else if service == self.scoped {
            "scoped:slot".to_string()
        } else {
            format!("other:{service}")
        }
    }
}

fn flag(args: &[String], name: &str) -> String {
    let at = args.iter().position(|a| a == name).unwrap();
    args[at + 1].clone()
}

impl Host for FakeWorld {
    fn keychain_slot(&self) -> bool {
        true
    }
    fn security(&self, args: &[String]) -> Result<SecurityRun, StoreError> {
        let role = self.role(&flag(args, "-s"));
        let run = |code: i32, stdout: String| SecurityRun {
            code,
            stdout,
            stderr: String::new(),
        };
        match args[0].as_str() {
            "find-generic-password" => Ok(match self.keychain.borrow().get(&role) {
                Some(Value::String(blob)) => run(0, format!("{blob}\n")),
                _ => run(44, String::new()),
            }),
            "add-generic-password" => {
                if !self.drop_writes.contains(&role) {
                    self.keychain
                        .borrow_mut()
                        .insert(role, Value::String(flag(args, "-w")));
                }
                Ok(run(0, String::new()))
            }
            other => panic!("unexpected security call {other}"),
        }
    }
    fn processes(&self) -> Vec<ProcInfo> {
        let mut pins = self.pins.borrow_mut();
        if pins.is_empty() {
            return Vec::new();
        }
        let row = pins.remove(0);
        row.as_array()
            .unwrap()
            .iter()
            .map(|p| ProcInfo {
                pid: p["pid"].as_u64().unwrap() as u32,
                // argv[0] matches the codex scan, a later part the claude one.
                name: None,
                cmdline: vec!["codex".to_string(), "claude".to_string()],
                started: p["started"].as_f64(),
            })
            .collect()
    }
    fn env_value(&self, _pid: u32, _key: &str) -> Result<Option<String>, ()> {
        Ok(None)
    }
    fn started_at(&self, _pid: u32) -> Option<f64> {
        None
    }
    fn pid_exists(&self, _pid: u32) -> bool {
        false
    }
    fn self_pid(&self) -> u32 {
        0
    }
    fn codex_login_status(&self, _codex_home: &Path) -> CodexRun {
        self.calls
            .borrow_mut()
            .push("codex-login-status".to_string());
        match self.codex_login.as_str() {
            "ok" => CodexRun::Exit(true),
            "rejected" => CodexRun::Exit(false),
            "timeout" => CodexRun::Timeout,
            _ => CodexRun::Missing,
        }
    }
}

impl Outside for FakeWorld {
    fn vault(&self, action: &str, _root: &Path, id: Option<&str>) -> Result<String, String> {
        let call = match id {
            Some(id) => format!("{action}:{id}"),
            None => action.to_string(),
        };
        self.calls.borrow_mut().push(call);
        let default = if action == "refresh" {
            "fresh"
        } else {
            "written"
        };
        Ok(self
            .vault
            .get(action)
            .and_then(Value::as_str)
            .unwrap_or(default)
            .to_string())
    }
    fn login(&self, _root: &Path, id: &str) -> i32 {
        self.calls.borrow_mut().push(format!("login:{id}"));
        self.login_code
    }
    fn terminal(&self) -> bool {
        self.terminal
    }
    fn emit(&self, fields: Map<String, Value>) {
        let mut row = Map::new();
        row.insert("kind".to_string(), json!("account_switched"));
        row.extend(fields);
        self.events.borrow_mut().push(Value::Object(row));
    }
    fn now(&self) -> f64 {
        self.now
    }
    fn lock_timeout(&self) -> Duration {
        Duration::from_millis(200)
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
    let fill = |s: &str| s.replace("{TMP}", tmp.to_str().unwrap());
    let home = tmp.join("home");
    let repo = tmp.join("repo");
    let store_root = tmp.join("state/providers");
    for dir in [
        home.join(".claude"),
        home.join(".fno"),
        home.join(".codex"),
        repo.join(".fno"),
        store_root.clone(),
        tmp.join("creds"),
    ] {
        std::fs::create_dir_all(dir).unwrap();
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
    for (key, path) in [
        ("global_cfg", home.join(".fno/config.toml")),
        ("project_cfg", repo.join(".fno/config.toml")),
        ("codex", home.join(".codex/auth.json")),
    ] {
        if let Some(text) = case[key].as_str().filter(|t| !t.is_empty()) {
            std::fs::write(path, fill(text)).unwrap();
        }
    }
    if let Some(files) = case["store"].as_object() {
        for (rel, text) in files {
            let path = store_root.join(rel);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, text.as_str().unwrap()).unwrap();
        }
    }
    let strings = |key: &str| -> Vec<String> {
        case[key]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|v| v.as_str().map(str::to_string))
            .collect()
    };
    let world = FakeWorld {
        scoped: binding::keychain_service(&home.join(".claude")).unwrap(),
        keychain: RefCell::new(case["keychain"].as_object().cloned().unwrap_or_default()),
        drop_writes: strings("drop_writes"),
        pins: RefCell::new(case["pins"].as_array().cloned().unwrap_or_default()),
        codex_login: case["codex_login"]
            .as_str()
            .unwrap_or("missing")
            .to_string(),
        vault: case["vault"].as_object().cloned().unwrap_or_default(),
        terminal: case["tty"].as_bool() == Some(true),
        login_code: case["login_code"].as_i64().unwrap_or(0) as i32,
        now: case["now"].as_f64().unwrap(),
        calls: RefCell::new(Vec::new()),
        events: RefCell::new(Vec::new()),
    };
    let fno_dir = home.join(".fno");
    let readonly = case["readonly_global"].as_bool() == Some(true);
    // SAFETY: geteuid has no preconditions.
    if readonly && unsafe { libc::geteuid() } == 0 {
        // Root ignores mode bits, so the refused write cannot be staged.
        return case["expect"].clone();
    }
    let _held = (case["hold_lock"].as_bool() == Some(true))
        .then(|| store::lock_switch(&store_root, Duration::ZERO).expect("the mutex is free"));
    if readonly {
        std::fs::set_permissions(&fno_dir, std::fs::Permissions::from_mode(0o500)).unwrap();
    }
    let got = if case["mode"] == "switch" {
        use_verb::run_switch_with(&case["payload"], &repo, &world, &world)
    } else {
        let args: Vec<String> = case["args"].as_array().unwrap()[1..]
            .iter()
            .map(|a| a.as_str().unwrap().to_string())
            .collect();
        use_verb::run_with(&args, &repo, &world, &world)
    };
    std::fs::set_permissions(&fno_dir, std::fs::Permissions::from_mode(0o700)).unwrap();
    let mut stderr = got.stderr.clone();
    if let Some(prefix) = case["stderr_prefix"].as_str() {
        assert!(stderr.starts_with(prefix), "stderr: {stderr}");
        stderr = prefix.to_string();
    }
    // The switch op's receipt compares parsed: Python's json.dumps spaces it.
    let stdout = match (case["mode"].as_str(), got.code) {
        (Some("switch"), 0) => {
            let receipt: Value = serde_json::from_str(got.stdout.trim()).unwrap();
            let expected: Value =
                serde_json::from_str(case["expect"]["stdout"].as_str().unwrap().trim()).unwrap();
            if receipt == expected {
                case["expect"]["stdout"].clone()
            } else {
                Value::String(got.stdout.clone())
            }
        }
        _ => Value::String(got.stdout.clone()),
    };
    let codex = std::fs::read_to_string(home.join(".codex/auth.json"))
        .map(Value::String)
        .unwrap_or(Value::Null);
    json!({
        "code": got.code,
        "stdout": stdout,
        "stderr": stderr,
        "global": parsed(&home.join(".fno/config.toml")),
        "project": parsed(&repo.join(".fno/config.toml")),
        "store": dump_tree(&store_root),
        "keychain": Value::Object(world.keychain.borrow().clone()),
        "codex": codex,
        "events": Value::Array(world.events.borrow().clone()),
        "calls": world.calls.borrow().clone(),
    })
}

#[test]
fn accounts_use_matches_the_python_command() {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/golden/accounts_use");
    let mut cases: Vec<PathBuf> = std::fs::read_dir(&dir)
        .unwrap()
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|e| e == "json"))
        .collect();
    cases.sort();
    assert!(cases.len() >= 20, "golden cases missing: {}", cases.len());
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
