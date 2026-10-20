//! The claude `--settings` route-settings writer, ported from Python
//! `model_routing._write_settings_env_file` (change 6). ONE writer owns the
//! payload shape (the env map, an optional sandbox block, the foreign-endpoint
//! tool denies), the sha256[:16] content address, the 0600 atomic publish and
//! the stable `lane-<row>.json` alias a routing row's native line prints. The
//! door verb reads the composed env on STDIN - it carries
//! `ANTHROPIC_AUTH_TOKEN`, which must never ride argv - and answers
//! `{"path", "alias_path"}` on stdout.

use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

/// Tools a worker on a non-Anthropic endpoint must not be offered. Artifact
/// publishes through Anthropic's own backend, and z.ai refuses any tools
/// pattern holding a `\p{...}` escape (model_routing.py:FOREIGN_ENDPOINT_DENY_TOOLS).
const FOREIGN_ENDPOINT_DENY_TOOLS: &[&str] = &["Artifact"];

const ANTHROPIC_API_HOST: &str = "api.anthropic.com";

/// The lowercased host of `base_url`, or "" when there is none. The authority
/// follows the first `//` (a scheme prefix or none) and runs to the first
/// `/`, `?` or `#` (model_routing.py:_base_url_host).
fn base_url_host(base_url: &str) -> String {
    let base = base_url.trim();
    if base.is_empty() {
        return String::new();
    }
    let authority = match base.find("//") {
        Some(index) => &base[index + 2..],
        None => base,
    }
    .split(['/', '?', '#'])
    .next()
    .unwrap_or("");
    // Drop userinfo; keep the host, lowercased.
    let host = authority.rsplit('@').next().unwrap_or("");
    let host = host.split(':').next().unwrap_or("");
    host.to_ascii_lowercase()
}

fn foreign_endpoint_tool_denies(env: &Map<String, Value>) -> Vec<&'static str> {
    let base_url = env
        .get("ANTHROPIC_BASE_URL")
        .and_then(Value::as_str)
        .unwrap_or("");
    let host = base_url_host(base_url);
    if host.is_empty() || host == ANTHROPIC_API_HOST {
        return Vec::new();
    }
    FOREIGN_ENDPOINT_DENY_TOOLS.to_vec()
}

/// One write ask: the composed env, the optional sandbox block, and the
/// optional stable lane alias (`lane-<alias>.json`).
pub struct WriteAsk {
    pub env: Map<String, Value>,
    pub sandbox: Option<Value>,
    pub alias: Option<String>,
}

pub struct WriteReceipt {
    pub path: PathBuf,
    pub alias_path: Option<PathBuf>,
}

/// The payload bytes: `{"env": ...}` plus the optional sandbox block and the
/// foreign-endpoint deny, keys inserted in sorted order so the content address
/// is deterministic (Python wrote `json.dumps(payload, sort_keys=True)`).
fn payload_bytes(env: &Map<String, Value>, sandbox: Option<&Value>) -> Vec<u8> {
    let mut sorted_env = Map::new();
    let mut keys: Vec<&String> = env.keys().collect();
    keys.sort();
    for k in keys {
        sorted_env.insert(k.clone(), env[k].clone());
    }
    let mut root = Map::new();
    root.insert("env".to_string(), Value::Object(sorted_env));
    let deny = foreign_endpoint_tool_denies(env);
    if !deny.is_empty() {
        root.insert("permissions".to_string(), json!({"deny": deny}));
    }
    if let Some(sb) = sandbox {
        root.insert("sandbox".to_string(), sb.clone());
    }
    serde_json::to_vec(&Value::Object(root)).expect("payload serializes")
}

fn digest_of(payload: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(payload);
    let full = hasher.finalize();
    full.iter().take(8).map(|b| format!("{b:02x}")).collect()
}

/// The scrub floor's var list: one list with Python's
/// `account_env.SCRUB_AUTH_VARS` (the route stamp included, the model tiers
/// folded in), which a Python test pins to this copy.
fn scrub_auth_vars() -> Vec<&'static str> {
    let mut vars: Vec<&'static str> = vec![
        "ANTHROPIC_API_KEY",
        "CLAUDE_CODE_OAUTH_TOKEN",
        "ANTHROPIC_BASE_URL",
        "ANTHROPIC_AUTH_TOKEN",
        "FNO_ROUTE_PROVIDER",
    ];
    vars.extend(crate::model_env_scrub::MODEL_ENV_KEYS);
    vars
}

/// The subset of a route whose VALUE is a credential; a self-authed route
/// drops the account secrets it does not name, else an api-key account's
/// vendor key rides the file against the route's endpoint.
const SECRET_ROUTE_VARS: [&str; 2] = ["ANTHROPIC_API_KEY", "ANTHROPIC_AUTH_TOKEN"];

