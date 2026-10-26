//! parity-stage: characterization
//! parity-oracle: fno.agents.account_env
//!
//! Characterization parity for `accounts::credentials` and
//! `accounts::launch`: the credentials compose and its receipt decision, the
//! state-root seal, the `--dispatch-account` answer and the dead worker's
//! account. Each `tests/golden/accounts_launch/*.json` case holds what the
//! deleted Python leg answered, captured by `accounts_launch_golden_driver.py`
//! (the case table lives there). The second test drives the spawn arm's seam
//! against the process env: an `--account` launch scrubs the inherited route,
//! a cutover onto the wrong harness refuses, and a spawn with no account
//! clears a stale stamp.
//!
//! Both tests rewrite the process env, so they hold one lock between them.

use fno_agents::accounts::binding::SlotProbe;
use fno_agents::accounts::env::Sources;
use fno_agents::accounts::{credentials, launch};
use fno_agents::claude_vault::{ExternalFailure, Principal};
use serde_json::{json, Map, Value};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

static ENV_LOCK: Mutex<()> = Mutex::new(());

struct NoLogin;

impl SlotProbe for NoLogin {
    fn keychain(&self, _service: &str) -> Result<Option<String>, ExternalFailure> {
        Ok(None)
    }
    fn profile(&self, _bearer: &str) -> Result<Principal, ExternalFailure> {
        Err(ExternalFailure::Unavailable)
    }
}

fn golden_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/golden/accounts_launch")
}

fn object(case: &Value, field: &str) -> Option<Map<String, Value>> {
    case.get(field).and_then(Value::as_object).cloned()
}

fn with_tmp(map: Map<String, Value>, tmp: &Path) -> Map<String, Value> {
    map.into_iter()
        .map(|(k, v)| {
            let text = v
                .as_str()
                .unwrap_or_default()
                .replace("{TMP}", tmp.to_str().unwrap());
            (k, Value::from(text))
        })
        .collect()
}

/// The accounts config, dirs and files a case names, as the driver's
/// `lay_down` writes them.
fn lay_down(case: &Value, tmp: &Path) {
    std::fs::create_dir_all(tmp.join("home/.claude")).unwrap();
    std::fs::create_dir_all(tmp.join("state/providers")).unwrap();
    for rel in case
        .get("dirs")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        std::fs::create_dir_all(tmp.join(rel.as_str().unwrap())).unwrap();
    }
    for (rel, text) in object(case, "files").unwrap_or_default() {
        let path = tmp.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text.as_str().unwrap()).unwrap();
    }
    let config = case.get("config").and_then(Value::as_str).unwrap_or("");
    std::fs::write(
        tmp.join("config.toml"),
        config.replace("{TMP}", tmp.to_str().unwrap()),
    )
    .unwrap();
}

fn clear_fno_env() {
    let keys: Vec<String> = std::env::vars()
        .map(|(k, _)| k)
        .filter(|k| {
            k.starts_with("FNO_") || k.starts_with("CLAUDE_") || k.starts_with("ANTHROPIC_")
        })
        .collect();
    for key in keys {
        std::env::remove_var(key);
    }
    std::env::remove_var("MISSING_TOKEN");
}

fn run_case(case: &Value, tmp: &Path, src: &Sources<'_>) -> Value {
    clear_fno_env();
    let text = |field: &str| case.get(field).and_then(Value::as_str);
    match text("op").unwrap() {
        "credentials" => {
            std::fs::write(tmp.join("config.toml"), "").unwrap();
            std::env::set_var("HOME", tmp.join("home"));
            std::env::set_var("FNO_CONFIG", tmp.join("config.toml"));
            let vendor = |endpoint: &str| credentials::vendor_for_endpoint(tmp, endpoint);
            let (env, made) = credentials::compose(
                object(case, "account_env").as_ref(),
                object(case, "route_env").as_ref(),
                &object(case, "base").unwrap_or_default(),
                text("account_id"),
                &vendor,
            );
            json!({ "env": env, "decision": made.to_value() })
        }
        "seal" => {
            let unset: Vec<&str> = case
                .get("unset")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .collect();
            for (key, dir) in [
                ("HOME", "home"),
                ("FNO_AGENTS_HOME", "agents"),
                ("FNO_STATE_DIR", "state"),
                ("FNO_CLAIMS_ROOT", "claims"),
            ] {
                if !unset.contains(&key) {
                    std::env::set_var(key, tmp.join(dir));
                }
            }
            let home = tmp.join("home").display().to_string();
            let env: Map<String, Value> = object(case, "env")
                .unwrap_or_default()
                .into_iter()
                .map(|(k, v)| (k, Value::from(v.as_str().unwrap().replace("{HOME}", &home))))
                .collect();
            json!({ "env": credentials::seal(&env, tmp) })
        }
        op => {
            lay_down(case, tmp);
            std::env::set_var("HOME", tmp.join("home"));
            std::env::set_var("FNO_CONFIG", tmp.join("config.toml"));
            std::env::set_var("FNO_STATE_DIR", tmp.join("state"));
            if op == "dispatch" {
                return launch::dispatch(
                    text("id"),
                    text("harness").unwrap(),
                    tmp,
                    text("carrier"),
                    src,
                );
            }
            let pinned = with_tmp(object(case, "env").unwrap_or_default(), tmp);
            json!({ "account": launch::account_for_env(&pinned, tmp, src) })
        }
    }
}

