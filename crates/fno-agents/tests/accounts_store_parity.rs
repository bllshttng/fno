//! parity-stage: differential
//! parity-oracle: fno.adapters.providers.managed
//!
//! Differential parity for `accounts::store`, the port of `managed.py`'s
//! store layer. Each `tests/golden/accounts_store/*.json` case lays down a
//! store tree, slot credential files, Keychain items keyed by role, a process
//! table and a `codex login status` answer, runs one store operation, and
//! holds in `expect` the result, the store tree and the Keychain writes the
//! Python leg produced, captured by `accounts_store_golden_driver.py` (the
//! case table lives there). This test runs the same operation through a fake
//! host and asserts equal JSON.

use fno_agents::accounts::binding;
use fno_agents::accounts::store::{
    self, CodexRun, Host, ProcInfo, SecurityRun, StoreError, TaintWriter,
};
use serde_json::{json, Map, Value};
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

struct FakeHost {
    keychain_slot: bool,
    /// Service name -> role (`unscoped`, `scoped:slot`, `scoped:alt`).
    roles: BTreeMap<String, String>,
    keychain: RefCell<Map<String, Value>>,
    fail: Map<String, Value>,
    writes: RefCell<Vec<Value>>,
    processes: Vec<Value>,
    tmp: String,
    started: Map<String, Value>,
    alive: Vec<u64>,
    login: Option<String>,
    login_home: RefCell<Option<String>>,
}

impl FakeHost {
    fn role(&self, args: &[String]) -> String {
        let at = args.iter().position(|a| a == "-s").unwrap() + 1;
        self.roles
            .get(&args[at])
            .cloned()
            .unwrap_or_else(|| "other".to_string())
    }
}

impl Host for FakeHost {
    fn keychain_slot(&self) -> bool {
        self.keychain_slot
    }
    fn security(&self, args: &[String]) -> Result<SecurityRun, StoreError> {
        let role = self.role(args);
        let value = self.keychain.borrow().get(&role).cloned();
        if value.as_ref().and_then(Value::as_str) == Some("TIMEOUT") {
            return Err(StoreError::Keychain(format!(
                "`security {}` timed out after 5s",
                args[0]
            )));
        }
        let run = |code: i32, stdout: String, stderr: String| SecurityRun {
            code,
            stdout,
            stderr,
        };
        if let Some(fail) = self.fail.get(&role) {
            return Ok(run(
                fail["code"].as_i64().unwrap() as i32,
                String::new(),
                fail["stderr"].as_str().unwrap().to_string(),
            ));
        }
        if args[0] == "add-generic-password" {
            let at = args.iter().position(|a| a == "-w").unwrap() + 1;
            let new = args[at].clone();
            self.writes.borrow_mut().push(json!([role, new]));
            self.keychain.borrow_mut().insert(role, json!(new));
            return Ok(run(0, String::new(), String::new()));
        }
        Ok(match value.as_ref().and_then(Value::as_str) {
            Some(text) => run(0, format!("{text}\n"), String::new()),
            None => run(44, String::new(), String::new()),
        })
    }
    fn processes(&self) -> Vec<ProcInfo> {
        self.processes
            .iter()
            .map(|p| ProcInfo {
                pid: p["pid"].as_u64().unwrap() as u32,
                name: p["name"].as_str().map(str::to_string),
                cmdline: p["cmdline"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|c| c.as_str().unwrap().to_string())
                    .collect(),
                started: p["started"].as_f64(),
            })
            .collect()
    }
    fn env_value(&self, pid: u32, key: &str) -> Result<Option<String>, ()> {
        let row = self
            .processes
            .iter()
            .find(|p| p["pid"].as_u64() == Some(u64::from(pid)))
            .ok_or(())?;
        match &row["env"] {
            Value::Object(env) => Ok(env
                .get(key)
                .and_then(Value::as_str)
                .map(|v| v.replace("{TMP}", &self.tmp))),
            _ => Err(()),
        }
    }
    fn started_at(&self, pid: u32) -> Option<f64> {
        self.started.get(&pid.to_string()).and_then(Value::as_f64)
    }
    fn pid_exists(&self, pid: u32) -> bool {
        self.alive.contains(&u64::from(pid))
    }
    fn self_pid(&self) -> u32 {
        0
    }
    fn codex_login_status(&self, codex_home: &Path) -> CodexRun {
        *self.login_home.borrow_mut() = Some(codex_home.to_string_lossy().to_string());
        match self.login.as_deref() {
            Some("ok") => CodexRun::Exit(true),
            Some("missing") => CodexRun::Missing,
            Some("timeout") => CodexRun::Timeout,
            _ => CodexRun::Exit(false),
        }
    }
}

fn golden_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/golden/accounts_store")
}

