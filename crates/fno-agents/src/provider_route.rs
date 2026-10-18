//! The one owner of the provider record and the claude-lane route env.
//!
//! Ports the route half of `cli/src/fno/agents/model_routing.py` (the
//! `_DEFAULT_PROVIDERS` preset, `_resolve_provider`'s merge, `_route_for_target`'s
//! env build and notices, `key_source`): the built-in vendor preset lives in
//! `src/model_reach.toml` (`[vendor.*]` with `builtin = true`), a config
//! `model_routing.providers.<name>` record overlays it per field, and
//! `claude_route` answers the `ANTHROPIC_*` env a routed claude spawn gets.
//! The role-to-target lookup and the business-role errors stay in Python;
//! Python reaches this owner through the spawn-overlay `claude-route` and
//! `providers` kinds. Characterized by the goldens under
//! `tests/golden/claude_route/` plus `tests/claude_route_parity.rs`.
//!
//! One merge rule across both readers: a config field set to the empty
//! string counts as unset, so the built-in preset fills it (the rule
//! `footnote_harness/endpoint.rs` already applied). The parity goldens pin
//! every decision the old Python leg made on non-pathological input.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::OnceLock;

use crate::agents_config::config_table_merged;
use crate::model_env_scrub::MODEL_ENV_KEYS;

/// How a provider speaks. A record without `protocol` reads `Anthropic`,
/// the same default Python applies.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Protocol {
    Anthropic,
    OpenAi,
    /// A lowered protocol string that is neither builtin name.
    Other(String),
}

impl Protocol {
    /// The lowered protocol text, as the notices spell it.
    pub fn as_str(&self) -> &str {
        match self {
            Protocol::Anthropic => "anthropic",
            Protocol::OpenAi => "openai",
            Protocol::Other(s) => s,
        }
    }

    fn parse(raw: Option<&str>) -> Protocol {
        match raw.map(str::trim).filter(|v| !v.is_empty()) {
            None => Protocol::Anthropic,
            Some(v) => match v.to_ascii_lowercase().as_str() {
                "anthropic" => Protocol::Anthropic,
                "openai" => Protocol::OpenAi,
                lowered => Protocol::Other(lowered.to_string()),
            },
        }
    }
}

/// A Claude tier Claude Code resolves through `ANTHROPIC_DEFAULT_<TIER>_MODEL`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum TierAlias {
    Fable,
    Haiku,
    Opus,
    Sonnet,
}

impl TierAlias {
    /// The alias text, as config and notices spell it.
    pub fn as_str(&self) -> &'static str {
        match self {
            TierAlias::Fable => "fable",
            TierAlias::Haiku => "haiku",
            TierAlias::Opus => "opus",
            TierAlias::Sonnet => "sonnet",
        }
    }

    fn parse(raw: &str) -> Option<TierAlias> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "fable" => Some(TierAlias::Fable),
            "haiku" => Some(TierAlias::Haiku),
            "opus" => Some(TierAlias::Opus),
            "sonnet" => Some(TierAlias::Sonnet),
            _ => None,
        }
    }

    fn env_key(&self) -> String {
        format!(
            "ANTHROPIC_DEFAULT_{}_MODEL",
            self.as_str().to_ascii_uppercase()
        )
    }
}

/// WHERE a provider's key resolved, never the key value itself: the
/// `fno config route ls` key column.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeySource {
    /// The env-var name or file path that yielded a key, `None` when missing.
    pub satisfying: Option<String>,
    /// Every source consulted, in precedence order.
    pub checked: Vec<String>,
}

/// The caller's context for the fail-safe notices: a routed role falls back
/// to the primary model, a peer is skipped entirely.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RouteCtx {
    Role(String),
    Peer(String),
}

impl RouteCtx {
    /// The notice fragment Python spelled `ctx`: `role 'tidy'` /
    /// `peer 'zai'`.
    pub fn label(&self) -> String {
        match self {
            RouteCtx::Role(name) => format!("role '{name}'"),
            RouteCtx::Peer(name) => format!("peer '{name}'"),
        }
    }

