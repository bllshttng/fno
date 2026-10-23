//! The provider runtime-state file, read side: provider health (locks and
//! backoff), usage snapshots, combo cursors and window opens, and the reads
//! the dispatcher asks of them (cooldown, usage, headroom, the projected
//! window close, the round-robin cursor). Ported from the read side of
//! `fno.adapters.providers.runtime_state`; the parity cases live in
//! `tests/accounts_state_read_parity.rs`.
//!
//! The file is where `provider_cap::runtime_state_path` resolves it. Every
//! parse drops a malformed entry and keeps the rest, as Python does: one
//! corrupt record never costs the whole file. Values coerce the way Python's
//! `float()`, `int()` and `str()` do, so a hand-edited `"50"` reads as 50.

use crate::fallback_chain::{self, HeadroomVerdict};
use serde_json::{json, Map, Value};
use std::collections::BTreeMap;
use std::path::Path;

/// A reactive health observation older than this drops, locks included.
pub const PROVIDER_HEALTH_TTL_SECONDS: f64 = 60.0 * 60.0;
/// A combo cursor not rotated for this long is stale.
pub const COMBO_CURSOR_TTL_SECONDS: f64 = 24.0 * 60.0 * 60.0;
/// A usage snapshot older than this reads as absent (`quota.probe_ttl_seconds`).
pub const DEFAULT_USAGE_TTL_SECONDS: f64 = 300.0;
/// A fresh window at or above this share of the quota reads LOW.
pub const DEFAULT_THRESHOLD_PCT: f64 = 90.0;
const MAX_BACKOFF_LEVEL: i64 = 15;
const SCHEMA_VERSION: i64 = 2;
const WINDOW_LABEL: &str = "window";

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Health {
    pub backoff_level: i64,
    pub rate_limited_until: Option<f64>,
    pub last_error_at: Option<f64>,
    /// Model id to cooldown expiry: a lock on one model of the record.
    pub model_locks: BTreeMap<String, f64>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Window {
    pub label: String,
    pub used_pct: f64,
    /// None binds on percentage alone: a reset-less window never "already reset".
    pub resets_at: Option<f64>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Snapshot {
    pub windows: Vec<Window>,
    pub probed_at: f64,
    pub source: String,
    /// A response with a rejected row: it floors headroom at LOW.
    pub partial: bool,
    pub confidence: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Cursor {
    pub cursor_index: i64,
    pub consecutive_use_count: i64,
    pub providers_hash: String,
    pub last_rotated_at: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct WindowOpen {
    pub opened_at: f64,
    pub warned: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct State {
    pub provider_health: BTreeMap<String, Health>,
    pub combo_cursors: BTreeMap<String, Cursor>,
    pub usage: BTreeMap<String, Snapshot>,
    pub windows_opened: BTreeMap<String, BTreeMap<String, WindowOpen>>,
    pub schema_version: i64,
}

/// Where a provider's rolling window stands, projected from a recorded open
/// plus the record's `window_minutes`. A projection, never a measurement:
/// `closes_in_s` is None when nothing is recorded or the window rolled.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct WindowProjection {
    pub opened_at: Option<f64>,
    pub closes_at: Option<f64>,
    pub closes_in_s: Option<f64>,
    pub warned: bool,
}

/// Python `float(v)` for a JSON value: numbers, booleans and numeric strings.
fn py_float(v: &Value) -> Option<f64> {
    match v {
        Value::Number(n) => n.as_f64(),
        Value::Bool(b) => Some(if *b { 1.0 } else { 0.0 }),
        Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }
}

/// Python `int(v)`: a float truncates, a string must be an integer literal.
fn py_int(v: &Value) -> Option<i64> {
    match v {
        Value::Number(n) => n.as_i64().or_else(|| {
            n.as_f64()
                .filter(|f| f.is_finite())
                .map(|f| f.trunc() as i64)
        }),
        Value::Bool(b) => Some(i64::from(*b)),
        Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }
}

/// Python `str(v)` for the scalar shapes a state file holds.
fn py_str(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => Some(s.clone()),
        Value::Bool(b) => Some(if *b { "True" } else { "False" }.to_string()),
        Value::Null => Some("None".to_string()),
        Value::Number(n) => Some(match (n.as_i64(), n.as_f64()) {
            (Some(i), _) => i.to_string(),
            (None, Some(f)) if f.fract() == 0.0 && f.abs() < 1e16 => format!("{f:.1}"),
            _ => n.to_string(),
        }),
        _ => None,
    }
}

/// Python truthiness.
fn py_truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|f| f != 0.0),
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
    }
}

