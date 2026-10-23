//! The provider runtime-state file: provider health (locks and backoff),
//! usage snapshots, combo cursors and window opens, the reads the dispatcher
//! asks of them (cooldown, usage, headroom, the projected window close, the
//! round-robin cursor) and the one writer of the file. Ported from
//! `fno.adapters.providers.runtime_state`, whose writers are now transports
//! over [run]; the parity cases live in `tests/accounts_state_read_parity.rs`
//! and `tests/accounts_state_write_parity.rs`.
//!
//! The file is where `provider_cap::runtime_state_path` resolves it. Every
//! parse drops a malformed entry and keeps the rest, as Python does: one
//! corrupt record never costs the whole file. Values coerce the way Python's
//! `float()`, `int()` and `str()` do, so a hand-edited `"50"` reads as 50.

use crate::fallback_chain::{self, HeadroomVerdict};
use serde_json::{json, Map, Value};
use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

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

// The write side, the one writer of the file. Every write takes the
// `<file>.update.lock` flock (Python's `filelock` takes the same lock),
// re-reads under it and replaces the file atomically. The serialized form is
// Python's `json.dumps(asdict, indent=2, sort_keys=True)`: each entry carries
// its own key (`provider_id`, `combo_name`).

const BASE_BACKOFF_MS: i64 = 2_000;
const MAX_BACKOFF_MS: i64 = 5 * 60 * 1000;
/// How long a writer waits for the update lock before its timeout branch.
pub const LOCK_TIMEOUT: Duration = Duration::from_secs(5);

/// The part of a classified error rule the health write reads: a backoff
/// rule ramps the level, a fixed rule holds it and cools for `cooldown_ms`.
#[derive(Clone, Copy, Debug)]
pub struct Rule {
    pub backoff: bool,
    pub cooldown_ms: Option<i64>,
}