#[test]
fn launch_answers_match_the_frozen_python_leg() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut cases: Vec<PathBuf> = std::fs::read_dir(golden_dir())
        .unwrap()
        .filter_map(|entry| entry.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|e| e == "json"))
        .collect();
    cases.sort();
    assert!(cases.len() >= 24, "golden cases missing: {}", cases.len());
    let probe = NoLogin;
    let secret = |_: &str| None;
    let env_get = |key: &str| std::env::var(key).ok();
    let src = Sources {
        slot: &probe,
        secret: &secret,
        env_get: &env_get,
    };
    let saved: Vec<(String, String)> = std::env::vars().collect();
    let mut failures = Vec::new();
    for path in &cases {
        let case: Value = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(tmp.path()).unwrap();
        let got = run_case(&case, &root, &src);
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
    restore(saved);
    assert!(
        failures.is_empty(),
        "{} case(s) differ:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

fn restore(saved: Vec<(String, String)>) {
    for (k, _) in std::env::vars() {
        std::env::remove_var(k);
    }
    for (k, v) in saved {
        std::env::set_var(k, v);
    }
}

#[test]
fn the_spawn_seam_applies_refuses_and_clears() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let saved: Vec<(String, String)> = std::env::vars().collect();
    clear_fno_env();
    let tmp = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(tmp.path()).unwrap();
    let alt = root.join("alt");
    std::fs::create_dir_all(&alt).unwrap();
    std::fs::write(alt.join(".credentials.json"), "{}").unwrap();
    std::fs::write(
        root.join("config.toml"),
        format!(
            "[[accounts.records]]\nid = \"alt\"\nname = \"alt\"\nharness = \"claude\"\n\
             auth = \"managed\"\nconfig_dir = \"{}\"\n\
             [[accounts.records]]\nid = \"cx\"\nname = \"cx\"\nharness = \"codex\"\n\
             auth = \"api_key\"\n[accounts.records.env]\nOPENAI_API_KEY = \"cx-key\"\n",
            alt.display()
        ),
    )
    .unwrap();
    std::env::set_var("HOME", root.join("home"));
    std::env::set_var("FNO_CONFIG", root.join("config.toml"));
    std::env::set_var("FNO_STATE_DIR", root.join("state"));
    let cwd = root.display().to_string();

    // AC7: an --account launch applies the overlay in process, with the
    // inherited route scrubbed and the row's stamp set.
    std::env::set_var("ANTHROPIC_BASE_URL", "https://parent.example.test");
    let params = json!({ "account": "alt", "cwd": cwd });
    let code = launch::seam_exit(&params, "claude");
    let applied = (
        std::env::var("CLAUDE_CONFIG_DIR").ok(),
        std::env::var("ANTHROPIC_BASE_URL").ok(),
        std::env::var("FNO_LAUNCH_ACCOUNT").ok(),
    );

    // AC8: a codex record on a claude spawn refuses before any env moves.
    let refused = launch::seam(None, Some("cx"), None, "claude", &root, &Sources::system());

    // A carrier with no --dispatch-account refuses, and the carrier is
    // claimed so no child inherits it.
    std::env::set_var(launch::DISPATCH_ACCOUNT_ENV, "{\"A\": \"1\"}");
    let carrier_code = launch::seam_exit(&json!({ "cwd": cwd }), "claude");
    let carrier_left = std::env::var(launch::DISPATCH_ACCOUNT_ENV).ok();

    // A spawn with no account clears the stamp a parent left behind.
    std::env::set_var("FNO_LAUNCH_ACCOUNT", "parent-acct");
    let plain_code = launch::seam_exit(&json!({ "cwd": cwd }), "claude");
    let stamp_left = std::env::var("FNO_LAUNCH_ACCOUNT").ok();

    // A routed account spawn is the route arm's to bounce, env untouched: a
    // vendor may read its key from a var the scrub would strip. The stamp
    // still dies at entry; the bounce re-execs Python, whose cmd_spawn
    // re-stamps after it resolves.
    std::env::set_var("FNO_LAUNCH_ACCOUNT", "parent-acct");
    std::env::set_var("ANTHROPIC_AUTH_TOKEN", "route-key");
    let routed = json!({ "account": "alt", "route": "zai,glm-5.2", "cwd": cwd });
    let routed_code = launch::seam_exit(&routed, "claude");
    let route_key = std::env::var("ANTHROPIC_AUTH_TOKEN").ok();
    let routed_stamp = std::env::var("FNO_LAUNCH_ACCOUNT").ok();

    // A routed spawn carrying the proven carrier but no --dispatch-account
    // refuses, and the carrier is claimed so no child inherits its values.
    std::env::set_var(launch::DISPATCH_ACCOUNT_ENV, "{\"A\": \"1\"}");
    let routed_carrier_code =
        launch::seam_exit(&json!({ "route": "zai,glm-5.2", "cwd": cwd }), "claude");
    let routed_carrier_left = std::env::var(launch::DISPATCH_ACCOUNT_ENV).ok();

    restore(saved);
    assert_eq!(
        (routed_code, route_key.as_deref(), routed_stamp.as_deref()),
        (None, Some("route-key"), None)
    );
    assert_eq!(
        (routed_carrier_code, routed_carrier_left.as_deref()),
        (Some(2), None)
    );

    assert_eq!(code, None);
    assert_eq!(
        applied,
        (
            Some(alt.display().to_string()),
            None,
            Some("alt".to_string())
        )
    );
    match refused {
        launch::Seam::Refuse(line) => assert_eq!(
            line,
            "refusing --dispatch-account 'cx': record is a codex account but the spawn \
             resolves claude; no worker launched"
        ),
        other => panic!("expected a refusal, got {other:?}"),
    }
    assert_eq!((carrier_code, carrier_left), (Some(2), None));
    assert_eq!((plain_code, stamp_left), (None, None));
}
