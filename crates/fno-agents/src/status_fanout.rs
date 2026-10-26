//! The status-sink fanout: the dumb dispatcher, ported from the deleted
//! Python leg `cli/src/fno/status_fanout.py`. Workers emit protocol-family
//! events once to the project journal and never know sinks exist; one tick
//! sweeps the committed rows and routes each to its configured sinks.
//!
//! The correctness spine survives the port verbatim (see the parity test
//! header for the goldens): a per-sink RFC3339 cursor beside the journal (a
//! byte offset dies at rotation), one shared pass from `min(cursors)`,
//! at-least-once delivery (a cursor advances only past a completed dispatch),
//! and a read-only stance toward the journal itself. A connect-class failure
//! short-circuits the sink's batch WITHOUT advancing, so it retries next
//! tick; drops and unknown dispatcher statuses advance with an errors row.
//!
//! The tick surface is `fno doctor event fanout tick [--dry-run|-N]`, claimed
//! lexically by the front door and exec'd here as the hidden `status-fanout`
//! op (matched with `==` beside `board`: not a routable `fno agents`
//! command, so no advertised surface grows). The deprecated root spelling
//! `fno status-fanout` prints its move line and lands here too.
//!
//! Channels: the native store read runs in-process beside [`crate::
//! event_store`], the machine note rides [`crate::backlog::note_history`]
//! in-process, the plan-doc append takes the shared sidecar lock through
//! [`crate::plan_doc`], and webhook delivery is one `curl` child per post
//! with the same secret shape [`crate::attention_http`] ships.

use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::io::BufRead;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::agents_config::{config_value_deep, state_dir};

/// Dispatch outcomes. A dispatch attempt classifies its result so the tick
/// knows whether to advance the cursor past the event (DELIVERED / DROPPED)
/// or hold it and retry next tick (SHORT_CIRCUIT - a connect-class failure).
pub const DELIVERED: &str = "delivered";
pub const DROPPED: &str = "dropped";
pub const SHORT_CIRCUIT: &str = "short_circuit";

/// 4xx codes that are a fixable or transient condition, not a permanent
/// client error: the same classes `attention_http` names as its one rule.
const TRANSIENT_4XX: [u16; 4] = [401, 403, 408, 429];

/// Backoff schedule between retries (seconds); index by attempt, clamped to
/// the last entry. Small and bounded - the tick holds the per-project lock.
const BACKOFF: [f64; 2] = [1.0, 3.0];

/// Cap a server's Retry-After so one sink cannot wedge a tick.
const MAX_RETRY_AFTER: f64 = 30.0;

/// Discord blocks the stdlib default user agent, so every webhook post sends
/// an explicit one.
const USER_AGENT: &str = "fno-status-fanout/1.0";

/// The scan cursor shares the cursor file format as `("", seq)`; the dot
/// keeps it apart from any sink name.
const SCAN: &str = ".scan";

/// The tick lock renews its lease on this cadence (the Python `_TickLock`
/// renewer thread), so a pass with slow sinks never crosses the steal age.
const TICK_LEASE_RENEW_EVERY_S: f64 = 30.0;

/// One sink's outcome.
#[derive(Debug, Clone, Default)]
pub struct SinkResult {
    pub name: String,
    pub matched: u64,
    pub dispatched: u64,
    pub dropped: u64,
    pub short_circuited: bool,
    pub new_cursor: Option<(String, i64)>,
}

/// One tick's outcome.
#[derive(Debug, Clone, Default)]
pub struct TickResult {
    pub sinks: Vec<SinkResult>,
    pub skipped_lines: u64,
    pub rows_read: usize,
    pub locked_out: bool,
    pub lease_lost: bool,
    pub undeliverable_sinks: Vec<String>,
    pub parked_sinks: Vec<String>,
}

/// One configured sink. The validation ladder (name token, type enum,
/// exactly-one-of url/url_env, whitelist match keys, positive tuning) is the
/// Python config model's; a semantic error refuses the tick loudly.
#[derive(Debug, Clone)]
pub struct SinkConfig {
    pub name: String,
    pub kind: String,
    pub events: Vec<String>,
    pub match_keys: BTreeMap<String, String>,
    pub url: Option<String>,
    pub url_env: Option<String>,
    pub template: Option<String>,
    pub field: String,
    pub raw_body: bool,
    pub cloudevents: bool,
    pub enabled: bool,
}

/// Fanout dispatcher tuning (`config.status_fanout`).
#[derive(Debug, Clone)]
pub struct FanoutTuning {
    pub http_timeout_secs: f64,
    pub retries: u32,
    pub short_circuit_backoff_secs: f64,
}

impl Default for FanoutTuning {
    fn default() -> Self {
        FanoutTuning {
            http_timeout_secs: 5.0,
            retries: 2,
            short_circuit_backoff_secs: 300.0,
        }
    }
}

/// One dispatch attempt's HTTP outcome. `status: None` is connect-class.
#[derive(Debug, Clone)]
pub struct HttpResult {
    pub ok: bool,
    pub status: Option<u16>,
    pub retry_after: Option<f64>,
}

/// The post seam: one call, one child. `timeout_secs` is the sink tuning's
/// wall clock, already floored at 1. Tests inject it (the same shape
/// `attention_http` ships).
pub trait HttpPost {
    fn post(&mut self, url: &str, content_type: &str, body: &str, timeout_secs: u64) -> HttpResult;
}

// ---------------------------------------------------------------------------
// Config read
// ---------------------------------------------------------------------------

/// Load the sink rows and the fanout tuning for one project root. A semantic
/// error in any row refuses the whole load (the Python settings model raised
/// at load time too, so a bad sink never silently half-delivered); an absent
/// or wrong-typed container degrades to no sinks.
pub fn load_configs(cwd: &Path) -> Result<(Vec<SinkConfig>, FanoutTuning), String> {
    let mut sinks = Vec::new();
    if let Some(rows) =
        config_value_deep(cwd, &["status_sinks"]).and_then(|v| v.as_array().cloned())
    {
        for row in rows {
            let row = serde_json::to_value(&row).map_err(|e| format!("status_sinks row: {e}"))?;
            sinks.push(parse_sink(&row)?);
        }
    }
    let tuning = FanoutTuning {
        http_timeout_secs: tuning_value(cwd, "http_timeout_secs", 5.0, 1.0)?,
        retries: tuning_value(cwd, "retries", 2.0, 0.0)? as u32,
        short_circuit_backoff_secs: tuning_value(cwd, "short_circuit_backoff_secs", 300.0, 0.0)?,
    };
    Ok((sinks, tuning))
}

fn tuning_value(cwd: &Path, key: &str, default: f64, min: f64) -> Result<f64, String> {
    let Some(v) = config_value_deep(cwd, &["status_fanout", key]) else {
        return Ok(default);
    };
    let raw = v
        .as_f64()
        .ok_or_else(|| format!("config.status_fanout.{key}: expected a number, got {v}"))?;
    if raw < min {
        return Err(format!("config.status_fanout.{key}: must be >= {min}"));
    }
    Ok(raw)
}

