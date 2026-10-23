//! `config.accounts`: the account records, their validation, and the
//! layered read every Rust reader of them goes through.
//!
//! A port of `model.py` (the record, pricing, agent-pin and combo checks with
//! their refusal text) and the read side of `loader.py` (candidates, the
//! settings.yaml fallback, the per-id overlay, `load_providers`,
//! `load_combos`, `load_active_combo`, the quota block, `known_account_ids`,
//! `record_harness`, `effective_active`, `load_scope_config`).
//!
//! Coercion follows pydantic's lax mode as measured on the live leg: an int
//! field takes a whole float, a bool or a numeric string; a str field strips
//! surrounding whitespace, a literal field does not. A refusal carries the
//! Python headline exactly (`invalid provider record 'x': auth_strategy_mismatch`)
//! and a one-line reason in place of pydantic's multi-line report.

use crate::claude_ask::py_repr as py_repr_str;
use serde_json::{json, Map, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::path::{Component, Path, PathBuf};

/// `model.py::_HARNESS_LITERAL`. `gemini` stays admissible so an existing
/// record gets the dispatch-time refusal rather than a load failure.
pub const HARNESS_LITERAL: [&str; 8] = [
    "claude", "codex", "opencode", "agy", "trae", "openclaw", "hermes", "gemini",
];

const AUTH_LITERAL: [&str; 3] = ["oauth_dir", "api_key", "managed"];

/// `_RECOGNIZED_API_KEY_NAMES`, sorted as the refusal prints them.
const RECOGNIZED_API_KEY_NAMES: [&str; 4] = [
    "ANTHROPIC_API_KEY",
    "ANTHROPIC_AUTH_TOKEN",
    "GEMINI_API_KEY",
    "OPENAI_API_KEY",
];

const VALID_STRATEGIES: [&str; 2] = ["fallback", "round_robin"];

/// `loader._AGENTS_RESERVED_KEYS`: settings blocks that share the
/// `config.agents` namespace with per-agent pins and are never read as one.
/// The `reserved_agent_keys` golden holds this list to the Python set.
pub const AGENTS_RESERVED_KEYS: &[&str] = &[
    "a2a",
    "auto_register_sessions",
    "claude",
    "codex",
    "confirm",
    "defaults",
    "fallback",
    "first_check_minutes",
    "footprint_sustained_cpu_cores",
    "gemini",
    "happy_routed_panes",
    "hold_escalate_after_s",
    "max_lanes",
    "max_live",
    "max_live_per_territory",
    "max_fleet_cpu_share",
    "max_load_per_cpu",
    "pane_group_max",
    "min_free_gb",
    "max_swap_pct",
    "naming",
    "orphan_reap_after_seconds",
    "profiles",
    "provider_limits",
    "reap",
    "reap_receipts",
    "retire_grace_s",
    "retire_interval_s",
    "silence_deadline_seconds",
    "single_flight_join_budget_seconds",
    "single_flight_ttl_seconds",
    "state_reap",
    "worker_qos",
];

/// The record keys the model declares. Every other key is retained as
/// metadata (`extra="allow"`).
const RECORD_FIELDS: [&str; 18] = [
    "id",
    "name",
    "harness",
    "auth",
    "priority",
    "route_provider_id",
    "model_name",
    "credentials_source",
    "env",
    "route",
    "reset_timezone",
    "window_minutes",
    "account_id",
    "tags",
    "description",
    "config_dir",
    "pricing",
    "cost_cap_usd_per_session",
];

/// A load refusal: the Python `ProviderConfigError` headline, plus the
/// reason pydantic would have reported under it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoadError {
    pub headline: String,
    pub detail: Option<String>,
}

impl LoadError {
    fn bare(headline: impl Into<String>) -> Self {
        LoadError {
            headline: headline.into(),
            detail: None,
        }
    }
}

impl fmt::Display for LoadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.detail {
            Some(detail) => write!(f, "{}: {}", self.headline, detail),
            None => f.write_str(&self.headline),
        }
    }
}

/// `model.Pricing`: a rate card, every rate `>= 0`.
#[derive(Debug, Clone, PartialEq)]
pub struct Pricing {
    pub input_per_million_usd: f64,
    pub output_per_million_usd: f64,
    pub cache_read_per_million_usd: Option<f64>,
    pub cache_write_per_million_usd: Option<f64>,
}

/// `model.ProviderRecord`: one working account of one harness.
#[derive(Debug, Clone, PartialEq)]
pub struct Record {
    pub id: String,
    pub name: String,
    pub harness: String,
    pub auth: String,
    pub priority: i64,
    pub route_provider_id: Option<String>,
    pub model_name: Option<String>,
    pub credentials_source: Option<PathBuf>,
    pub env: Option<BTreeMap<String, String>>,
    pub route: Option<String>,
    pub reset_timezone: Option<String>,
    pub window_minutes: Option<i64>,
    pub account_id: String,
    pub tags: Vec<String>,
    pub description: Option<String>,
    pub config_dir: Option<PathBuf>,
    pub pricing: Option<Pricing>,
    pub cost_cap_usd_per_session: Option<f64>,
    /// Unknown keys, kept for forward-compatible round trips.
    pub extra: Map<String, Value>,
}

/// `model.ProvidersConfig`: the validated accounts block.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Accounts {
    pub records: Vec<Record>,
    pub active: Option<String>,
    pub auto_switch: bool,
    /// Per-agent pins: agent name -> record id.
    pub agents: BTreeMap<String, String>,
}

/// `rotation.Combo`: a named ordered account list with its strategy.
#[derive(Debug, Clone, PartialEq)]
pub struct Combo {
    pub name: String,
    pub strategy: String,
    pub sticky_limit: i64,
    pub providers: Vec<String>,
}

