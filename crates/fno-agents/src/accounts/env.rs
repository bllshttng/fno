//! The launch env for one account: `dispatch.py` (`resolve_env_value`,
//! `dispatch_env` and its per-auth arms) and the `--account` overlay of
//! `account_env.resolve_account_overlay`. Both refuse a pin whose credential
//! root provably serves another account, from the one binding.

use crate::accounts::binding::{self, BindingRequest, SlotProbe, MISMATCH, PRINCIPAL_TTL_S};
use crate::accounts::records::{self, Record};
use crate::accounts::staging;
use crate::provider_route::{ClaudeRoute, RouteCtx};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// The reads a launch env needs from outside the process.
pub struct Sources<'a> {
    pub slot: &'a dyn SlotProbe,
    /// `security find-generic-password -w -s <item>`; `None` when it fails.
    pub secret: &'a dyn Fn(&str) -> Option<String>,
    pub env_get: &'a dyn Fn(&str) -> Option<String>,
}

fn system_secret(item: &str) -> Option<String> {
    let out = std::process::Command::new("security")
        .args(["find-generic-password", "-w", "-s", item])
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn system_env(key: &str) -> Option<String> {
    std::env::var(key).ok()
}

impl Sources<'static> {
    pub fn system() -> Self {
        Sources {
            slot: &binding::SystemProbe,
            secret: &system_secret,
            env_get: &system_env,
        }
    }
}

/// Which Python exception a refusal is: `ProviderNotFoundError`,
/// `ProviderUnavailableError`, or the loader's `ProviderConfigError`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnvErrorKind {
    NotFound,
    Unavailable,
    Config,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnvError {
    pub kind: EnvErrorKind,
    pub message: String,
}

fn unavailable(message: String) -> EnvError {
    EnvError {
        kind: EnvErrorKind::Unavailable,
        message,
    }
}

/// `dispatch.resolve_env_value`: `${ENV:X}`, `${KEYCHAIN:item}`,
/// `${FILE:path}` (first line); any other `${...}` is its literal inside and
/// a plain string passes as-is.
pub fn resolve_env_value(value: &str, src: &Sources<'_>) -> Result<String, EnvError> {
    let Some(inner) = value
        .strip_prefix("${")
        .and_then(|rest| rest.strip_suffix('}'))
    else {
        return Ok(value.to_string());
    };
    let Some((prefix, rest)) = inner.split_once(':') else {
        return Ok(inner.to_string());
    };
    match prefix {
        "ENV" => (src.env_get)(rest).ok_or_else(|| {
            unavailable(format!(
                "Environment variable not set: '{rest}' (from reference '{value}')"
            ))
        }),
        "KEYCHAIN" => (src.secret)(rest).ok_or_else(|| {
            unavailable(format!(
                "Keychain item not found: '{rest}' (from reference '{value}')"
            ))
        }),
        "FILE" => std::fs::read_to_string(rest)
            .ok()
            .and_then(|text| text.lines().next().map(|l| l.trim().to_string()))
            .ok_or_else(|| {
                unavailable(format!(
                    "Cannot read key file: {rest} (from reference '{value}')"
                ))
            }),
        _ => Ok(inner.to_string()),
    }
}

/// `dispatch._env_for_oauth`: claude reads its staged dir, every other
/// harness its staged HOME.
fn env_for_oauth(record: &Record, root: &Path) -> BTreeMap<String, String> {
    let account_dir = root.join(&record.id);
    let (key, dir) = if record.harness == "claude" {
        ("CLAUDE_CONFIG_DIR", account_dir.join(".claude"))
    } else {
        ("HOME", account_dir.join("home"))
    };
    BTreeMap::from([(key.to_string(), dir.display().to_string())])
}