fn parse_sink(row: &Value) -> Result<SinkConfig, String> {
    let obj = row
        .as_object()
        .ok_or("status_sinks row: expected a table")?;
    let str_of = |k: &str| {
        obj.get(k)
            .and_then(|v| v.as_str())
            .map(str::to_string)
            .filter(|s| !s.is_empty())
    };
    let name = str_of("name").unwrap_or_default();
    if !name
        .chars()
        .next()
        .is_some_and(|c| c.is_ascii_alphanumeric())
        || !name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
    {
        return Err(format!(
            "status sink name {name:?} must match [A-Za-z0-9][A-Za-z0-9._-]* \
             (it keys the cursor filename; no path separators or leading dot)"
        ));
    }
    let kind = str_of("type").unwrap_or_default();
    if !matches!(
        kind.as_str(),
        "json-webhook" | "text-webhook" | "backlog-progress"
    ) {
        return Err(format!(
            "unknown status sink type {kind:?} (allowed: json-webhook, \
             text-webhook, backlog-progress)"
        ));
    }
    let url = str_of("url");
    let url_env = str_of("url_env");
    if matches!(kind.as_str(), "json-webhook" | "text-webhook")
        && url.is_some() == url_env.is_some()
    {
        return Err(format!(
            "status sink {name:?} ({kind}) requires exactly one of url / url_env"
        ));
    }
    let mut match_keys = BTreeMap::new();
    if let Some(map) = obj.get("match").and_then(|v| v.as_object()) {
        for (k, v) in map {
            match_keys.insert(k.clone(), value_str(v));
        }
    }
    Ok(SinkConfig {
        name,
        kind,
        events: obj
            .get("events")
            .and_then(|v| v.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|e| e.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default(),
        match_keys,
        url,
        url_env,
        template: str_of("template"),
        field: str_of("field").unwrap_or_else(|| "content".to_string()),
        raw_body: obj
            .get("raw_body")
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
        cloudevents: obj
            .get("cloudevents")
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
        enabled: obj.get("enabled").and_then(|v| v.as_bool()).unwrap_or(true),
    })
}

fn value_str(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

// ---------------------------------------------------------------------------
// State layout
// ---------------------------------------------------------------------------

fn state_root(cwd: &Path) -> PathBuf {
    if let Some(dir) = std::env::var_os("FNO_STATE_DIR").filter(|v| !v.is_empty()) {
        return PathBuf::from(dir);
    }
    state_dir(cwd).unwrap_or_else(|| {
        let home = std::env::var_os("HOME").unwrap_or_else(|| ".".into());
        PathBuf::from(home).join(".fno")
    })
}

/// The fanout state beside the journal's physical target (the Python
/// `_state_dir`): `<space>/status-sinks`.
pub fn state_dir_for(cwd: &Path) -> PathBuf {
    crate::paths::space_dir(cwd).join("status-sinks")
}

fn journal_path(cwd: &Path) -> PathBuf {
    crate::paths::events_path(cwd)
}

fn cursor_path(state: &Path, name: &str) -> PathBuf {
    state.join(format!("{name}.cursor"))
}

fn errors_path(state: &Path, name: &str) -> PathBuf {
    state.join(format!("{name}.errors.jsonl"))
}

/// Read a sink's `(ts, n)` cursor, or `None` when absent/unreadable/malformed
/// (all read as fresh: a harmless re-init at the floor).
fn read_cursor(state: &Path, name: &str) -> Option<(String, i64)> {
    let raw = std::fs::read_to_string(cursor_path(state, name)).ok()?;
    let obj = serde_json::from_str::<Value>(&raw).ok()?;
    let ts = obj.get("ts")?.as_str()?.to_string();
    let count = obj.get("n")?.as_i64()?;
    if count < 0 || (!ts.is_empty() && crate::event_store::parse_rfc3339_ms(&ts).is_none()) {
        return None;
    }
    Some((ts, count))
}

fn read_parks(state: &Path) -> BTreeMap<String, f64> {
    let raw = std::fs::read_to_string(state.join("short-circuit-backoff.json")).unwrap_or_default();
    let Ok(obj) = serde_json::from_str::<Value>(&raw) else {
        return BTreeMap::new();
    };
    obj.as_object()
        .map(|m| {
            m.iter()
                .filter_map(|(k, v)| v.as_f64().map(|d| (k.clone(), d)))
                .collect()
        })
        .unwrap_or_default()
}

fn write_cursor(state: &Path, name: &str, cursor: &(String, i64)) -> std::io::Result<()> {
    std::fs::create_dir_all(state)?;
    let path = cursor_path(state, name);
    let tmp = path.with_extension("cursor.tmp");
    std::fs::write(&tmp, json!({"ts": cursor.0, "n": cursor.1}).to_string())?;
    std::fs::rename(&tmp, &path)
}

fn log_error(state: &Path, name: &str, record: Value) {
    let path = errors_path(state, name);
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    {
        use std::io::Write;
        let _ = writeln!(f, "{}", record);
    }
}

fn now_unix() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

fn now_rfc3339_micros() -> String {
    let dur = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let secs = dur.as_secs();
    let micros = dur.subsec_micros();
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    let (hour, min, sec) = (
        (rem / 3600) as u32,
        (rem % 3600) / 60 as u32,
        (rem % 60) as u32,
    );
    let (year, month, day) = civil_from_days(days);
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{min:02}:{sec:02}.{micros:06}Z")
}

fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (y, m as u32, d as u32)
}

fn ts_key(ts: &str) -> i64 {
    if ts.is_empty() {
        return i64::MIN;
    }
    crate::event_store::parse_rfc3339_ms(ts).unwrap_or(i64::MIN)
}

// ---------------------------------------------------------------------------
// The event stream (rotation-aware, skip-and-count)
// ---------------------------------------------------------------------------

fn active_inode(active: &Path) -> Option<u64> {
    use std::os::unix::fs::MetadataExt;
    std::fs::metadata(active).ok().map(|m| m.ino())
}

/// Rotation-stable read: snapshot the active file's inode around the read
/// pass. A worker rotating the log mid-pass can put old content in both files
/// and inflate the same-`ts` occurrence index, whose over-count would skip
/// real same-second events next tick. If the inode changed (or the file
/// appeared/vanished) between the stats, discard and re-run the pass ONCE;
/// at-least-once tolerates the duplicate, only the index inflation had a
/// loss path.
pub fn stream_since(
    active: &Path,
    since_ts: &str,
    after_seq: Option<i64>,
) -> (Vec<Value>, u64, Option<i64>) {
    let ino_before = active_inode(active);
    let result = stream_pass(active, since_ts, after_seq);
    if active_inode(active) == ino_before {
        return result;
    }
    stream_pass(active, since_ts, after_seq)
}

/// Read actionable status rows. With a store, only rows committed past
/// `after_seq` come back, plus the store's high-water seq as the next scan
/// cursor; a store-less journal answers no seq and is read whole.
fn stream_pass(
    active: &Path,
    since_ts: &str,
    after_seq: Option<i64>,
) -> (Vec<Value>, u64, Option<i64>) {
    let since_ms = if since_ts.is_empty() {
        None
    } else {
        crate::event_store::parse_rfc3339_ms(since_ts)
    };
    if crate::event_store::store_path(active).exists() {
        let high = crate::event_store::max_seq(active).unwrap_or(0);
        let query = crate::event_store::EventQuery {
            after_seq: after_seq.filter(|s| *s <= high),
            until_seq: Some(high),
            since_ms,
            ..Default::default()
        };
        let rows = crate::event_store::query_events(active, &query).unwrap_or_default();
        let values: Vec<Value> = rows
            .iter()
            .filter(|r| !r.history_only)
            .filter_map(|r| serde_json::from_str(&r.line).ok())
            .filter(|v| {
                v.get("ts")
                    .and_then(Value::as_str)
                    .and_then(crate::event_store::parse_rfc3339_ms)
                    .is_some_and(|t| since_ms.is_none_or(|s| t >= s))
            })
            .collect();
        return (values, 0, Some(high));
    }
    // Store-less legacy journal: active plus the last rotation generation,
    // parsed line-wise, malformed lines skipped and counted.
    let mut rows = Vec::new();
    let mut skipped = 0u64;
    let read_file = |path: &Path, rows: &mut Vec<Value>, skipped: &mut u64| {
        let Ok(file) = std::fs::File::open(path) else {
            return;
        };
        for line in std::io::BufReader::new(file).lines().map_while(Result::ok) {
            if line.trim().is_empty() {
                continue;
            }
            match serde_json::from_str::<Value>(&line).ok().filter(|v| {
                v.is_object()
                    && v.get("ts")
                        .and_then(Value::as_str)
                        .and_then(crate::event_store::parse_rfc3339_ms)
                        .is_some()
            }) {
                Some(v) => rows.push(v),
                None => *skipped += 1,
            }
        }
    };
    // The rotation race re-read mirrors the Python inode guard in
    // [`stream_since`]; the inner pass reads both generations once.
    let first_ts = std::fs::File::open(active).ok().and_then(|f| {
        std::io::BufReader::new(f)
            .lines()
            .map_while(Result::ok)
            .find_map(|l| {
                serde_json::from_str::<Value>(&l).ok().and_then(|v| {
                    v.get("ts")
                        .and_then(Value::as_str)
                        .and_then(crate::event_store::parse_rfc3339_ms)
                })
            })
    });
    let read_rotated = since_ms.zip(first_ts).is_some_and(|(s, f)| s > f);
    if read_rotated {
        let rotated = active.with_file_name(format!(
            "{}.1",
            active.file_name().unwrap_or_default().to_string_lossy()
        ));
        read_file(&rotated, &mut rows, &mut skipped);
    }
    read_file(active, &mut rows, &mut skipped);
    rows.retain(|v| {
        v.get("ts")
            .and_then(Value::as_str)
            .and_then(crate::event_store::parse_rfc3339_ms)
            .is_some_and(|t| since_ms.is_none_or(|s| t >= s))
    });
    (rows, skipped, None)
}

/// The fresh-sink starting cursor: `(max_ts, count_of_events_at_max_ts)` so
/// no historical event is replayed AND a later event in the SAME second as
/// EOF is still delivered (it lands at occurrence index >= count). `("", 0)`
/// for an empty/absent log means "deliver everything henceforth".
/// `pub` for the parity test seam.
pub fn eof_cursor(active: &Path) -> (String, i64) {
    let Ok(file) = std::fs::File::open(active) else {
        return (String::new(), 0);
    };
    let mut last_ts = String::new();
    let mut last_key = i64::MIN;
    let mut count = 0i64;
    for line in std::io::BufReader::new(file).lines().map_while(Result::ok) {
        if line.trim().is_empty() {
            continue;
        }
        let Ok(ev) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        let Some(ts) = ev.get("ts").and_then(Value::as_str) else {
            continue;
        };
        if crate::event_store::parse_rfc3339_ms(ts).is_none() {
            continue;
        }
        let key = ts_key(ts);
        if key == last_key {
            count += 1;
        } else if key > last_key {
            last_ts = ts.to_string();
            last_key = key;
            count = 1;
        }
    }
    (last_ts, count)
}

/// Fresh-sink floor on a store-backed journal: wall-clock now. A store
/// answers with every retained generation, so the file-EOF floor reads as
/// "from the epoch" over the store. Store-less keeps the read.
fn fresh_floor(active: &Path) -> (String, i64) {
    if crate::event_store::store_path(active).exists() {
        return (now_rfc3339_micros(), 0);
    }
    eof_cursor(active)
}

// ---------------------------------------------------------------------------
// The tick
// ---------------------------------------------------------------------------

/// A sink this tick could deliver through: a webhook whose url resolves now,
/// or a type that needs no url. An enabled webhook with an unresolved
/// url_env is not configured work.
fn sink_configured(sink: &SinkConfig) -> bool {
    if sink.kind == "json-webhook" || sink.kind == "text-webhook" {
        return resolve_url(sink).0.is_some();
    }
    true
}

/// Resolve a webhook URL from `url` or `url_env`; process env wins so an
/// exported value is unchanged, then `~/.fno/.env` (the daemon shells the
/// tick with its own env, which never saw the operator's export). A missing
/// env secret is short-circuit-worthy (fixable), not a hard drop.
fn resolve_url(sink: &SinkConfig) -> (Option<String>, Option<String>) {
    if let Some(url) = &sink.url {
        return (Some(url.clone()), None);
    }
    if let Some(env_name) = &sink.url_env {
        let from_env = std::env::var(env_name).ok().filter(|v| !v.is_empty());
        let val = match from_env {
            Some(v) => Some(v),
            None => {
                let env_file = state_root(&std::env::current_dir().unwrap_or_else(|_| ".".into()))
                    .join(".env");
                crate::provider_key::read_var_from_env_file(&env_file.to_string_lossy(), env_name)
            }
        };
        if let Some(v) = val.filter(|v| !v.is_empty()) {
            return (Some(v), None);
        }
        return (
            None,
            Some(format!(
                "url_env {env_name} unset (checked process env and ~/.fno/.env)"
            )),
        );
    }
    (None, Some("no url configured".to_string()))
}

fn matches(sink: &SinkConfig, event: &Value) -> bool {
    let kind = event.get("type").and_then(Value::as_str).unwrap_or("");
    if !sink.events.iter().any(|e| e == kind) {
        return false;
    }
    sink.match_keys.iter().all(|(k, v)| {
        let got = match event.get(k) {
            Some(Value::String(s)) => s.clone(),
            Some(Value::Null) | None => String::new(),
            Some(other) => other.to_string(),
        };
        got == *v
    })
}

/// One fanout pass. `post` is the HTTP seam; the production caller hands
/// [`CurlPost`], tests inject a recording fake.
pub fn run_tick(
    project_root: &Path,
    sinks: &[SinkConfig],
    tuning: &FanoutTuning,
    dry_run: bool,
    post: &mut dyn HttpPost,
    dispatch: Option<&mut DispatchFn>,
    stream: Option<&mut StreamFn>,
) -> TickResult {
    let enabled: Vec<&SinkConfig> = sinks.iter().filter(|s| s.enabled).collect();
    if enabled.is_empty() {
        return TickResult::default();
    }
    if !enabled.iter().any(|s| sink_configured(s)) {
        // Every enabled sink's url is unresolved (e.g. url_env unset):
        // dispatch would short-circuit and hold the scan cursor, so every
        // later tick rereads the whole journal. Skip before any read; the
        // sink's stored cursor resumes the backlog once the url resolves.
        return TickResult {
            undeliverable_sinks: enabled.iter().map(|s| s.name.clone()).collect(),
            ..Default::default()
        };
    }
    let state = state_dir_for(project_root);
    let parks = if dry_run {
        BTreeMap::new()
    } else {
        read_parks(&state)
    };
    let now = now_unix();
    let live: Vec<&SinkConfig> = enabled
        .iter()
        .copied()
        .filter(|s| parks.get(&s.name).copied().unwrap_or(0.0) <= now)
        .collect();
    if live.is_empty() {
        return TickResult {
            parked_sinks: enabled.iter().map(|s| s.name.clone()).collect(),
            ..Default::default()
        };
    }
    let mut lock = TickLock::acquire(&state);
    let Some(lock) = lock.as_mut() else {
        return TickResult {
            locked_out: true,
            ..Default::default()
        };
    };
    match dispatch {
        Some(d) => run_locked(
            project_root,
            &state,
            &live,
            tuning,
            dry_run,
            lock,
            &parks,
            d,
            stream.unwrap_or(&mut |active, since, after| stream_since(active, since, after)),
        ),
        None => run_locked(
            project_root,
            &state,
            &live,
            tuning,
            dry_run,
            lock,
            &parks,
            &mut |sink, event| dispatch_event(sink, event, tuning, project_root, post),
            &mut |active, since, after| stream_since(active, since, after),
        ),
    }
}

/// One dispatch call's outcome, scripted in tests and produced by the sink's
/// adapter in production - the same seam shape the deleted Python leg's
/// `run_tick(..., dispatch_fn=...)` carried.
pub type DispatchFn<'a> = dyn FnMut(&SinkConfig, &Value) -> (String, String) + 'a;

/// One stream read's rows: scripted in tests, produced by the journal reader
/// in production.
pub type StreamFn<'a> = dyn FnMut(&Path, &str, Option<i64>) -> (Vec<Value>, u64, Option<i64>) + 'a;

fn run_locked(
    project_root: &Path,
    state: &Path,
    sinks: &[&SinkConfig],
    tuning: &FanoutTuning,
    dry_run: bool,
    lock: &mut TickLock,
    parks: &BTreeMap<String, f64>,
    dispatch: &mut DispatchFn,
    stream: &mut StreamFn,
) -> TickResult {
    let active = journal_path(project_root);
    let scan = read_cursor(state, SCAN).map(|(_, n)| n);
    let stored: Vec<Option<(String, i64)>> =
        sinks.iter().map(|s| read_cursor(state, &s.name)).collect();
    let any_fresh = stored.iter().any(|c| c.is_none());

    // Each sink's starting cursor: a fresh sink (no file) starts at the floor
    // so no history is replayed; an existing sink resumes from its stored
    // cursor. With a scan seq the floor is now: a real ts that survives a
    // lost scan cursor.
    let now_stamp = now_rfc3339_micros();
    let floor = if scan.is_some() {
        (now_stamp, 0i64)
    } else if any_fresh {
        fresh_floor(&active)
    } else {
        (String::new(), 0i64)
    };
    let mut start: Vec<(String, i64)> = stored
        .iter()
        .map(|c| c.clone().unwrap_or_else(|| floor.clone()))
        .collect();

    // Read from the oldest cursor ts INCLUSIVE so every sink sees its own
    // same-ts boundary events; the per-sink (ts, n) tiebreak below decides
    // what is new.
    let min_ts = start
        .iter()
        .min_by_key(|(ts, _)| ts_key(ts))
        .map(|(ts, _)| ts.clone())
        .unwrap_or_default();
    let (mut events, skipped, high) = stream(&active, &min_ts, scan);
    if scan.is_none() && high.is_some() && !events.is_empty() {
        // A first store-backed pass: the journal EOF can predate the store's
        // newest rows, so a fresh sink starts at the store's EOF instead.
        let last_key = events
            .iter()
            .map(|e| ts_of(e))
            .max_by_key(|k| *k)
            .unwrap_or(i64::MIN);
        let last_ts = events
            .iter()
            .filter(|e| ts_of(e) == last_key)
            .next()
            .and_then(|e| e.get("ts").and_then(Value::as_str))
            .unwrap_or_default()
            .to_string();
        let count_at_last = events.iter().filter(|e| ts_of(e) == last_key).count() as i64;
        for s in start.iter_mut() {
            if ts_key(&s.0) < last_key {
                *s = (last_ts.clone(), count_at_last);
            }
        }
    }
    // Setup migration appends a worktree segment after the canonical tail, so
    // append order is not necessarily timestamp order. The cursor compares by
    // timestamp; process a stable timestamp ordering so an older migrated row
    // cannot follow a newer row that advanced the cursor past it. Stability
    // preserves the occurrence order of same-second peers.
    events.sort_by_key(|e| ts_of(e));

    let mut state_rows: Vec<SinkResult> = sinks
        .iter()
        .enumerate()
        .map(|(i, s)| SinkResult {
            name: s.name.clone(),
            new_cursor: Some(start[i].clone()),
            ..Default::default()
        })
        .collect();

    // Occurrence index among equal-instant peers, in stable file order.
    // Events routinely share one timestamp; (ts, idx) is the stable identity
    // a bare-ts cursor lacked (the same-second drop bug).
    let mut occ: BTreeMap<i64, i64> = BTreeMap::new();
    for event in &events {
        let ets = event
            .get("ts")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let event_key = ts_key(&ets);
        let idx = occ.get(&event_key).copied().unwrap_or(0);
        occ.insert(event_key, idx + 1);
        for (si, sink) in sinks.iter().copied().enumerate() {
            let st = &mut state_rows[si];
            if st.short_circuited {
                continue;
            }
            let (cts, cn) = st.new_cursor.clone().unwrap_or_default();
            let cursor_key = ts_key(&cts);
            // Already processed: a strictly-older ts, or a same-ts occurrence
            // the cursor already advanced past.
            if event_key < cursor_key || (event_key == cursor_key && idx < cn) {
                continue;
            }
            if !matches(sink, event) {
                continue;
            }
            st.matched += 1;
            if dry_run {
                continue;
            }
            let (status, detail) = dispatch(sink, event);
            if status == DELIVERED {
                st.dispatched += 1;
                st.new_cursor = Some((ets.clone(), idx + 1));
            } else if status == SHORT_CIRCUIT {
                // Hold the cursor: this event + everything after retries next
                // tick.
                st.short_circuited = true;
                log_error(
                    state,
                    &sink.name,
                    json!({"sink": sink.name, "event_ts": ets,
                           "type": event.get("type"),
                           "reason": detail, "class": "short_circuit"}),
                );
            } else {
                // DROPPED, or any unrecognized status -> drop + log and
                // advance, never a silent short-circuit-forever on a typo'd
                // dispatcher.
                st.dropped += 1;
                st.new_cursor = Some((ets.clone(), idx + 1));
                let reason = if status == DROPPED {
                    detail
                } else {
                    format!("unknown dispatch status {status:?}")
                };
                log_error(
                    state,
                    &sink.name,
                    json!({"sink": sink.name, "event_ts": ets,
                           "type": event.get("type"),
                           "reason": reason, "class": "dropped"}),
                );
            }
        }
    }

    // Persist advanced cursors, ISOLATED per sink: one sink's cursor-write
    // failure (disk full / perms) must not abort the others' persistence and
    // drive a silent re-delivery storm. A fresh sink persists its floor even
    // with zero dispatch so the next tick never backfills.
    if !dry_run {
        // A held (short-circuited) sink re-reads this window, so the seq
        // stays. Past an advance no same-ts peer remains, so counts restart
        // at 0. Cursors land first: a crash between the writes re-delivers.
        let mut advance = high.is_some() && high != scan;
        let now = now_unix();
        let held: BTreeMap<String, f64> = parks
            .iter()
            .filter(|(_, deadline)| **deadline > now)
            .map(|(k, d)| (k.clone(), *d))
            .collect();
        advance = advance && held.is_empty() && !state_rows.iter().any(|st| st.short_circuited);
        let mut lease_lost = false;
        for (idx, st) in state_rows.iter_mut().enumerate() {
            let mut cursor = st.new_cursor.clone().unwrap_or_default();
            if advance && cursor.1 != 0 {
                cursor.1 = 0;
            }
            // Persist when this tick moved the cursor, or when the sink is
            // fresh (its floor must survive even a zero-dispatch pass).
            let changed = stored[idx].is_none() || cursor != start[idx];
            if changed {
                if !lock.verify() {
                    lease_lost = true;
                    break;
                }
                if let Err(e) = write_cursor(state, &st.name, &cursor) {
                    advance = false; // a stale count must not meet the next window
                    log_error(
                        state,
                        &st.name,
                        json!({"sink": st.name,
                               "reason": format!("cursor write failed: {e}"),
                               "class": "cursor_write_failed"}),
                    );
                }
            }
        }
        if lease_lost {
            return TickResult {
                sinks: state_rows,
                skipped_lines: skipped,
                rows_read: events.len(),
                lease_lost: true,
                ..Default::default()
            };
        }
        if advance {
            if let Some(h) = high {
                if lock.verify() {
                    let _ = write_cursor(state, SCAN, &("", h));
                }
            }
        }
        let mut new_parks = held.clone();
        for st in state_rows.iter() {
            if tuning.short_circuit_backoff_secs > 0.0 && st.short_circuited {
                new_parks.insert(
                    st.name.clone(),
                    now_unix() + tuning.short_circuit_backoff_secs,
                );
            }
        }
        if new_parks != *parks && lock.verify() {
            let path = state.join("short-circuit-backoff.json");
            let tmp = state.join("short-circuit-backoff.tmp");
            if std::fs::write(&tmp, json!(new_parks).to_string()).is_ok() {
                let _ = std::fs::rename(&tmp, &path);
            }
        }
    }

    TickResult {
        sinks: state_rows,
        skipped_lines: skipped,
        rows_read: events.len(),
        ..Default::default()
    }
}

fn ts_of(event: &Value) -> i64 {
    ts_key(event.get("ts").and_then(Value::as_str).unwrap_or(""))
}

// ---------------------------------------------------------------------------
// The per-project tick lock
// ---------------------------------------------------------------------------

/// Non-blocking cross-language mutex beside the shared fanout state. A
/// hand-run tick racing the daemon's tick skips rather than double-advancing
/// cursors, including when the callers entered through different worktrees.
/// The renewer thread keeps the lease fresh so a slow pass (several stalled
/// sinks x retries x http timeouts) never crosses the steal age.
pub struct TickLock {
    path: PathBuf,
    token: String,
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    lost: std::sync::Arc<std::sync::atomic::AtomicBool>,
    renewer: Option<std::thread::JoinHandle<()>>,
}

impl TickLock {
    fn acquire(state: &Path) -> Option<TickLock> {
        let path = state.join(".tick.lock.d");
        if let Err(e) = std::fs::create_dir_all(state) {
            // A read-only fs / permission error creating the lock: treat as
            // "cannot lock" (the tick skips as locked_out) rather than
            // crashing.
            eprintln!("status-fanout: lock dir unavailable: {e}");
            return None;
        }
        let token = crate::claims::acquire_dir_mutex(&path, Duration::from_secs(0), false)?;
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let lost = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let renew_path = path.clone();
        let renew_token = token.clone();
        let renew_stop = stop.clone();
        let renew_lost = lost.clone();
        let renewer = std::thread::spawn(move || loop {
            std::thread::sleep(Duration::from_secs_f64(TICK_LEASE_RENEW_EVERY_S));
            if renew_stop.load(std::sync::atomic::Ordering::SeqCst) {
                return;
            }
            if !renew_dir_mutex(&renew_path, &renew_token) {
                renew_lost.store(true, std::sync::atomic::Ordering::SeqCst);
                return;
            }
        });
        Some(TickLock {
            path,
            token,
            stop,
            lost,
            renewer: Some(renewer),
        })
    }

    /// Confirm ownership at a cursor-publication boundary.
    fn verify(&mut self) -> bool {
        if self.lost.load(std::sync::atomic::Ordering::SeqCst)
            || !renew_dir_mutex(&self.path, &self.token)
        {
            self.lost.store(true, std::sync::atomic::Ordering::SeqCst);
            return false;
        }
        true
    }
}

impl Drop for TickLock {
    fn drop(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::SeqCst);
        if let Some(handle) = self.renewer.take() {
            let _ = handle.join();
        }
        crate::claims::release_dir_mutex(&self.path, &self.token);
    }
}