impl Accounts {
    pub fn by_id(&self, id: &str) -> Option<&Record> {
        self.records.iter().find(|r| r.id == id)
    }

    /// `ProvidersConfig.model_dump(mode="json")`.
    pub fn to_json(&self) -> Value {
        let agents: Map<String, Value> = self
            .agents
            .iter()
            .map(|(name, provider)| (name.clone(), json!({ "provider": provider })))
            .collect();
        json!({
            "records": self.records.iter().map(Record::to_json).collect::<Vec<_>>(),
            "active": self.active,
            "failover": Value::Null,
            "auto_switch": self.auto_switch,
            "agents": agents,
        })
    }
}

fn path_json(p: &Option<PathBuf>) -> Value {
    match p {
        Some(p) => Value::String(p.to_string_lossy().into_owned()),
        None => Value::Null,
    }
}

impl Record {
    /// `ProviderRecord.model_dump(mode="json")`: the declared fields in
    /// order, then the retained metadata.
    pub fn to_json(&self) -> Value {
        let mut out = Map::new();
        out.insert("id".into(), json!(self.id));
        out.insert("name".into(), json!(self.name));
        out.insert("harness".into(), json!(self.harness));
        out.insert("auth".into(), json!(self.auth));
        out.insert("priority".into(), json!(self.priority));
        out.insert("route_provider_id".into(), json!(self.route_provider_id));
        out.insert("model_name".into(), json!(self.model_name));
        out.insert(
            "credentials_source".into(),
            path_json(&self.credentials_source),
        );
        out.insert("env".into(), json!(self.env));
        out.insert("route".into(), json!(self.route));
        out.insert("reset_timezone".into(), json!(self.reset_timezone));
        out.insert("window_minutes".into(), json!(self.window_minutes));
        out.insert("account_id".into(), json!(self.account_id));
        out.insert("tags".into(), json!(self.tags));
        out.insert("description".into(), json!(self.description));
        out.insert("config_dir".into(), path_json(&self.config_dir));
        out.insert(
            "pricing".into(),
            match &self.pricing {
                Some(p) => json!({
                    "input_per_million_usd": p.input_per_million_usd,
                    "output_per_million_usd": p.output_per_million_usd,
                    "cache_read_per_million_usd": p.cache_read_per_million_usd,
                    "cache_write_per_million_usd": p.cache_write_per_million_usd,
                }),
                None => Value::Null,
            },
        );
        out.insert(
            "cost_cap_usd_per_session".into(),
            json!(self.cost_cap_usd_per_session),
        );
        for (k, v) in &self.extra {
            out.insert(k.clone(), v.clone());
        }
        Value::Object(out)
    }
}

impl Combo {
    pub fn to_json(&self) -> Value {
        json!({
            "name": self.name,
            "strategy": self.strategy,
            "sticky_limit": self.sticky_limit,
            "providers": self.providers,
        })
    }
}

// ---------------------------------------------------------------------------
// Lax coercion (pydantic's default mode, as measured on the live leg).
// ---------------------------------------------------------------------------

/// The Python type name a refusal prints for a TOML value.
fn py_type(v: &toml::Value) -> &'static str {
    match v {
        toml::Value::String(_) => "str",
        toml::Value::Integer(_) => "int",
        toml::Value::Float(_) => "float",
        toml::Value::Boolean(_) => "bool",
        toml::Value::Datetime(_) => "datetime",
        toml::Value::Array(_) => "list",
        toml::Value::Table(_) => "dict",
    }
}

/// `str(v)` for a scalar, as an f-string renders it.
fn py_str(v: &toml::Value) -> String {
    match v {
        toml::Value::String(s) => s.clone(),
        toml::Value::Integer(i) => i.to_string(),
        toml::Value::Float(f) => py_float(*f),
        toml::Value::Boolean(true) => "True".to_string(),
        toml::Value::Boolean(false) => "False".to_string(),
        other => other.to_string(),
    }
}

/// `repr(v)`: a string quoted, anything else as `str`.
fn py_repr(v: &toml::Value) -> String {
    match v {
        toml::Value::String(s) => py_repr_str(s),
        other => py_str(other),
    }
}

fn py_float(f: f64) -> String {
    if f.fract() == 0.0 && f.is_finite() && f.abs() < 1e16 {
        format!("{f:.1}")
    } else {
        f.to_string()
    }
}

fn py_list_repr<S: AsRef<str>>(items: &[S]) -> String {
    let inner: Vec<String> = items.iter().map(|s| py_repr_str(s.as_ref())).collect();
    format!("[{}]", inner.join(", "))
}

/// A lax `int`: an int, a bool, a whole float, or a numeric string.
pub(crate) fn lax_int(v: &toml::Value) -> Option<i64> {
    match v {
        toml::Value::Integer(i) => Some(*i),
        toml::Value::Boolean(b) => Some(i64::from(*b)),
        toml::Value::Float(f) if f.is_finite() && f.fract() == 0.0 => Some(*f as i64),
        toml::Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }
}

/// A lax `float`: an int, a float, a bool, or a numeric string.
pub(crate) fn lax_float(v: &toml::Value) -> Option<f64> {
    match v {
        toml::Value::Integer(i) => Some(*i as f64),
        toml::Value::Float(f) => Some(*f),
        toml::Value::Boolean(b) => Some(if *b { 1.0 } else { 0.0 }),
        toml::Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }
}

/// A lax `bool`: a bool, 0/1, or one of pydantic's truthy/falsy words.
pub(crate) fn lax_bool(v: &toml::Value) -> Option<bool> {
    match v {
        toml::Value::Boolean(b) => Some(*b),
        toml::Value::Integer(0) => Some(false),
        toml::Value::Integer(1) => Some(true),
        toml::Value::Float(f) if *f == 0.0 => Some(false),
        toml::Value::Float(f) if *f == 1.0 => Some(true),
        toml::Value::String(s) => match s.trim().to_ascii_lowercase().as_str() {
            "0" | "off" | "f" | "false" | "n" | "no" => Some(false),
            "1" | "on" | "t" | "true" | "y" | "yes" => Some(true),
            _ => None,
        },
        _ => None,
    }
}