fn block<'a>(raw: &'a Value, key: &str) -> impl Iterator<Item = (&'a String, &'a Value)> {
    raw.get(key)
        .and_then(Value::as_object)
        .into_iter()
        .flatten()
}

/// `_read_disk_payload`: the file's top-level object, or None when it is
/// missing, empty, unparseable or not an object.
pub fn read_raw(path: &Path) -> Option<Value> {
    let text = std::fs::read_to_string(path).ok()?;
    if text.trim().is_empty() {
        return None;
    }
    serde_json::from_str::<Value>(&text)
        .ok()
        .filter(Value::is_object)
}

/// `_parse_state_payload`: backoff level clamped into range, model locks
/// that are not positive timestamps dropped one by one.
pub fn parse_health(raw: &Value) -> BTreeMap<String, Health> {
    let mut out = BTreeMap::new();
    for (pid, entry) in block(raw, "provider_health") {
        let Some(e) = entry.as_object() else {
            continue;
        };
        let opt = |key: &str| match e.get(key) {
            None | Some(Value::Null) => Some(None),
            Some(v) => py_float(v).map(Some),
        };
        let level = match e.get("backoff_level") {
            None => Some(0),
            Some(v) => py_int(v),
        };
        let (Some(level), Some(rate_limited_until), Some(last_error_at)) =
            (level, opt("rate_limited_until"), opt("last_error_at"))
        else {
            continue;
        };
        if pid.is_empty() {
            continue;
        }
        let model_locks = e
            .get("model_locks")
            .and_then(Value::as_object)
            .into_iter()
            .flatten()
            .filter(|(model, _)| !model.is_empty())
            .filter_map(|(model, ts)| Some((model.clone(), py_float(ts).filter(|t| *t > 0.0)?)))
            .collect();
        out.insert(
            pid.clone(),
            Health {
                backoff_level: level.clamp(0, MAX_BACKOFF_LEVEL),
                rate_limited_until,
                last_error_at,
                model_locks,
            },
        );
    }
    out
}

/// One usage row (`_parse_usage_payload`'s body). A row with any malformed
/// window drops whole; `used_pct` re-clamps into [0, 100].
pub fn parse_snapshot(entry: &Value) -> Option<Snapshot> {
    let e = entry.as_object()?;
    let mut windows = Vec::new();
    for w in e.get("windows")?.as_array()? {
        let w = w.as_object()?;
        windows.push(Window {
            label: py_str(w.get("label")?)?,
            used_pct: py_float(w.get("used_pct")?)?.clamp(0.0, 100.0),
            resets_at: match w.get("resets_at") {
                None | Some(Value::Null) => None,
                Some(v) => Some(py_float(v)?),
            },
        });
    }
    Some(Snapshot {
        windows,
        probed_at: py_float(e.get("probed_at")?)?,
        source: match e.get("source") {
            None => String::new(),
            Some(v) => py_str(v)?,
        },
        partial: e.get("partial").is_some_and(py_truthy),
        confidence: match e.get("confidence").filter(|v| py_truthy(v)) {
            None => "unknown".to_string(),
            Some(v) => py_str(v)?,
        },
    })
}

pub fn parse_usage(raw: &Value) -> BTreeMap<String, Snapshot> {
    block(raw, "usage")
        .filter_map(|(pid, entry)| Some((pid.clone(), parse_snapshot(entry)?)))
        .collect()
}

/// `_parse_cursors_payload`: an entry missing a field or failing the
/// cursor's own checks drops, so the caller starts fresh.
pub fn parse_cursors(raw: &Value) -> BTreeMap<String, Cursor> {
    let mut out = BTreeMap::new();
    for (name, entry) in block(raw, "combo_cursors") {
        let Some(e) = entry.as_object() else {
            continue;
        };
        let cursor = (|| {
            Some(Cursor {
                cursor_index: py_int(e.get("cursor_index")?)?,
                consecutive_use_count: py_int(e.get("consecutive_use_count")?)?,
                providers_hash: py_str(e.get("providers_hash")?)?,
                last_rotated_at: py_float(e.get("last_rotated_at")?)?,
            })
        })();
        if let Some(c) = cursor.filter(|c| {
            !name.is_empty()
                && c.cursor_index >= 0
                && c.consecutive_use_count >= 0
                && !c.providers_hash.is_empty()
        }) {
            out.insert(name.clone(), c);
        }
    }
    out
}

