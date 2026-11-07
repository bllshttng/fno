//! The account switch while claude sessions are live: a stale target never
//! stamps silently, a terminal switch moves the global pointer, a reconcile
//! proves the slot under live pinned sessions, and the output names who now
//! bills to the new account.

use fno_agents::accounts::binding::{self, SlotProbe};
use fno_agents::accounts::reconcile;
use fno_agents::accounts::records::Record;
use fno_agents::accounts::store::{self, CodexRun, Host, ProcInfo, SecurityRun, StoreError};
use fno_agents::accounts::usage_probe::{self, Ctx};
use fno_agents::accounts::use_switch::{self, Outside, VaultAnswer};
use fno_agents::claude_vault::{ExternalFailure, Principal};
use serde_json::{Map, Value};
use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

/// The tests set HOME and the state dir, which are process-wide.
static ENV: Mutex<()> = Mutex::new(());

const WORK: &str = r#"{"claudeAiOauth": {"accessToken": "t-work", "refreshToken": "r-work"}}"#;
const MAKERS: &str = r#"{"claudeAiOauth": {"accessToken": "t-makers", "refreshToken": "r-makers", "expiresAt": 99999999999999}}"#;
const MAKERS_STALE: &str =
    r#"{"claudeAiOauth": {"accessToken": "t-makers", "refreshToken": "r-makers", "expiresAt": 1}}"#;

fn records_toml(ids: &[&str]) -> String {
    let mut out = String::from("[accounts]\nauto_switch = true\n");
    for id in ids {
        out.push_str(&format!(
            "\n[[accounts.records]]\nid = \"{id}\"\nname = \"{id}\"\nharness = \"claude\"\nauth = \"managed\"\npriority = 10\n"
        ));
    }
    out
}

struct World {
    scoped: String,
    keychain: RefCell<Map<String, Value>>,
    pins: RefCell<Vec<Vec<u32>>>,
    verdict: String,
    sync_note: Option<String>,
    terminal: bool,
    calls: RefCell<Vec<String>>,
}

impl World {
    fn new(home: &Path, verdict: &str, terminal: bool) -> Self {
        let keychain: Map<String, Value> = [
            ("unscoped".to_string(), Value::String(WORK.to_string())),
            ("scoped:slot".to_string(), Value::String(WORK.to_string())),
        ]
        .into_iter()
        .collect();
        World {
            scoped: binding::keychain_service(&home.join(".claude")).unwrap(),
            keychain: RefCell::new(keychain),
            pins: RefCell::new(Vec::new()),
            verdict: verdict.to_string(),
            sync_note: None,
            terminal,
            calls: RefCell::new(Vec::new()),
        }
    }

    fn role(&self, service: &str) -> String {
        if service == store::UNSCOPED_SERVICE {
            "unscoped".to_string()
        } else if service == self.scoped {
            "scoped:slot".to_string()
        } else {
            format!("other:{service}")
        }
    }

    fn item(&self, role: &str) -> String {
        self.keychain.borrow()[role].as_str().unwrap().to_string()
    }
}

fn flag(args: &[String], name: &str) -> String {
    let at = args.iter().position(|a| a == name).unwrap();
    args[at + 1].clone()
}