/// `^[a-z][a-z0-9-]{0,63}$` (`model.py::_ID_PATTERN`).
pub(crate) fn valid_record_id(s: &str) -> bool {
    let b = s.as_bytes();
    !b.is_empty()
        && b.len() <= 64
        && b[0].is_ascii_lowercase()
        && b[1..]
            .iter()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == b'-')
}

/// `Path(s).expanduser()` with pathlib's normalization (repeated and
/// trailing separators and `.` parts dropped).
fn py_path(raw: &str) -> PathBuf {
    let expanded = match raw.strip_prefix('~') {
        Some(rest) if rest.is_empty() || rest.starts_with('/') => match std::env::var_os("HOME") {
            Some(home) => format!("{}{}", home.to_string_lossy(), rest),
            None => raw.to_string(),
        },
        _ => raw.to_string(),
    };
    let normalized: PathBuf = Path::new(&expanded)
        .components()
        .filter(|c| !matches!(c, Component::CurDir))
        .collect();
    if normalized.as_os_str().is_empty() {
        PathBuf::from(".")
    } else {
        normalized
    }
}

pub(crate) fn toml_to_json(v: &toml::Value) -> Value {
    match v {
        toml::Value::String(s) => json!(s),
        toml::Value::Integer(i) => json!(i),
        toml::Value::Float(f) => json!(f),
        toml::Value::Boolean(b) => json!(b),
        toml::Value::Datetime(d) => json!(d.to_string()),
        toml::Value::Array(a) => Value::Array(a.iter().map(toml_to_json).collect()),
        toml::Value::Table(t) => Value::Object(
            t.iter()
                .map(|(k, v)| (k.clone(), toml_to_json(v)))
                .collect(),
        ),
    }
}

// ---------------------------------------------------------------------------
// Record validation (`ProviderRecord`).
// ---------------------------------------------------------------------------

/// Why one record failed: the reason, and whether it came from the auth
/// strategy check (the headline then carries `auth_strategy_mismatch`).
struct RecordError {
    auth_mismatch: bool,
    detail: String,
}

fn field_err(field: &str, why: &str) -> RecordError {
    RecordError {
        auth_mismatch: false,
        detail: format!("{field}: {why}"),
    }
}

fn opt_str(t: &toml::Table, key: &str) -> Result<Option<String>, RecordError> {
    match t.get(key) {
        None => Ok(None),
        Some(toml::Value::String(s)) => Ok(Some(s.trim().to_string())),
        Some(_) => Err(field_err(key, "Input should be a valid string")),
    }
}

fn req_str(t: &toml::Table, key: &str) -> Result<String, RecordError> {
    opt_str(t, key)?.ok_or_else(|| field_err(key, "Field required"))
}

fn opt_path(t: &toml::Table, key: &str) -> Result<Option<PathBuf>, RecordError> {
    match t.get(key) {
        None => Ok(None),
        Some(toml::Value::String(s)) => Ok(Some(py_path(s))),
        Some(_) => Err(field_err(key, "Input should be a valid path")),
    }
}

fn opt_nonneg_float(t: &toml::Table, key: &str) -> Result<Option<f64>, RecordError> {
    match t.get(key) {
        None => Ok(None),
        Some(v) => match lax_float(v) {
            Some(f) if f >= 0.0 => Ok(Some(f)),
            Some(_) => Err(field_err(key, "Input should be greater than or equal to 0")),
            None => Err(field_err(key, "Input should be a valid number")),
        },
    }
}

fn parse_pricing(v: &toml::Value) -> Result<Pricing, RecordError> {
    let toml::Value::Table(t) = v else {
        return Err(field_err("pricing", "Input should be a valid dictionary"));
    };
    const KEYS: [&str; 4] = [
        "input_per_million_usd",
        "output_per_million_usd",
        "cache_read_per_million_usd",
        "cache_write_per_million_usd",
    ];
    if let Some(extra) = t.keys().find(|k| !KEYS.contains(&k.as_str())) {
        return Err(field_err(
            &format!("pricing.{extra}"),
            "Extra inputs are not permitted",
        ));
    }
    let required = |key: &str| -> Result<f64, RecordError> {
        opt_nonneg_float(t, key)?
            .ok_or_else(|| field_err(&format!("pricing.{key}"), "Field required"))
    };
    Ok(Pricing {
        input_per_million_usd: required(KEYS[0])?,
        output_per_million_usd: required(KEYS[1])?,
        cache_read_per_million_usd: opt_nonneg_float(t, KEYS[2])?,
        cache_write_per_million_usd: opt_nonneg_float(t, KEYS[3])?,
    })
}