fn dump_tree(root: &Path) -> (Map<String, Value>, Map<String, Value>) {
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(&path, out);
            } else {
                out.push(path);
            }
        }
    }
    let mut paths = Vec::new();
    walk(root, &mut paths);
    paths.sort();
    let mut files = Map::new();
    let mut modes = Map::new();
    for path in paths {
        let rel = path
            .strip_prefix(root)
            .unwrap()
            .to_string_lossy()
            .to_string();
        let text = std::fs::read_to_string(&path).unwrap();
        let value = serde_json::from_str::<Value>(&text).unwrap_or(Value::String(text));
        files.insert(rel.clone(), value);
        #[cfg(unix)]
        let private = {
            use std::os::unix::fs::PermissionsExt;
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777 == 0o600
        };
        #[cfg(not(unix))]
        let private = false;
        modes.insert(rel, json!(private));
    }
    (files, modes)
}

fn error_json(e: &StoreError, roles: &BTreeMap<String, String>) -> Value {
    let mut message = e.message().to_string();
    let mut by_len: Vec<(&String, &String)> = roles.iter().collect();
    by_len.sort_by_key(|(service, _)| std::cmp::Reverse(service.len()));
    for (service, role) in by_len {
        message = message.replace(service.as_str(), &format!("<{role}>"));
    }
    json!({ "type": e.kind(), "message": message })
}