impl Host for World {
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
                self.keychain
                    .borrow_mut()
                    .insert(role, Value::String(flag(args, "-w")));
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
        pins.remove(0)
            .into_iter()
            .map(|pid| ProcInfo {
                pid,
                name: None,
                cmdline: vec!["codex".to_string(), "claude".to_string()],
                started: Some(1_700_000_000.0),
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
        CodexRun::Missing
    }
}

impl Outside for World {
    fn vault(&self, action: &str, _root: &Path, id: Option<&str>) -> Result<VaultAnswer, String> {
        self.calls
            .borrow_mut()
            .push(format!("{action}:{}", id.unwrap_or("")));
        Ok(if action == "refresh" {
            VaultAnswer::verdict(self.verdict.clone())
        } else {
            VaultAnswer {
                verdict: "written".to_string(),
                note: self.sync_note.clone(),
            }
        })
    }
    fn login(&self, _root: &Path, id: &str) -> i32 {
        self.calls.borrow_mut().push(format!("login:{id}"));
        0
    }
    fn terminal(&self) -> bool {
        self.terminal
    }
    fn emit(&self, _fields: Map<String, Value>) {}
    fn now(&self) -> f64 {
        1_800_000_000.0
    }
    fn lock_timeout(&self) -> Duration {
        Duration::from_millis(200)
    }
}

struct Lab {
    _tmp: tempfile::TempDir,
    home: PathBuf,
    repo: PathBuf,
    store: PathBuf,
}

impl Lab {
    fn new(global: &[&str], project: &[&str], makers_blob: &str) -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(tmp.path()).unwrap();
        let home = root.join("home");
        let repo = root.join("repo");
        let store = root.join("state/providers");
        for dir in [home.join(".claude"), home.join(".fno"), repo.join(".fno")] {
            std::fs::create_dir_all(dir).unwrap();
        }
        for (key, _) in std::env::vars() {
            if key.starts_with("FNO_") || key.starts_with("CLAUDE_") {
                std::env::remove_var(key);
            }
        }
        std::env::set_var("HOME", &home);
        std::env::set_var("FNO_STATE_DIR", root.join("state"));
        std::env::set_var("FNO_REPO_ROOT", &repo);
        std::env::set_var("PWD", &repo);
        if !global.is_empty() {
            std::fs::write(home.join(".fno/config.toml"), records_toml(global)).unwrap();
        }
        if !project.is_empty() {
            std::fs::write(repo.join(".fno/config.toml"), records_toml(project)).unwrap();
        }
        let files = [
            (".active-claude", "work"),
            ("work/blob", WORK),
            ("makers/blob", makers_blob),
            (
                "makers/meta.json",
                r#"{"harness": "claude", "account_id": "makers", "captured_at": "2026-01-01T00:00:00Z", "kind": "keychain"}"#,
            ),
        ];
        for (rel, text) in files {
            let path = store.join(rel);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, text).unwrap();
        }
        Lab {
            _tmp: tmp,
            home,
            repo,
            store,
        }
    }

    fn stamp(&self) -> String {
        std::fs::read_to_string(self.store.join(".active-claude"))
            .unwrap()
            .trim()
            .to_string()
    }

    fn global_active(&self) -> Option<String> {
        let text = std::fs::read_to_string(self.home.join(".fno/config.toml")).ok()?;
        let doc: toml::Table = text.parse().ok()?;
        doc["accounts"]
            .get("active")
            .and_then(|v| v.as_str())
            .map(str::to_string)
    }

    fn use_makers(&self, world: &World, extra: &[&str]) -> fno_agents::accounts::register::Run {
        let mut args = vec!["makers".to_string()];
        args.extend(extra.iter().map(|a| a.to_string()));
        use_switch::run_with(&args, &self.repo, world, world)
    }
}

#[test]
fn a_stale_target_without_a_terminal_refuses_and_names_both_logins() {
    let _env = ENV.lock().unwrap_or_else(|e| e.into_inner());
    let lab = Lab::new(&["work", "makers"], &[], MAKERS_STALE);
    let world = World::new(&lab.home, "live-owner", false);
    let got = lab.use_makers(&world, &[]);
    assert_ne!(got.code, 0);
    assert!(got.stderr.contains("claude /login"));
    assert!(got.stderr.contains("CLAUDE_CONFIG_DIR="));
    assert_eq!(lab.stamp(), "work");
    assert_eq!(world.item("unscoped"), WORK);
    assert_eq!(world.item("scoped:slot"), WORK);
    assert!(!world.calls.borrow().iter().any(|c| c.starts_with("sync")));
}

