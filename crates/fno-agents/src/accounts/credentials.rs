//! The one composition rule every spawn seam shares: scrub the inherited
//! auth vars, layer the account overlay, layer the route last so it wins
//! endpoint, auth and model as one unit. The receipt facts are read off the
//! composed env, never off the flags. Also the state-root seal for a seam
//! that must forward a moved HOME.

use serde_json::{json, Map, Value};
use std::path::Path;

/// Inherited vars that would override an account's own login and bill the
/// wrong account. The model tiers ride along because endpoint, auth and model
/// are ONE provider route: scrubbing the endpoint but keeping a foreign tier
/// remap asks the pinned account for another vendor's model. The route stamp
/// names the route, so whatever clears the route clears it too. The tail is
/// `model_env_scrub::MODEL_ENV_KEYS` in order; a test pins the two together.
pub const SCRUB_AUTH_VARS: [&str; 10] = [
    "ANTHROPIC_API_KEY",
    "CLAUDE_CODE_OAUTH_TOKEN",
    "ANTHROPIC_BASE_URL",
    "ANTHROPIC_AUTH_TOKEN",
    "FNO_ROUTE_PROVIDER",
    "ANTHROPIC_MODEL",
    "ANTHROPIC_DEFAULT_OPUS_MODEL",
    "ANTHROPIC_DEFAULT_SONNET_MODEL",
    "ANTHROPIC_DEFAULT_HAIKU_MODEL",
    "ANTHROPIC_DEFAULT_FABLE_MODEL",
];

/// The subset of a route whose VALUE is a credential. A different question
/// from [`SCRUB_AUTH_VARS`] ("clear these so the route wins"): these values
/// must never land where a reader other than the child can see them, and a
/// self-authed route drops the ones it does not name.
pub const SECRET_ROUTE_VARS: [&str; 2] = ["ANTHROPIC_API_KEY", "ANTHROPIC_AUTH_TOKEN"];

/// The roots a seal pins around a moved HOME. Each is read env-first by the
/// runtime that owns it, so footnote's state stays where it was while the
/// harness credential follows the override.
const STATE_ROOT_PINS: [&str; 3] = ["FNO_AGENTS_HOME", "FNO_CLAIMS_ROOT", "FNO_STATE_DIR"];

/// What fno made live for one worker. Facts fno owns: the composed env is
/// decided before the process starts, but only claude knows which credential
/// it sent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Decision {
    /// The account id whose profile (CLAUDE_CONFIG_DIR) is set.
    pub profile: Option<String>,
    pub config_dir: Option<String>,
    /// `route:<vendor>` | `account:<id>` | `ambient`.
    pub auth: String,
    /// The base url, or `anthropic-default`.
    pub endpoint: String,
    pub model: Option<String>,
    /// A vendor, an account id, or `unknown`.
    pub bills: String,
}

impl Decision {
    pub fn to_value(&self) -> Value {
        json!({
            "profile": self.profile,
            "config_dir": self.config_dir,
            "auth": self.auth,
            "endpoint": self.endpoint,
            "model": self.model,
            "bills": self.bills,
        })
    }
}

fn truthy<'a>(env: &'a Map<String, Value>, key: &str) -> Option<&'a str> {
    env.get(key)
        .and_then(Value::as_str)
        .filter(|v| !v.is_empty())
}

/// The URL host, as Python's `urlparse(..).hostname` reads it: userinfo and
/// port dropped, lowercased. `None` without a scheme.
fn url_host(endpoint: &str) -> Option<String> {
    let (_, rest) = endpoint.split_once("://")?;
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    let host_port = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
    let host = match host_port.strip_prefix('[') {
        Some(v6) => v6.split(']').next().unwrap_or(""),
        None => host_port.split(':').next().unwrap_or(""),
    };
    (!host.is_empty()).then(|| host.to_ascii_lowercase())
}

/// The provider that owns `endpoint`, or its URL host. Derived from the
/// composed env, so the vendor label travels with the endpoint actually
/// applied, never with the spelling a caller typed.
pub fn vendor_for_endpoint(cwd: &Path, endpoint: &str) -> Option<String> {
    crate::provider_route::providers(cwd)
        .into_iter()
        .find(|(_, record)| record.base_url.as_deref() == Some(endpoint))
        .map(|(name, _)| name)
        .or_else(|| url_host(endpoint))
}

fn decision(
    composed: &Map<String, Value>,
    account_id: Option<&str>,
    route_self_authed: bool,
    vendor: &dyn Fn(&str) -> Option<String>,
) -> Decision {
    let endpoint = truthy(composed, "ANTHROPIC_BASE_URL").unwrap_or("anthropic-default");
    let vendor = (endpoint != "anthropic-default")
        .then(|| vendor(endpoint))
        .flatten();
    let model = match composed.get("ANTHROPIC_MODEL").and_then(Value::as_str) {
        Some(model) => Some(model.to_string()),
        None => crate::model_env_scrub::MODEL_ENV_KEYS
            .iter()
            .find_map(|key| truthy(composed, key))
            .map(str::to_string),
    };
    let config_dir = composed
        .get("CLAUDE_CONFIG_DIR")
        .and_then(Value::as_str)
        .map(str::to_string);
    let (profile, auth, bills) = if route_self_authed {
        let named = vendor.unwrap_or_else(|| "unknown".to_string());
        (
            account_id.map(str::to_string),
            format!("route:{named}"),
            named,
        )
    } else if let Some(id) = account_id {
        // No route credential: the account's own login authenticates, and an
        // api-key record's endpoint names its vendor.
        let payer = vendor.unwrap_or_else(|| id.to_string());
        (Some(id.to_string()), format!("account:{id}"), payer)
    } else {
        (None, "ambient".to_string(), "unknown".to_string())
    };
    Decision {
        profile,
        config_dir,
        auth,
        endpoint: endpoint.to_string(),
        model,
        bills,
    }
}