/// Refresh a held dir-mutex lease only while its owner token still matches.
/// Mirrors `fno.mutex.renew_dir_mutex`: read the owner file, touch the dir's
/// mtime (the age signal the steal gate reads), re-read the owner.
fn renew_dir_mutex(lock_dir: &Path, token: &str) -> bool {
    let owner = lock_dir.join("owner");
    let read_owner = || std::fs::read_to_string(&owner).unwrap_or_default();
    if read_owner() != token {
        return false;
    }
    if touch_now(lock_dir).is_err() {
        return false;
    }
    read_owner() == token
}

fn touch_now(path: &Path) -> std::io::Result<()> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let tv_sec = now.as_secs() as libc::time_t;
    let tv_usec = now.subsec_micros();
    let times = [libc::timeval {
        tv_sec,
        tv_usec: tv_usec as libc::suseconds_t,
    }; 2];
    let c = std::ffi::CString::new(path.as_os_str().as_encoded_bytes())
        .map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidInput, "nul in path"))?;
    let rc = unsafe { libc::utimes(c.as_ptr(), times.as_ptr()) };
    if rc != 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// HTTP delivery (shared by the two webhook adapters)
// ---------------------------------------------------------------------------

/// The real post: one `curl` child, config on stdin, body from a temp file -
/// the same secret shape `attention_http` ships.
pub struct CurlPost;