fn parse_record(t: &toml::Table) -> Result<Record, RecordError> {
    let id = req_str(t, "id")?;
    if !valid_record_id(&id) {
        return Err(field_err(
            "id",
            "String should match pattern '^[a-z][a-z0-9-]{0,63}$'",
        ));
    }
    let name = req_str(t, "name")?;
    // AliasChoices("harness", "cli"): the first PRESENT key wins, and an
    // unused `cli` beside `harness` stays as metadata. A literal is not
    // stripped.
    let harness_key = if t.contains_key("harness") {
        "harness"
    } else {
        "cli"
    };
    let harness = match t.get(harness_key) {
        None => return Err(field_err("harness", "Field required")),
        Some(toml::Value::String(s)) if HARNESS_LITERAL.contains(&s.as_str()) => s.clone(),
        Some(_) => {
            let (head, last) = HARNESS_LITERAL.split_at(HARNESS_LITERAL.len() - 1);
            let quoted: Vec<String> = head.iter().map(|h| format!("'{h}'")).collect();
            return Err(field_err(
                "harness",
                &format!("Input should be {} or '{}'", quoted.join(", "), last[0]),
            ));
        }
    };
    let auth = match t.get("auth") {
        None => return Err(field_err("auth", "Field required")),
        Some(toml::Value::String(s)) if AUTH_LITERAL.contains(&s.as_str()) => s.clone(),
        Some(_) => {
            return Err(field_err(
                "auth",
                "Input should be 'oauth_dir', 'api_key' or 'managed'",
            ))
        }
    };
    let priority = match t.get("priority") {
        None => 100,
        Some(v) => match lax_int(v) {
            Some(p) if p >= 0 => p,
            Some(_) => {
                return Err(field_err(
                    "priority",
                    "Input should be greater than or equal to 0",
                ))
            }
            None => return Err(field_err("priority", "Input should be a valid integer")),
        },
    };
    let route_provider_id = opt_str(t, "route_provider_id")?;
    if let Some(rp) = &route_provider_id {
        if !valid_record_id(rp) {
            return Err(field_err(
                "route_provider_id",
                "String should match pattern '^[a-z][a-z0-9-]{0,63}$'",
            ));
        }
    }
    let model_name = opt_str(t, "model_name")?;
    if model_name.as_deref() == Some("") {
        return Err(field_err(
            "model_name",
            "String should have at least 1 character",
        ));
    }
    let credentials_source = opt_path(t, "credentials_source")?;
    let env = match t.get("env") {
        None => None,
        Some(toml::Value::Table(env)) => {
            let mut out = BTreeMap::new();
            for (k, v) in env {
                let toml::Value::String(s) = v else {
                    return Err(field_err(
                        &format!("env.{k}"),
                        "Input should be a valid string",
                    ));
                };
                out.insert(k.clone(), s.trim().to_string());
            }
            Some(out)
        }
        Some(_) => return Err(field_err("env", "Input should be a valid dictionary")),
    };
    let route = opt_str(t, "route")?;
    if let Some(r) = &route {
        if crate::spawn_route::parse_target(r).is_none() {
            return Err(field_err(
                "route",
                "Value error, route must use provider/model spelling with a non-empty model",
            ));
        }
    }
    let reset_timezone = opt_str(t, "reset_timezone")?;
    let window_minutes = match t.get("window_minutes") {
        None => None,
        Some(v) => match lax_int(v) {
            Some(w) if w > 0 => Some(w),
            Some(_) => {
                return Err(field_err(
                    "window_minutes",
                    "Input should be greater than 0",
                ))
            }
            None => {
                return Err(field_err(
                    "window_minutes",
                    "Input should be a valid integer",
                ))
            }
        },
    };
    let account_id = opt_str(t, "account_id")?;
    let tags = match t.get("tags") {
        None => Vec::new(),
        Some(toml::Value::Array(items)) => {
            let mut out = Vec::with_capacity(items.len());
            for (i, item) in items.iter().enumerate() {
                let toml::Value::String(s) = item else {
                    return Err(field_err(
                        &format!("tags.{i}"),
                        "Input should be a valid string",
                    ));
                };
                out.push(s.trim().to_string());
            }
            out
        }
        Some(_) => return Err(field_err("tags", "Input should be a valid list")),
    };
    let description = opt_str(t, "description")?;
    let config_dir = match t.get("config_dir") {
        None => None,
        Some(toml::Value::String(raw)) => {
            let expanded = py_path(raw);
            if !expanded.is_absolute() {
                return Err(field_err(
                    "config_dir",
                    &format!(
                        "Value error, config_dir must be an absolute path (got PosixPath({})); \
                         a relative dir resolves against the worker's cwd",
                        py_repr_str(&py_path_no_home(raw))
                    ),
                ));
            }
            Some(expanded)
        }
        Some(_) => return Err(field_err("config_dir", "Input should be a valid path")),
    };
    let pricing = t.get("pricing").map(parse_pricing).transpose()?;
    let cost_cap_usd_per_session = opt_nonneg_float(t, "cost_cap_usd_per_session")?;

    let mut extra = Map::new();
    for (k, v) in t {
        if RECORD_FIELDS.contains(&k.as_str()) || (k == "cli" && harness_key == "cli") {
            continue;
        }
        extra.insert(k.clone(), toml_to_json(v));
    }

    let record = Record {
        account_id: account_id.unwrap_or_else(|| id.clone()),
        id,
        name,
        harness,
        auth,
        priority,
        route_provider_id,
        model_name,
        credentials_source,
        env,
        route,
        reset_timezone,
        window_minutes,
        tags,
        description,
        config_dir,
        pricing,
        cost_cap_usd_per_session,
        extra,
    };
    check_auth_strategy(&record)?;
    Ok(record)
}

/// `ProviderRecord(**fields)` as the add verb builds it. The refusal is
/// `str(ValidationError)` without pydantic's ` [type=...]` suffix and its
/// docs-URL line, which carry the pydantic version.
pub(crate) fn build_record(t: &toml::Table) -> Result<Record, String> {
    parse_record(t).map_err(|e| {
        let body = match e.detail.split_once(": ") {
            Some((loc, msg)) if !e.auth_mismatch => format!("{loc}\n  {msg}"),
            _ => format!("  Value error, {}", e.detail),
        };
        format!("1 validation error for ProviderRecord\n{body}")
    })
}

/// The relative path as pathlib renders it, without the home expansion
/// (only an expanded `~` path can be absolute, so a refused one never
/// started with `~/`).
fn py_path_no_home(raw: &str) -> String {
    let normalized: PathBuf = Path::new(raw)
        .components()
        .filter(|c| !matches!(c, Component::CurDir))
        .collect();
    let s = normalized.to_string_lossy().into_owned();
    if s.is_empty() {
        ".".to_string()
    } else {
        s
    }
}