fn run_case(case: &Value, tmp: &Path) -> Value {
    let home = tmp.join("home");
    let root = tmp.join("state/providers");
    let alt = tmp.join("alt");
    for dir in [&home.join(".claude"), &root, &alt, &tmp.join("codex")] {
        std::fs::create_dir_all(dir).unwrap();
    }
    let scrub: Vec<String> = std::env::vars()
        .map(|(k, _)| k)
        .filter(|k| k.starts_with("FNO_") || k.starts_with("CLAUDE_") || k.starts_with("CODEX_"))
        .collect();
    for key in scrub {
        std::env::remove_var(key);
    }
    std::env::set_var("HOME", &home);
    std::env::set_var("FNO_STATE_DIR", tmp.join("state"));
    std::env::set_var("USER", "tester");
    let tmp_text = tmp.to_str().unwrap().to_string();
    if let Some(env) = case.get("env").and_then(Value::as_object) {
        for (key, value) in env {
            std::env::set_var(key, value.as_str().unwrap().replace("{TMP}", &tmp_text));
        }
    }
    if let Some(files) = case.get("store").and_then(Value::as_object) {
        for (rel, text) in files {
            let path = root.join(rel);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, text.as_str().unwrap()).unwrap();
        }
    }
    let slot_files = [
        ("slot", home.join(".claude/.credentials.json")),
        ("alt", alt.join(".credentials.json")),
        ("codex", store::codex_slot_auth_path()),
    ];
    if let Some(files) = case.get("files").and_then(Value::as_object) {
        for (label, path) in &slot_files {
            if let Some(text) = files.get(*label).and_then(Value::as_str) {
                std::fs::create_dir_all(path.parent().unwrap()).unwrap();
                std::fs::write(path, text).unwrap();
            }
        }
    }
    let mut roles = BTreeMap::new();
    roles.insert(store::UNSCOPED_SERVICE.to_string(), "unscoped".to_string());
    roles.insert(
        binding::keychain_service(&home.join(".claude")).unwrap(),
        "scoped:slot".to_string(),
    );
    roles.insert(
        binding::keychain_service(&alt).unwrap(),
        "scoped:alt".to_string(),
    );
    let obj = |key: &str| {
        case.get(key)
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default()
    };
    let host = FakeHost {
        keychain_slot: case
            .get("platform")
            .and_then(Value::as_str)
            .unwrap_or("darwin")
            == "darwin",
        roles: roles.clone(),
        keychain: RefCell::new(obj("keychain")),
        fail: obj("keychain_fail"),
        writes: RefCell::new(Vec::new()),
        processes: case
            .get("processes")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default(),
        tmp: tmp_text,
        started: obj("started"),
        alive: case
            .get("alive")
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(Value::as_u64).collect())
            .unwrap_or_default(),
        login: case
            .get("codex_login")
            .and_then(Value::as_str)
            .map(str::to_string),
        login_home: RefCell::new(None),
    };
    let now = case["now"].as_f64().unwrap();
    let a = &case["args"];
    let s = |key: &str| a[key].as_str().unwrap_or_default().to_string();
    let rec = |key: &str| {
        let r = &a[key];
        (
            r["id"].as_str().unwrap().to_string(),
            r["harness"].as_str().unwrap().to_string(),
            r["account_id"].as_str().unwrap_or_default().to_string(),
        )
    };
    let path_json = |p: PathBuf| json!(p.to_string_lossy());
    let outcome: Result<Value, StoreError> = match case["op"].as_str().unwrap() {
        "snapshot_current" => {
            let (id, harness, account) = rec("record");
            store::snapshot_current(&root, &id, &harness, &account, &host, now).map(path_json)
        }
        "write_snapshot" => {
            let (id, harness, account) = rec("record");
            let keychain = harness == "claude" && host.keychain_slot();
            store::write_snapshot(&root, &id, &harness, &account, &s("blob"), keychain, now)
                .map(path_json)
        }
        "duplicate_holder" => Ok(json!(store::duplicate_credential_holder(
            &root,
            &s("blob"),
            &s("exclude_id")
        ))),
        "age_labels" => Ok(json!(a["seconds"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| store::age_label_from_seconds(v.as_f64().unwrap()))
            .collect::<Vec<_>>())),
        "snapshot_age_label" => Ok(json!(store::snapshot_age_label(&root, &s("id"), now))),
        "stamp" => {
            store::stamp_active_slot(&root, &s("cli"), &s("id")).unwrap();
            Ok(Value::Null)
        }
        "set_taint" => {
            let writers: Vec<TaintWriter> = a["pids"]
                .as_array()
                .unwrap()
                .iter()
                .map(|p| match p {
                    Value::Array(pair) => {
                        TaintWriter::Sampled(pair[0].as_u64().unwrap() as u32, pair[1].as_f64())
                    }
                    other => TaintWriter::Pid(other.as_u64().unwrap() as u32),
                })
                .collect();
            let tainted = a["tainted"].as_bool().unwrap();
            store::set_slot_taint(&root, &s("cli"), tainted, &writers, &host).unwrap();
            Ok(Value::Null)
        }
        "tainting_writers" => Ok(match store::tainting_writers(&root, &s("cli")) {
            Some(rows) => json!(rows
                .iter()
                .map(|(pid, started)| json!([pid, started]))
                .collect::<Vec<_>>()),
            None => Value::Null,
        }),
        "still_live" => store::taint_writers_still_live(&root, &s("cli"), &host).map(|v| json!(v)),
        "write_principal" => {
            store::write_record_principal(&root, &s("id"), &a["principal"], now).unwrap();
            Ok(Value::Null)
        }
        "clear_principal" => {
            store::clear_record_principal(&root, &s("id"));
            Ok(Value::Null)
        }
        "clear_cache" => {
            store::clear_slot_principal_cache(&root, &s("cli"));
            Ok(Value::Null)
        }
        "write_slot_blob" => {
            store::write_slot_blob(&s("cli"), &s("blob"), None, &host).map(|()| Value::Null)
        }
        "read_slot_blob" => {
            let cfg = (a["config_dir"].as_str() == Some("alt")).then_some(alt.as_path());
            store::read_slot_blob(&s("cli"), cfg, &host).map(|v| json!(v))
        }
        "verify_slot" => {
            let (_, harness, _) = rec("record");
            store::verify_slot(&harness, &s("expected"), &host).map(|v| json!(v))
        }
        "codex_auth" => Ok(json!(a["blobs"]
            .as_array()
            .unwrap()
            .iter()
            .map(|b| store::codex_auth_present(b.as_str().unwrap()))
            .collect::<Vec<_>>())),
        "codex_login" => store::codex_login_ok(&host).map(
            |got| json!({ "ok": got.ok, "reason": got.reason, "home": *host.login_home.borrow() }),
        ),
        "matchers" => Ok(json!(a["rows"]
            .as_array()
            .unwrap()
            .iter()
            .map(|row| {
                let name = row[0].as_str();
                let cmdline: Vec<String> = row[1]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|c| c.as_str().unwrap().to_string())
                    .collect();
                json!([
                    store::looks_like_claude(name, &cmdline),
                    store::looks_like_codex(name, &cmdline)
                ])
            })
            .collect::<Vec<_>>())),
        "pinning" => store::pinning_sessions_for(&s("cli"), &host).map(|sessions| {
            json!(sessions
                .iter()
                .map(|p| json!({ "pid": p.pid, "cmdline": p.cmdline, "started": p.started }))
                .collect::<Vec<_>>())
        }),
        other => panic!("unknown op {other}"),
    };
    let (result, error) = match outcome {
        Ok(v) => (v, Value::Null),
        Err(e) => (Value::Null, error_json(&e, &roles)),
    };
    let (tree, modes) = dump_tree(&root);
    let mut files = Map::new();
    for (label, path) in &slot_files {
        if let Ok(text) = std::fs::read_to_string(path) {
            files.insert(label.to_string(), json!(text));
        }
    }
    json!({
        "result": result,
        "error": error,
        "store": tree,
        "modes": modes,
        "files": files,
        "keychain_writes": host.writes.borrow().clone(),
    })
}

#[test]
fn managed_store_matches_the_python_leg() {
    let mut cases: Vec<PathBuf> = std::fs::read_dir(golden_dir())
        .unwrap()
        .filter_map(|entry| entry.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|e| e == "json"))
        .collect();
    cases.sort();
    assert!(cases.len() >= 40, "golden cases missing: {}", cases.len());
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