impl HttpPost for CurlPost {
    fn post(&mut self, url: &str, content_type: &str, body: &str, timeout_secs: u64) -> HttpResult {
        let body_path = std::env::temp_dir().join(format!(
            "fno-fanout-post-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        let _ = std::fs::write(&body_path, body);
        let resp_path = body_path.with_extension("resp");
        let quoted = |v: &str| v.replace('\\', "\\\\").replace('"', "\\\"");
        let mut config = format!(
            "url = \"{}\"\nmax-time = \"{}\"\nsilent\nshow-error\n",
            quoted(url),
            timeout_secs
        );
        config.push_str(&format!(
            "header = \"Content-Type: {}\"\nheader = \"User-Agent: {USER_AGENT}\"\n",
            quoted(content_type)
        ));
        config.push_str(&format!(
            "data-binary = \"@{}\"\noutput = \"{}\"\nwrite-out = \"%{{http_code}}\"\n",
            quoted(&body_path.display().to_string()),
            quoted(&resp_path.display().to_string())
        ));
        let output = std::process::Command::new("curl")
            .arg("--config")
            .arg("-")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .and_then(|mut child| {
                use std::io::Write;
                if let Some(mut stdin) = child.stdin.take() {
                    let _ = stdin.write_all(config.as_bytes());
                }
                child.wait_with_output()
            });
        let _ = std::fs::remove_file(&body_path);
        let result = output.ok().and_then(|out| {
            let code: Option<u16> = String::from_utf8_lossy(&out.stdout).trim().parse().ok();
            let resp = std::fs::read_to_string(&resp_path).unwrap_or_default();
            let _ = std::fs::remove_file(&resp_path);
            if out.status.success() {
                Some(HttpResult {
                    ok: (200..300).contains(&code.unwrap_or(0)),
                    status: code,
                    retry_after: None,
                })
            } else {
                None
            }
        });
        result.unwrap_or(HttpResult {
            ok: false,
            status: None,
            retry_after: None,
        })
    }
}

/// Retry/failure-class driver shared by the webhook adapters. 4xx except the
/// transient classes drops immediately (permanent; advance past it);
/// connect-class / 5xx / 401 / 403 / 408 / 429 retries on a bounded schedule,
/// then short-circuits (transient; hold the cursor). 429 honors Retry-After
/// within budget.
fn deliver(
    post: &mut dyn HttpPost,
    url: &str,
    body: &str,
    content_type: &str,
    tuning: &FanoutTuning,
) -> (String, String) {
    let attempts = (tuning.retries + 1).max(1);
    let mut last = HttpResult {
        ok: false,
        status: None,
        retry_after: None,
    };
    let timeout_secs = if tuning.http_timeout_secs < 1.0 {
        1
    } else {
        tuning.http_timeout_secs as u64
    };
    for i in 0..attempts {
        last = post.post(url, content_type, body, timeout_secs);
        if last.ok {
            return (DELIVERED.to_string(), String::new());
        }
        if let Some(status) = last.status {
            if (400..500).contains(&status) && !TRANSIENT_4XX.contains(&status) {
                return (DROPPED.to_string(), format!("http {status}"));
            }
        }
        if i + 1 < attempts {
            let delay = if last.status == Some(429) {
                last.retry_after
                    .map(|ra| ra.min(MAX_RETRY_AFTER))
                    .unwrap_or_else(|| BACKOFF[(i as usize).min(BACKOFF.len() - 1)])
            } else {
                BACKOFF[(i as usize).min(BACKOFF.len() - 1)]
            };
            std::thread::sleep(Duration::from_secs_f64(delay));
        }
    }
    let reason = match last.status {
        Some(s) => format!("http {s}"),
        None => "connect-class".to_string(),
    };
    (
        SHORT_CIRCUIT.to_string(),
        format!("exhausted retries ({reason})"),
    )
}

// ---------------------------------------------------------------------------
// Adapter router + adapters
// ---------------------------------------------------------------------------

/// Route an event to the sink's adapter by type.
pub fn dispatch_event(
    sink: &SinkConfig,
    event: &Value,
    tuning: &FanoutTuning,
    project_root: &Path,
    post: &mut dyn HttpPost,
) -> (String, String) {
    match sink.kind.as_str() {
        "json-webhook" => dispatch_json_webhook(sink, event, tuning, post),
        "text-webhook" => dispatch_text_webhook(sink, event, tuning, post),
        "backlog-progress" => dispatch_backlog_progress(sink, event, project_root),
        other => (DROPPED.to_string(), format!("unknown sink type {other}")),
    }
}

/// Wrap the canonical event in the 5-field CloudEvents envelope. `id` is
/// run+ts+type plus a content-hash suffix: deterministic, so a re-delivered
/// event keeps a stable id (receiver dedup works), while two distinct events
/// in the same run+ts+type second get distinct ids and neither is deduped
/// away. Byte-identical duplicate lines share an id - correct, they are the
/// same event.
fn cloudevents_wrap(event: &Value) -> Value {
    let canonical = python_json_compact_ascii(event);
    let digest = Sha256::digest(canonical.as_bytes());
    let hex: String = digest.iter().map(|b| format!("{b:02x}")).collect();
    json!({
        "id": format!(
            "{}:{}:{}:{}",
            event.get("run").and_then(Value::as_str).unwrap_or(""),
            event.get("ts").and_then(Value::as_str).unwrap_or(""),
            event.get("type").and_then(Value::as_str).unwrap_or(""),
            &hex[..8]
        ),
        "source": event.get("source").and_then(Value::as_str).unwrap_or("fno"),
        "type": event.get("type").and_then(Value::as_str).unwrap_or(""),
        "time": event.get("ts").and_then(Value::as_str).unwrap_or(""),
        "data": event,
    })
}

/// `json.dumps(value, sort_keys=True, separators=(",", ":"), ensure_ascii=True)`
/// - the exact canonical form the Python leg hashed, including the `\uXXXX`
/// escapes for every non-ASCII code point (surrogate pairs past the BMP), so
/// the deterministic id cannot drift between legs. `pub` so the parity test
/// serializes its decision records with the same function.
pub fn python_json_compact_ascii(value: &Value) -> String {
    match value {
        Value::Null => "null".to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => n.to_string(),
        Value::String(s) => py_json_string(s),
        Value::Array(items) => {
            let parts: Vec<String> = items.iter().map(python_json_compact_ascii).collect();
            format!("[{}]", parts.join(","))
        }
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            let parts: Vec<String> = keys
                .into_iter()
                .map(|k| {
                    format!(
                        "{}:{}",
                        py_json_string(k),
                        python_json_compact_ascii(&map[k])
                    )
                })
                .collect();
            format!("{{{}}}", parts.join(","))
        }
    }
}

fn py_json_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\x08' => out.push_str("\\b"),
            '\x0c' => out.push_str("\\f"),
            c if (c as u32) < 0x20 => {
                out.push_str(&format!("\\u{:04x}", c as u32));
            }
            c if (c as u32) <= 0x7E => out.push(c),
            c => {
                // ensure_ascii: every non-ASCII code point escapes, astral
                // code points as a UTF-16 surrogate pair.
                let mut buf = [0u16; 2];
                for unit in c.encode_utf16(&mut buf) {
                    out.push_str(&format!("\\u{unit:04x}"));
                }
            }
        }
    }
    out.push('"');
    out
}