/// `ProviderRecord._check_auth_strategy`.
fn check_auth_strategy(r: &Record) -> Result<(), RecordError> {
    let mismatch = |why: String| RecordError {
        auth_mismatch: true,
        detail: format!("auth_strategy_mismatch: {}: {why}", r.id),
    };
    let route = r.route.as_deref().is_some_and(|s| !s.is_empty());
    let env_nonempty = r.env.as_ref().is_some_and(|e| !e.is_empty());
    if route && r.auth != "api_key" {
        return Err(mismatch("route requires auth=api_key".into()));
    }
    match r.auth.as_str() {
        "oauth_dir" if r.credentials_source.is_none() => Err(mismatch(
            "auth=oauth_dir requires credentials_source".into(),
        )),
        "api_key" => {
            if env_nonempty && route {
                return Err(mismatch(
                    "auth=api_key requires exactly one of env or route".into(),
                ));
            }
            if !env_nonempty && !route {
                return Err(mismatch(
                    "auth=api_key requires exactly one of non-empty env or route".into(),
                ));
            }
            if let Some(env) = r.env.as_ref().filter(|e| !e.is_empty()) {
                if !env
                    .keys()
                    .any(|k| RECOGNIZED_API_KEY_NAMES.contains(&k.as_str()))
                {
                    let got: Vec<&String> = env.keys().collect();
                    return Err(mismatch(format!(
                        "auth=api_key env must contain at least one of {}, got {}",
                        py_list_repr(&RECOGNIZED_API_KEY_NAMES),
                        py_list_repr(&got)
                    )));
                }
            }
            Ok(())
        }
        "managed" if r.credentials_source.is_some() || env_nonempty => Err(mismatch(
            "auth=managed takes neither credentials_source nor env \
             (the credential is snapshotted into the managed store)"
                .into(),
        )),
        _ => Ok(()),
    }
}

// ---------------------------------------------------------------------------
// The layered read.
// ---------------------------------------------------------------------------

/// `fno.paths.resolve_repo_root`: the resolved `FNO_REPO_ROOT` pin, then the
/// git toplevel of `cwd`, then `cwd`.
pub(crate) fn resolve_repo_root(cwd: &Path) -> PathBuf {
    if let Some(pin) = std::env::var_os("FNO_REPO_ROOT").filter(|p| !p.is_empty()) {
        return crate::paths::resolve_loose(Path::new(&pin));
    }
    let Ok(out) = std::process::Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .current_dir(cwd)
        .output()
    else {
        return cwd.to_path_buf();
    };
    if out.status.success() {
        let text = String::from_utf8_lossy(&out.stdout);
        let trimmed = text.trim();
        if !trimmed.is_empty() {
            return crate::paths::resolve_loose(Path::new(trimmed));
        }
    }
    cwd.to_path_buf()
}

/// `loader._provider_candidates`, FIRST layer wins: `$FNO_CONFIG` alone, or
/// the repo-root local, the PWD local when no explicit root is given and it
/// differs, then the global path.
pub fn candidates(repo_root: Option<&Path>, cwd: &Path) -> Vec<PathBuf> {
    if let Some(explicit) = std::env::var_os("FNO_CONFIG").filter(|p| !p.is_empty()) {
        return vec![PathBuf::from(explicit)];
    }
    let root = match repo_root {
        Some(root) => root.to_path_buf(),
        None => resolve_repo_root(cwd),
    };
    let mut out = vec![root.join(".fno/config.toml")];
    if repo_root.is_none() {
        out.push(pwd(cwd).join(".fno/config.toml"));
    }
    if let Some(global) = crate::agents_config::global_config_path() {
        out.push(global);
    }
    let mut seen = BTreeSet::new();
    out.retain(|p| seen.insert(p.clone()));
    out
}

fn pwd(cwd: &Path) -> PathBuf {
    std::env::var_os("PWD")
        .filter(|p| !p.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| cwd.to_path_buf())
}

/// `loader._read_candidates`: config.toml then its settings.yaml sibling.
pub(crate) fn read_candidates(path: &Path) -> Vec<PathBuf> {
    match path.file_name().and_then(|n| n.to_str()) {
        Some("config.toml") => vec![path.to_path_buf(), path.with_file_name("settings.yaml")],
        Some("settings.yaml") => vec![path.with_file_name("config.toml"), path.to_path_buf()],
        _ => vec![path.to_path_buf()],
    }
}

/// `loader._read_parsed`: the first existing candidate, parsed by suffix.
/// A read or parse failure reads as empty and stops; it never raises.
pub(crate) fn read_layer(path: &Path) -> toml::Table {
    for candidate in read_candidates(path) {
        if !candidate.is_file() {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&candidate) else {
            break;
        };
        if candidate.extension().is_some_and(|e| e == "toml") {
            return text.parse::<toml::Table>().unwrap_or_default();
        }
        let Ok(doc) = serde_yaml_ng::from_str::<serde_yaml_ng::Value>(&text) else {
            break;
        };
        return match yaml_value_to_toml(&doc) {
            Some(toml::Value::Table(t)) => t,
            _ => toml::Table::new(),
        };
    }
    toml::Table::new()
}