#[test]
fn a_stale_target_on_a_terminal_logs_in_and_defaults_to_the_global_scope() {
    let _env = ENV.lock().unwrap_or_else(|e| e.into_inner());
    let lab = Lab::new(&["work", "makers"], &[], MAKERS_STALE);
    let world = World::new(&lab.home, "live-owner", true);
    let got = lab.use_makers(&world, &[]);
    assert_eq!(got.code, 0);
    assert!(world.calls.borrow().contains(&"login:makers".to_string()));
    assert!(got.stdout.contains("scope=global"));
    assert_eq!(lab.global_active().as_deref(), Some("makers"));
}

#[test]
fn a_switch_names_an_expired_outgoing_item_and_both_items_land() {
    let _env = ENV.lock().unwrap_or_else(|e| e.into_inner());
    let lab = Lab::new(&["work", "makers"], &[], MAKERS);
    let mut world = World::new(&lab.home, "fresh", false);
    world.sync_note = Some("skipped expired: scoped-keychain".to_string());
    let got = lab.use_makers(&world, &["--scope", "global"]);
    assert_eq!(got.code, 0);
    assert!(got.stderr.contains("scoped-keychain"));
    assert!(got.stdout.contains("unscoped: makers"));
    assert!(got.stdout.contains("scoped: makers"));
    assert_eq!(world.item("unscoped"), MAKERS);
    assert_eq!(world.item("scoped:slot"), MAKERS);
}

#[test]
fn a_project_only_record_refuses_the_terminal_global_default() {
    let _env = ENV.lock().unwrap_or_else(|e| e.into_inner());
    let lab = Lab::new(&["work"], &["makers"], MAKERS);
    let world = World::new(&lab.home, "fresh", true);
    let got = lab.use_makers(&world, &[]);
    assert_ne!(got.code, 0);
    assert!(got.stderr.contains("--scope project"));
    assert_eq!(lab.stamp(), "work");
    assert!(lab.global_active().is_none());
}

#[test]
fn three_live_sessions_are_named_when_the_bill_moves() {
    let _env = ENV.lock().unwrap_or_else(|e| e.into_inner());
    let lab = Lab::new(&["work", "makers"], &[], MAKERS);
    let world = World::new(&lab.home, "fresh", false);
    world.pins.borrow_mut().push(vec![4242, 4343, 4444]);
    let got = lab.use_makers(&world, &["--scope", "global"]);
    assert_eq!(got.code, 0);
    assert!(got
        .stdout
        .contains("3 live claude sessions now bill to 'makers' (were 'work')"));
    for pid in ["pid 4242", "pid 4343", "pid 4444"] {
        assert!(got.stdout.contains(pid));
    }
}

struct Probe {
    keychain: RefCell<Map<String, Value>>,
    scoped: String,
}

impl SlotProbe for Probe {
    fn keychain(&self, service: &str) -> Result<Option<String>, ExternalFailure> {
        let role = if service == store::UNSCOPED_SERVICE {
            "unscoped"
        } else if service == self.scoped {
            "scoped:slot"
        } else {
            "other"
        };
        Ok(self
            .keychain
            .borrow()
            .get(role)
            .and_then(Value::as_str)
            .map(str::to_string))
    }
    fn profile(&self, bearer: &str) -> Result<Principal, ExternalFailure> {
        match bearer {
            "t-makers" => Ok(Principal {
                account_uuid: "acc-makers".to_string(),
                organization_uuid: "org-1".to_string(),
                email: Some("makers@x".to_string()),
            }),
            "t-ready2" => Ok(Principal {
                account_uuid: "acc-ready".to_string(),
                organization_uuid: "org-r".to_string(),
                email: Some("ready@x".to_string()),
            }),
            "t-old" => Err(ExternalFailure::Rejected),
            "t-ready" => Err(ExternalFailure::Rejected),
            _ => Err(ExternalFailure::Unavailable),
        }
    }
}