    fn is_peer(&self) -> bool {
        matches!(self, RouteCtx::Peer(_))
    }
}

/// The answer `claude_route` gives: a routed env, or a skip with the
/// fail-safe notices. One enum, so "a route plus a refusal" cannot be
/// written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClaudeRoute {
    Routed {
        env: BTreeMap<String, String>,
        provider: String,
        notices: Vec<String>,
    },
    Skipped {
        notices: Vec<String>,
    },
}

/// The merged provider record: the built-in vendor preset under a config
/// `model_routing.providers.<name>` overlay, config winning per field. The
/// record never carries a key value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderRecord {
    pub protocol: Protocol,
    pub base_url: Option<String>,
    pub api_key_env: Option<String>,
    pub api_key_file: Option<String>,
    pub tier_models: BTreeMap<TierAlias, String>,
}

/// The string fields a record carries before the tier fold.
#[derive(Clone, Default)]
struct RawRecord {
    protocol: Option<String>,
    base_url: Option<String>,
    api_key_env: Option<String>,
    api_key_file: Option<String>,
    haiku_model: Option<String>,
    tier_models: Option<toml::Table>,
}

/// A present, non-empty string field: the merge treats anything else as
/// unset, so the built-in preset fills it.
fn field(table: &toml::Table, name: &str) -> Option<String> {
    table
        .get(name)
        .and_then(toml::Value::as_str)
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(str::to_string)
}

impl From<&toml::Table> for RawRecord {
    fn from(table: &toml::Table) -> RawRecord {
        RawRecord {
            protocol: field(table, "protocol"),
            base_url: field(table, "base_url"),
            api_key_env: field(table, "api_key_env"),
            api_key_file: field(table, "api_key_file"),
            haiku_model: field(table, "haiku_model"),
            tier_models: table
                .get("tier_models")
                .and_then(toml::Value::as_table)
                .cloned(),
        }
    }
}

impl RawRecord {
    /// Per-field overlay, config winning: a present, non-empty config field
    /// replaces the preset value, every other field keeps the preset.
    fn overlay(self, config: RawRecord) -> RawRecord {
        RawRecord {
            protocol: config.protocol.or(self.protocol),
            base_url: config.base_url.or(self.base_url),
            api_key_env: config.api_key_env.or(self.api_key_env),
            api_key_file: config.api_key_file.or(self.api_key_file),
            haiku_model: config.haiku_model.or(self.haiku_model),
            tier_models: config.tier_models.or(self.tier_models),
        }
    }

    /// `haiku_model` folds in as the haiku tier and an explicit
    /// `tier_models` entry wins over it, so the two set together resolve
    /// deterministically, not by map order.
    fn fold_tiers(self) -> ProviderRecord {
        let mut tiers = BTreeMap::new();
        if let Some(haiku) = self.haiku_model {
            tiers.insert(TierAlias::Haiku, haiku);
        }
        if let Some(declared) = self.tier_models {
            for (alias, tier_model) in &declared {
                let Some(alias) = TierAlias::parse(alias) else {
                    continue;
                };
                let Some(value) = tier_model.as_str().map(str::trim).filter(|v| !v.is_empty())
                else {
                    continue;
                };
                tiers.insert(alias, value.to_string());
            }
        }
        ProviderRecord {
            protocol: Protocol::parse(self.protocol.as_deref()),
            base_url: self.base_url,
            api_key_env: self.api_key_env,
            api_key_file: self.api_key_file,
            tier_models: tiers,
        }
    }
}