/// The update lock, held until the returned file drops. None when another
/// writer held it past `timeout`.
pub fn lock(path: &Path, timeout: Duration) -> io::Result<Option<File>> {
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        fs::create_dir_all(parent)?;
    }
    let mut lock_path = path.as_os_str().to_os_string();
    lock_path.push(".update.lock");
    let file = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(lock_path)?;
    let deadline = Instant::now() + timeout;
    loop {
        // SAFETY: flock on a descriptor this function owns.
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
            return Ok(Some(file));
        }
        if Instant::now() >= deadline {
            return Ok(None);
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// A temp file beside the target, then one rename: a reader sees the old
/// file or the new one, never half of either. The caller holds the lock.
pub fn write_atomic(path: &Path, content: &str) -> io::Result<()> {
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    fs::create_dir_all(parent)?;
    let name = path.file_name().unwrap_or_default().to_string_lossy();
    let mut tmp = tempfile::Builder::new()
        .prefix(&format!(".{name}."))
        .suffix(".tmp")
        .tempfile_in(parent)?;
    tmp.write_all(content.as_bytes())?;
    tmp.persist(path).map_err(|e| e.error)?;
    Ok(())
}

fn sort_keys(v: Value) -> Value {
    match v {
        Value::Object(m) => {
            let mut entries: Vec<(String, Value)> = m.into_iter().collect();
            entries.sort_by(|a, b| a.0.cmp(&b.0));
            Value::Object(
                entries
                    .into_iter()
                    .map(|(k, v)| (k, sort_keys(v)))
                    .collect(),
            )
        }
        Value::Array(a) => Value::Array(a.into_iter().map(sort_keys).collect()),
        other => other,
    }
}

/// `value` with its own key added, as `dataclasses.asdict` writes an entry.
pub fn keyed(mut value: Value, field: &str, id: &str) -> Value {
    if let Some(m) = value.as_object_mut() {
        m.insert(field.to_string(), json!(id));
    }
    value
}

/// `_serialize_state`.
pub fn serialize(state: &State) -> String {
    let mut v = state.to_json();
    for (block, field) in [
        ("provider_health", "provider_id"),
        ("usage", "provider_id"),
        ("combo_cursors", "combo_name"),
    ] {
        if let Some(m) = v.get_mut(block).and_then(Value::as_object_mut) {
            for (id, entry) in m.iter_mut() {
                *entry = keyed(entry.take(), field, id);
            }
        }
    }
    serde_json::to_string_pretty(&sort_keys(v)).unwrap_or_default()
}

fn schema_of(raw: &Value) -> i64 {
    raw.get("schema_version")
        .and_then(py_int)
        .unwrap_or(SCHEMA_VERSION)
}

/// The file as a writer rewrites it: stale health and stale cursors dropped.
fn rewritable(raw: Option<&Value>, now: f64) -> State {
    let empty = json!({});
    let raw = raw.unwrap_or(&empty);
    let cursor_cutoff = now - COMBO_CURSOR_TTL_SECONDS;
    State {
        provider_health: drop_stale(parse_health(raw), now),
        combo_cursors: parse_cursors(raw)
            .into_iter()
            .filter(|(_, c)| c.last_rotated_at >= cursor_cutoff)
            .collect(),
        usage: parse_usage(raw),
        windows_opened: parse_windows_opened(raw),
        schema_version: schema_of(raw),
    }
}

/// One read-modify-write under the lock; None when the lock timed out.
fn locked<T>(
    path: &Path,
    timeout: Duration,
    f: impl FnOnce(Option<Value>) -> io::Result<T>,
) -> io::Result<Option<T>> {
    let Some(_guard) = lock(path, timeout)? else {
        return Ok(None);
    };
    f(read_raw(path)).map(Some)
}

fn cooldown_ms(level: i64) -> i64 {
    (BASE_BACKOFF_MS << level.clamp(0, MAX_BACKOFF_LEVEL)).min(MAX_BACKOFF_MS)
}

/// `_next_health`: the cooldown comes from the OLD level, and a reset the
/// provider named in the future beats the guessed cooldown, so a stale stamp
/// never shortens a lock.
fn next_health(
    current: &Health,
    rule: Rule,
    now: f64,
    model: Option<&str>,
    resets_at: Option<f64>,
) -> Health {
    let (level, ms) = if rule.backoff {
        (
            (current.backoff_level + 1).min(MAX_BACKOFF_LEVEL),
            cooldown_ms(current.backoff_level),
        )
    } else {
        (current.backoff_level, rule.cooldown_ms.unwrap_or(0))
    };
    let until = now + ms as f64 / 1000.0;
    let harvested = resets_at.filter(|r| *r > now);
    let mut next = Health {
        backoff_level: level,
        rate_limited_until: Some(harvested.unwrap_or(until)),
        last_error_at: Some(now),
        model_locks: current.model_locks.clone(),
    };
    if let Some(model) = model {
        next.model_locks.insert(model.to_string(), until);
        next.rate_limited_until = harvested.or(current.rate_limited_until);
    }
    next
}

/// `update_provider_health`. `span` is the record's window length: a
/// harvested reset then stamps the window it closed, keeping `warned` while
/// it is the same window. On lock timeout nothing is written and the last
/// known entry (or a zero one) comes back.
#[allow(clippy::too_many_arguments)]
pub fn update_health(
    path: &Path,
    provider_id: &str,
    rule: Rule,
    model: Option<&str>,
    resets_at: Option<f64>,
    span: Option<f64>,
    now: f64,
    timeout: Duration,
) -> io::Result<Health> {
    let written = locked(path, timeout, |raw| {
        let mut state = rewritable(raw.as_ref(), now);
        let current = state
            .provider_health
            .get(provider_id)
            .cloned()
            .unwrap_or_default();
        let next = next_health(&current, rule, now, model, resets_at);
        state
            .provider_health
            .insert(provider_id.to_string(), next.clone());
        if let (Some(reset), Some(span)) =
            (resets_at.filter(|r| *r > now), span.filter(|s| *s != 0.0))
        {
            let labels = state
                .windows_opened
                .entry(provider_id.to_string())
                .or_default();
            let opened_at = reset - span;
            let warned = labels
                .get(WINDOW_LABEL)
                .is_some_and(|prior| (prior.opened_at - opened_at).abs() < 1.0 && prior.warned);
            labels.insert(WINDOW_LABEL.to_string(), WindowOpen { opened_at, warned });
        }
        write_atomic(path, &serialize(&state))?;
        Ok(next)
    })?;
    Ok(written.unwrap_or_else(|| {
        read_raw(path)
            .and_then(|raw| parse_health(&raw).remove(provider_id))
            .unwrap_or_default()
    }))
}

/// `write_usage_snapshot`: true when the snapshot landed, false when the
/// lock timed out (the stale snapshot then decays to UNKNOWN).
pub fn write_usage(
    path: &Path,
    provider_id: &str,
    snapshot: Snapshot,
    now: f64,
    timeout: Duration,
) -> io::Result<bool> {
    let written = locked(path, timeout, |raw| {
        let mut state = rewritable(raw.as_ref(), now);
        state.usage.insert(provider_id.to_string(), snapshot);
        write_atomic(path, &serialize(&state))
    })?;
    Ok(written.is_some())
}

/// `_next_cursor`: a fresh cursor (none, edited combo, stale) starts at
/// (0, 1); under the sticky limit it stays and counts; at the limit it moves
/// to the next index with a count of 1.
fn next_cursor(prev: Option<&Cursor>, sticky: i64, hash: &str, count: i64, now: f64) -> Cursor {
    let start = |index| Cursor {
        cursor_index: index,
        consecutive_use_count: 1,
        providers_hash: hash.to_string(),
        last_rotated_at: now,
    };
    match prev
        .filter(|p| p.providers_hash == hash && p.last_rotated_at >= now - COMBO_CURSOR_TTL_SECONDS)
    {
        None => start(0),
        Some(p) if p.consecutive_use_count < sticky => Cursor {
            consecutive_use_count: p.consecutive_use_count + 1,
            ..start(p.cursor_index)
        },
        Some(p) => start((p.cursor_index + 1).rem_euclid(count)),
    }
}

/// `advance_cursor`: always writes schema 2. On lock timeout it computes
/// from a lock-free read, which may duplicate a rotation across racers.
pub fn advance_cursor(
    path: &Path,
    combo: &str,
    sticky: i64,
    hash: &str,
    count: i64,
    now: f64,
    timeout: Duration,
) -> io::Result<Cursor> {
    let written = locked(path, timeout, |raw| {
        let mut state = rewritable(raw.as_ref(), now);
        let next = next_cursor(state.combo_cursors.get(combo), sticky, hash, count, now);
        state.combo_cursors.insert(combo.to_string(), next.clone());
        state.schema_version = SCHEMA_VERSION;
        write_atomic(path, &serialize(&state))?;
        Ok(next)
    })?;
    Ok(written.unwrap_or_else(|| {
        next_cursor(
            read_cursor(path, combo, hash, now).as_ref(),
            sticky,
            hash,
            count,
            now,
        )
    }))
}

/// `stamp_window_open`: true when written. A stamp inside a recorded window
/// still in force is a no-op, so a repeated dispatch never resets it.
pub fn stamp_window(
    path: &Path,
    provider_id: &str,
    opened_at: f64,
    label: &str,
    span: Option<f64>,
    now: f64,
    timeout: Duration,
) -> io::Result<bool> {
    let written = locked(path, timeout, |raw| {
        let mut state = rewritable(raw.as_ref(), now);
        let prior = state
            .windows_opened
            .get(provider_id)
            .and_then(|labels| labels.get(label));
        if let (Some(prior), Some(span)) = (prior, span.filter(|s| *s != 0.0)) {
            if prior.opened_at + span > opened_at {
                return Ok(false);
            }
        }
        state
            .windows_opened
            .entry(provider_id.to_string())
            .or_default()
            .insert(
                label.to_string(),
                WindowOpen {
                    opened_at,
                    warned: false,
                },
            );
        write_atomic(path, &serialize(&state))?;
        Ok(true)
    })?;
    Ok(written.unwrap_or(false))
}

/// `mark_window_warned`: true only when this call set the flag.
pub fn mark_warned(
    path: &Path,
    provider_id: &str,
    now: f64,
    timeout: Duration,
) -> io::Result<bool> {
    let written = locked(path, timeout, |raw| {
        let mut state = rewritable(raw.as_ref(), now);
        let Some(entry) = state
            .windows_opened
            .get_mut(provider_id)
            .and_then(|labels| labels.get_mut(WINDOW_LABEL))
            .filter(|e| !e.warned)
        else {
            return Ok(false);
        };
        entry.warned = true;
        write_atomic(path, &serialize(&state))?;
        Ok(true)
    })?;
    Ok(written.unwrap_or(false))
}

fn modified(path: &Path) -> io::Result<std::time::SystemTime> {
    fs::metadata(path)?.modified()
}

/// `Path.resolve()`: an existing parent canonicalizes even when the file
/// itself is absent.
fn resolved(path: &Path) -> PathBuf {
    fs::canonicalize(path).unwrap_or_else(|_| {
        match (
            path.parent().and_then(|p| fs::canonicalize(p).ok()),
            path.file_name(),
        ) {
            (Some(parent), Some(name)) => parent.join(name),
            _ => path.to_path_buf(),
        }
    })
}

/// `_fold_one_legacy_file`: merge a legacy per-cwd file into `target` when
/// it is newer, per key by the newest inner timestamp. The legacy file stays
/// in place, so a rolled-back binary loses nothing.
pub fn fold_legacy(target: &Path, legacy: &Path, now: f64, timeout: Duration) -> io::Result<()> {
    if !legacy.is_file() || resolved(legacy) == resolved(target) {
        return Ok(());
    }
    let current =
        || -> io::Result<bool> { Ok(target.is_file() && modified(target)? >= modified(legacy)?) };
    if current()? {
        return Ok(());
    }
    let Some(_guard) = lock(target, timeout)? else {
        return Ok(());
    };
    // A racing process may have folded already.
    if current()? {
        return Ok(());
    }
    let dst = read_raw(target).unwrap_or_else(|| json!({}));
    let src = read_raw(legacy).unwrap_or_else(|| json!({}));
    let ts = |v: Option<f64>| v.unwrap_or(0.0);
    let mut health = parse_health(&dst);
    for (id, entry) in parse_health(&src) {
        if health.get(&id).map_or(true, |prior| {
            ts(entry.last_error_at) > ts(prior.last_error_at)
        }) {
            health.insert(id, entry);
        }
    }
    let mut usage = parse_usage(&dst);
    for (id, snap) in parse_usage(&src) {
        if usage
            .get(&id)
            .map_or(true, |prior| snap.probed_at > prior.probed_at)
        {
            usage.insert(id, snap);
        }
    }
    let mut cursors = parse_cursors(&dst);
    for (name, cur) in parse_cursors(&src) {
        if cursors
            .get(&name)
            .map_or(true, |prior| cur.last_rotated_at > prior.last_rotated_at)
        {
            cursors.insert(name, cur);
        }
    }
    let mut windows = parse_windows_opened(&dst);
    for (id, labels) in parse_windows_opened(&src) {
        let slot = windows.entry(id).or_default();
        for (label, open) in labels {
            if slot
                .get(&label)
                .map_or(true, |prior| open.opened_at > prior.opened_at)
            {
                slot.insert(label, open);
            }
        }
    }
    let state = State {
        provider_health: drop_stale(health, now),
        combo_cursors: cursors,
        usage,
        windows_opened: windows,
        schema_version: schema_of(&dst),
    };
    write_atomic(target, &serialize(&state))
}

fn wall_now() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0.0, |d| d.as_secs_f64())
}