/// `_parse_windows_opened`: `{provider: {label: {opened_at, warned}}}`.
pub fn parse_windows_opened(raw: &Value) -> BTreeMap<String, BTreeMap<String, WindowOpen>> {
    let mut out = BTreeMap::new();
    for (pid, labels) in block(raw, "windows_opened") {
        let Some(labels) = labels.as_object() else {
            continue;
        };
        let kept: BTreeMap<String, WindowOpen> = labels
            .iter()
            .filter_map(|(label, entry)| {
                let e = entry.as_object()?;
                Some((
                    label.clone(),
                    WindowOpen {
                        opened_at: py_float(e.get("opened_at")?)?,
                        warned: e.get("warned").is_some_and(py_truthy),
                    },
                ))
            })
            .collect();
        if !kept.is_empty() {
            out.insert(pid.clone(), kept);
        }
    }
    out
}

/// `_drop_stale`: the health TTL is record-level, so an aged-out entry takes
/// its model locks and its provider lock with it.
pub fn drop_stale(health: BTreeMap<String, Health>, now: f64) -> BTreeMap<String, Health> {
    let cutoff = now - PROVIDER_HEALTH_TTL_SECONDS;
    health
        .into_iter()
        .filter(|(_, h)| h.last_error_at.map_or(true, |at| at >= cutoff))
        .collect()
}

/// `read_state`: the whole file with stale health and stale cursors dropped
/// in memory (only a locked write rewrites the disk).
pub fn read_state(path: &Path, now: f64) -> State {
    let Some(raw) = read_raw(path) else {
        return State {
            provider_health: BTreeMap::new(),
            combo_cursors: BTreeMap::new(),
            usage: BTreeMap::new(),
            windows_opened: BTreeMap::new(),
            schema_version: SCHEMA_VERSION,
        };
    };
    let cursor_cutoff = now - COMBO_CURSOR_TTL_SECONDS;
    State {
        provider_health: drop_stale(parse_health(&raw), now),
        combo_cursors: parse_cursors(&raw)
            .into_iter()
            .filter(|(_, c)| c.last_rotated_at >= cursor_cutoff)
            .collect(),
        usage: parse_usage(&raw),
        windows_opened: parse_windows_opened(&raw),
        schema_version: raw
            .get("schema_version")
            .and_then(py_int)
            .unwrap_or(SCHEMA_VERSION),
    }
}

fn health_of(raw: Option<&Value>, provider_id: &str, now: f64) -> Option<Health> {
    drop_stale(parse_health(raw?), now).remove(provider_id)
}

/// `is_in_cooldown`: a lock on `model` first, then the provider-level lock.
pub fn is_in_cooldown(path: &Path, provider_id: &str, model: Option<&str>, now: f64) -> bool {
    let Some(health) = health_of(read_raw(path).as_ref(), provider_id, now) else {
        return false;
    };
    model
        .and_then(|m| health.model_locks.get(m))
        .is_some_and(|until| *until > now)
        || health.rate_limited_until.is_some_and(|until| until > now)
}

/// `read_usage`: the cached snapshot, or None when absent or older than `ttl`.
pub fn read_usage(path: &Path, provider_id: &str, ttl: f64, now: f64) -> Option<Snapshot> {
    parse_usage(&read_raw(path)?)
        .remove(provider_id)
        .filter(|s| s.probed_at >= now - ttl)
}

/// `headroom`: the verdict from ONE read of the file, the lock and the
/// usage snapshot both taken from it. An unreadable file reads UNKNOWN.
pub fn headroom(
    path: &Path,
    provider_id: &str,
    now: f64,
    ttl: f64,
    threshold_pct: f64,
) -> HeadroomVerdict {
    let raw = read_raw(path);
    let health = health_of(raw.as_ref(), provider_id, now);
    let snap = raw.and_then(|r| parse_usage(&r).remove(provider_id));
    fallback_chain::headroom_at(health.as_ref(), snap.as_ref(), now, ttl, threshold_pct)
}