const ENV_FALSY_WORDS: [&str; 5] = ["", "0", "false", "no", "off"];

/// The inherited model claims worth flooring, read from the spawn's own
/// environment: the five model vars carrying a value, but ONLY while no
/// vendor lane is on and the endpoint is Anthropic's own (or unset). A
/// foreign base serves those model ids, so flooring would break a working
/// lane; Bedrock and Vertex serve Anthropic models under other id shapes.
fn unrouted_model_keys(spawn_env: &Map<String, Value>) -> Vec<&'static str> {
    for flag in ["CLAUDE_CODE_USE_BEDROCK", "CLAUDE_CODE_USE_VERTEX"] {
        let value = spawn_env
            .get(flag)
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim()
            .to_ascii_lowercase();
        if !ENV_FALSY_WORDS.contains(&value.as_str()) {
            return Vec::new();
        }
    }
    let base = spawn_env
        .get("ANTHROPIC_BASE_URL")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    if !base.is_empty() {
        let host = base_url_host(&base);
        if host != "anthropic.com" && !host.ends_with(".anthropic.com") {
            return Vec::new();
        }
    }
    crate::model_env_scrub::MODEL_ENV_KEYS
        .iter()
        .filter(|key| {
            spawn_env
                .get(**key)
                .and_then(Value::as_str)
                .map(|v| !v.trim().is_empty())
                .unwrap_or(false)
        })
        .copied()
        .collect()
}

/// The composed settings env from the RAW overlays - the port of Python's
/// `route_settings_path_for` compose, so the Python leg is a transport to
/// this door and a fourth seam cannot hand-roll a disagreeing order. The
/// scrub floor first, the account overlay minus its config dir (the config
/// dir selects the config the file is read FROM, so it rides the process
/// env), the route written last so it wins every credential var as one unit,
/// a self-authed route's drop of the account secrets it does not name, and
/// the unrouted-model floor.
pub fn compose_env(
    route_env: &Map<String, Value>,
    account_env: &Map<String, Value>,
    spawn_env: &Map<String, Value>,
) -> Map<String, Value> {
    let mut merged: Map<String, Value> = Map::new();
    for var in scrub_auth_vars() {
        merged.insert(var.to_string(), Value::String(String::new()));
    }
    for (key, value) in account_env {
        if key.as_str() == "CLAUDE_CONFIG_DIR" {
            continue;
        }
        merged.insert(key.clone(), value.clone());
    }
    for (key, value) in route_env {
        merged.insert(key.clone(), value.clone());
    }
    let self_authed = route_env
        .get("ANTHROPIC_AUTH_TOKEN")
        .or_else(|| route_env.get("ANTHROPIC_API_KEY"))
        .map(|v| !v.as_str().unwrap_or_default().trim().is_empty())
        .unwrap_or(false);
    if self_authed {
        for var in SECRET_ROUTE_VARS {
            if !route_env.contains_key(var) {
                merged.remove(var);
            }
        }
    }
    for key in unrouted_model_keys(spawn_env) {
        merged
            .entry(key.to_string())
            .or_insert_with(|| Value::String(String::new()));
    }
    merged
}

/// A lane alias is a CLI-handle-shaped name: one write per config row, and a
/// stranger name must never turn into a path outside route-settings.
fn valid_alias(alias: &str) -> bool {
    !alias.is_empty()
        && alias != "."
        && alias != ".."
        && !alias.starts_with('.')
        && alias
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

fn write_atomic_0600(path: &Path, bytes: &[u8]) -> Result<(), String> {
    if path.exists() {
        return Ok(());
    }
    let base = path
        .parent()
        .ok_or_else(|| format!("{}: no parent", path.display()))?;
    let tmp = base.join(format!(
        ".{}.{}.tmp",
        path.file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default(),
        std::process::id()
    ));
    std::fs::write(&tmp, bytes).map_err(|e| format!("{}: {e}", tmp.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600));
    }
    std::fs::rename(&tmp, path).map_err(|e| format!("{}: {e}", path.display()))
}