const STATE_USAGE: &str = "usage: fno-agents spawn-overlay accounts state <update-health|write-usage|advance-cursor|stamp-window|mark-warned>  (a JSON payload on stdin)";

/// `accounts state <op>`: the door the Python writers call. The payload is
/// one JSON object on stdin, the answer one JSON object on stdout.
pub fn run(args: &[String]) -> i32 {
    let Some(op) = args.first() else {
        eprintln!("{STATE_USAGE}");
        return 2;
    };
    if matches!(op.as_str(), "-h" | "--help") {
        println!("{STATE_USAGE}");
        return 0;
    }
    let mut input = String::new();
    if let Err(error) = io::stdin().read_to_string(&mut input) {
        eprintln!("accounts state: stdin unreadable: {error}");
        return 2;
    }
    let payload: Value = if input.trim().is_empty() {
        json!({})
    } else {
        match serde_json::from_str(&input) {
            Ok(v @ Value::Object(_)) => v,
            _ => {
                eprintln!("accounts state: the payload is not a JSON object");
                return 2;
            }
        }
    };
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let path = crate::provider_cap::runtime_state_path(&cwd);
    // The env override names one explicit file, so nothing folds into it.
    if std::env::var_os("FNO_RUNTIME_STATE_PATH").is_none() {
        let legacy = Path::new(".fno").join("provider-runtime-state.json");
        let repo_root = payload
            .get("repo_root")
            .and_then(Value::as_str)
            .map(Path::new);
        for root in std::iter::once(cwd.as_path()).chain(repo_root) {
            if let Err(error) = fold_legacy(&path, &root.join(&legacy), wall_now(), LOCK_TIMEOUT) {
                eprintln!(
                    "runtime_state: legacy per-cwd fold skipped for {}: {error}",
                    root.display()
                );
            }
        }
    }
    let now = payload
        .get("now")
        .and_then(py_float)
        .unwrap_or_else(wall_now);
    let text = |key: &str| payload.get(key).and_then(Value::as_str);
    let int = |key: &str| payload.get(key).and_then(py_int);
    let id = text("provider_id").unwrap_or_default();
    let answer = match op.as_str() {
        "update-health" => {
            let rule = Rule {
                backoff: payload.get("backoff").is_some_and(py_truthy),
                cooldown_ms: int("cooldown_ms"),
            };
            let resets_at = payload.get("resets_at").and_then(py_float);
            let span = resets_at
                .filter(|r| *r > now)
                .and_then(|_| record_window_seconds(&cwd, id));
            update_health(
                &path,
                id,
                rule,
                text("model"),
                resets_at,
                span,
                now,
                LOCK_TIMEOUT,
            )
            .map(|h| keyed(h.to_json(), "provider_id", id))
        }
        "write-usage" => {
            let Some(snapshot) = payload.get("snapshot").and_then(parse_snapshot) else {
                eprintln!("accounts state write-usage: the snapshot is malformed");
                return 2;
            };
            write_usage(&path, id, snapshot, now, LOCK_TIMEOUT).map(|ok| json!({"persisted": ok}))
        }
        "advance-cursor" => {
            let count = int("providers_count").unwrap_or(0);
            if count < 1 {
                eprintln!("advance_cursor: providers_count must be >= 1, got {count}");
                return 2;
            }
            let combo = text("combo_name").unwrap_or_default();
            let hash = text("providers_hash").unwrap_or_default();
            let sticky = int("sticky_limit").unwrap_or(1);
            advance_cursor(&path, combo, sticky, hash, count, now, LOCK_TIMEOUT)
                .map(|c| keyed(c.to_json(), "combo_name", combo))
        }
        "stamp-window" => {
            let span = record_window_seconds(&cwd, id);
            let label = text("label").unwrap_or(WINDOW_LABEL);
            let opened_at = payload.get("opened_at").and_then(py_float).unwrap_or(now);
            stamp_window(&path, id, opened_at, label, span, now, LOCK_TIMEOUT)
                .map(|ok| json!({"written": ok}))
        }
        "mark-warned" => mark_warned(&path, id, now, LOCK_TIMEOUT).map(|ok| json!({"set": ok})),
        _ => {
            eprintln!("{STATE_USAGE}");
            return 2;
        }
    };
    match answer {
        Ok(v) => {
            println!("{v}");
            0
        }
        Err(error) => {
            eprintln!("accounts state {op}: {error} at {}", path.display());
            1
        }
    }
}
