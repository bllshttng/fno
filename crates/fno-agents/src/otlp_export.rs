//! The OTLP export arm: the feed window ships as one OTLP/HTTP JSON logs
//! batch per beat to a user-named collector. Every gate must open before a
//! byte leaves: an opt-in `[telemetry.otlp] enabled = true`, an http(s)
//! endpoint, and a non-empty headers env (auth rides there, never in the
//! config file). Identity fields (ids, kinds, enums) always export; free
//! text only with `log_content`. Delivery is at-least-once keyed on
//! `fno.row.id`, so a redelivered batch dedupes downstream.

use crate::feed::FeedRow;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

/// How far back each beat reads, regardless of what already shipped: a
/// collector outage older than this forgets its backlog.
const LOOKBACK_S: u64 = 86_400;
/// Rows per post. One beat posts at most one batch; the rest wait.
const MAX_BATCH: usize = 1000;
/// The env var carrying the post headers as `k=v,k2=v2`. Overridable in
/// config (`headers_env`) so two collectors can hold different secrets.
const HEADERS_ENV: &str = "FNO_OTLP_HEADERS";
/// Delivered and dropped ids older than this prune from the state maps.
const STATE_TTL_HOURS: i64 = 48;
/// The arm's cadence; also the tick row's declared interval.
pub const OTLP_INTERVAL_S: u64 = 300;
/// A tick exceeding this emits a stuck row naming the stage.
const OTLP_TICK_BUDGET_S: u64 = 300;