/// Apply the scrub, account and route precedence once and report the
/// outcome. The account's login is dormant for model traffic under a route
/// and still supplies the profile.
pub fn compose(
    account_env: Option<&Map<String, Value>>,
    route_env: Option<&Map<String, Value>>,
    base: &Map<String, Value>,
    account_id: Option<&str>,
    vendor: &dyn Fn(&str) -> Option<String>,
) -> (Map<String, Value>, Decision) {
    let mut composed = base.clone();
    for var in SCRUB_AUTH_VARS {
        composed.shift_remove(var);
    }
    if let Some(account) = account_env {
        composed.extend(account.clone());
    }
    let route = route_env.filter(|r| !r.is_empty());
    let self_authed = route.is_some_and(|r| {
        truthy(r, "ANTHROPIC_AUTH_TOKEN").is_some() || truthy(r, "ANTHROPIC_API_KEY").is_some()
    });
    if let Some(route) = route {
        composed.extend(route.clone());
        // A self-authed route owns the credential slot outright, or an
        // api-key account's vendor key rides against the route's endpoint.
        if self_authed {
            for var in SECRET_ROUTE_VARS {
                if !route.contains_key(var) {
                    composed.shift_remove(var);
                }
            }
        }
    }
    let made = decision(&composed, account_id, self_authed, vendor);
    (composed, made)
}

/// Pin the state roots around a moved HOME, so a footnote child's registry
/// row, claim, locks and graph stay findable. A no-op unless `env` moves
/// HOME; a root `env` already names wins. `roots` is the process's own
/// `(key, value)` set; [`seal`] reads it from the environment.
pub fn seal_with(
    env: &Map<String, Value>,
    home: Option<&str>,
    roots: &[(&str, String)],
) -> Map<String, Value> {
    let mut out = env.clone();
    let Some(moved) = truthy(env, "HOME") else {
        return out;
    };
    if Some(moved) == home {
        return out;
    }
    for key in STATE_ROOT_PINS {
        if out.contains_key(key) {
            continue;
        }
        if let Some((_, value)) = roots.iter().find(|(k, _)| *k == key) {
            out.insert(key.to_string(), Value::from(value.clone()));
        }
    }
    out
}

/// [`seal_with`] over the roots this process resolves.
pub fn seal(env: &Map<String, Value>, cwd: &Path) -> Map<String, Value> {
    let var = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
    let mut roots = Vec::new();
    if let Some(agents) = crate::paths::AgentsHome::from_env_opt() {
        roots.push(("FNO_AGENTS_HOME", agents.root().display().to_string()));
    }
    if let Some(state) = crate::agents_config::state_dir(cwd) {
        roots.push(("FNO_STATE_DIR", state.display().to_string()));
    }
    if let Some(claims) = var("FNO_CLAIMS_ROOT").or_else(|| var("HOME")) {
        roots.push(("FNO_CLAIMS_ROOT", claims));
    }
    seal_with(env, var("HOME").as_deref(), &roots)
}

/// `spawn-overlay` kind `credentials`: the composed env and its decision
/// for one seam, plus both var lists so a Python seam reads them from here.
pub fn compose_kind(payload: &Value) -> Result<Value, String> {
    let cwd = payload
        .get("cwd")
        .and_then(Value::as_str)
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_default());
    let map = |key: &str| payload.get(key).and_then(Value::as_object);
    let empty = Map::new();
    let vendor = |endpoint: &str| vendor_for_endpoint(&cwd, endpoint);
    let (env, made) = compose(
        map("account_env").filter(|m| !m.is_empty()),
        map("route_env"),
        map("base").unwrap_or(&empty),
        payload.get("account_id").and_then(Value::as_str),
        &vendor,
    );
    Ok(json!({
        "env": env,
        "decision": made.to_value(),
        "scrub": SCRUB_AUTH_VARS,
        "secret": SECRET_ROUTE_VARS,
    }))
}

/// `spawn-overlay` kind `state-root-seal`: `env` with the roots pinned.
pub fn seal_kind(payload: &Value) -> Result<Value, String> {
    let cwd = std::env::current_dir().unwrap_or_default();
    let empty = Map::new();
    let env = payload
        .get("env")
        .and_then(Value::as_object)
        .unwrap_or(&empty);
    Ok(json!({ "env": seal(env, &cwd) }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_scrub_tail_is_the_model_key_list() {
        assert_eq!(
            &SCRUB_AUTH_VARS[5..],
            &crate::model_env_scrub::MODEL_ENV_KEYS[..]
        );
    }
}