fn dispatch_json_webhook(
    sink: &SinkConfig,
    event: &Value,
    tuning: &FanoutTuning,
    post: &mut dyn HttpPost,
) -> (String, String) {
    let (url, err) = resolve_url(sink);
    let Some(url) = url else {
        return (
            SHORT_CIRCUIT.to_string(),
            err.unwrap_or_else(|| "no url".into()),
        );
    };
    let body = if sink.cloudevents {
        cloudevents_wrap(event)
    } else {
        event.clone()
    };
    deliver(
        post,
        &url,
        &python_json_compact_ascii(&body),
        "application/json",
        tuning,
    )
}

/// Render `template` against the event: `{key}` and `{key.sub.sub}` read the
/// envelope's top-level keys and nested data paths; any missing field renders
/// empty instead of raising, positional refs render empty, and a malformed
/// template (an unbalanced brace) degrades to the raw text rather than
/// crashing the tick. Format specs are ignored (divergence from the Python
/// `format_field`, declared in the parity test header). `pub` for the
/// parity test seam.
pub fn render_template(template: Option<&str>, event: &Value) -> String {
    let Some(template) = template else {
        return String::new();
    };
    let mut out = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(open) = rest.find('{') {
        out.push_str(&rest[..open]);
        let after = &rest[open + 1..];
        if let Some(close) = after.find('}') {
            let field = &after[..close];
            if let Some(escaped) = field.strip_prefix('{') {
                out.push('{');
                out.push_str(escaped);
            } else {
                out.push_str(lookup_field(field, event).as_str());
            }
            rest = &after[close + 1..];
        } else {
            // Unbalanced brace: the raw text wins (the Python formatter's
            // ValueError path).
            return template.to_string();
        }
    }
    out.push_str(rest);
    out
}

