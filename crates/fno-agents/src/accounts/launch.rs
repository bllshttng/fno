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

/// One spawn's resolved account: the sealed overlay and the id its row
/// stamps.
pub struct Applied {
    pub env: Map<String, Value>,
    pub launch_account: String,
}

/// The account step every lane shares: the inherited stamp and the carrier
/// are claimed whatever the flags say, so neither reaches a child that did
/// not ask for it; a refusal prints its receipt and answers exit 2. The
/// overlay comes back sealed, so a moved HOME cannot move this client's own
/// state reads.
fn resolve(params: &Value, provider: &str) -> Result<Option<Applied>, i32> {
    // The stamp dies at entry: an inherited id is the PARENT's account.
    std::env::remove_var(crate::state::LAUNCH_ACCOUNT_ENV_KEY);
    let carrier = std::env::var(DISPATCH_ACCOUNT_ENV).ok();
    std::env::remove_var(DISPATCH_ACCOUNT_ENV);
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
        Seam::None => Ok(None),
        Seam::Refuse(line) => {
            eprintln!("{line}");
            Err(2)
        }
        Seam::Apply {
            env,
            launch_account,
            note,
        } => {
            if let Some(note) = note {
                eprintln!("{note}");
            }
            std::env::set_var(crate::state::LAUNCH_ACCOUNT_ENV_KEY, &launch_account);
            Ok(Some(Applied {
                env: credentials::seal(&env, &cwd),
                launch_account,
            }))
        }
    }
}

/// The receipt a routed account spawn prints: the account contributes its
/// profile, the route owns auth and billing.
pub fn routed_receipt(applied: &Applied, route: &Map<String, Value>, cwd: &Path) -> String {
    let vendor = |endpoint: &str| credentials::vendor_for_endpoint(cwd, endpoint);
    let (_, made) = credentials::compose(
        Some(&applied.env),
        Some(route),
        &Map::new(),
        Some(&applied.launch_account),
        &vendor,
    );
    format!(
        "account: {} (profile only; auth {}, bills {})",
        applied.launch_account, made.auth, made.bills
    )
}

/// The thread and headless lanes' account step: the overlay goes into this
/// process's env, which every harness child inherits; a claude launch scrubs
/// the inherited auth vars first. `route_env` is the resolved route the
/// launch layers over this overlay; the caller resolves it first, because
/// the scrub can remove the var a vendor key lives in.
pub fn seam_exit(
    params: &Value,
    provider: &str,
    route_env: Option<&std::collections::BTreeMap<String, String>>,
) -> Option<i32> {
    let applied = match resolve(params, provider) {
        Err(code) => return Some(code),
        Ok(None) => return None,
        Ok(Some(applied)) => applied,
    };
    if provider == "claude" {
        for var in credentials::SCRUB_AUTH_VARS {
            std::env::remove_var(var);
        }
    }
    for (key, value) in &applied.env {
        if let Some(value) = value.as_str() {
            std::env::set_var(key, value);
        }
    }
    if let Some(route) = route_env.filter(|r| !r.is_empty()) {
        let route: Map<String, Value> = route
            .iter()
            .map(|(k, v)| (k.clone(), Value::String(v.clone())))
            .collect();
        // A self-authed route owns the credential slot: the account's own
        // secret vars leave this env, and the launch layers the route.
        if route.contains_key("ANTHROPIC_AUTH_TOKEN") || route.contains_key("ANTHROPIC_API_KEY") {
            for var in credentials::SECRET_ROUTE_VARS {
                if !route.contains_key(var) {
                    std::env::remove_var(var);
                }
            }
        }
        let cwd = params
            .get("cwd")
            .and_then(Value::as_str)
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| std::env::current_dir().unwrap_or_default());
        eprintln!("{}", routed_receipt(&applied, &route, &cwd));
    }
    None
}

/// The pane lane's account step: the pane shell is the mux server's child,
/// not this process's, so the overlay rides the env wrapper instead.
pub fn pane_overlay(params: &Value, provider: &str) -> Result<Option<Applied>, i32> {
    resolve(params, provider)
}

/// The launch-account pair a pane carries, set only when the account step
/// applied one: the row stamps it, and the wrapper hands it to the worker a
/// thread child would inherit it as. An unaccounted pane keeps no stamp.
pub fn pane_stamp() -> Vec<(String, String)> {
    let applied = std::env::var(crate::state::LAUNCH_ACCOUNT_ENV_KEY).is_ok_and(|v| !v.is_empty());
    if !applied {
        return Vec::new();
    }
    let (account, source) = crate::state::launch_provenance_from_env();
    [
        (crate::state::LAUNCH_ACCOUNT_ENV_KEY, account),
        (crate::state::LAUNCH_ACCOUNT_SOURCE_ENV_KEY, source),
    ]
    .into_iter()
    .filter_map(|(k, v)| v.map(|v| (k.to_string(), v)))
    .collect()
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