/// Twenty recorded taint writers, all alive.
struct BusyHost {
    pids: Vec<u32>,
}

impl Host for BusyHost {
    fn keychain_slot(&self) -> bool {
        true
    }
    fn security(&self, _args: &[String]) -> Result<SecurityRun, StoreError> {
        Err(StoreError::Keychain(
            "reconcile reads through the probe".into(),
        ))
    }
    fn processes(&self) -> Vec<ProcInfo> {
        Vec::new()
    }
    fn env_value(&self, _pid: u32, _key: &str) -> Result<Option<String>, ()> {
        Ok(None)
    }
    fn started_at(&self, pid: u32) -> Option<f64> {
        self.pids.contains(&pid).then_some(100.0)
    }
    fn pid_exists(&self, pid: u32) -> bool {
        self.pids.contains(&pid)
    }
    fn self_pid(&self) -> u32 {
        0
    }
    fn codex_login_status(&self, _codex_home: &Path) -> CodexRun {
        CodexRun::Missing
    }
}

/// The scoped item claude launches read, expired while no claude ran.
const MAKERS_EXPIRED: &str =
    r#"{"claudeAiOauth": {"accessToken": "t-old", "refreshToken": "r-old", "expiresAt": 1}}"#;

#[test]
fn reconcile_proves_the_slot_while_twenty_writers_are_alive() {
    let _env = ENV.lock().unwrap_or_else(|e| e.into_inner());
    let lab = Lab::new(&["work", "makers"], &[], MAKERS);
    let pids: Vec<u32> = (1000..1020).collect();
    let writers: Vec<Value> = pids
        .iter()
        .map(|p| serde_json::json!({"pid": p, "started": 100.0}))
        .collect();
    std::fs::write(
        lab.store.join(".active-claude.tainted"),
        serde_json::json!({"pids": pids, "writers": writers}).to_string(),
    )
    .unwrap();
    std::fs::write(
        lab.store.join("makers/meta.json"),
        r#"{"harness": "claude", "account_id": "makers", "captured_at": "2026-01-01T00:00:00Z", "kind": "keychain", "principal": {"account_uuid": "acc-makers", "organization_uuid": "org-1", "email": "makers@x"}, "principal_at": "2026-01-01T00:00:00Z"}"#,
    )
    .unwrap();
    let scoped = binding::keychain_service(&lab.home.join(".claude")).unwrap();
    let slot: Map<String, Value> = [
        ("unscoped".to_string(), Value::String(MAKERS.to_string())),
        (
            "scoped:slot".to_string(),
            Value::String(MAKERS_EXPIRED.to_string()),
        ),
    ]
    .into_iter()
    .collect();
    let probe = Probe {
        keychain: RefCell::new(slot),
        scoped,
    };
    let records: Vec<Record> = ["work", "makers"]
        .iter()
        .map(|id| Record {
            id: id.to_string(),
            name: id.to_string(),
            harness: "claude".to_string(),
            auth: "managed".to_string(),
            priority: 100,
            route_provider_id: None,
            model_name: None,
            credentials_source: None,
            env: None,
            route: None,
            reset_timezone: None,
            window_minutes: None,
            account_id: id.to_string(),
            tags: Vec::new(),
            description: None,
            config_dir: None,
            pricing: None,
            cost_cap_usd_per_session: None,
            extra: Map::new(),
        })
        .collect();
    let run = || {
        reconcile::reconcile_slot(
            "claude",
            &records,
            &lab.store,
            &probe,
            &BusyHost { pids: pids.clone() },
            Duration::from_millis(200),
            1_800_000_000.0,
        )
    };
    // The endpoint rejects the expired item, and nothing ever proved it.
    assert_eq!(run().outcome, "credential-rejected");
    // A proof cached for exactly that token still names its account.
    binding::note_slot_principal("claude", &lab.store, "acc-makers/org-1", "t-old", 1.0);
    let got = run();
    assert_eq!(got.outcome, "matched");
    assert_eq!(lab.stamp(), "makers");
    assert!(!lab.store.join(".active-claude.tainted").exists());
    assert_eq!(
        std::fs::read_to_string(lab.store.join("makers/blob")).unwrap(),
        MAKERS
    );
}