/// The built-in vendor presets from the canonical reach TOML: every
/// `[vendor.*]` with `builtin = true`, its `anthropic` table read as
/// `base_url` / `api_key_env` / `api_key_file`.
fn builtin_providers() -> &'static BTreeMap<String, RawRecord> {
    static BUILTIN: OnceLock<BTreeMap<String, RawRecord>> = OnceLock::new();
    BUILTIN.get_or_init(|| {
        let parsed: toml::Table =
            toml::from_str(include_str!("model_reach.toml")).expect("model_reach.toml parses");
        let mut out = BTreeMap::new();
        let Some(vendor) = parsed.get("vendor").and_then(toml::Value::as_table) else {
            return out;
        };
        for (name, table) in vendor {
            if !table
                .get("builtin")
                .and_then(toml::Value::as_bool)
                .unwrap_or(false)
            {
                continue;
            }
            let Some(endpoint) = table.get("anthropic").and_then(toml::Value::as_table) else {
                continue;
            };
            let mut raw = RawRecord::from(endpoint);
            raw.protocol = Some("anthropic".to_string());
            raw.haiku_model = table
                .get("haiku_model")
                .and_then(toml::Value::as_str)
                .filter(|v| !v.is_empty())
                .map(str::to_string);
            out.insert(name.clone(), raw);
        }
        out
    })
}

/// The merged provider records: the built-in preset under every config
/// `model_routing.providers` record, config winning per field.
pub fn providers(cwd: &Path) -> BTreeMap<String, ProviderRecord> {
    let mut out: BTreeMap<String, RawRecord> = BTreeMap::new();
    for (name, raw) in builtin_providers() {
        out.insert(name.clone(), raw.clone());
    }
    let config_table = config_table_merged(cwd, &["model_routing", "providers"]);
    if let Some(table) = config_table {
        for (name, value) in &table {
            let Some(record) = value.as_table() else {
                continue;
            };
            let base = out.remove(name).unwrap_or_default();
            out.insert(name.clone(), base.overlay(RawRecord::from(record)));
        }
    }
    out.into_iter()
        .map(|(name, raw)| (name, raw.fold_tiers()))
        .collect()
}

/// The one named record, merged the same way [`providers`] merges.
pub fn provider_record(cwd: &Path, name: &str) -> Option<ProviderRecord> {
    providers(cwd).remove(name)
}

/// The one named record under a caller-pinned block: the built-in preset
/// under the block's `model_routing.providers.<name>` overlay, the same
/// merge [`providers`] runs against the config stack. A dispatch caller
/// holds settings a repository settings file owns, which the stack never
/// answers; the block rides the payload so this owner still merges it.
fn record_from_block(block: &toml::Table, name: &str) -> Option<ProviderRecord> {
    let overlay = block
        .get("providers")
        .and_then(toml::Value::as_table)
        .and_then(|records| records.get(name))
        .and_then(toml::Value::as_table)
        .map(RawRecord::from);
    let raw = match (builtin_providers().get(name).cloned(), overlay) {
        (None, None) => return None,
        (Some(preset), None) => preset,
        (preset, Some(overlay)) => preset.unwrap_or_default().overlay(overlay),
    };
    Some(raw.fold_tiers())
}

/// The record source a route build reads: the caller-pinned block when one
/// rides the payload, else the config stack at `cwd`.
fn record_for(block: Option<&toml::Table>, cwd: &Path, name: &str) -> Option<ProviderRecord> {
    match block {
        Some(block) => record_from_block(block, name),
        None => provider_record(cwd, name),
    }
}

/// A payload's `block` JSON as the `toml::Table` the merge reads. A JSON
/// round trip, because the settings dumps these blocks carry hold only the
/// scalars and tables TOML has; None when the field is absent or unsuitable.
pub fn block_from_json(value: &serde_json::Value) -> Option<toml::Table> {
    toml::from_str(&serde_json::to_string(value).ok()?).ok()
}

/// The record as the `toml::Table` the footnote-harness endpoint readers
/// consume: `protocol`, `base_url`, `api_key_env`, `api_key_file`, plus the
/// tier map when the record declares one. The built-in preset fills the
/// fields the config leaves absent.
pub fn record_table(cwd: &Path, name: &str) -> Option<toml::Table> {
    let record = provider_record(cwd, name)?;
    let mut table = toml::Table::new();
    table.insert(
        "protocol".into(),
        toml::Value::String(record.protocol.as_str().to_string()),
    );
    if let Some(v) = record.base_url {
        table.insert("base_url".into(), toml::Value::String(v));
    }
    if let Some(v) = record.api_key_env {
        table.insert("api_key_env".into(), toml::Value::String(v));
    }
    if let Some(v) = record.api_key_file {
        table.insert("api_key_file".into(), toml::Value::String(v));
    }
    if !record.tier_models.is_empty() {
        let mut tiers = toml::Table::new();
        for (alias, tier_model) in &record.tier_models {
            tiers.insert(
                alias.as_str().into(),
                toml::Value::String(tier_model.clone()),
            );
        }
        table.insert("tier_models".into(), toml::Value::Table(tiers));
    }
    Some(table)
}