/// A YAML value as `yaml.safe_load` hands it to the loader. A null maps to
/// `None`, so a null key reads as absent, which is what every reader here
/// does with Python's `None`.
pub(crate) fn yaml_value_to_toml(value: &serde_yaml_ng::Value) -> Option<toml::Value> {
    use serde_yaml_ng::Value as Y;
    Some(match value {
        Y::Null => return None,
        Y::Bool(b) => toml::Value::Boolean(*b),
        Y::Number(n) => match n.as_i64() {
            Some(i) => toml::Value::Integer(i),
            None => toml::Value::Float(n.as_f64()?),
        },
        Y::String(s) => toml::Value::String(s.clone()),
        Y::Sequence(seq) => toml::Value::Array(
            seq.iter()
                .map(|v| yaml_value_to_toml(v).unwrap_or(toml::Value::String(String::new())))
                .collect(),
        ),
        Y::Mapping(map) => {
            let mut out = toml::Table::new();
            for (key, item) in map {
                if let (Some(key), Some(item)) = (key.as_str(), yaml_value_to_toml(item)) {
                    out.insert(key.to_string(), item);
                }
            }
            toml::Value::Table(out)
        }
        Y::Tagged(tagged) => return yaml_value_to_toml(&tagged.value),
    })
}

/// True when [extract_accounts_block] answers from the pre-rename
/// `providers` key, which is when the Python loader logs its rename notice.
pub(crate) fn reads_legacy_block(data: &toml::Table) -> bool {
    let wrapper = data.get("config").and_then(toml::Value::as_table);
    let is_block = |key: &str| {
        data.get(key).is_some_and(toml::Value::is_table)
            || wrapper
                .and_then(|w| w.get(key))
                .is_some_and(toml::Value::is_table)
    };
    !is_block("accounts") && is_block("providers")
}

/// `loader._extract_accounts_block`: `accounts` then the pre-rename
/// `providers`, each at the top level then under a legacy `config:` wrapper.
pub(crate) fn extract_accounts_block(data: &toml::Table) -> Option<&toml::Table> {
    let wrapper = data.get("config").and_then(toml::Value::as_table);
    for key in ["accounts", "providers"] {
        if let Some(block) = data.get(key).and_then(toml::Value::as_table) {
            return Some(block);
        }
        if let Some(block) = wrapper
            .and_then(|w| w.get(key))
            .and_then(toml::Value::as_table)
        {
            return Some(block);
        }
    }
    None
}

/// `loader._extract_agents_block`: flat `agents`, else `config.agents`.
fn extract_agents_block(data: &toml::Table) -> Option<&toml::Table> {
    if let Some(agents) = data.get("agents") {
        if let Some(t) = agents.as_table() {
            return Some(t);
        }
    }
    data.get("config")
        .and_then(toml::Value::as_table)
        .and_then(|c| c.get("agents"))
        .and_then(toml::Value::as_table)
}

fn record_id(v: &toml::Value) -> Option<&str> {
    v.as_table()
        .and_then(|t| t.get("id"))
        .and_then(toml::Value::as_str)
}

/// `loader._overlay_records`: `incoming` wins per id, new ids append, and a
/// duplicate id WITHIN one file is preserved for the duplicate check.
fn overlay_records(accum: &mut Vec<toml::Value>, incoming: &[toml::Value]) {
    let mut seen: BTreeSet<String> = BTreeSet::new();
    for record in incoming {
        if let Some(rid) = record_id(record) {
            if !seen.insert(rid.to_string()) {
                accum.push(record.clone());
                continue;
            }
            accum.retain(|r| record_id(r) != Some(rid));
        }
        accum.push(record.clone());
    }
}

/// The Python truthiness of a block value.
fn truthy(v: &toml::Value) -> bool {
    match v {
        toml::Value::String(s) => !s.is_empty(),
        toml::Value::Integer(i) => *i != 0,
        toml::Value::Float(f) => *f != 0.0,
        toml::Value::Boolean(b) => *b,
        toml::Value::Datetime(_) => true,
        toml::Value::Array(a) => !a.is_empty(),
        toml::Value::Table(t) => !t.is_empty(),
    }
}

/// `loader._parse_providers_block`.
fn parse_block(block: &toml::Table, agents: Option<&toml::Table>) -> Result<Accounts, LoadError> {
    let raw_records: Vec<toml::Value> = match block.get("records") {
        Some(toml::Value::Array(a)) => a.clone(),
        // Iterating a non-list block value walks its keys or characters,
        // each a str, which the mapping check refuses.
        Some(v) if truthy(v) => vec![toml::Value::String(String::new())],
        _ => Vec::new(),
    };
    let mut records = Vec::with_capacity(raw_records.len());
    for raw in &raw_records {
        let toml::Value::Table(t) = raw else {
            return Err(LoadError::bare(format!(
                "provider record must be a mapping, got {}",
                py_type(raw)
            )));
        };
        let rid = t
            .get("id")
            .map(py_str)
            .unwrap_or_else(|| "<unknown>".to_string());
        match parse_record(t) {
            Ok(r) => records.push(r),
            Err(e) => {
                let mut headline = format!("invalid provider record '{rid}'");
                if e.auth_mismatch {
                    headline.push_str(": auth_strategy_mismatch");
                }
                return Err(LoadError {
                    headline,
                    detail: Some(e.detail),
                });
            }
        }
    }

    let invalid = |detail: String| LoadError {
        headline: "invalid providers config".to_string(),
        detail: Some(detail),
    };
    let active = match block.get("active") {
        None => None,
        Some(toml::Value::String(s)) => Some(s.clone()),
        Some(_) => return Err(invalid("active: Input should be a valid string".into())),
    };
    let auto_switch = match block.get("auto_switch") {
        None => false,
        Some(v) => lax_bool(v)
            .ok_or_else(|| invalid("auto_switch: Input should be a valid boolean".into()))?,
    };
    let mut seen = BTreeSet::new();
    let mut duplicates = BTreeSet::new();
    for r in &records {
        if !seen.insert(r.id.as_str()) {
            duplicates.insert(r.id.as_str());
        }
    }
    if !duplicates.is_empty() {
        let dups: Vec<&str> = duplicates.into_iter().collect();
        return Err(LoadError {
            headline: "invalid providers config: duplicate_record_ids".to_string(),
            detail: Some(format!("duplicate_record_ids: {}", py_list_repr(&dups))),
        });
    }
    if let Some(active) = &active {
        if !records.iter().any(|r| r.id == *active) {
            return Err(LoadError::bare(format!(
                "active_record_not_found: '{active}' is not in records"
            )));
        }
    }

    let mut pins = BTreeMap::new();
    for (agent, raw) in agents.into_iter().flatten() {
        if AGENTS_RESERVED_KEYS.contains(&agent.as_str()) {
            continue;
        }
        let Some(binding) = raw.as_table().filter(|t| t.contains_key("provider")) else {
            continue;
        };
        let bad = |why: &str| LoadError {
            headline: format!("invalid agent binding for '{agent}'"),
            detail: Some(why.to_string()),
        };
        if binding.keys().any(|k| k != "provider") {
            return Err(bad("Extra inputs are not permitted"));
        }
        let provider = match binding.get("provider") {
            Some(toml::Value::String(p)) if valid_record_id(p) => p.clone(),
            Some(toml::Value::String(_)) => {
                return Err(bad(
                    "provider: String should match pattern '^[a-z][a-z0-9-]{0,63}$'",
                ))
            }
            _ => return Err(bad("provider: Input should be a valid string")),
        };
        // A dangling pin refuses the whole load (the Python posture, kept by
        // ruling over the warn-only verify_evidence read).
        if !records.iter().any(|r| r.id == provider) {
            return Err(LoadError::bare(format!(
                "agent '{agent}' references unknown provider id '{provider}'"
            )));
        }
        pins.insert(agent.clone(), provider);
    }

    Ok(Accounts {
        records,
        active,
        auto_switch,
        agents: pins,
    })
}