/// The config verdict for one beat. `Refused` means the operator asked for
/// an export the config cannot express: the beat reports it and sends
/// nothing, every beat, until the config heals.
pub enum Resolved {
    Off { forced_off_by: Option<&'static str> },
    Refused(String),
    On(OtlpConfig),
}

/// One beat's export posture, resolved once per tick.
pub struct OtlpConfig {
    pub endpoint: String,
    pub headers: Vec<(String, String)>,
    pub log_content: bool,
    pub host: String,
}

/// Gates in order: env kill switches first (`OTEL_SDK_DISABLED`,
/// `DO_NOT_TRACK`), because a machine saying "no telemetry" outranks any
/// config file. Then the opt-in flag, then the endpoint scheme, then the
/// headers env: an unset or empty value refuses rather than shipping an
/// unauthenticated export to whatever the endpoint is.
pub fn resolve(cwd: &Path) -> Resolved {
    for var in ["OTEL_SDK_DISABLED", "DO_NOT_TRACK"] {
        let val = std::env::var(var).unwrap_or_default();
        let val = val.trim().to_ascii_lowercase();
        if val == "true" || val == "1" {
            return Resolved::Off {
                forced_off_by: Some(var),
            };
        }
    }
    if !crate::agents_config::otlp_export_enabled(cwd) {
        return Resolved::Off {
            forced_off_by: None,
        };
    }
    let table = match crate::agents_config::config_lookup(cwd, &["telemetry", "otlp"]) {
        Some(t) => t,
        None => {
            return Resolved::Off {
                forced_off_by: None,
            }
        }
    };
    let endpoint = table
        .get("endpoint")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .trim()
        .to_string();
    if endpoint.is_empty() {
        return Resolved::Refused("telemetry.otlp.endpoint is empty".to_string());
    }
    if !(endpoint.starts_with("http://") || endpoint.starts_with("https://")) {
        return Resolved::Refused(format!(
            "telemetry.otlp.endpoint must be http(s), got: {endpoint}"
        ));
    }
    let mut endpoint = endpoint;
    // The logs path is fixed; a bare collector URL is the whole signal.
    if !endpoint.ends_with("/v1/logs") {
        endpoint.push_str("/v1/logs");
    }
    let headers_env = table
        .get("headers_env")
        .and_then(|v| v.as_str())
        .unwrap_or(HEADERS_ENV)
        .trim()
        .to_string();
    let raw = std::env::var(&headers_env).unwrap_or_default();
    if raw.trim().is_empty() {
        return Resolved::Refused(format!(
            "headers env {headers_env} is unset or empty; the export would be unauthenticated"
        ));
    }
    let mut headers = Vec::new();
    for pair in raw.split(',') {
        let pair = pair.trim();
        if pair.is_empty() {
            continue;
        }
        let Some((name, value)) = pair.split_once('=') else {
            return Resolved::Refused(format!("headers env entry has no '=': {pair}"));
        };
        let (name, value) = (name.trim(), value.trim());
        if name.is_empty() || value.is_empty() {
            return Resolved::Refused(format!("headers env entry has an empty side: {pair}"));
        }
        headers.push((name.to_string(), value.to_string()));
    }
    let log_content = table
        .get("log_content")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let rest = endpoint
        .split_once("://")
        .map(|(_, r)| r)
        .unwrap_or(&endpoint);
    let host = rest.split('/').next().unwrap_or(rest).to_string();
    Resolved::On(OtlpConfig {
        endpoint,
        headers,
        log_content,
        host,
    })
}

/// The stable row id: sha256 over the identity fields (kind, ts, node,
/// session, ref, team, holder, name), 0x1f-joined, hex. Title is excluded so
/// a title edit does not resend the row.
pub(crate) fn row_id(row: &FeedRow) -> String {
    let parts = [
        row.kind.as_str(),
        row.ts.as_str(),
        row.node.as_deref().unwrap_or(""),
        row.session_id.as_deref().unwrap_or(""),
        row.r#ref.as_deref().unwrap_or(""),
        row.team.as_deref().unwrap_or(""),
        row.holder.as_deref().unwrap_or(""),
        row.name.as_deref().unwrap_or(""),
    ];
    let joined = parts.join("\u{1f}");
    let digest = Sha256::digest(joined.as_bytes());
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

fn str_attr(key: &str, value: &str) -> Value {
    json!({"key": key, "value": {"stringValue": value}})
}

fn record(row: &FeedRow, log_content: bool, now_nanos: u128) -> Value {
    let time_nanos = crate::event_store::parse_rfc3339_ms(&row.ts)
        .map(|ms| (ms as u128) * 1_000_000)
        .map(|n| n.to_string())
        .unwrap_or_default();
    let mut attrs = vec![
        str_attr("fno.kind", &row.kind),
        str_attr("fno.area", &row.area),
        str_attr("session.id", row.session_id.as_deref().unwrap_or("")),
        str_attr("fno.row.id", &row_id(row)),
        json!({
            "key": "event.name",
            "value": {"stringValue": format!("fno.{}", row.kind)}
        }),
    ];
    for (key, value) in [
        ("fno.node.id", row.node.as_deref()),
        ("fno.harness", row.harness.as_deref()),
        ("fno.model", row.model.as_deref()),
        ("fno.effort", row.effort.as_deref()),
        ("fno.phase", row.phase.as_deref()),
        ("fno.team", row.team.as_deref()),
        ("fno.actor", row.actor.as_deref()),
        ("fno.parent.session.id", row.parent.as_deref()),
        ("fno.ref", row.r#ref.as_deref()),
    ] {
        if let Some(v) = value {
            attrs.push(str_attr(key, v));
        }
    }
    if log_content {
        for (key, value) in [
            ("fno.title", Some(row.title.as_str())),
            ("fno.cwd", row.cwd.as_deref()),
            ("fno.reason", row.reason.as_deref()),
            ("fno.detail", row.detail.as_deref()),
            ("fno.url", row.url.as_deref()),
            ("fno.name", row.name.as_deref()),
            ("fno.holder", row.holder.as_deref()),
            ("fno.owner", row.owner.as_deref()),
            ("fno.lead", row.lead.as_deref()),
        ] {
            if let Some(v) = value {
                attrs.push(str_attr(key, v));
            }
        }
    }
    json!({
        "timeUnixNano": time_nanos,
        "observedTimeUnixNano": now_nanos.to_string(),
        "severityNumber": 9,
        "severityText": "INFO",
        "eventName": format!("fno.{}", row.kind),
        "body": {"stringValue": if log_content { row.title.as_str() } else { row.kind.as_str() }},
        "attributes": attrs,
    })
}

/// The OTLP/HTTP JSON logs request body for one batch: one resource, one
/// scope, one record per row.
pub(crate) fn body(rows: &[&FeedRow], log_content: bool) -> String {
    let now_nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let records: Vec<Value> = rows
        .iter()
        .map(|r| record(r, log_content, now_nanos))
        .collect();
    let doc = json!({
        "resourceLogs": [{
            "resource": {"attributes": [
                str_attr("service.name", "fno"),
                str_attr("service.version", env!("CARGO_PKG_VERSION")),
            ]},
            "scopeLogs": [{
                "scope": {"name": "fno.feed"},
                "logRecords": records,
            }],
        }],
    });
    serde_json::to_string(&doc).expect("serializing an owned value never fails")
}

/// What one beat remembers: the ids already delivered (never resent), the
/// ids the endpoint permanently rejected (never retried), the newest row ts
/// shipped, and the last held error for `status_line`. The 48h prune keeps
/// the maps bounded; the collector side dedupes on `fno.row.id`.
#[derive(Default, Serialize, Deserialize)]
struct ExportState {
    #[serde(default)]
    delivered: BTreeMap<String, String>,
    #[serde(default)]
    dropped: BTreeMap<String, String>,
    #[serde(default)]
    last_delivered_ts: Option<String>,
    #[serde(default)]
    held_error: Option<String>,
}

fn state_path(home: &crate::paths::AgentsHome) -> PathBuf {
    home.otlp_export_dir().join("state.json")
}

fn receipts_path(home: &crate::paths::AgentsHome) -> PathBuf {
    home.otlp_export_dir().join("receipts.jsonl")
}

fn load_state(home: &crate::paths::AgentsHome) -> ExportState {
    std::fs::read_to_string(state_path(home))
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

/// Atomic write: tmp file then rename, the attention save_state shape. The
/// export dir is created on first save; a failed save silently reads as an
/// empty state next beat, and at-least-once delivery absorbs the re-send.
fn save_state(home: &crate::paths::AgentsHome, state: &ExportState) {
    let dir = home.otlp_export_dir();
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    let path = state_path(home);
    let tmp = path.with_extension("json.tmp");
    if let Ok(text) = serde_json::to_string(state) {
        if std::fs::write(&tmp, text).is_ok() {
            let _ = std::fs::rename(&tmp, &path);
        }
    }
}

fn append_receipt(home: &crate::paths::AgentsHome, row: &Value) {
    let dir = home.otlp_export_dir();
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    use std::io::Write;
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(receipts_path(home))
    {
        let _ = writeln!(f, "{row}");
    }
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn now_rfc3339() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

fn prune(state: &mut ExportState, now_ms: i64) {
    let cutoff = now_ms - STATE_TTL_HOURS * 3600 * 1000;
    let live =
        |ts: &String| crate::event_store::parse_rfc3339_ms(ts).is_some_and(|ms| ms >= cutoff);
    state.delivered.retain(|_, ts| live(ts));
    state.dropped.retain(|_, ts| live(ts));
}

/// One beat's outcome for the tick row.
pub(crate) struct BeatReport {
    pub acted: u64,
    pub skip: Option<&'static str>,
    pub detail: Vec<String>,
}

/// The rows a beat owes: dated, inside the lookback window, neither
/// delivered nor dropped, oldest first, capped at MAX_BATCH. run_beat and
/// `preview` share this selection, so a preview prints exactly the batch
/// the next beat posts. The detail lines name what selection left behind.
fn select_pending<'a>(
    rows: &'a [FeedRow],
    state: &ExportState,
    now_ms: i64,
) -> (Vec<(String, &'a FeedRow)>, Vec<String>) {
    let mut detail: Vec<String> = Vec::new();
    let cutoff = now_ms - LOOKBACK_S as i64 * 1000;
    let mut undated = 0usize;
    let mut pending: Vec<(String, &FeedRow)> = Vec::new();
    for r in rows {
        let Some(ms) = crate::event_store::parse_rfc3339_ms(&r.ts) else {
            undated += 1;
            continue;
        };
        if ms < cutoff {
            continue;
        }
        let id = row_id(r);
        if state.delivered.contains_key(&id) || state.dropped.contains_key(&id) {
            continue;
        }
        pending.push((id, r));
    }
    if undated > 0 {
        detail.push(format!("otlp: {undated} undated rows skipped"));
    }
    // Oldest first: the export catches up in feed order.
    pending.sort_by_key(|(_, r)| crate::event_store::parse_rfc3339_ms(&r.ts).unwrap_or(i64::MAX));
    if pending.len() > MAX_BATCH {
        detail.push(format!(
            "otlp: {} rows beyond the batch cap wait for the next beat",
            pending.len() - MAX_BATCH
        ));
        pending.truncate(MAX_BATCH);
    }
    (pending, detail)
}

/// One beat: dedupe against state, post the pending window once, record the
/// verdict. At-least-once: a hold (no status, 5xx) ships nothing and
/// retries next beat; a permanent 4xx drops the rows (never retried); a 2xx
/// records delivered. Undated rows never ship: `timeUnixNano` cannot lie
/// about them. Selection is `select_pending`, shared with `preview`.
pub(crate) fn run_beat(
    cfg: &OtlpConfig,
    home: &crate::paths::AgentsHome,
    rows: Vec<FeedRow>,
    poster: &mut dyn crate::attention_http::HttpPost,
    now_ms: i64,
) -> BeatReport {
    let mut state = load_state(home);
    let (pending, mut detail) = select_pending(&rows, &state, now_ms);
    if pending.is_empty() {
        let skip = if rows.is_empty() {
            "no_rows"
        } else {
            "caught_up"
        };
        if skip == "no_rows" {
            detail.push(format!(
                "otlp: nothing in the {}h window",
                LOOKBACK_S / 3600
            ));
        } else {
            detail.push("otlp: every row in the window already shipped".to_string());
        }
        return BeatReport {
            acted: 0,
            skip: Some(skip),
            detail,
        };
    }
    let payload = body(
        &pending.iter().map(|(_, r)| *r).collect::<Vec<_>>(),
        cfg.log_content,
    );
    let result = poster.post(&cfg.endpoint, &cfg.headers, &payload);
    let mut acted: u64 = 0;
    let mut skip: Option<&'static str> = None;
    if result.delivered() {
        for (id, r) in &pending {
            state.delivered.insert(id.clone(), r.ts.clone());
        }
        state.last_delivered_ts = pending.last().map(|(_, r)| r.ts.clone());
        state.held_error = None;
        acted = pending.len() as u64;
        detail.push(format!(
            "otlp: delivered {} record(s) to {}",
            pending.len(),
            cfg.host
        ));
    } else if result.dropped() {
        for (id, r) in &pending {
            state.dropped.insert(id.clone(), r.ts.clone());
        }
        state.held_error = None;
        acted = pending.len() as u64;
        detail.push(format!(
            "otlp: endpoint rejected {} record(s) permanently (status {})",
            pending.len(),
            result.status.unwrap_or(0)
        ));
    } else {
        let body_summary: String = result.body.chars().take(200).collect();
        state.held_error = Some(match result.status {
            Some(code) => format!("status {code}: {body_summary}"),
            None => format!("no status: {body_summary}"),
        });
        skip = Some("held");
        detail.push(format!("otlp: held {} record(s) for retry", pending.len()));
    }
    prune(&mut state, now_ms);
    append_receipt(
        home,
        &json!({
            "ts": now_rfc3339(),
            "records": pending.len(),
            "status": result.status,
            "first_row_ts": pending.first().map(|(_, r)| r.ts.clone()).unwrap_or_default(),
            "last_row_ts": pending.last().map(|(_, r)| r.ts.clone()).unwrap_or_default(),
        }),
    );
    save_state(home, &state);
    BeatReport {
        acted,
        skip,
        detail,
    }
}

/// What the next beat would post, for `fno agents feed --otlp`: the same
/// resolve, state and selection as run_beat, minus the post and the state
/// write. An off or refused posture still previews, with content attrs
/// off, so an operator sees the shape before opting in.
pub(crate) fn preview(
    cwd: &Path,
    home: &crate::paths::AgentsHome,
    rows: &[FeedRow],
) -> (String, Vec<String>) {
    let log_content = match resolve(cwd) {
        Resolved::On(cfg) => cfg.log_content,
        _ => false,
    };
    let state = load_state(home);
    let (pending, detail) = select_pending(rows, &state, now_ms());
    let payload = body(
        &pending.iter().map(|(_, r)| *r).collect::<Vec<_>>(),
        log_content,
    );
    (payload, detail)
}

/// The one-line posture `fno agents feed --otlp` prints to stderr.
pub(crate) fn status_line(cwd: &Path, home: &crate::paths::AgentsHome) -> String {
    match resolve(cwd) {
        Resolved::Off {
            forced_off_by: Some(var),
        } => format!("otlp export: off - forced off by {var}"),
        Resolved::Off {
            forced_off_by: None,
        } => "otlp export: off - nothing leaves this machine".to_string(),
        Resolved::Refused(reason) => format!("otlp export: refused: {reason}"),
        Resolved::On(cfg) => {
            let state = load_state(home);
            let last = state.last_delivered_ts.as_deref().unwrap_or("never");
            let held = state.held_error.as_deref().unwrap_or("none");
            format!(
                "otlp export: on -> {}, content {}, last delivered {}, held: {}",
                cfg.host,
                if cfg.log_content { "on" } else { "off" },
                last,
                held
            )
        }
    }
}

/// The arm state mirrors the attention arm: one config cwd, a due check, a
/// one-in-flight gate, a stage label for the stuck row.
pub struct Arm {
    config_cwd: PathBuf,
    last_tick: Mutex<Option<Instant>>,
    in_flight: Arc<AtomicBool>,
    stage: Arc<Mutex<Option<(&'static str, Instant)>>>,
}

impl Arm {
    pub fn new(config_cwd: PathBuf) -> Self {
        Self {
            config_cwd,
            last_tick: Mutex::new(None),
            in_flight: Arc::new(AtomicBool::new(false)),
            stage: Arc::new(Mutex::new(None)),
        }
    }
}

fn stuck_detail(stage: Option<(&'static str, Instant)>, budget_s: u64) -> Option<String> {
    let (name, started) = stage?;
    let secs = started.elapsed().as_secs();
    (secs > budget_s).then(|| format!("tick still in stage {name} after {secs}s"))
}

fn emit_tick_row(
    home: &crate::paths::AgentsHome,
    acted: u64,
    skip_reason: Option<&str>,
    detail: &str,
) {
    let journal = crate::loop_runtime::Journal::new_raw(
        home.events_jsonl(),
        crate::daemon::global_events_path(home),
    );
    crate::tick_ledger::emit_tick(
        &journal,
        "otlp_export",
        crate::tick_ledger::SCHED_DAEMON,
        acted,
        skip_reason,
        Some(detail),
        OTLP_INTERVAL_S,
    );
}

/// The daemon-facing wrapper, the attention shape: due check, one-in-flight
/// gate, the body off-loop behind `spawn_blocking`.
pub fn maybe_tick(arm: &Arm, home: crate::paths::AgentsHome) {
    let interval = std::time::Duration::from_secs(OTLP_INTERVAL_S);
    {
        let mut last = arm.last_tick.lock().unwrap_or_else(|e| e.into_inner());
        if last.is_some_and(|t| t.elapsed() < interval) {
            return;
        }
        *last = Some(Instant::now());
    }
    if arm.in_flight.swap(true, Ordering::SeqCst) {
        let stage = arm.stage.lock().unwrap_or_else(|e| e.into_inner()).clone();
        if let Some(d) = stuck_detail(stage, OTLP_TICK_BUDGET_S) {
            emit_tick_row(&home, 0, Some("timeout"), &d);
        }
        return;
    }
    let flag = Arc::clone(&arm.in_flight);
    let stage = Arc::clone(&arm.stage);
    let cwd = arm.config_cwd.clone();
    tokio::task::spawn_blocking(move || {
        let _gate = crate::daemon::SweepGate(flag);
        let started = Instant::now();
        let mark = |name: &'static str| {
            *stage.lock().unwrap_or_else(|e| e.into_inner()) = Some((name, started));
        };
        mark("resolve");
        match resolve(&cwd) {
            Resolved::Off { forced_off_by } => {
                mark("emit");
                let why = forced_off_by
                    .map(|v| format!("forced off by {v}"))
                    .unwrap_or_else(|| "telemetry.otlp.enabled is false".to_string());
                emit_tick_row(&home, 0, Some("disabled"), &why);
            }
            Resolved::Refused(reason) => {
                mark("emit");
                emit_tick_row(
                    &home,
                    0,
                    Some("config"),
                    &format!("otlp export refused: {reason}"),
                );
            }
            Resolved::On(cfg) => {
                mark("collect");
                let now = now_ms();
                let since_s = (now / 1000).saturating_sub(LOOKBACK_S as i64) as u64;
                let (rows, leg_notes) = crate::feed::collect_window(&home, Some(since_s), None);
                mark("post");
                let mut poster = crate::attention_http::CurlPost;
                let report = run_beat(&cfg, &home, rows, &mut poster, now);
                mark("emit");
                // A store-skip note explains a thin window; it reads first.
                let mut detail = leg_notes;
                detail.extend(report.detail);
                emit_tick_row(&home, report.acted, report.skip, &detail.join("; "));
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn beat_cfg() -> OtlpConfig {
        OtlpConfig {
            endpoint: "https://collector.example/v1/logs".to_string(),
            headers: Vec::new(),
            log_content: false,
            host: "collector.example".to_string(),
        }
    }

    fn row(ts: &str, kind: &str, node: &str) -> FeedRow {
        FeedRow {
            ts: ts.to_string(),
            kind: kind.to_string(),
            title: format!("{kind} {node}"),
            node: Some(node.to_string()),
            ..Default::default()
        }
    }

    /// `observedTimeUnixNano` is the wall clock at body-build time, so two
    /// builds never match byte for byte; compare everything but that stamp.
    fn sans_observed_time(payload: &str) -> Value {
        let mut doc: Value = serde_json::from_str(payload).unwrap();
        let records = doc["resourceLogs"][0]["scopeLogs"][0]["logRecords"]
            .as_array_mut()
            .unwrap();
        for r in records {
            r["observedTimeUnixNano"] = json!("0");
        }
        doc
    }

    #[derive(Default)]
    struct FakePost {
        status: Option<u16>,
        body: String,
        calls: usize,
        posted: Vec<String>,
    }

    impl crate::attention_http::HttpPost for FakePost {
        fn post(
            &mut self,
            _url: &str,
            _headers: &[(String, String)],
            _body: &str,
        ) -> crate::attention_http::PostResult {
            self.calls += 1;
            self.posted.push(_body.to_string());
            crate::attention_http::PostResult {
                status: self.status,
                body: self.body.clone(),
            }
        }
    }

    static ENV_LOCK: std::sync::LazyLock<&'static std::sync::Mutex<()>> =
        std::sync::LazyLock::new(crate::claims::test_env_lock);

    /// Snapshot the vars the gate chain reads, so a test can set and restore
    /// them without leaking state to a parallel test.
    fn hermetic_env() -> Vec<(&'static str, Option<std::ffi::OsString>)> {
        [
            "FNO_CONFIG",
            "FNO_NO_CANONICAL_CONFIG",
            "FNO_GLOBAL_SETTINGS_PATH",
            "FNO_OTLP_HEADERS",
            "FNO_TEST_OTLP_HEADERS",
            "OTEL_SDK_DISABLED",
            "DO_NOT_TRACK",
        ]
        .iter()
        .map(|k| (*k, std::env::var_os(k)))
        .collect()
    }

    fn restore_env(saved: &[(&'static str, Option<std::ffi::OsString>)]) {
        for (k, v) in saved {
            match v {
                Some(v) => std::env::set_var(k, v),
                None => std::env::remove_var(k),
            }
        }
    }

    /// Write a config file and point $FNO_CONFIG at it: the sole candidate
    /// when set, read as-is.
    fn write_cfg(dir: &Path, toml: &str) {
        std::fs::write(dir.join("config.toml"), toml).unwrap();
        std::env::set_var("FNO_CONFIG", dir.join("config.toml"));
    }

    /// The three status classes decide the row's fate: a 2xx delivers and
    /// never resends, a permanent 4xx drops and never resends, a 5xx holds
    /// and retries the same row on a later beat.
    #[test]
    fn a_beat_delivers_drops_and_holds_by_status_class() {
        let dir = tempfile::tempdir().unwrap();
        let home = crate::paths::AgentsHome::at(dir.path());
        let rows = vec![
            row("2026-10-12T01:00:00Z", "node_shipped", "n1"),
            row("2026-10-12T02:00:00Z", "node_started", "n2"),
        ];
        let mut poster = FakePost {
            status: Some(200),
            ..Default::default()
        };

        let r = run_beat(
            &beat_cfg(),
            &home,
            rows.clone(),
            &mut poster,
            1_760_000_000_000,
        );
        assert_eq!(r.acted, 2);
        assert_eq!(r.skip, None);
        let state = load_state(&home);
        assert_eq!(state.delivered.len(), 2);
        assert_eq!(
            state.last_delivered_ts.as_deref(),
            Some("2026-10-12T02:00:00Z")
        );

        // The same window again: caught up, nothing posted.
        let r = run_beat(
            &beat_cfg(),
            &home,
            rows.clone(),
            &mut poster,
            1_760_000_000_000,
        );
        assert_eq!(r.skip, Some("caught_up"));
        assert_eq!(poster.calls, 1);

        // A permanent 4xx drops the row; the next beat does not resend it.
        let fresh = vec![row("2026-10-12T03:00:00Z", "node_ended", "n3")];
        poster.status = Some(400);
        let r = run_beat(
            &beat_cfg(),
            &home,
            fresh.clone(),
            &mut poster,
            1_760_000_000_000,
        );
        assert_eq!(r.acted, 1);
        assert_eq!(load_state(&home).dropped.len(), 1);
        let r = run_beat(
            &beat_cfg(),
            &home,
            fresh.clone(),
            &mut poster,
            1_760_000_000_000,
        );
        assert_eq!(r.skip, Some("caught_up"));
        assert_eq!(poster.calls, 2);

        // A 5xx holds: nothing recorded, the same row retries and delivers.
        let held = vec![row("2026-10-12T04:00:00Z", "session_spawned", "n4")];
        poster.status = Some(503);
        let r = run_beat(
            &beat_cfg(),
            &home,
            held.clone(),
            &mut poster,
            1_760_000_000_000,
        );
        assert_eq!(r.skip, Some("held"));
        let state = load_state(&home);
        assert_eq!(state.delivered.len(), 2);
        assert_eq!(state.dropped.len(), 1);
        assert!(state.held_error.is_some());
        poster.status = Some(204);
        let r = run_beat(&beat_cfg(), &home, held, &mut poster, 1_760_000_000_000);
        assert_eq!(r.acted, 1);
        // Four posts: deliver, drop, the held attempt, then this one.
        assert_eq!(poster.calls, 4);
    }

    /// `log_content` gates every free-text attribute and the body; the row
    /// id is blind to the title, so a title edit cannot resend a row.
    #[test]
    fn content_attrs_gate_on_log_content_and_row_id_ignores_the_title() {
        let mut r = FeedRow {
            ts: "2026-10-12T01:00:00Z".to_string(),
            kind: "node_shipped".to_string(),
            title: "the title".to_string(),
            node: Some("n1".to_string()),
            session_id: Some("s1".to_string()),
            detail: Some("the detail".to_string()),
            ..Default::default()
        };
        let off = body(&[&r], false);
        assert!(off.contains("fno.node.id"), "identity attrs always export");
        assert!(
            !off.contains("the detail"),
            "content stays home with log_content off"
        );
        assert!(
            !off.contains("the title"),
            "the body is the kind with log_content off"
        );
        let on = body(&[&r], true);
        assert!(on.contains("the detail"));
        let id = row_id(&r);
        r.title = "edited".to_string();
        assert_eq!(row_id(&r), id, "a title edit must not resend the row");
    }

    /// The preview and the beat agree: same selection, same order, same
    /// body. An undated row stays home with a note; a delivered row is not
    /// sent twice. Rows carry a far-future stamp so the real clock in
    /// `preview` never drops them from the lookback window.
    #[test]
    fn preview_matches_the_beat_body_and_selection() {
        let _env = ENV_LOCK.lock().unwrap();
        let saved = hermetic_env();
        std::env::set_var("FNO_NO_CANONICAL_CONFIG", "1");
        std::env::remove_var("FNO_CONFIG");
        std::env::remove_var("FNO_GLOBAL_SETTINGS_PATH");

        let dir = tempfile::tempdir().unwrap();
        let home = crate::paths::AgentsHome::at(dir.path());
        let mut rows = vec![
            row("2099-01-01T00:00:00Z", "node_shipped", "n1"),
            row("2099-01-01T00:01:00Z", "node_started", "n2"),
        ];
        rows.push(FeedRow {
            kind: "node_started".to_string(),
            title: "node_started n3".to_string(),
            node: Some("n3".to_string()),
            ..Default::default()
        });

        // The preview shows the two dated rows, keeps the undated one home
        // with a note, and never sends.
        let (body, detail) = preview(Path::new("/"), &home, &rows);
        assert!(body.contains("n1") && body.contains("n2"));
        assert!(!body.contains("n3"), "an undated row never ships");
        assert!(
            detail.iter().any(|d| d.contains("undated")),
            "the undated row is named in the detail: {detail:?}"
        );

        // The beat posts exactly what the preview printed.
        let mut poster = FakePost {
            status: Some(200),
            ..Default::default()
        };
        let report = run_beat(
            &beat_cfg(),
            &home,
            rows.clone(),
            &mut poster,
            1_760_000_000_000,
        );
        assert_eq!(
            report.acted, 2,
            "two dated rows post; the undated one stays home"
        );
        assert_eq!(
            sans_observed_time(&body),
            sans_observed_time(&poster.posted[0]),
            "preview equals the posted body but for the observation time"
        );

        // After the beat the preview is quiet: delivered rows do not resend.
        let (again, _) = preview(Path::new("/"), &home, &rows);
        assert!(
            !again.contains("n1") && !again.contains("n2"),
            "delivered rows do not resend"
        );

        restore_env(&saved);
    }

    /// The gate chain decides in a fixed order: kill switches first, then
    /// the enabled flag, then endpoint shape, then headers. Every refusal
    /// names what it read; every kill switch outranks the config file.
    #[test]
    fn resolve_gates() {
        let _env = ENV_LOCK.lock().unwrap();
        let saved = hermetic_env();
        std::env::set_var("FNO_NO_CANONICAL_CONFIG", "1");
        std::env::remove_var("FNO_CONFIG");
        std::env::remove_var("FNO_GLOBAL_SETTINGS_PATH");
        std::env::remove_var("OTEL_SDK_DISABLED");
        std::env::remove_var("DO_NOT_TRACK");
        let dir = tempfile::tempdir().unwrap();
        let cwd = dir.path();

        // No config: off, with no switch to blame.
        assert!(
            matches!(
                resolve(cwd),
                Resolved::Off {
                    forced_off_by: None
                }
            ),
            "no config reads off with no switch named"
        );

        // enabled = true: past the enabled gate; headers unset refuses
        // rather than shipping an unauthenticated export.
        write_cfg(
            cwd,
            "[telemetry.otlp]\nenabled = true\nendpoint = \"https://collector.example/v1/logs\"\n",
        );
        assert!(
            matches!(resolve(cwd), Resolved::Refused(ref msg) if msg.contains("FNO_OTLP_HEADERS")),
            "unset headers refuse the unauthenticated export"
        );

        // A kill switch outranks the enabled config and names itself.
        std::env::set_var("DO_NOT_TRACK", "1");
        assert!(
            matches!(
                resolve(cwd),
                Resolved::Off {
                    forced_off_by: Some("DO_NOT_TRACK")
                }
            ),
            "DO_NOT_TRACK outranks the enabled config"
        );
        std::env::set_var("OTEL_SDK_DISABLED", "true");
        std::env::remove_var("DO_NOT_TRACK");
        assert!(
            matches!(
                resolve(cwd),
                Resolved::Off {
                    forced_off_by: Some("OTEL_SDK_DISABLED")
                }
            ),
            "OTEL_SDK_DISABLED outranks the enabled config"
        );

        // The full chain passes: endpoint, host, headers, content gate.
        std::env::remove_var("OTEL_SDK_DISABLED");
        std::env::set_var("FNO_OTLP_HEADERS", "Authorization=Bearer t, X-One=1");
        match resolve(cwd) {
            Resolved::On(cfg) => {
                assert_eq!(cfg.endpoint, "https://collector.example/v1/logs");
                assert_eq!(cfg.host, "collector.example");
                assert!(!cfg.log_content);
                assert_eq!(
                    cfg.headers,
                    vec![
                        ("Authorization".to_string(), "Bearer t".to_string()),
                        ("X-One".to_string(), "1".to_string())
                    ]
                );
            }
            Resolved::Off { .. } | Resolved::Refused(_) => {
                panic!("enabled with headers resolves on")
            }
        }

        // A non-http endpoint refuses.
        write_cfg(
            cwd,
            "[telemetry.otlp]\nenabled = true\nendpoint = \"ftp://collector.example\"\n",
        );
        assert!(
            matches!(resolve(cwd), Resolved::Refused(_)),
            "a non-http endpoint refuses"
        );

        // A header entry with no '=' refuses.
        write_cfg(
            cwd,
            "[telemetry.otlp]\nenabled = true\nendpoint = \"https://collector.example/v1/logs\"\n",
        );
        std::env::set_var("FNO_OTLP_HEADERS", "novalue");
        assert!(
            matches!(resolve(cwd), Resolved::Refused(ref msg) if msg.contains("novalue")),
            "a header entry without '=' refuses"
        );

        // A headers_env override reroutes where headers come from.
        write_cfg(
            cwd,
            "[telemetry.otlp]\nenabled = true\nendpoint = \"https://collector.example/v1/logs\"\nheaders_env = \"FNO_TEST_OTLP_HEADERS\"\n",
        );
        std::env::remove_var("FNO_OTLP_HEADERS");
        assert!(
            matches!(resolve(cwd), Resolved::Refused(ref msg) if msg.contains("FNO_TEST_OTLP_HEADERS")),
            "the override env is the one that gets read"
        );
        std::env::set_var("FNO_TEST_OTLP_HEADERS", "a=b");
        assert!(matches!(resolve(cwd), Resolved::On(_)));

        restore_env(&saved);
    }
}