/// The key precedence rule against the CALLER's env: the env var named by
/// `api_key_env` wins over the same name read from the `api_key_file`
/// dotenv file. The accessor twin of `provider_key::resolve_key`, which
/// reads the process env; `resolve_key` stays the owner for those callers.
pub fn resolve_key_with(
    env_get: &dyn Fn(&str) -> Option<String>,
    key_env: &str,
    api_key_file: Option<&str>,
) -> Option<String> {
    if key_env.is_empty() {
        return None;
    }
    if let Some(from_env) = env_get(key_env).filter(|v| !v.is_empty()) {
        return Some(from_env);
    }
    api_key_file.and_then(|file| crate::provider_key::read_var_from_env_file(file, key_env))
}

/// The lowercased host of `base_url` with scheme, userinfo, port and path
/// stripped; empty when there is none. A bare authority like
/// `api.anthropic.com/v1` carries no scheme, and urlsplit reads the whole
/// thing as a path, so give it one first (the Python twin's move).
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
    let after_scheme = with_scheme
        .split_once("://")
        .map(|(_, rest)| rest)
        .unwrap_or(&with_scheme);
    let authority = after_scheme.trim_start_matches('/').split('/').next();
    let authority = authority
        .and_then(|a| a.rsplit_once('@').map(|(_, host_port)| host_port))
        .or(authority);
    authority
        .unwrap_or_default()
        .to_ascii_lowercase()
        .split(':')
        .next()
        .unwrap_or_default()
        .to_string()
}

/// A TOML scalar as the config string Python's `str(v)` made. Strings carry
/// verbatim; other scalars render through their Display.
fn scalar_string(value: &toml::Value) -> Option<String> {
    match value {
        toml::Value::String(s) => Some(s.clone()),
        other => Some(other.to_string()),
    }
}

impl ClaudeRoute {
    /// The spawn-overlay answer shape: `{"route": {...} | null, "notices":
    /// [...]}`.
    pub fn to_json(&self) -> serde_json::Value {
        match self {
            ClaudeRoute::Routed {
                env,
                provider: _,
                notices,
            } => {
                let mut map = serde_json::Map::new();
                for (k, v) in env {
                    map.insert(k.clone(), serde_json::Value::String(v.clone()));
                }
                serde_json::json!({ "route": map, "notices": notices })
            }
            ClaudeRoute::Skipped { notices } => {
                serde_json::json!({ "route": serde_json::Value::Null, "notices": notices })
            }
        }
    }
}

/// Build the routed `ANTHROPIC_*` env for an explicit `(provider, model)`,
/// port of `_route_for_target`. Fail-safe, never refuses: every
/// misconfiguration answers `Skipped` with the Python leg's notice text.
/// The role-to-target lookup and the business-role errors stay in Python.
pub fn claude_route(
    cwd: &Path,
    provider: &str,
    model: &str,
    ctx: &RouteCtx,
    env_get: &dyn Fn(&str) -> Option<String>,
) -> ClaudeRoute {
    claude_route_with(None, cwd, provider, model, ctx, env_get)
}