/// `loader.load_providers`: every layer's accounts block merged, the local
/// layer winning per key and records overlaid by id; agent pins merged the
/// same way. An empty `records` list in a layer declares nothing.
pub fn load(repo_root: Option<&Path>, cwd: &Path) -> Result<Accounts, LoadError> {
    let mut merged = toml::Table::new();
    let mut merged_agents = toml::Table::new();
    let mut found_any = false;
    for path in candidates(repo_root, cwd).iter().rev() {
        let data = read_layer(path);
        if let Some(block) = extract_accounts_block(&data) {
            found_any = true;
            for (k, v) in block {
                let records = (k == "records").then(|| v.as_array()).flatten();
                if let Some(items) = records {
                    // An empty local list declares nothing; it never erases
                    // the global records.
                    if !items.is_empty() {
                        let mut acc = match merged.remove("records") {
                            Some(toml::Value::Array(acc)) => acc,
                            _ => Vec::new(),
                        };
                        overlay_records(&mut acc, items);
                        merged.insert(k.clone(), toml::Value::Array(acc));
                    }
                    continue;
                }
                merged.insert(k.clone(), v.clone());
            }
        }
        if let Some(agents) = extract_agents_block(&data) {
            for (k, v) in agents {
                merged_agents.insert(k.clone(), v.clone());
            }
        }
    }
    if !found_any || merged.is_empty() {
        return Ok(Accounts::default());
    }
    let agents = (!merged_agents.is_empty()).then_some(&merged_agents);
    parse_block(&merged, agents)
}

/// `loader.known_account_ids`: every record id declared in any layer.
pub fn known_account_ids(repo_root: Option<&Path>, cwd: &Path) -> BTreeSet<String> {
    let mut ids = BTreeSet::new();
    for path in candidates(repo_root, cwd) {
        let data = read_layer(&path);
        let Some(records) = extract_accounts_block(&data)
            .and_then(|b| b.get("records"))
            .and_then(toml::Value::as_array)
        else {
            continue;
        };
        ids.extend(records.iter().filter_map(record_id).map(str::to_string));
    }
    ids
}

/// `loader.record_harness`: the harness `record_id` runs on, `None` when
/// there is no such record or the config does not load.
pub fn record_harness(record_id: &str, repo_root: Option<&Path>, cwd: &Path) -> Option<String> {
    let accounts = load(repo_root, cwd).ok()?;
    let harness = accounts.by_id(record_id)?.harness.trim().to_string();
    (!harness.is_empty()).then_some(harness)
}

/// `loader._active_id_for`: the id in force on `record`'s lane. A managed
/// record answers from its slot stamp (failing open to `None`); any other
/// answers the routing-active pointer.
fn active_id_for(record: &Record, accounts: &Accounts, cwd: &Path) -> Option<String> {
    if record.auth == "managed" {
        return super::slot::active_slot_id(&record.harness, None, cwd)
            .ok()
            .flatten();
    }
    accounts.active.clone()
}

/// `loader.is_effective_active`.
pub fn is_effective_active(record: &Record, accounts: &Accounts, cwd: &Path) -> bool {
    active_id_for(record, accounts, cwd).as_deref() == Some(record.id.as_str())
}

/// `loader.effective_active`: the record id a default dispatch runs on.
pub fn effective_active(accounts: &Accounts, cwd: &Path) -> Option<String> {
    let active = accounts.active.as_ref()?;
    match accounts.by_id(active) {
        Some(record) => active_id_for(record, accounts, cwd),
        None => Some(active.clone()),
    }
}

