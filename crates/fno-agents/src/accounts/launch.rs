//! The spawn's account answers over [`env::overlay`] and
//! [`env::launch_env`]: the `--account` overlay with the scrub list, the
//! `--dispatch-account` cutover overlay, and which record a dead worker's
//! env pinned. Every refusal is a receipt and returns no env.

use crate::accounts::env::{self, Sources};
use crate::accounts::{credentials, records};
use crate::claude_ask::py_repr;
use serde_json::{json, Map, Value};
use std::path::Path;

/// The env carrier a supervisor canary passes its PROVEN overlay through
/// (never argv, it carries real credential values).
pub const DISPATCH_ACCOUNT_ENV: &str = "FNO_DISPATCH_ACCOUNT_ENV";

/// The `--account <id>` answer: the overlay, its lane, the scrub list, and
/// the gate's last login-probe refusal for the overlay's dir.
pub fn account(id: &str, cwd: &Path, src: &Sources<'_>) -> Value {
    match env::overlay(id, None, None, cwd, src) {
        Ok(got) => json!({
            "account_id": got.account_id,
            "env": got.env,
            "lane": got.lane,
            "scrub": credentials::SCRUB_AUTH_VARS,
            "login_probe": crate::paths::AgentsHome::from_env_opt().and_then(|home| {
                crate::claude_login::probe_refusal(
                    &home,
                    id,
                    got.env.get("CLAUDE_CONFIG_DIR").map(String::as_str),
                )
            }),
        }),
        Err(receipt) => json!({ "refusal": receipt }),
    }
}

/// Parse the proven-credential carrier: a non-empty flat string map.
fn parse_carrier(raw: &str) -> Result<Map<String, Value>, String> {
    let parsed: Value = serde_json::from_str(raw).map_err(|e| e.to_string())?;
    let map = parsed
        .as_object()
        .filter(|m| m.values().all(Value::is_string))
        .ok_or("carrier payload must be a flat string map")?;
    if map.is_empty() {
        return Err("carrier payload is empty".to_string());
    }
    Ok(map.clone())
}

/// The `--dispatch-account` answer: the record must exist under `workdir`
/// and its harness must equal the spawn's resolved harness. A proven carrier
/// IS the overlay (re-deriving could stage other credentials than the ones
/// the canary proved); otherwise the record's launch env is.
pub fn dispatch(
    account: Option<&str>,
    harness: &str,
    workdir: &Path,
    carrier: Option<&str>,
    src: &Sources<'_>,
) -> Value {
    let Some(account) = account else {
        return match carrier {
            Some(_) => json!({ "refusal": "refusing: the proven-credential carrier is set but no \
                --dispatch-account was given; no worker launched" }),
            None => json!({ "env": Value::Null }),
        };
    };
    let shown = py_repr(account);
    let proven = match carrier.filter(|raw| !raw.is_empty()).map(parse_carrier) {
        Some(Err(why)) => {
            return json!({ "refusal": format!(
                "refusing --dispatch-account {shown}: the proven-credential carrier is \
                 unreadable ({why}); no worker launched"
            ) })
        }
        Some(Ok(map)) => Some(map),
        None => None,
    };
    let resolved = records::load(Some(workdir), workdir)
        .map_err(|e| e.to_string())
        .and_then(|accounts| {
            let record = accounts
                .by_id(account)
                .ok_or("not a registered provider record")?;
            let got = record.harness.trim();
            if got != harness {
                let got = if got.is_empty() { "<no harness>" } else { got };
                return Err(format!(
                    "record is a {got} account but the spawn resolves {harness}"
                ));
            }
            match &proven {
                Some(map) => Ok(map.clone()),
                None => env::launch_env(account, Some(workdir), None, workdir, src)
                    .map(|env| {
                        env.into_iter()
                            .map(|(k, v)| (k, Value::from(v)))
                            .collect::<Map<String, Value>>()
                    })
                    .map_err(|e| e.message),
            }
        });
    match resolved {
        Ok(env) => {
            let mut keys: Vec<&String> = env.keys().collect();
            keys.sort();
            let source = if proven.is_some() {
                "canary-proven carrier"
            } else {
                "record-derived"
            };
            json!({ "env": env, "credential_source": source, "credential_env_keys": keys })
        }
        Err(why) => json!({ "refusal": format!(
            "refusing --dispatch-account {shown}: {why}; no worker launched"
        ) }),
    }
}

/// Which claude record's overlay equals `pinned`, or `None`. A quota death
/// is recorded against this id; an unresolvable record is never a match.
pub fn account_for_env(
    pinned: &Map<String, Value>,
    cwd: &Path,
    src: &Sources<'_>,
) -> Option<String> {
    if pinned.is_empty() {
        return None;
    }
    let accounts = records::load(None, cwd).ok()?;
    accounts
        .records
        .iter()
        .filter(|r| r.harness == "claude")
        .find(|r| {
            env::overlay(&r.id, None, None, cwd, src).is_ok_and(|got| {
                got.env.len() == pinned.len()
                    && got
                        .env
                        .iter()
                        .all(|(k, v)| pinned.get(k).and_then(Value::as_str) == Some(v))
            })
        })
        .map(|r| r.id.clone())
}

/// How a spawn's account axes proceed on the Rust arm.
#[derive(Debug)]
pub enum Seam {
    /// No account axis on the spawn.
    None,
    /// The overlay to apply before launch, the id the row stamps as its
    /// launch account, and the cutover receipt line when one was asked.
    Apply {
        env: Map<String, Value>,
        launch_account: String,
        note: Option<String>,
    },
    /// A refusal receipt; the spawn exits 2 before the gate takes a slot.
    Refuse(String),
}