/// Write the content-addressed settings file (and the lane alias when asked)
/// under `<state>/route-settings`. Identical payloads collapse to one file;
/// racing writers publish identical bytes, so a last-writer-wins rename stays
/// correct (model_routing.py:_write_settings_env_file).
pub fn write(ask: &WriteAsk, cwd: &Path) -> Result<WriteReceipt, String> {
    let state = crate::agents_config::state_dir(cwd)
        .ok_or_else(|| "no resolvable state root (set FNO_STATE_DIR or HOME)".to_string())?;
    // The row's destination, always: a drain row never relies on the legacy
    // answer, so a fresh overlay never lands beside the registry paths that
    // outlive it.
    let base = crate::state_layout::place_dir(&state, "route-settings")
        .ok_or_else(|| "route-settings is not a state-root dir".to_string())?;
    std::fs::create_dir_all(&base).map_err(|e| format!("{}: {e}", base.display()))?;
    let payload = payload_bytes(&ask.env, ask.sandbox.as_ref());
    let digest = digest_of(&payload);
    let path = base.join(format!("{digest}.json"));
    write_atomic_0600(&path, &payload)?;
    let alias_path = match &ask.alias {
        Some(name) => {
            if !valid_alias(name) {
                return Err(format!("lane alias {name:?} is not a CLI handle"));
            }
            let alias_path = base.join(format!("lane-{name}.json"));
            write_atomic_0600(&alias_path, &payload)?;
            Some(alias_path)
        }
        None => None,
    };
    Ok(WriteReceipt { path, alias_path })
}

/// Compose from the RAW overlays and write, the one helper behind both
/// doors (the native exec and the print kind). `Ok(None)` when the ask
/// names neither overlay nor sandbox: a plain lane launch carries no
/// settings file, but a write policy alone still owes the scrub floor.
pub fn write_composed(
    route_env: &Map<String, Value>,
    account_env: &Map<String, Value>,
    sandbox: Option<Value>,
    alias: Option<&str>,
    cwd: &Path,
) -> Result<Option<String>, String> {
    if route_env.is_empty() && account_env.is_empty() && sandbox.is_none() {
        return Ok(None);
    }
    let spawn_env: Map<String, Value> = std::env::vars()
        .map(|(k, v)| (k, Value::String(v)))
        .collect();
    let env = compose_env(route_env, account_env, &spawn_env);
    let receipt = write(
        &WriteAsk {
            env,
            sandbox,
            alias: alias.map(str::to_string),
        },
        cwd,
    )
    .map_err(|e| format!("lane settings: {e}"))?;
    Ok(Some(receipt.path.to_string_lossy().into_owned()))
}