/// `project_window`: `span` is the record's window length in seconds
/// ([`record_window_seconds`]). Reads local files only.
pub fn project_window(
    path: &Path,
    provider_id: &str,
    span: Option<f64>,
    now: f64,
) -> WindowProjection {
    let Some(open) = read_raw(path)
        .map(|raw| parse_windows_opened(&raw))
        .and_then(|mut w| w.remove(provider_id)?.remove(WINDOW_LABEL))
    else {
        return WindowProjection::default();
    };
    let Some(span) = span.filter(|s| *s != 0.0) else {
        return WindowProjection {
            opened_at: Some(open.opened_at),
            ..WindowProjection::default()
        };
    };
    let closes_at = open.opened_at + span;
    if closes_at <= now {
        // The window rolled and nothing stamped the new one: a negative
        // countdown must never read as a positive one.
        return WindowProjection {
            opened_at: Some(open.opened_at),
            warned: open.warned,
            ..WindowProjection::default()
        };
    }
    WindowProjection {
        opened_at: Some(open.opened_at),
        closes_at: Some(closes_at),
        closes_in_s: Some(closes_at - now),
        warned: open.warned,
    }
}

/// `accounts.<id>.window_minutes` in seconds; None on a miss or a bad config.
pub fn record_window_seconds(cwd: &Path, provider_id: &str) -> Option<f64> {
    let accounts = super::records::load(None, cwd).ok()?;
    let minutes = accounts.by_id(provider_id)?.window_minutes?;
    (minutes != 0).then(|| minutes as f64 * 60.0)
}

/// `accounts.<id>.reset_timezone`, which only a naive reset stamp needs;
/// None on a miss or a bad config.
pub fn record_reset_timezone(cwd: &Path, provider_id: &str) -> Option<String> {
    let accounts = super::records::load(None, cwd).ok()?;
    accounts
        .by_id(provider_id)?
        .reset_timezone
        .clone()
        .filter(|tz| !tz.is_empty())
}

/// `read_cursor`: the combo's cursor unless it is absent, edited (hash
/// mismatch) or stale.
pub fn read_cursor(path: &Path, combo: &str, providers_hash: &str, now: f64) -> Option<Cursor> {
    parse_cursors(&read_raw(path)?)
        .remove(combo)
        .filter(|c| c.providers_hash == providers_hash)
        .filter(|c| c.last_rotated_at >= now - COMBO_CURSOR_TTL_SECONDS)
}

impl Health {
    pub fn to_json(&self) -> Value {
        json!({
            "backoff_level": self.backoff_level,
            "rate_limited_until": self.rate_limited_until,
            "last_error_at": self.last_error_at,
            "model_locks": self.model_locks,
        })
    }
}

impl Snapshot {
    pub fn to_json(&self) -> Value {
        json!({
            "windows": self.windows.iter().map(|w| json!({
                "label": w.label,
                "used_pct": w.used_pct,
                "resets_at": w.resets_at,
            })).collect::<Vec<_>>(),
            "probed_at": self.probed_at,
            "source": self.source,
            "partial": self.partial,
            "confidence": self.confidence,
        })
    }
}

impl Cursor {
    pub fn to_json(&self) -> Value {
        json!({
            "cursor_index": self.cursor_index,
            "consecutive_use_count": self.consecutive_use_count,
            "providers_hash": self.providers_hash,
            "last_rotated_at": self.last_rotated_at,
        })
    }
}

impl State {
    pub fn to_json(&self) -> Value {
        let map = |m: Map<String, Value>| Value::Object(m);
        json!({
            "provider_health": map(self.provider_health.iter().map(|(k, h)| (k.clone(), h.to_json())).collect()),
            "combo_cursors": map(self.combo_cursors.iter().map(|(k, c)| (k.clone(), c.to_json())).collect()),
            "usage": map(self.usage.iter().map(|(k, s)| (k.clone(), s.to_json())).collect()),
            "windows_opened": map(self.windows_opened.iter().map(|(pid, labels)| {
                (pid.clone(), map(labels.iter().map(|(l, w)| {
                    (l.clone(), json!({"opened_at": w.opened_at, "warned": w.warned}))
                }).collect()))
            }).collect()),
            "schema_version": self.schema_version,
        })
    }
}