/// `loader.load_combos`: combos merged across layers (local wins per name),
/// each validated against the declared record ids.
pub fn load_combos(repo_root: Option<&Path>, cwd: &Path) -> Result<Vec<Combo>, LoadError> {
    let mut merged: Vec<(String, toml::Value)> = Vec::new();
    let mut merged_records: Vec<toml::Value> = Vec::new();
    let mut malformed_top: Option<(PathBuf, &'static str)> = None;
    for path in candidates(repo_root, cwd).iter().rev() {
        let data = read_layer(path);
        let Some(block) = extract_accounts_block(&data) else {
            continue;
        };
        if let Some(combos) = block.get("combos") {
            match combos {
                toml::Value::Table(t) => {
                    for (name, spec) in t {
                        match merged.iter_mut().find(|(n, _)| n == name) {
                            Some(slot) => slot.1 = spec.clone(),
                            None => merged.push((name.clone(), spec.clone())),
                        }
                    }
                    malformed_top = None;
                }
                other => malformed_top = Some((path.clone(), py_type(other))),
            }
        }
        if let Some(items) = block.get("records").and_then(toml::Value::as_array) {
            if !items.is_empty() {
                overlay_records(&mut merged_records, items);
            }
        }
    }
    if let Some((path, kind)) = malformed_top {
        return Err(LoadError::bare(format!(
            "config.providers.combos must be a mapping of name -> spec, got {kind} ({})",
            path.display()
        )));
    }
    let known: BTreeSet<&str> = merged_records.iter().filter_map(record_id).collect();
    let mut out = Vec::with_capacity(merged.len());
    for (name, spec) in &merged {
        let quoted = py_repr_str(name);
        let toml::Value::Table(spec) = spec else {
            return Err(LoadError::bare(format!(
                "combo {quoted} spec must be a mapping, got {}",
                py_type(spec)
            )));
        };
        let providers_raw = match spec.get("providers") {
            None => Vec::new(),
            Some(toml::Value::Array(a)) => a.clone(),
            Some(other) => {
                return Err(LoadError::bare(format!(
                    "combo {quoted} providers must be a list, got {}",
                    py_type(other)
                )))
            }
        };
        let mut providers = Vec::with_capacity(providers_raw.len());
        for pid in &providers_raw {
            match pid.as_str().filter(|p| known.contains(p)) {
                Some(p) => providers.push(p.to_string()),
                None => {
                    return Err(LoadError::bare(format!(
                        "combo {quoted} references unknown provider id {} \
                         (not in config.providers.records)",
                        py_repr(pid)
                    )))
                }
            }
        }
        let sticky_limit = match spec.get("sticky_limit") {
            None => 1,
            Some(toml::Value::Float(f)) => f.trunc() as i64,
            Some(toml::Value::String(s)) => s.trim().parse::<i64>().map_err(|_| {
                LoadError::bare(format!(
                    "invalid literal for int() with base 10: {}",
                    py_repr_str(s)
                ))
            })?,
            Some(v) => lax_int(v).ok_or_else(|| {
                LoadError::bare(format!("combo {quoted} sticky_limit must be an integer"))
            })?,
        };
        if providers.is_empty() {
            return Err(LoadError::bare(format!(
                "Combo {quoted} has empty providers list (combos must name at least one provider)"
            )));
        }
        let strategy = match spec.get("strategy") {
            None => "fallback".to_string(),
            Some(toml::Value::String(s)) if VALID_STRATEGIES.contains(&s.as_str()) => s.clone(),
            Some(other) => {
                return Err(LoadError::bare(format!(
                    "Combo {quoted} has invalid strategy {}; valid: ('fallback', 'round_robin')",
                    py_repr(other)
                )))
            }
        };
        out.push(Combo {
            name: name.clone(),
            strategy,
            sticky_limit: sticky_limit.max(1),
            providers,
        });
    }
    Ok(out)
}

/// `loader.load_active_combo`: the first layer naming one wins.
pub fn load_active_combo(repo_root: Option<&Path>, cwd: &Path) -> Option<String> {
    candidates(repo_root, cwd).iter().find_map(|path| {
        let data = read_layer(path);
        extract_accounts_block(&data)?
            .get("active_combo")
            .and_then(toml::Value::as_str)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    })
}

/// The `accounts.quota` leaves merged across layers, local winning per
/// leaf: the input `load_quota_config` validates leaf by leaf.
pub fn quota_block(repo_root: Option<&Path>, cwd: &Path) -> toml::Table {
    let mut merged = toml::Table::new();
    for path in candidates(repo_root, cwd).iter().rev() {
        let data = read_layer(path);
        let Some(quota) = extract_accounts_block(&data)
            .and_then(|b| b.get("quota"))
            .and_then(toml::Value::as_table)
        else {
            continue;
        };
        for (k, v) in quota {
            merged.insert(k.clone(), v.clone());
        }
    }
    merged
}

/// Which one file `load_scope` reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    Project,
    Global,
}

/// The one file a scope reads and writes: `$PWD/.fno/config.toml` for the
/// project, the global config.toml otherwise. `None` when no home resolves.
pub fn scope_path(scope: Scope, cwd: &Path) -> Option<PathBuf> {
    match scope {
        Scope::Project => Some(pwd(cwd).join(".fno/config.toml")),
        Scope::Global => crate::agents_config::global_config_path(),
    }
}

/// `loader.load_scope_config`: the accounts block as stored in ONE scope's
/// own file. A string `active` is kept unvalidated, because a project may
/// point at a global record.
pub fn load_scope(scope: Scope, cwd: &Path) -> Result<Accounts, LoadError> {
    let Some(target) = scope_path(scope, cwd) else {
        return Ok(Accounts::default());
    };
    let data = read_layer(&target);
    let Some(block) = extract_accounts_block(&data).filter(|b| !b.is_empty()) else {
        return Ok(Accounts::default());
    };
    if let Some(toml::Value::String(active)) = block.get("active") {
        let mut rest = block.clone();
        rest.remove("active");
        let parsed = parse_block(&rest, None)?;
        return Ok(Accounts {
            records: parsed.records,
            active: Some(active.clone()),
            ..Accounts::default()
        });
    }
    parse_block(block, None)
}