/// Decide `--account` and `--dispatch-account`. `--account` is claude-only
/// and resolves first; the cutover's overlay layers over it, as `cmd_spawn`
/// merged them.
pub fn seam(
    account: Option<&str>,
    dispatch_account: Option<&str>,
    carrier: Option<&str>,
    provider: &str,
    cwd: &Path,
    src: &Sources<'_>,
) -> Seam {
    if account.is_none() && dispatch_account.is_none() && carrier.is_none() {
        return Seam::None;
    }
    let refusal = |got: &Value| {
        got.get("refusal")
            .and_then(Value::as_str)
            .map(str::to_string)
    };
    let mut env = Map::new();
    if let Some(id) = account {
        if provider != "claude" {
            return Seam::Refuse(format!(
                "--account is claude-only; got harness {}",
                py_repr(provider)
            ));
        }
        let got = self::account(id, cwd, src);
        if let Some(receipt) = refusal(&got) {
            return Seam::Refuse(receipt);
        }
        env.extend(got["env"].as_object().cloned().unwrap_or_default());
    }
    let got = dispatch(dispatch_account, provider, cwd, carrier, src);
    if let Some(receipt) = refusal(&got) {
        return Seam::Refuse(receipt);
    }
    let note = dispatch_account.map(|id| {
        env.extend(got["env"].as_object().cloned().unwrap_or_default());
        let keys: Vec<&str> = got["credential_env_keys"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .collect();
        format!(
            "dispatch account: {id} ({}; env keys: {})",
            got["credential_source"].as_str().unwrap_or(""),
            keys.join(", ")
        )
    });
    Seam::Apply {
        env,
        launch_account: account.or(dispatch_account).unwrap_or_default().to_string(),
        note,
    }
}

/// The spawn arm's account step: a refusal prints its receipt and exits 2;
/// an overlay goes into this process's env, which every harness child
/// inherits. A claude launch scrubs the inherited auth vars first, and a
/// moved HOME is sealed so this client's own state reads stay put. The
/// carrier is claimed whatever the flags say, so it never reaches a child.
/// A routed spawn is left whole to the route arm.
pub fn seam_exit(params: &Value, provider: &str) -> Option<i32> {
    // The stamp dies at entry, before anything: an inherited id is the
    // PARENT's account, and every path below either re-stamps it (Apply) or
    // hands the spawn to Python, whose cmd_spawn re-stamps after it resolves.
    std::env::remove_var(crate::state::LAUNCH_ACCOUNT_ENV_KEY);
    // The carrier is claimed before any early return, so its proven values
    // never ride into a child that did not ask for them: a routed spawn
    // without --dispatch-account refuses exactly as cmd_spawn would.
    let carrier = std::env::var(DISPATCH_ACCOUNT_ENV).ok();
    std::env::remove_var(DISPATCH_ACCOUNT_ENV);
    // A route with either account flag composes in the Python CLI, which the
    // route arm re-execs. Scrubbing here first would strip a vendor key the
    // route reads from a scrubbed var (ANTHROPIC_AUTH_TOKEN as api_key_env).
    if params.get("route").is_some() {
        if carrier.is_some() {
            eprintln!(
                "refusing: the proven-credential carrier is set but no \
                 --dispatch-account was given; no worker launched"
            );
            return Some(2);
        }
        return None;
    }
    let text = |key: &str| params.get(key).and_then(Value::as_str);
    let cwd = text("cwd")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_default());
    let src = Sources::system();
    match seam(
        text("account"),
        text("dispatch_account"),
        carrier.as_deref(),
        provider,
        &cwd,
        &src,
    ) {
        Seam::None => None,
        Seam::Refuse(line) => {
            eprintln!("{line}");
            Some(2)
        }
        Seam::Apply {
            env,
            launch_account,
            note,
        } => {
            if provider == "claude" {
                for var in credentials::SCRUB_AUTH_VARS {
                    std::env::remove_var(var);
                }
            }
            for (key, value) in credentials::seal(&env, &cwd) {
                if let Some(value) = value.as_str() {
                    std::env::set_var(key, value);
                }
            }
            std::env::set_var(crate::state::LAUNCH_ACCOUNT_ENV_KEY, launch_account);
            if let Some(note) = note {
                eprintln!("{note}");
            }
            None
        }
    }
}

fn payload_cwd(payload: &Value) -> std::path::PathBuf {
    payload
        .get("cwd")
        .and_then(Value::as_str)
        .filter(|v| !v.is_empty())
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_default())
}

/// The `spawn-overlay` kinds `account-overlay`, `dispatch-account`,
/// `account-for-env` and `login-present`, over the system sources.
pub fn resolve_kind(kind: &str, payload: &Value) -> Result<Value, String> {
    let src = Sources::system();
    let cwd = payload_cwd(payload);
    let text = |key: &str| payload.get(key).and_then(Value::as_str);
    Ok(match kind {
        "account-overlay" => account(text("account").unwrap_or(""), &cwd, &src),
        "dispatch-account" => dispatch(
            text("account"),
            text("harness").unwrap_or(""),
            &cwd,
            text("carrier"),
            &src,
        ),
        "account-for-env" => {
            let empty = Map::new();
            let pinned = payload
                .get("env")
                .and_then(Value::as_object)
                .unwrap_or(&empty);
            json!({ "account": account_for_env(pinned, &cwd, &src) })
        }
        _ => {
            json!({ "login_present": env::login_present(Path::new(text("dir").unwrap_or("")), src.slot) })
        }
    })
}