/// [`claude_route`] with a caller-pinned routing block. When a Python caller
/// holds settings a repository settings file owns (`dispatch` hands the
/// account repository's `model_routing` over), the block rides the payload
/// and REPLACES the config stack as both overlay sources - the same
/// preset-under-overlay merge, one precedence layer up.
pub fn claude_route_with(
    block: Option<&toml::Table>,
    cwd: &Path,
    provider: &str,
    model: &str,
    ctx: &RouteCtx,
    env_get: &dyn Fn(&str) -> Option<String>,
) -> ClaudeRoute {
    let fallback = if ctx.is_peer() {
        "skipping the peer"
    } else {
        "using the primary model"
    };
    let fallback_key = if ctx.is_peer() {
        "skipping the peer"
    } else {
        "falling back to the primary Anthropic model"
    };
    let model = model.trim();
    let record = match record_for(block, cwd, provider) {
        Some(record) => record,
        None => {
            return ClaudeRoute::Skipped {
                notices: vec![format!(
                    "model-routing: provider '{provider}' for {} is not configured; {fallback}",
                    ctx.label()
                )],
            }
        }
    };
    if record.protocol.as_str() != "anthropic" {
        return ClaudeRoute::Skipped {
            notices: vec![format!(
                "model-routing: provider '{provider}' uses the '{}' protocol, which a claude worker cannot use; {fallback}",
                record.protocol.as_str()
            )],
        };
    }
    let Some(base_url) = record.base_url else {
        return ClaudeRoute::Skipped {
            notices: vec![format!(
                "model-routing: provider '{provider}' has no base_url; {fallback}"
            )],
        };
    };
    let key = resolve_key_with(
        env_get,
        record.api_key_env.as_deref().unwrap_or_default(),
        record.api_key_file.as_deref(),
    );
    let Some(key) = key else {
        return ClaudeRoute::Skipped {
            notices: vec![format!(
                "model-routing: no API key for provider '{provider}' ({}); {fallback_key}",
                ctx.label()
            )],
        };
    };
    // A bare tier alias is resolved by Claude Code against the endpoint it is
    // pointed at, and a third-party endpoint has no "sonnet": the spawn would
    // print a live receipt and die on its first turn. Refuse, never coerce.
    if crate::model_env_scrub::is_anthropic_model(model)
        && !matches!(base_url_host(&base_url).as_str(), "" | "api.anthropic.com")
    {
        return ClaudeRoute::Skipped {
            notices: vec![format!(
                "model-routing: refusing model '{model}' for provider '{provider}': a bare tier alias resolves against the endpoint the worker is pointed at, and {base_url} is not Anthropic's; name a model id this provider serves"
            )],
        };
    }
    let mut env = BTreeMap::new();
    env.insert("ANTHROPIC_BASE_URL".to_string(), base_url.clone());
    env.insert("ANTHROPIC_AUTH_TOKEN".to_string(), key);
    for k in MODEL_ENV_KEYS {
        env.insert(k.to_string(), model.to_string());
    }
    for (alias, tier_model) in &record.tier_models {
        env.insert(alias.env_key(), tier_model.clone());
    }
    if model.ends_with("[1m]") {
        env.insert(
            "CLAUDE_CODE_AUTO_COMPACT_WINDOW".to_string(),
            "800000".to_string(),
        );
    }
    let extra_env = match block {
        Some(block) => block
            .get("extra_env")
            .and_then(toml::Value::as_table)
            .cloned(),
        None => config_table_merged(cwd, &["model_routing", "extra_env"]),
    };
    if let Some(extra) = extra_env {
        for (k, v) in &extra {
            if let Some(text) = scalar_string(v) {
                env.insert(k.clone(), text);
            }
        }
    }
    // Checked after extra_env so a hand pin that already differentiates the
    // tiers silences it. Four identical /model rows read as "the config did
    // not take"; this names the one lever that fixes it.
    let tier_values: BTreeSet<&String> =
        MODEL_ENV_KEYS.iter().filter_map(|k| env.get(*k)).collect();
    let mut notices = Vec::new();
    if tier_values.len() == 1 {
        notices.push(format!(
            "model-routing: every tier resolves to {}; /model will offer no alternative; declare model_routing.providers.{provider}.tier_models to differentiate",
            tier_values
                .into_iter()
                .next()
                .map(|value| value.as_str())
                .unwrap_or_default()
        ));
    }
    ClaudeRoute::Routed {
        env,
        provider: provider.to_string(),
        notices,
    }
}