/// The expired blob a switch materialized: byte-equal to the record's stored
/// blob, proofed by no token cache.
const READYRULE_EXPIRED: &str =
    r#"{"claudeAiOauth": {"accessToken": "t-ready", "refreshToken": "r-ready", "expiresAt": 1}}"#;
/// The same account's live item, served by the slot's other reader.
const READYRULE_LIVE: &str = r#"{"claudeAiOauth": {"accessToken": "t-ready2", "refreshToken": "r2", "expiresAt": 99999999999999}}"#;
const READYRULE_META: &str = r#"{"harness": "claude", "account_id": "readyrule", "captured_at": "2026-01-01T00:00:00Z", "kind": "keychain", "principal": {"account_uuid": "acc-ready", "organization_uuid": "org-r", "email": "ready@x"}, "principal_at": "2026-01-01T00:00:00Z"}"#;
const MAKERS_META_PRINCIPAL: &str = r#"{"harness": "claude", "account_id": "makers", "captured_at": "2026-01-01T00:00:00Z", "kind": "keychain", "principal": {"account_uuid": "acc-makers", "organization_uuid": "org-1", "email": "makers@x"}, "principal_at": "2026-01-01T00:00:00Z"}"#;

fn claude_record(id: &str) -> Record {
    Record {
        id: id.to_string(),
        name: id.to_string(),
        harness: "claude".to_string(),
        auth: "managed".to_string(),
        priority: 100,
        route_provider_id: None,
        model_name: None,
        credentials_source: None,
        env: None,
        route: None,
        reset_timezone: None,
        window_minutes: None,
        account_id: id.to_string(),
        tags: Vec::new(),
        description: None,
        config_dir: None,
        pricing: None,
        cost_cap_usd_per_session: None,
        extra: Map::new(),
    }
}

fn seed_record(store: &Path, id: &str, blob: &str, meta: &str) {
    std::fs::create_dir_all(store.join(id)).unwrap();
    std::fs::write(store.join(id).join("blob"), blob).unwrap();
    std::fs::write(store.join(id).join("meta.json"), meta).unwrap();
}

fn readyrule_slot(scoped: &str) -> Probe {
    let slot: Map<String, Value> = [
        (
            "unscoped".to_string(),
            Value::String(READYRULE_LIVE.to_string()),
        ),
        (
            "scoped:slot".to_string(),
            Value::String(READYRULE_EXPIRED.to_string()),
        ),
    ]
    .into_iter()
    .collect();
    Probe {
        keychain: RefCell::new(slot),
        scoped: scoped.to_string(),
    }
}

fn readyrule_lab() -> Lab {
    let lab = Lab::new(&[], &[], MAKERS);
    seed_record(&lab.store, "readyrule", READYRULE_EXPIRED, READYRULE_META);
    std::fs::write(lab.store.join(".active-claude"), "readyrule").unwrap();
    lab
}

#[test]
fn resolve_names_the_stamped_record_from_its_materialized_expired_blob() {
    let _env = ENV.lock().unwrap_or_else(|e| e.into_inner());
    let lab = readyrule_lab();
    let probe = readyrule_slot(&binding::keychain_service(&lab.home.join(".claude")).unwrap());
    let records = [claude_record("makers"), claude_record("readyrule")];
    let got = binding::resolve(
        &binding::BindingRequest {
            record: records.iter().find(|r| r.id == "readyrule"),
            harness: "claude",
            bearer: None,
            root: &lab.store,
            records: &records,
            now: 1_800_000_000.0,
            ttl: binding::PRINCIPAL_TTL_S,
        },
        &probe,
    );
    assert_eq!(got.status, binding::MATCHED);
    assert_eq!(got.matched_record.as_deref(), Some("readyrule"));
}