/// `dispatch._env_for_api_key`: a route record takes the routed env for its
/// `provider/model` (resolved against `route_cwd`'s config); any other
/// record resolves every env reference, never returning a partial map.
fn env_for_api_key(
    record: &Record,
    route_cwd: &Path,
    src: &Sources<'_>,
) -> Result<BTreeMap<String, String>, EnvError> {
    if let Some(route) = record.route.as_deref().filter(|r| !r.is_empty()) {
        let refuse = |why: &str| {
            unavailable(format!(
                "Cannot resolve route '{route}' for account '{}': {why}",
                record.id
            ))
        };
        let (provider, model) = crate::spawn_route::parse_target(route)
            .ok_or_else(|| refuse("malformed provider/model"))?;
        let ctx = RouteCtx::Peer(provider.clone());
        return match crate::provider_route::claude_route(
            route_cwd,
            &provider,
            &model,
            &ctx,
            src.env_get,
        ) {
            ClaudeRoute::Routed { env, .. } => Ok(env),
            ClaudeRoute::Skipped { .. } => Err(refuse("route unavailable")),
        };
    }
    let mut resolved = BTreeMap::new();
    for (key, raw) in record.env.iter().flatten() {
        let value = resolve_env_value(raw, src).map_err(|e| {
            unavailable(format!(
                "Cannot resolve env value for key '{key}': {}",
                e.message
            ))
        })?;
        resolved.insert(key.clone(), value);
    }
    Ok(resolved)
}

fn now() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or_default()
}

/// The binding's mismatch receipt when `record`'s credential root provably
/// serves another account. An unprovable identity never refuses: an
/// unreachable profile endpoint must not ground the fleet.
fn identity_mismatch(
    record: &Record,
    root: &Path,
    records: &[Record],
    src: &Sources<'_>,
) -> Option<String> {
    let got = binding::resolve(
        &BindingRequest {
            record: Some(record),
            harness: &record.harness,
            bearer: None,
            root,
            records,
            now: now(),
            ttl: PRINCIPAL_TTL_S,
        },
        src.slot,
    );
    (got.status == MISMATCH).then(|| got.receipt())
}

fn load(repo_root: Option<&Path>, cwd: &Path) -> Result<records::Accounts, EnvError> {
    records::load(repo_root, cwd).map_err(|e| EnvError {
        kind: EnvErrorKind::Config,
        message: e.to_string(),
    })
}

/// `dispatch.dispatch_env`: the subprocess env for launching `provider_id`'s
/// harness. No account record is mutated.
pub fn launch_env(
    provider_id: &str,
    repo_root: Option<&Path>,
    root: Option<&Path>,
    cwd: &Path,
    src: &Sources<'_>,
) -> Result<BTreeMap<String, String>, EnvError> {
    let root = root.map_or_else(|| staging::providers_root(cwd), Path::to_path_buf);
    let accounts = load(repo_root, cwd)?;
    let Some(record) = accounts.by_id(provider_id) else {
        return Err(EnvError {
            kind: EnvErrorKind::NotFound,
            message: format!(
                "Provider '{provider_id}' is not configured. \
                 Check config.providers.records in settings.yaml."
            ),
        });
    };
    // A config-dir account lives in its own dir, whatever its auth; falling
    // through to the managed arm would launch on the ambient slot.
    if let Some(dir) = &record.config_dir {
        if let Some(receipt) = identity_mismatch(record, &root, &accounts.records, src) {
            return Err(unavailable(receipt));
        }
        return Ok(BTreeMap::from([(
            "CLAUDE_CONFIG_DIR".to_string(),
            dir.display().to_string(),
        )]));
    }
    match record.auth.as_str() {
        "oauth_dir" => {
            if !staging::verify_staged(record, &root) {
                return Err(unavailable(format!(
                    "Provider '{provider_id}' is not staged. \
                     Call stage(record) before dispatch_env()."
                )));
            }
            Ok(env_for_oauth(record, &root))
        }
        "managed" => match identity_mismatch(record, &root, &accounts.records, src) {
            Some(receipt) => Err(unavailable(receipt)),
            None => Ok(BTreeMap::new()),
        },
        _ => env_for_api_key(record, repo_root.unwrap_or(cwd), src),
    }
}

/// The resolved env overlay for one `--account` launch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Overlay {
    pub account_id: String,
    pub env: BTreeMap<String, String>,
    /// `own-dir` | `config-dir` | `managed-active` | `api-key`.
    pub lane: &'static str,
}

/// `account_env._login_present`: a credential specific to `dir`, on disk or
/// in its scoped Keychain item. `.claude.json` is metadata a logged-out dir
/// also holds, so it never counts.
fn login_present(dir: &Path, src: &Sources<'_>) -> bool {
    if dir.join(".credentials.json").exists() {
        return true;
    }
    binding::keychain_service(dir)
        .and_then(|service| src.slot.keychain(&service).ok().flatten())
        .is_some_and(|blob| !blob.trim().is_empty())
}

fn home_slot() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default()
        .join(".claude")
}