fn lookup_field(field: &str, event: &Value) -> String {
    let (first, rest) = match field.split_once('.') {
        Some((f, r)) => (f, Some(r)),
        None => (field, None),
    };
    let mut obj = event.get(first).cloned().unwrap_or(Value::Null);
    if let Some(parts) = rest {
        for part in parts.split('.') {
            obj = match &obj {
                Value::Object(map) => map.get(part).cloned().unwrap_or(Value::Null),
                _ => Value::Null,
            };
        }
    }
    match obj {
        Value::String(s) => s,
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

fn dispatch_text_webhook(
    sink: &SinkConfig,
    event: &Value,
    tuning: &FanoutTuning,
    post: &mut dyn HttpPost,
) -> (String, String) {
    let (url, err) = resolve_url(sink);
    let Some(url) = url else {
        return (
            SHORT_CIRCUIT.to_string(),
            err.unwrap_or_else(|| "no url".into()),
        );
    };
    let rendered = render_template(sink.template.as_deref(), event);
    if sink.raw_body {
        // The ntfy shape: with the topic in the URL the body IS the raw
        // message, and a JSON envelope would be shown to the reader
        // literally.
        return deliver(post, &url, &rendered, "text/plain", tuning);
    }
    let mut rendered_out = rendered;
    if sink.field != "content" {
        // Slack's broadcast tokens render as literal text instead of pinging
        // the workspace (Slack shows `&lt;` as `<`, so the visible text is
        // unchanged); raw mode posts before that point.
        rendered_out = rendered_out.replace("<!", "&lt;!");
    }
    let mut body = Map::new();
    body.insert(sink.field.clone(), Value::String(rendered_out));
    if sink.field == "content" {
        // A Discord-shaped post cannot ping the server through a
        // worker-influenced reason containing @everyone.
        body.insert("allowed_mentions".into(), json!({"parse": []}));
    }
    deliver(
        post,
        &url,
        &python_json_compact_ascii(&Value::Object(body)),
        "application/json",
        tuning,
    )
}

/// One-line human summary of a task-boundary event for the node/plan.
fn progress_line(event: &Value) -> String {
    let kind = event.get("type").and_then(Value::as_str).unwrap_or("");
    let outcome = event.get("outcome").and_then(Value::as_str);
    let reason = event
        .get("data")
        .filter(|d| d.is_object())
        .and_then(|d| d.get("reason"))
        .and_then(Value::as_str);
    let mut bits = vec![kind.to_string()];
    if let Some(o) = outcome {
        bits.push(o.to_string());
    }
    if let Some(r) = reason {
        bits.push(r.to_string());
    }
    bits.join(" - ")
}

/// On `task_done` / `run_summary` carrying a `node`: record the row in
/// permanent history (never the human current state) AND append to the plan
/// doc's `## Progress` section. Other kinds / node-less events are a no-op
/// (advance the cursor).
fn dispatch_backlog_progress(
    sink: &SinkConfig,
    event: &Value,
    project_root: &Path,
) -> (String, String) {
    let _ = sink;
    let kind = event.get("type").and_then(Value::as_str).unwrap_or("");
    if !matches!(kind, "task_done" | "run_summary") {
        return (DELIVERED.to_string(), String::new());
    }
    let Some(node_id) = event.get("node").and_then(Value::as_str) else {
        return (DELIVERED.to_string(), String::new());
    };
    let text = progress_line(event);
    let graph = crate::state_layout::graph_anchor(&state_root(project_root));
    let rev = crate::backlog::node_state::current_revision(&graph, node_id).unwrap_or(0);
    let note = crate::backlog::note_history::append(
        &graph,
        node_id,
        crate::backlog::note_history::REASON_MACHINE_RECORD,
        Some(rev),
        None,
        &json!({"body": text, "kind": kind}),
        None,
        None,
    );
    if let Err(e) = note {
        return (DROPPED.to_string(), format!("graph note failed: {e}"));
    }
    // Plan-doc append is best-effort and never fails the delivery.
    let plan_path = graph_plan_path(&graph, node_id);
    if let Some(plan) = plan_path {
        let stamp = event.get("ts").and_then(Value::as_str).unwrap_or("");
        append_plan_progress(&plan, &format!("{stamp} {text}"), project_root);
    }
    (DELIVERED.to_string(), String::new())
}

/// The node's bound plan path, read in-process from the graph rows. Absent
/// when the row or the key is missing - the append is best-effort anyway.
fn graph_plan_path(graph: &Path, node_id: &str) -> Option<String> {
    let rows = crate::graph_store::read_rows(graph).ok()?;
    rows.iter()
        .find(|r| crate::graph_store::entry_id(r) == Some(node_id))
        .and_then(|r| {
            r.get("plan_path")
                .and_then(Value::as_str)
                .filter(|p| !p.is_empty())
                .map(str::to_string)
        })
}

/// Append `- <text>` under a `## Progress` heading in the node's plan doc.
/// Body-only: NEVER touches frontmatter (the ship-gate stamp owns that).
/// Best-effort: a missing/unresolvable path or any IO error is skipped
/// silently (a vault miss is never a delivery failure). The heading is
/// created at EOF on first use; this adapter is the sole body-appender, so
/// Progress stays the trailing section. Serialized against the ship-gate
/// stamp via the shared sidecar lock (both are whole-file rewrites; an
/// atomic write alone would still lose one side's update); the read happens
/// INSIDE the lock so a concurrent stamp's change is never clobbered.
fn append_plan_progress(plan_path: &str, text: &str, project_root: &Path) {
    let p = PathBuf::from(plan_path);
    let p = if p.is_absolute() {
        p
    } else {
        project_root.join(plan_path)
    };
    let p = match std::fs::canonicalize(&p) {
        Ok(c) => c,
        Err(_) => return,
    };
    if !p.is_file() {
        return;
    }
    let line = format!("- {text}");
    let lock = match crate::plan_doc::lock::PlanDocLock::acquire(&p, Duration::from_secs(2)) {
        Ok(l) => l,
        Err(_) => return,
    };
    let _guard = lock;
    let Ok(content) = std::fs::read_to_string(&p) else {
        return;
    };
    let new = if content.contains("## Progress") {
        format!("{}\n{}\n", content.trim_end_matches('\n'), line)
    } else {
        format!(
            "{}\n\n## Progress\n\n{}\n",
            content.trim_end_matches('\n'),
            line
        )
    };
    let _ = crate::plan_doc::codec::atomic_write(&p, &new);
}

// ---------------------------------------------------------------------------
// The command
// ---------------------------------------------------------------------------

/// `status-fanout tick [--dry-run|-N]`: the hidden op behind the front door's
/// `fno doctor event fanout tick` claim. One pass, the Python leg's receipt
/// lines byte for byte; an idle pass prints nothing.
pub fn run_tick_command(args: &[String]) -> i32 {
    let mut dry_run = false;
    for a in args {
        match a.as_str() {
            "--dry-run" | "-N" => dry_run = true,
            other => {
                eprintln!("status-fanout: unknown flag {other}");
                return 2;
            }
        }
    }
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let (sinks, tuning) = match load_configs(&cwd) {
        Ok(pair) => pair,
        Err(e) => {
            eprintln!("status-fanout: {e}");
            return 1;
        }
    };
    let mut post = CurlPost;
    let result = run_tick(&cwd, &sinks, &tuning, dry_run, &mut post, None, None);
    if result.locked_out {
        println!("status-fanout: another tick holds the lock; skipped");
        return 0;
    }
    // The daemon ticks every few seconds: an idle pass prints nothing.
    if result.sinks.is_empty() && dry_run {
        if !result.undeliverable_sinks.is_empty() {
            println!(
                "status-fanout: no deliverable sink (url unresolved; no-op): {}",
                result.undeliverable_sinks.join(", ")
            );
        } else {
            println!("status-fanout: no enabled sinks (no-op)");
        }
    }
    let command = if dry_run { "would-send" } else { "dispatched" };
    for sr in &result.sinks {
        if !(dry_run || sr.matched > 0 || sr.dropped > 0 || sr.short_circuited) {
            continue;
        }
        let sent = if dry_run { sr.matched } else { sr.dispatched };
        let extra = if sr.short_circuited {
            " short-circuited"
        } else {
            ""
        };
        println!(
            "status-fanout: {} matched={} {}={} dropped={}{extra}",
            sr.name, sr.matched, command, sent, sr.dropped
        );
    }
    if result.skipped_lines > 0 {
        println!(
            "status-fanout: skipped {} malformed line(s)",
            result.skipped_lines
        );
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    struct ScriptedPost {
        responses: Vec<HttpResult>,
        bodies: std::cell::RefCell<Vec<(String, String)>>,
    }

    impl ScriptedPost {
        fn ok() -> Self {
            ScriptedPost {
                responses: vec![HttpResult {
                    ok: true,
                    status: Some(200),
                    retry_after: None,
                }],
                bodies: std::cell::RefCell::new(Vec::new()),
            }
        }
    }

    impl HttpPost for ScriptedPost {
        fn post(
            &mut self,
            url: &str,
            content_type: &str,
            body: &str,
            timeout_secs: u64,
        ) -> HttpResult {
            self.bodies
                .borrow_mut()
                .push((format!("{url}|{content_type}"), body.to_string()));
            self.responses.pop().unwrap_or(HttpResult {
                ok: false,
                status: None,
                retry_after: None,
            })
        }
    }

    fn sink(name: &str, kind: &str, events: &[&str]) -> SinkConfig {
        SinkConfig {
            name: name.to_string(),
            kind: kind.to_string(),
            events: events.iter().map(|s| s.to_string()).collect(),
            match_keys: BTreeMap::new(),
            url: Some("https://example.invalid/hook".to_string()),
            url_env: None,
            template: None,
            field: "content".to_string(),
            raw_body: false,
            cloudevents: false,
            enabled: true,
        }
    }

    #[test]
    fn cloudevents_id_is_deterministic_and_ascii_escaped() {
        let event = json!({"ts": "2026-01-01T00:00:00Z", "type": "task_done",
                           "run": "r1", "source": "target", "data": {"note": "caf\u{e9}"}});
        let wrap = cloudevents_wrap(&event);
        let id = wrap.get("id").and_then(Value::as_str).unwrap();
        let digest = &id[id.rfind(':').unwrap() + 1..];
        assert_eq!(digest.len(), 8);
        // ensure_ascii: the canonical form hashes é, not UTF-8 bytes.
        let wrap2 = cloudevents_wrap(&event);
        assert_eq!(wrap, wrap2);
        let canonical = python_json_compact_ascii(&event);
        assert!(canonical.contains("\\u00e9"), "{canonical}");
    }

    #[test]
    fn template_reads_top_level_and_nested_data_paths() {
        let event = json!({"type": "task_done", "run": "r1", "data": {"outcome": "ok"}});
        assert_eq!(
            render_template(Some("{type}/{run}"), &event),
            "task_done/r1"
        );
        assert_eq!(render_template(Some("{data.outcome}"), &event), "ok");
        assert_eq!(render_template(Some("[{nope}]"), &event), "[]");
        assert_eq!(render_template(Some("[{data.nope.deep}]"), &event), "[]");
        assert_eq!(render_template(Some("[{0}]"), &event), "[]");
        assert_eq!(render_template(Some("{unbalanced"), &event), "{unbalanced");
    }

    #[test]
    fn text_webhook_defangs_slack_tokens_but_not_discord() {
        let event = json!({"type": "task_done", "data": {"reason": "<!channel x"}});
        let mut slack = sink("s", "text-webhook", &["task_done"]);
        slack.field = "text".to_string();
        slack.template = Some("<!channel {type}".to_string());
        let mut post = ScriptedPost::ok();
        let tuning = FanoutTuning::default();
        let (status, _) = dispatch_text_webhook(&slack, &event, &tuning, &mut post);
        assert_eq!(status, DELIVERED);
        assert!(
            post.bodies.borrow()[0].1.contains("&lt;!channel"),
            "{:?}",
            post.bodies.borrow()
        );
        let mut discord = sink("d", "text-webhook", &["task_done"]);
        discord.field = "content".to_string();
        discord.template = Some("<!channel {type}".to_string());
        let mut post = ScriptedPost::ok();
        let (status, _) = dispatch_text_webhook(&discord, &event, &tuning, &mut post);
        assert_eq!(status, DELIVERED);
        let body = &post.bodies.borrow()[0].1;
        assert!(body.contains("<!channel"), "{body}");
        assert!(body.contains("allowed_mentions"), "{body}");
    }

    #[test]
    fn raw_body_posts_text_plain_verbatim() {
        let event = json!({"type": "task_done", "data": {}});
        let mut s = sink("n", "text-webhook", &["task_done"]);
        s.raw_body = true;
        s.template = Some("raw {type}".to_string());
        let mut post = ScriptedPost::ok();
        let tuning = FanoutTuning::default();
        let (status, _) = dispatch_text_webhook(&s, &event, &tuning, &mut post);
        assert_eq!(status, DELIVERED);
        let (meta, body) = &post.bodies.borrow()[0];
        assert!(meta.ends_with("text/plain"), "{meta}");
        assert_eq!(body, "raw task_done");
    }

    #[test]
    fn non_transient_4xx_drops_and_transient_short_circuits() {
        let tuning = FanoutTuning {
            http_timeout_secs: 0.01,
            retries: 0,
            short_circuit_backoff_secs: 0.0,
        };
        let mut permanent = ScriptedPost {
            responses: vec![HttpResult {
                ok: false,
                status: Some(422),
                retry_after: None,
            }],
            bodies: Default::default(),
        };
        let (status, detail) = deliver(&mut permanent, "u", "b", "application/json", &tuning);
        assert_eq!(status, DROPPED);
        assert_eq!(detail, "http 422");
        let mut transient = ScriptedPost {
            responses: vec![HttpResult {
                ok: false,
                status: None,
                retry_after: None,
            }],
            bodies: Default::default(),
        };
        let (status, detail) = deliver(&mut transient, "u", "b", "application/json", &tuning);
        assert_eq!(status, SHORT_CIRCUIT);
        assert_eq!(detail, "exhausted retries (connect-class)");
    }

    #[test]
    fn progress_line_joins_kind_outcome_reason() {
        let event = json!({"type": "task_done", "outcome": "ok",
                           "data": {"reason": "ci red"}});
        assert_eq!(progress_line(&event), "task_done - ok - ci red");
    }
}