#[test]
fn a_stored_blob_match_never_hides_a_second_identity() {
    let _env = ENV.lock().unwrap_or_else(|e| e.into_inner());
    let lab = readyrule_lab();
    let scoped = binding::keychain_service(&lab.home.join(".claude")).unwrap();
    let slot: Map<String, Value> = [
        ("unscoped".to_string(), Value::String(MAKERS.to_string())),
        (
            "scoped:slot".to_string(),
            Value::String(READYRULE_EXPIRED.to_string()),
        ),
    ]
    .into_iter()
    .collect();
    let probe = Probe {
        keychain: RefCell::new(slot),
        scoped,
    };
    let records = [claude_record("makers"), claude_record("readyrule")];
    let got = binding::resolve(
        &binding::BindingRequest {
            record: None,
            harness: "claude",
            bearer: None,
            root: &lab.store,
            records: &records,
            now: 1_800_000_000.0,
            ttl: binding::PRINCIPAL_TTL_S,
        },
        &probe,
    );
    assert_eq!(got.status, binding::AMBIGUOUS);
}

#[test]
fn reconcile_names_the_record_whose_stored_blob_the_slot_holds() {
    let _env = ENV.lock().unwrap_or_else(|e| e.into_inner());
    let lab = readyrule_lab();
    let probe = readyrule_slot(&binding::keychain_service(&lab.home.join(".claude")).unwrap());
    let records = vec![claude_record("makers"), claude_record("readyrule")];
    let got = reconcile::reconcile_slot(
        "claude",
        &records,
        &lab.store,
        &probe,
        &BusyHost { pids: Vec::new() },
        Duration::from_millis(200),
        1_800_000_000.0,
    );
    assert_eq!(got.outcome, "matched");
    assert_eq!(got.record_id.as_deref(), Some("readyrule"));
    assert_eq!(lab.stamp(), "readyrule");
    // The snapshot rotates the stored blob to the live item.
    assert_eq!(
        std::fs::read_to_string(lab.store.join("readyrule/blob")).unwrap(),
        READYRULE_LIVE
    );
}

#[test]
fn a_record_s_own_stored_bearer_is_attributed_before_the_endpoint() {
    let _env = ENV.lock().unwrap_or_else(|e| e.into_inner());
    let lab = Lab::new(&[], &[], MAKERS);
    // makers holds a dead token no endpoint can prove, but the record binds
    // its own principal, so its stored bearer files without an endpoint.
    seed_record(&lab.store, "makers", MAKERS_EXPIRED, MAKERS_META_PRINCIPAL);
    let scoped = binding::keychain_service(&lab.home.join(".claude")).unwrap();
    let probe = Probe {
        keychain: RefCell::new(Map::new()),
        scoped,
    };
    let records = [claude_record("makers")];
    let http_calls = Cell::new(0u32);
    let http = |_: &str| {
        http_calls.set(http_calls.get() + 1);
        Ok((401u16, "{}".to_string()))
    };
    let ctx = Ctx {
        cwd: &lab.repo,
        repo_root: None,
        store: Some(lab.store.clone()),
        records: &records,
        slot: &probe,
        host: &BusyHost { pids: Vec::new() },
        http: &http,
        route_env: &|_: &str| None,
        codex_home: lab.repo.join(".codex"),
    };
    let (snap, reason) = usage_probe::probe_detail(&ctx, &records[0], 1_800_000_000.0);
    assert!(snap.is_none());
    assert_eq!(reason, Some("credential-rejected"));
    // The record's own dead token was trusted enough to be tried.
    assert_eq!(http_calls.get(), 1);
}