/// `account_env.resolve_account_overlay`: the env overlay for a claude
/// `--account <id>` launch, or the refusal receipt. It reads the slot stamp
/// and never writes it.
pub fn overlay(
    account_id: &str,
    repo_root: Option<&Path>,
    root: Option<&Path>,
    cwd: &Path,
    src: &Sources<'_>,
) -> Result<Overlay, String> {
    let root = root.map_or_else(|| staging::providers_root(cwd), Path::to_path_buf);
    let accounts = load(repo_root, cwd).map_err(|e| e.message)?;
    let Some(record) = accounts.by_id(account_id) else {
        let mut ids: Vec<&str> = accounts
            .records
            .iter()
            .filter(|r| r.harness == "claude")
            .map(|r| r.id.as_str())
            .collect();
        ids.sort_unstable();
        let listing = if ids.is_empty() {
            "(none registered)".to_string()
        } else {
            ids.join(", ")
        };
        return Err(format!(
            "account '{account_id}' is not registered. claude accounts: {listing}"
        ));
    };
    if record.harness != "claude" {
        return Err(format!(
            "account '{account_id}' is a {}/{} record; \
             --account is claude-only (codex has its own CODEX_HOME slot)",
            record.harness, record.auth
        ));
    }
    let made = |env: BTreeMap<String, String>, lane| Overlay {
        account_id: account_id.to_string(),
        env,
        lane,
    };
    let dir_env =
        |dir: &Path| BTreeMap::from([("CLAUDE_CONFIG_DIR".to_string(), dir.display().to_string())]);

    if let Some(cfg) = &record.config_dir {
        if !cfg.exists() {
            return Err(format!(
                "account '{account_id}' config_dir {} does not exist; \
                 register a live login there first",
                cfg.display()
            ));
        }
        if !login_present(cfg, src) {
            return Err(format!(
                "account '{account_id}' config_dir {0} holds no claude login \
                 (run: CLAUDE_CONFIG_DIR={0} claude /login)",
                cfg.display()
            ));
        }
        // A login being present says nothing about whose login it is.
        if let Some(receipt) = identity_mismatch(record, &root, &accounts.records, src) {
            return Err(receipt);
        }
        return Ok(made(dir_env(cfg.as_path()), "config-dir"));
    }

    match record.auth.as_str() {
        "oauth_dir" => {
            if !staging::verify_staged(record, &root) {
                return Err(format!(
                    "account '{account_id}' is not staged; run `fno config accounts \
                     register`/stage before spawning against it"
                ));
            }
            let env = env_for_oauth(record, &root);
            // verify_staged checks only the symlink and its target, so a
            // logged-out staged dir would spawn an auth-prompt zombie.
            if let Some(staged) = env.get("CLAUDE_CONFIG_DIR") {
                if !login_present(Path::new(staged), src) {
                    return Err(format!(
                        "account '{account_id}' staged dir {staged} holds no claude login \
                         (run: CLAUDE_CONFIG_DIR={staged} claude /login)"
                    ));
                }
            }
            Ok(made(env, "own-dir"))
        }
        "managed" => {
            let active = crate::accounts::slot::active_slot_id("claude", Some(root.as_path()), cwd)
                .ok()
                .flatten();
            // The requested id, never record.account_id: that is metadata, and
            // matching it would bill a non-active record to the active slot.
            if active.as_deref() == Some(account_id) {
                if let Some(receipt) = identity_mismatch(record, &root, &accounts.records, src) {
                    return Err(receipt);
                }
                // Pinned rather than empty, so a stale parent CLAUDE_CONFIG_DIR
                // cannot leak through and bill another account.
                return Ok(made(dir_env(home_slot().as_path()), "managed-active"));
            }
            Err(format!(
                "account '{account_id}' is managed and not the active ~/.claude \
                 account (active: {}). Per-spawn selection for a non-active account \
                 needs its own config dir: register it with a config_dir (e.g. \
                 --config-dir ~/.claude-{account_id}, a full second login) so the \
                 worker gets CLAUDE_CONFIG_DIR. The setup-token env lane bills the \
                 wrong account and is deliberately not used.\n  or make it active:  \
                 fno config accounts use {account_id}  (daemon-wide)",
                active.as_deref().unwrap_or("none")
            ))
        }
        _ => env_for_api_key(record, cwd, src)
            .map(|env| made(env, "api-key"))
            .map_err(|e| format!("account '{account_id}' env is unresolvable: {}", e.message)),
    }
}
