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
use std::io::Read;
use std::path::{Path, PathBuf};

/// Tools a worker on a non-Anthropic endpoint must not be offered. Artifact
/// publishes through Anthropic's own backend, and z.ai refuses any tools
/// pattern holding a `\p{...}` escape (model_routing.py:FOREIGN_ENDPOINT_DENY_TOOLS).
const FOREIGN_ENDPOINT_DENY_TOOLS: &[&str] = &["Artifact"];

const ANTHROPIC_API_HOST: &str = "api.anthropic.com";

/// The lowercased host of `base_url`, or "" when there is none. A bare
/// authority like `api.anthropic.com/v1` carries no scheme, so give it one
/// before urlsplit-style parsing (model_routing.py:_base_url_host).
fn base_url_host(base_url: &str) -> String {
    let base = base_url.trim();
    if base.is_empty() {
        return String::new();
    }
    let with_scheme = if base.contains("//") {
        base.to_string()
    } else {
        format!("//{base}")
    };
    // authority is everything after "//" up to the first "/", "?", or "#".
    let authority = with_scheme
        .strip_prefix("//")
        .unwrap_or(&with_scheme)
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
    let base = crate::agents_config::state_dir(cwd)
        .ok_or_else(|| "no resolvable state root (set FNO_STATE_DIR or HOME)".to_string())?
        .join("route-settings");
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

/// The door: one JSON payload on stdin (`{"env", "sandbox"?, "alias"?}`), one
/// JSON receipt on stdout. The transport is the contract Python's forwarder
/// holds, so a refusal is a loud non-zero, never a silent fallback to a
/// Python write leg.
pub fn run_route_settings(args: &[String]) -> i32 {
    if !args.is_empty() {
        eprintln!("route-settings: the door takes no argv; the payload rides stdin");
        return 2;
    }
    let mut raw = String::new();
    if std::io::stdin().read_to_string(&mut raw).is_err() {
        eprintln!("route-settings: stdin is not readable text");
        return 2;
    }
    let payload: Value = match serde_json::from_str(&raw) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("route-settings: payload is not valid JSON: {e}");
            return 2;
        }
    };
    let Some(env) = payload.get("env").and_then(Value::as_object).cloned() else {
        eprintln!("route-settings: payload carries no env object");
        return 2;
    };
    for (k, v) in &env {
        if !v.is_string() {
            eprintln!("route-settings: env[{k}] is not a string");
            return 2;
        }
    }
    let ask = WriteAsk {
        env,
        sandbox: payload.get("sandbox").filter(|v| v.is_object()).cloned(),
        alias: payload
            .get("alias")
            .and_then(Value::as_str)
            .map(str::to_string),
    };
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    match write(&ask, &cwd) {
        Ok(receipt) => {
            println!(
                "{}",
                json!({
                    "path": receipt.path.to_string_lossy(),
                    "alias_path": receipt
                        .alias_path
                        .map(|p| p.to_string_lossy().into_owned()),
                })
            );
            0
        }
        Err(e) => {
            eprintln!("route-settings: {e}");
            2
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn identical_payloads_collapse_and_a_sandbox_splits() {
        let base = WriteAsk {
            env: env_of(&[("ANTHROPIC_BASE_URL", "https://api.z.ai/api/anthropic")]),
            sandbox: None,
            alias: None,
        };
        let root = tempfile::tempdir().unwrap();
        let receipt = write(&base, root.path()).unwrap();
        let again = write(&base, root.path()).unwrap();
        assert_eq!(receipt.path, again.path);
        let composed = WriteAsk {
            sandbox: Some(json!({"enabled": true})),
            ..base
        };
        let other = write(&composed, root.path()).unwrap();
        assert_ne!(other.path, receipt.path);
        let on_disk = std::fs::read_to_string(&other.path).unwrap();
        let payload: Value = serde_json::from_str(&on_disk).unwrap();
        assert_eq!(payload["sandbox"]["enabled"], json!(true));
    }

    #[test]
    fn the_alias_lands_at_lane_name_and_refuses_a_stranger() {
        let root = tempfile::tempdir().unwrap();
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
    }
}