/// `key_source` port: WHERE the provider's key resolved, never the value.
pub fn key_source(record: &ProviderRecord, env_get: &dyn Fn(&str) -> Option<String>) -> KeySource {
    let mut checked: Vec<String> = Vec::new();
    let key_name = record.api_key_env.clone().unwrap_or_default();
    if !key_name.is_empty() {
        checked.push(key_name.clone());
        if env_get(&key_name).filter(|v| !v.is_empty()).is_some() {
            return KeySource {
                satisfying: Some(key_name),
                checked,
            };
        }
    }
    let Some(file) = record.api_key_file.as_deref() else {
        return KeySource {
            satisfying: None,
            checked,
        };
    };
    if !key_name.is_empty() {
        checked.push(file.to_string());
        if crate::provider_key::read_var_from_env_file(file, &key_name).is_some() {
            return KeySource {
                satisfying: Some(file.to_string()),
                checked,
            };
        }
    }
    KeySource {
        satisfying: None,
        checked,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn caller_pinned_block_overlays_the_builtin_preset() {
        let block: toml::Table = toml::from_str(
            "[providers.zai]\n\
             base_url = \"https://project.example/anthropic\"\n\
             api_key_env = \"PROJECT_ZAI_KEY\"\n",
        )
        .expect("block parses");
        let record = record_from_block(&block, "zai").expect("zai resolves");
        assert_eq!(record.protocol, Protocol::Anthropic);
        assert_eq!(
            record.base_url.as_deref(),
            Some("https://project.example/anthropic")
        );
        assert_eq!(record.api_key_env.as_deref(), Some("PROJECT_ZAI_KEY"));
        // The preset's cheap background tier survives a partial override.
        assert_eq!(
            record
                .tier_models
                .get(&TierAlias::Haiku)
                .map(String::as_str),
            Some("glm-4.7")
        );
    }

    #[test]
    fn block_extra_env_replaces_the_stack_read() {
        let block: toml::Table =
            toml::from_str("[extra_env]\nCLAUDE_CODE_TIMEOUT = \"600\"\n").expect("block parses");
        let env_get = |_key: &str| Some("zk-secret".to_string());
        match claude_route_with(
            Some(&block),
            Path::new("/nonexistent-fno-cwd"),
            "zai",
            "glm-5.3",
            &RouteCtx::Peer("zai".to_string()),
            &env_get,
        ) {
            ClaudeRoute::Routed { env, .. } => assert_eq!(
                env.get("CLAUDE_CODE_TIMEOUT").map(String::as_str),
                Some("600")
            ),
            ClaudeRoute::Skipped { notices } => {
                panic!("zai with a key routes: {notices:?}")
            }
        }
    }
}

/// The merged registry as the spawn-overlay `providers` answer: one record
/// object per name, the key column naming its source and never the value.
pub fn providers_json(cwd: &Path, env_get: &dyn Fn(&str) -> Option<String>) -> serde_json::Value {
    let mut registry = serde_json::Map::new();
    for (name, record) in providers(cwd) {
        registry.insert(name, record_json(&record, env_get));
    }
    serde_json::json!({ "providers": registry })
}

fn record_json(
    record: &ProviderRecord,
    env_get: &dyn Fn(&str) -> Option<String>,
) -> serde_json::Value {
    let key = key_source(record, env_get);
    let opt = |v: &Option<String>| match v {
        Some(s) => serde_json::json!(s),
        None => serde_json::Value::Null,
    };
    let mut tier_models = serde_json::Map::new();
    for (alias, tier_model) in &record.tier_models {
        tier_models.insert(alias.as_str().to_string(), serde_json::json!(tier_model));
    }
    serde_json::json!({
        "protocol": record.protocol.as_str(),
        "base_url": opt(&record.base_url),
        "api_key_env": opt(&record.api_key_env),
        "api_key_file": opt(&record.api_key_file),
        "tier_models": tier_models,
        "key": {
            "satisfying": match key.satisfying {
                Some(ref s) => serde_json::json!(s),
                None => serde_json::Value::Null,
            },
            "checked": key.checked,
        },
    })
}