/// The write kind behind the spawn-overlay action (law d-fe66560a allows no
/// new action): Python's forwarders send `{"kind": "write-settings", ...}`.
/// Two ask shapes: `{"env", ...}` is a pre-composed env (the revive
/// readers), and `{"route_env", "account_env", ...}` names the RAW overlays
/// for the door's own compose. The transport is the contract Python's
/// forwarder holds, so a refusal is a loud error, never a silent fallback to
/// a Python write leg.
pub fn write_settings_kind(payload: &Value) -> Result<Value, String> {
    let string_object = |field: &str| -> Result<Option<Map<String, Value>>, String> {
        let obj = match payload.get(field).and_then(Value::as_object) {
            Some(obj) => obj,
            None => return Ok(None),
        };
        if obj.values().all(Value::is_string) {
            Ok(Some(obj.clone()))
        } else {
            Err(format!(
                "route-settings: {field} carries a non-string value"
            ))
        }
    };
    let env = if payload.get("route_env").is_some() || payload.get("account_env").is_some() {
        let route_env = string_object("route_env")?;
        let account_env = string_object("account_env")?;
        let (Some(route_env), Some(account_env)) = (route_env, account_env) else {
            return Err(
                "route-settings: the overlay compose needs route_env and account_env objects"
                    .into(),
            );
        };
        let spawn_env: Map<String, Value> = std::env::vars()
            .map(|(k, v)| (k, Value::String(v)))
            .collect();
        compose_env(&route_env, &account_env, &spawn_env)
    } else {
        match string_object("env")? {
            Some(env) => env,
            None => {
                return Err("route-settings: payload carries no env or overlay object".into());
            }
        }
    };
    let ask = WriteAsk {
        env,
        sandbox: payload.get("sandbox").filter(|v| v.is_object()).cloned(),
        alias: payload
            .get("alias")
            .and_then(Value::as_str)
            .map(str::to_string),
    };
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let receipt = write(&ask, &cwd)?;
    Ok(json!({
        "path": receipt.path.to_string_lossy(),
        "alias_path": receipt
            .alias_path
            .map(|p| p.to_string_lossy().into_owned()),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The writer resolves its state root from the env (FNO_STATE_DIR, then
    /// config, then $HOME/.fno); a test that writes pins it under the claims
    /// env lock, so the sandbox never touches the real state root.
    fn state_pin(
        root: &Path,
    ) -> (
        std::sync::MutexGuard<'static, ()>,
        Option<std::ffi::OsString>,
    ) {
        let guard = crate::claims::test_env_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let saved = std::env::var_os("FNO_STATE_DIR");
        std::env::set_var("FNO_STATE_DIR", root);
        (guard, saved)
    }

    fn state_restore(saved: Option<std::ffi::OsString>) {
        match saved {
            Some(v) => std::env::set_var("FNO_STATE_DIR", v),
            None => std::env::remove_var("FNO_STATE_DIR"),
        }
    }

    fn env_of(pairs: &[(&str, &str)]) -> Map<String, Value> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), json!(v)))
            .collect()
    }

    /// The whole-host table the Python side pinned as
    /// test_deny_matches_the_whole_host: a substring test got both ends wrong.
    #[test]
    fn the_deny_decision_parses_the_whole_host() {
        let rows: Vec<(&str, bool)> = vec![
            ("https://api.anthropic.com", false),
            ("https://api.anthropic.com/v1", false),
            ("HTTPS://API.ANTHROPIC.COM", false),
            ("api.anthropic.com", false),
            ("https://api.anthropic.com:443", false),
            // A host that merely STARTS with Anthropic's is somebody else's.
            ("https://api.anthropic.com.proxy.example", true),
            ("https://evil.example/api.anthropic.com", true),
            ("https://api.z.ai/api/anthropic", true),
            ("", false),
        ];
        for (base_url, denied) in rows {
            let got = foreign_endpoint_tool_denies(&env_of(&[("ANTHROPIC_BASE_URL", base_url)]));
            assert_eq!(got.is_empty(), !denied, "{base_url}");
        }
    }

    #[test]
    fn the_alias_lands_at_lane_name_and_refuses_a_stranger() {
        let root = tempfile::tempdir().unwrap();
        let (guard, saved) = state_pin(root.path());
        let ask = WriteAsk {
            env: env_of(&[("ANTHROPIC_BASE_URL", "https://api.z.ai/api/anthropic")]),
            sandbox: None,
            alias: Some("zai-flash".to_string()),
        };
        let receipt = write(&ask, root.path()).unwrap();
        let alias = receipt.alias_path.unwrap();
        assert!(alias.ends_with("lane-zai-flash.json"));
        assert_eq!(
            std::fs::read(&alias).unwrap(),
            std::fs::read(&receipt.path).unwrap()
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&alias).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        let bad = WriteAsk {
            alias: Some("../escape".to_string()),
            ..ask
        };
        assert!(write(&bad, root.path()).is_err());
        state_restore(saved);
        drop(guard);
    }

    /// The compose from raw overlays: the scrub floor, the account overlay
    /// minus its config dir, the route winning the credential slot, and the
    /// self-authed drop of the account secret the route does not name.
    #[test]
    fn the_compose_floors_overlays_and_drops_the_foreign_secret() {
        let route_env = env_of(&[
            ("ANTHROPIC_BASE_URL", "https://api.z.ai/api/anthropic"),
            ("ANTHROPIC_AUTH_TOKEN", "zk-route"),
            ("ANTHROPIC_MODEL", "glm-5.2"),
        ]);
        let account_env = env_of(&[
            ("ANTHROPIC_API_KEY", "acct-key"),
            ("CLAUDE_CONFIG_DIR", "/accounts/ccm"),
            ("ANTHROPIC_LOG_LEVEL", "debug"),
        ]);
        let spawn_env = env_of(&[("ANTHROPIC_MODEL", "claude-opus-5-5")]);
        let merged = compose_env(&route_env, &account_env, &spawn_env);
        let as_str = |k: &str| merged.get(k).and_then(Value::as_str).expect(k).to_string();
        // The route wins the credential slot; the account secret a
        // self-authed route does not name is dropped, and the config dir
        // never rides the file.
        assert_eq!(
            as_str("ANTHROPIC_BASE_URL"),
            "https://api.z.ai/api/anthropic"
        );
        assert_eq!(as_str("ANTHROPIC_AUTH_TOKEN"), "zk-route");
        assert!(!merged.contains_key("ANTHROPIC_API_KEY"));
        assert!(!merged.contains_key("CLAUDE_CONFIG_DIR"));
        assert_eq!(as_str("ANTHROPIC_LOG_LEVEL"), "debug");
        assert_eq!(as_str("ANTHROPIC_MODEL"), "glm-5.2");
        // The floor covers every scrub var; a key the overlays do not name
        // stays at the floor's empty string.
        assert_eq!(as_str("CLAUDE_CODE_OAUTH_TOKEN"), "");
        assert_eq!(as_str("FNO_ROUTE_PROVIDER"), "");
        assert_eq!(as_str("ANTHROPIC_DEFAULT_FABLE_MODEL"), "");
    }
}
