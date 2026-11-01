//! The provider outage instrument: transcript and pane evidence, folded to breakers.

mod fold;

pub use fold::{fold_provider_outages, measure_and_persist};

use crate::accounts::error_class::reset_epoch_from;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

pub const FUP_QUORUM_WINDOW_S: f64 = 300.0;
pub const OVERLOAD_QUORUM_WINDOW_S: f64 = 600.0;
pub const OVERLOAD_PERSISTENCE_S: f64 = 120.0;
pub const PANE_FRESHNESS_S: f64 = 120.0;
const MAX_PANE_SNAPSHOT_BYTES: usize = 64 * 1024;
const MAX_PANE_SNAPSHOTS: usize = 512;
const TRANSCRIPT_TAIL_BYTES: u64 = 256 * 1024;

/// Validated recovery settings consumed by the pure outage fold.
#[derive(Clone, Debug, PartialEq)]
pub struct OutagePolicy {
    pub quorum: i64,
    pub fup_window_s: f64,
    pub overload_count: i64,
    pub overload_span_s: f64,
    pub overload_cross_session_window_s: f64,
    pub pane_freshness_s: f64,
    pub evidence_freshness_s: f64,
    pub reset_grace_s: f64,
    pub health_marker_ttl_s: f64,
}

impl Default for OutagePolicy {
    fn default() -> Self {
        OutagePolicy {
            quorum: 2,
            fup_window_s: FUP_QUORUM_WINDOW_S,
            overload_count: 3,
            overload_span_s: OVERLOAD_PERSISTENCE_S,
            overload_cross_session_window_s: OVERLOAD_QUORUM_WINDOW_S,
            pane_freshness_s: PANE_FRESHNESS_S,
            evidence_freshness_s: 600.0,
            reset_grace_s: 120.0,
            health_marker_ttl_s: PANE_FRESHNESS_S,
        }
    }
}

/// One `recovery.<key>` integer: absent is the default, below its floor is `Err`.
fn recovery_int(cwd: &Path, key: &str, floor: i64, default: i64) -> Result<i64, ()> {
    let Some(value) = crate::agents_config::config_lookup(cwd, &["recovery", key]) else {
        return Ok(default);
    };
    let parsed = match &value {
        toml::Value::Integer(i) => Some(*i),
        toml::Value::Float(f) if f.fract() == 0.0 => Some(*f as i64),
        toml::Value::String(s) => s.trim().parse::<i64>().ok(),
        _ => None,
    };
    match parsed {
        Some(i) if i >= floor => Ok(i),
        _ => Err(()),
    }
}

impl OutagePolicy {
    /// The `recovery.*` settings. One value below its schema floor refuses the
    /// whole read and answers the defaults, because the settings model refuses
    /// a whole file the same way. The quorum floor is 1, not 2: an operator who
    /// configures 1 has decided a lone worker's outage is authority enough.
    pub fn from_cwd(cwd: &Path) -> Self {
        Self::read(cwd).unwrap_or_default()
    }

    fn read(cwd: &Path) -> Result<Self, ()> {
        Ok(OutagePolicy {
            quorum: recovery_int(cwd, "provider_outage_quorum", 1, 2)?,
            fup_window_s: recovery_int(cwd, "provider_outage_fup_window_seconds", 300, 300)? as f64,
            overload_count: recovery_int(cwd, "provider_outage_529_count", 3, 3)?,
            overload_span_s: recovery_int(cwd, "provider_outage_529_span_seconds", 120, 120)?
                as f64,
            overload_cross_session_window_s: recovery_int(
                cwd,
                "provider_outage_529_cross_session_window_seconds",
                600,
                600,
            )? as f64,
            pane_freshness_s: recovery_int(cwd, "provider_outage_pane_freshness_seconds", 120, 120)?
                as f64,
            evidence_freshness_s: recovery_int(
                cwd,
                "provider_outage_evidence_freshness_seconds",
                600,
                600,
            )? as f64,
            reset_grace_s: recovery_int(cwd, "provider_outage_reset_grace_seconds", 120, 120)?
                as f64,
            health_marker_ttl_s: recovery_int(cwd, "provider_health_marker_ttl_seconds", 120, 120)?
                as f64,
        })
    }
}

/// Machine-recorded axes required to join one transcript to a route.
#[derive(Clone, Debug, PartialEq)]
pub struct EvidenceIdentity {
    pub row_id: String,
    pub harness: String,
    pub provider: Option<String>,
    pub account: Option<String>,
    pub session_id: String,
    pub cwd: String,
}

/// One explicit raw record; provider and account are resolved upstream.
#[derive(Clone, Debug, PartialEq)]
pub struct OutageEvidence {
    pub source: String,
    pub observed_at: f64,
    pub row_id: String,
    pub harness: String,
    pub provider: Option<String>,
    pub account: Option<String>,
    pub role: String,
    pub raw_status: Option<i64>,
    pub raw_kind: String,
    pub content: String,
    pub content_fingerprint: String,
    pub pane_id: Option<String>,
    pub persisted: bool,
    pub snapshot_at: Option<f64>,
    /// Resolved once at collect time through the account's reset_timezone, so
    /// the pure fold can tell a live cap from an expired one without config.
    pub reset_at: Option<f64>,
}

impl OutageEvidence {
    /// Fill an empty `content_fingerprint`. The journal stores these hashes, so
    /// the hashed bytes must stay compact JSON with non-ASCII left raw.
    pub fn sealed(mut self) -> Self {
        if self.content_fingerprint.is_empty() {
            let raw = json!([
                self.source,
                self.observed_at,
                self.row_id,
                self.harness,
                self.provider,
                self.account,
                self.role,
                self.raw_status,
                self.raw_kind,
                self.content,
                self.pane_id,
            ])
            .to_string();
            self.content_fingerprint = format!("{:x}", Sha256::digest(raw.as_bytes()));
        }
        self
    }

    pub fn fingerprint(&self) -> &str {
        &self.content_fingerprint
    }

    /// The journal's record shape, keys in field order.
    pub fn to_json(&self) -> Value {
        json!({
            "source": self.source,
            "observed_at": self.observed_at,
            "row_id": self.row_id,
            "harness": self.harness,
            "provider": self.provider,
            "account": self.account,
            "role": self.role,
            "raw_status": self.raw_status,
            "raw_kind": self.raw_kind,
            "content": self.content,
            "content_fingerprint": self.content_fingerprint,
            "pane_id": self.pane_id,
            "persisted": self.persisted,
            "snapshot_at": self.snapshot_at,
            "reset_at": self.reset_at,
        })
    }

    /// One journal record, or None for an unknown key, a missing required key,
    /// or a field of the wrong type. A bad record never costs the rest.
    pub fn from_json(value: &Value) -> Option<OutageEvidence> {
        const REQUIRED: [&str; 10] = [
            "source",
            "observed_at",
            "row_id",
            "harness",
            "provider",
            "account",
            "role",
            "raw_status",
            "raw_kind",
            "content",
        ];
        const OPTIONAL: [&str; 5] = [
            "content_fingerprint",
            "pane_id",
            "persisted",
            "snapshot_at",
            "reset_at",
        ];
        let map = value.as_object()?;
        if !REQUIRED.iter().all(|key| map.contains_key(*key)) {
            return None;
        }
        let known =
            |key: &String| REQUIRED.contains(&key.as_str()) || OPTIONAL.contains(&key.as_str());
        if !map.keys().all(known) {
            return None;
        }
        let text = |key: &str| -> Option<String> {
            map.get(key).and_then(Value::as_str).map(str::to_string)
        };
        let nullable = |key: &str| -> Option<Option<String>> {
            match map.get(key) {
                None | Some(Value::Null) => Some(None),
                Some(Value::String(s)) => Some(Some(s.clone())),
                Some(_) => None,
            }
        };
        let raw_status = match map.get("raw_status") {
            None | Some(Value::Null) => None,
            Some(status) => Some(status.as_i64()?),
        };
        let number = |key: &str| -> Option<Option<f64>> {
            match map.get(key) {
                None | Some(Value::Null) => Some(None),
                Some(found) => py_number(found).map(Some),
            }
        };
        Some(
            OutageEvidence {
                source: text("source")?,
                observed_at: py_number(map.get("observed_at")?)?,
                row_id: text("row_id")?,
                harness: text("harness")?,
                provider: nullable("provider")?,
                account: nullable("account")?,
                role: text("role")?,
                raw_status,
                raw_kind: text("raw_kind")?,
                content: text("content")?,
                content_fingerprint: text("content_fingerprint").unwrap_or_default(),
                pane_id: nullable("pane_id")?,
                persisted: map
                    .get("persisted")
                    .and_then(Value::as_bool)
                    .unwrap_or(true),
                snapshot_at: number("snapshot_at")?,
                reset_at: number("reset_at")?,
            }
            .sealed(),
        )
    }
}

/// `isinstance(v, (int, float))` as a float; a JSON boolean is a Python int.
pub(super) fn py_number(value: &Value) -> Option<f64> {
    match value {
        Value::Number(n) => n.as_f64(),
        Value::Bool(b) => Some(if *b { 1.0 } else { 0.0 }),
        _ => None,
    }
}

/// `str(v)` for a scalar journal or mux field.
pub(super) fn py_str(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        Value::Null => "None".to_string(),
        Value::Bool(true) => "True".to_string(),
        Value::Bool(false) => "False".to_string(),
        other => other.to_string(),
    }
}

/// `bool(v)` for a JSON value.
pub(super) fn py_truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64() != Some(0.0),
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
    }
}

/// `str.strip()`: Unicode whitespace plus the separators `\x1c`..`\x1f`.
fn py_strip(text: &str) -> &str {
    text.trim_matches(|c: char| c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c))
}

fn message_text(value: Option<&Value>) -> String {
    match value {
        Some(Value::String(s)) => py_strip(s).to_string(),
        Some(Value::Array(parts)) => {
            let joined: Vec<String> = parts
                .iter()
                .filter(|part| part.get("type").and_then(Value::as_str) == Some("text"))
                .map(|part| match part.get("text") {
                    Some(text) if py_truthy(text) => py_strip(&py_str(text)).to_string(),
                    _ => String::new(),
                })
                .collect();
            py_strip(&joined.join(" ")).to_string()
        }
        _ => String::new(),
    }
}

/// `datetime.fromisoformat(v.replace("Z", "+00:00")).timestamp()`. A naive
/// stamp reads local time. An aware stamp divides whole microseconds so the
/// float, and the fingerprint hashed over it, matches the journal's.
fn iso_epoch(value: &str) -> Option<f64> {
    use chrono::TimeZone;
    let folded = value.replace('Z', "+00:00");
    let aware = chrono::DateTime::parse_from_rfc3339(&folded)
        .ok()
        .or_else(|| {
            ["%Y-%m-%dT%H:%M:%S%.f%:z", "%Y-%m-%d %H:%M:%S%.f%:z"]
                .iter()
                .find_map(|fmt| chrono::DateTime::parse_from_str(&folded, fmt).ok())
        });
    if let Some(stamp) = aware {
        return Some(stamp.timestamp_micros() as f64 / 1e6);
    }
    let naive = [
        "%Y-%m-%dT%H:%M:%S%.f",
        "%Y-%m-%d %H:%M:%S%.f",
        "%Y-%m-%dT%H:%M",
        "%Y-%m-%d %H:%M",
    ]
    .iter()
    .find_map(|fmt| chrono::NaiveDateTime::parse_from_str(&folded, fmt).ok())
    .or_else(|| {
        chrono::NaiveDate::parse_from_str(&folded, "%Y-%m-%d")
            .ok()
            .and_then(|date| date.and_hms_opt(0, 0, 0))
    })?;
    let local = chrono::Local.from_local_datetime(&naive).earliest()?;
    Some(local.timestamp() as f64 + f64::from(local.timestamp_subsec_micros()) / 1e6)
}

fn record_epoch(value: Option<&Value>) -> Option<f64> {
    match value? {
        Value::Number(n) => n.as_f64(),
        Value::Bool(b) => Some(if *b { 1.0 } else { 0.0 }),
        Value::String(s) if !s.is_empty() => iso_epoch(s),
        _ => None,
    }
}

/// The status a visible `API Error` line names, for a surface with no status
/// field.
pub fn pane_status(content: &str) -> Option<i64> {
    static STATUS: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let re = STATUS.get_or_init(|| {
        regex::Regex::new(r"(?i)API Error[^\r\n]{0,80}?(?:\b(429|529)\b|\((429|529)\))")
            .expect("the status pattern compiles")
    });
    let found = re.captures(content)?;
    found
        .get(1)
        .or_else(|| found.get(2))?
        .as_str()
        .parse::<i64>()
        .ok()
}

/// The session's own transcript file, for the refusal text.
pub fn resolve_transcript_path(identity: &EvidenceIdentity) -> Option<PathBuf> {
    crate::backlog::birth::resolve_transcript_detail(
        &identity.harness,
        &identity.session_id,
        &identity.cwd,
    )
    .path
}

/// A transcript's parsed tail: the last 256 KiB, whole lines only, records that
/// are JSON objects. None when no line could be read, which the collector
/// refuses as `transcript_unreadable`.
pub fn read_tail_entries(path: Option<&Path>) -> Option<Vec<Value>> {
    use std::io::{Read, Seek, SeekFrom};
    let mut handle = std::fs::File::open(path?).ok()?;
    let size = handle.seek(SeekFrom::End(0)).ok()?;
    handle
        .seek(SeekFrom::Start(size.saturating_sub(TRANSCRIPT_TAIL_BYTES)))
        .ok()?;
    let mut raw = Vec::new();
    handle.read_to_end(&mut raw).ok()?;
    let text = String::from_utf8(raw).ok()?;
    let mut lines: Vec<&str> = text.lines().collect();
    if size > TRANSCRIPT_TAIL_BYTES && !lines.is_empty() {
        lines.remove(0);
    }
    if lines.is_empty() {
        return None;
    }
    Some(
        lines
            .into_iter()
            .filter_map(|line| serde_json::from_str::<Value>(line).ok())
            .filter(Value::is_object)
            .collect(),
    )
}

/// `accounts.<id>.reset_timezone`, the reader the collectors take: only a
/// naive reset stamp needs it.
pub fn reset_timezone_reader(cwd: &Path) -> impl Fn(&str) -> Option<String> + '_ {
    move |account: &str| crate::accounts::state::record_reset_timezone(cwd, account)
}

/// Read raw assistant records without consulting liveness projections.
///
/// Only a shape with a parser here is read. A harness with no parser is a named
/// refusal carrying the harness and the path, never an empty result that reads
/// as a quiet, healthy fleet. `entries_for(row_id)` hands in the row's parsed
/// transcript; None refuses as unreadable.
pub fn collect_transcript_evidence(
    identities: &[EvidenceIdentity],
    now_s: f64,
    evidence_freshness_s: f64,
    entries_for: &mut dyn FnMut(&str) -> Option<Vec<Value>>,
    transcript_path_for: &dyn Fn(&EvidenceIdentity) -> Option<PathBuf>,
    reset_timezone_for: &dyn Fn(&str) -> Option<String>,
) -> (Vec<OutageEvidence>, Vec<Value>) {
    let mut records: Vec<OutageEvidence> = Vec::new();
    let mut refusals: Vec<Value> = Vec::new();
    let path_text = |path: Option<PathBuf>| -> Value {
        match path {
            Some(path) => Value::String(path.display().to_string()),
            None => Value::Null,
        }
    };
    for identity in identities {
        let named = !identity.row_id.is_empty()
            && !identity.harness.is_empty()
            && identity.provider.as_deref().is_some_and(|p| !p.is_empty())
            && identity.account.as_deref().is_some_and(|a| !a.is_empty());
        if !named {
            refusals.push(json!({
                "row_id": identity.row_id,
                "reason": "unknown_route_identity",
                "harness": identity.harness,
                "count": 1,
            }));
            continue;
        }
        if identity.harness != "claude" {
            refusals.push(json!({
                "row_id": identity.row_id,
                "reason": "transcript_shape_unsupported",
                "harness": identity.harness,
                "path": path_text(transcript_path_for(identity)),
                "count": 1,
            }));
            continue;
        }
        let path = transcript_path_for(identity);
        let Some(parsed) = entries_for(&identity.row_id) else {
            refusals.push(json!({
                "row_id": identity.row_id,
                "reason": "transcript_unreadable",
                "harness": identity.harness,
                "path": path_text(path),
                "count": 1,
            }));
            continue;
        };
        let (row_records, row_refusals) = outage_records_from_parsed(
            &parsed,
            identity,
            now_s,
            evidence_freshness_s,
            reset_timezone_for,
        );
        records.extend(row_records);
        refusals.extend(row_refusals);
    }
    (records, refusals)
}

/// Filter one row's parsed records down to outage evidence. Past-window
/// evidence is refused by name unless its reset epoch is still in the future:
/// the freshness expired, not the fact, and a capped fleet that went quiet must
/// not read as healthy.
fn outage_records_from_parsed(
    parsed: &[Value],
    identity: &EvidenceIdentity,
    now_s: f64,
    evidence_freshness_s: f64,
    reset_timezone_for: &dyn Fn(&str) -> Option<String>,
) -> (Vec<OutageEvidence>, Vec<Value>) {
    let tz: Option<String> = identity
        .account
        .as_deref()
        .filter(|account| !account.is_empty())
        .and_then(|account| reset_timezone_for(account));
    let mut out: Vec<OutageEvidence> = Vec::new();
    let mut refusals: Vec<Value> = Vec::new();
    for raw in parsed {
        if raw.get("type").and_then(Value::as_str) != Some("assistant") {
            continue;
        }
        let Some(message) = raw.get("message").filter(|m| m.is_object()) else {
            continue;
        };
        if message.get("role").and_then(Value::as_str) != Some("assistant") {
            continue;
        }
        let Some(observed_at) = record_epoch(raw.get("timestamp")) else {
            continue;
        };
        let content = message_text(message.get("content"));
        if content.is_empty() {
            continue;
        }
        let is_error = raw.get("isApiErrorMessage") == Some(&Value::Bool(true));
        let mut raw_status: Option<i64> = None;
        if is_error {
            // A client that writes error rows without the int status is still
            // evidence: classify the visible text the way the pane path does.
            raw_status = raw
                .get("apiErrorStatus")
                .and_then(Value::as_i64)
                .or_else(|| pane_status(&content));
            if raw_status.is_none() {
                continue;
            }
        }
        let reset_at = reset_epoch_from(&content, tz.as_deref(), now_s);
        let age = now_s - observed_at;
        if (age < 0.0 || age > evidence_freshness_s)
            && !(age > 0.0 && reset_at.is_some_and(|reset| now_s < reset))
        {
            refusals.push(json!({
                "row_id": identity.row_id,
                "reason": "evidence_stale",
                "age_s": age as i64,
                "reset_at": reset_at,
                "count": 1,
            }));
            continue;
        }
        out.push(
            OutageEvidence {
                source: "transcript".to_string(),
                observed_at,
                row_id: identity.row_id.clone(),
                harness: identity.harness.clone(),
                provider: identity.provider.clone(),
                account: identity.account.clone(),
                role: "assistant".to_string(),
                raw_status,
                raw_kind: if is_error { "api_error" } else { "content" }.to_string(),
                content,
                content_fingerprint: String::new(),
                pane_id: None,
                persisted: true,
                snapshot_at: None,
                reset_at,
            }
            .sealed(),
        );
    }
    (out, refusals)
}

/// Read one mux pane's visible buffer. The Err text is the mux's own.
pub fn pane_read_via_mux(
    session: &str,
    pane_id: &str,
    lines: u32,
    timeout_s: u64,
) -> Result<String, String> {
    let lines_text = lines.to_string();
    let mut command = std::process::Command::new(crate::scrape::fno_bin());
    command.args([
        "mux",
        "pane",
        "read",
        "--server",
        session,
        pane_id,
        "--lines",
        lines_text.as_str(),
    ]);
    let output = crate::bounded_cmd::output_with_timeout_result(command, timeout_s)
        .map_err(|error| error.to_string())?;
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    if output.status.success() {
        return Ok(stdout);
    }
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    let detail = if !stderr.is_empty() {
        stderr
    } else if !stdout.is_empty() {
        stdout
    } else {
        "mux pane read failed".to_string()
    };
    Err(py_strip(&detail).to_string())
}

/// `repr(RuntimeError(text))[:400]`, the refusal's detail shape.
fn read_failure_detail(text: &str) -> String {
    let quote = if text.contains('\'') && !text.contains('"') {
        '"'
    } else {
        '\''
    };
    let mut body = String::new();
    for c in text.chars() {
        match c {
            '\\' => body.push_str("\\\\"),
            '\n' => body.push_str("\\n"),
            '\r' => body.push_str("\\r"),
            '\t' => body.push_str("\\t"),
            c if c == quote => {
                body.push('\\');
                body.push(c);
            }
            c => body.push(c),
        }
    }
    format!("RuntimeError({quote}{body}{quote})")
        .chars()
        .take(400)
        .collect()
}

/// The last 64 KiB of a screen, cut at a character boundary and stripped.
fn bounded_screen(content: &str) -> String {
    let bytes = content.as_bytes();
    let mut start = bytes.len().saturating_sub(MAX_PANE_SNAPSHOT_BYTES);
    while start < bytes.len() && !content.is_char_boundary(start) {
        start += 1;
    }
    py_strip(&content[start..]).to_string()
}

/// Persist exact mux screens before returning any pane-backed vote: a screen
/// that cannot be read back from disk cannot vote.
pub fn collect_pane_evidence(
    identities: &[EvidenceIdentity],
    mux_by_row: &HashMap<String, Value>,
    now_s: f64,
    snapshot_dir: &Path,
    pane_read_fn: &mut dyn FnMut(&str, &str) -> Result<String, String>,
    reset_timezone_for: &dyn Fn(&str) -> Option<String>,
) -> (Vec<OutageEvidence>, Vec<Value>) {
    let _ = std::fs::create_dir_all(snapshot_dir);
    let mut records: Vec<OutageEvidence> = Vec::new();
    let mut refusals: Vec<Value> = Vec::new();
    for identity in identities {
        let mux = mux_by_row.get(&identity.row_id);
        let session = mux.and_then(|m| m.get("session")).filter(|s| py_truthy(s));
        let pane = mux.and_then(|m| m.get("pane_id")).filter(|p| !p.is_null());
        let (Some(session), Some(pane)) = (session, pane) else {
            refusals.push(json!({
                "row_id": identity.row_id,
                "reason": "pane_identity_missing",
                "count": 1,
            }));
            continue;
        };
        let session = py_str(session);
        let pane_id = py_str(pane);
        let content = match pane_read_fn(&session, &pane_id) {
            Ok(content) => content,
            Err(error) => {
                refusals.push(json!({
                    "row_id": identity.row_id,
                    "reason": "pane_read_failed",
                    "detail": read_failure_detail(&error),
                    "count": 1,
                }));
                continue;
            }
        };
        if py_strip(&content).is_empty() {
            refusals.push(json!({
                "row_id": identity.row_id,
                "reason": "pane_read_failed",
                "count": 1,
            }));
            continue;
        }
        let bounded = bounded_screen(&content);
        let status = pane_status(&bounded);
        let tz: Option<String> = identity
            .account
            .as_deref()
            .filter(|account| !account.is_empty())
            .and_then(|account| reset_timezone_for(account));
        let reset_at = reset_epoch_from(&bounded, tz.as_deref(), now_s);
        let record = OutageEvidence {
            source: "pane".to_string(),
            observed_at: now_s,
            row_id: identity.row_id.clone(),
            harness: identity.harness.clone(),
            provider: identity.provider.clone(),
            account: identity.account.clone(),
            role: "assistant".to_string(),
            raw_status: status,
            raw_kind: if status.is_some() {
                "api_error"
            } else {
                "content"
            }
            .to_string(),
            content: bounded.clone(),
            content_fingerprint: String::new(),
            pane_id: Some(pane_id.clone()),
            persisted: true,
            snapshot_at: Some(now_s),
            reset_at,
        }
        .sealed();
        let snapshot = json!({
            "observed_at": now_s,
            "mux_session": session,
            "pane_id": pane_id,
            "row_id": identity.row_id,
            "harness": identity.harness,
            "provider": identity.provider,
            "account": identity.account,
            "fingerprint": record.fingerprint(),
            "content": bounded,
        });
        let target = snapshot_dir.join(format!("{}.json", record.fingerprint()));
        let body = crate::status_fanout::python_json_sorted(&snapshot);
        let persisted = crate::accounts::state::write_atomic(&target, &body)
            .ok()
            .and_then(|_| std::fs::read_to_string(&target).ok())
            .and_then(|text| serde_json::from_str::<Value>(&text).ok());
        let same = persisted
            .as_ref()
            .and_then(|value| value.get("fingerprint"))
            .and_then(Value::as_str)
            == Some(record.fingerprint());
        if !same {
            refusals.push(json!({
                "row_id": identity.row_id,
                "reason": "pane_snapshot_unpersisted",
                "count": 1,
            }));
            continue;
        }
        records.push(record);
    }
    prune_snapshots(snapshot_dir);
    (records, refusals)
}

/// Keep the newest snapshots, by mtime.
fn prune_snapshots(snapshot_dir: &Path) {
    let Ok(entries) = std::fs::read_dir(snapshot_dir) else {
        return;
    };
    let mut snapshots: Vec<(std::time::SystemTime, PathBuf)> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
        .filter_map(|path| {
            let modified = std::fs::metadata(&path).ok()?.modified().ok()?;
            Some((modified, path))
        })
        .collect();
    snapshots.sort_by(|a, b| b.0.cmp(&a.0));
    for (_, stale) in snapshots.iter().skip(MAX_PANE_SNAPSHOTS) {
        let _ = std::fs::remove_file(stale);
    }
}

/// Route identity from the observed model (transcript first, then the spawn-time
/// intent) when the declared fields are absent, the cap actor's observed-first
/// precedence. `(None, None)` leaves the named refusal standing.
fn route_fallback_identity(
    entry: Option<&crate::state::RegistryEntry>,
    entries: Option<&[Value]>,
) -> (Option<String>, Option<String>) {
    let observed = entries.and_then(|records| {
        records.iter().rev().find_map(|record| {
            record
                .get("message")
                .and_then(|message| message.get("model"))
                .and_then(Value::as_str)
                .filter(|model| !model.is_empty())
                .map(str::to_string)
        })
    });
    let Some(model) = observed.or_else(|| {
        entry
            .and_then(|e| e.model_name.clone())
            .filter(|m| !m.is_empty())
    }) else {
        return (None, None);
    };
    (
        entry
            .and_then(|e| e.route_provider_id.clone())
            .filter(|p| !p.is_empty())
            .or_else(|| Some(model.clone())),
        entry
            .and_then(|e| e.account_record_id.clone())
            .filter(|a| !a.is_empty())
            .or(Some(model)),
    )
}

/// Collect durable transcript records, then persist exact pane fallbacks.
/// `entries` is the tick's shared transcript parse, so no transcript opens here.
/// An unreadable registry refuses action rather than joining rows to nothing.
pub(crate) fn measure_provider_outages(
    rows: &[crate::backlog::worked::FleetRow],
    now_s: f64,
    entries: &std::collections::BTreeMap<String, Option<Vec<Value>>>,
) -> Value {
    let home = crate::paths::AgentsHome::from_env();
    let registry = match crate::state::load_registry(&home.registry_json()) {
        Ok(registry) => registry,
        Err(exc) => {
            let mut report =
                super::unknown_provider_report("provider_identity_registry_unreadable");
            report["refusals"][0]["detail"] = json!(exc.to_string());
            return report;
        }
    };
    let by_session: HashMap<&str, &crate::state::RegistryEntry> = registry
        .entries
        .iter()
        .filter_map(|e| {
            e.harness_session_id
                .as_deref()
                .filter(|s| !s.is_empty())
                .map(|s| (s, e))
        })
        .collect();
    let mut identities = Vec::with_capacity(rows.len());
    let mut mux_by_row: HashMap<String, Value> = HashMap::new();
    for row in rows {
        let entry = by_session.get(row.row_id.as_str()).copied();
        let parsed = entries.get(&row.row_id).and_then(|e| e.as_deref());
        let (fallback_provider, fallback_account) = route_fallback_identity(entry, parsed);
        identities.push(EvidenceIdentity {
            row_id: row.row_id.clone(),
            harness: entry.and_then(|e| e.harness.clone()).unwrap_or_default(),
            provider: entry
                .and_then(|e| e.route_provider_id.clone())
                .filter(|p| !p.is_empty())
                .or(fallback_provider),
            account: entry
                .and_then(|e| e.account_record_id.clone())
                .filter(|a| !a.is_empty())
                .or(fallback_account),
            session_id: entry
                .and_then(|e| e.harness_session_id.clone())
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| row.row_id.clone()),
            cwd: entry
                .map(|e| e.cwd.clone())
                .filter(|c| !c.is_empty())
                .unwrap_or_else(|| row.cwd.clone()),
        });
        if let Some(mux) = entry.and_then(|e| e.mux.as_ref()) {
            mux_by_row.insert(
                row.row_id.clone(),
                json!({"session": mux.session, "pane_id": mux.pane_id}),
            );
        }
    }
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let policy = OutagePolicy::from_cwd(&cwd);
    let reset_timezone = reset_timezone_reader(&cwd);
    let mut entries_for = |row_id: &str| entries.get(row_id).cloned().flatten();
    let (mut records, mut refusals) = collect_transcript_evidence(
        &identities,
        now_s,
        policy.evidence_freshness_s,
        &mut entries_for,
        &resolve_transcript_path,
        &reset_timezone,
    );
    // Both a missing transcript and an unsupported shape leave the pane as the
    // only instrument that can see a 429 or 529. Without the second reason every
    // codex or opencode pane row reads as a refusal and the quorum never hears it.
    const PANE_FALLBACK: [&str; 2] = ["transcript_unreadable", "transcript_shape_unsupported"];
    let reason_of = |item: &Value| {
        item.get("reason")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string()
    };
    let row_of = |item: &Value| {
        item.get("row_id")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string()
    };
    let unreadable: std::collections::HashSet<String> = refusals
        .iter()
        .filter(|item| PANE_FALLBACK.contains(&reason_of(item).as_str()))
        .map(row_of)
        .collect();
    if !unreadable.is_empty() {
        let snapshot_root = journal_path()
            .parent()
            .map_or_else(|| PathBuf::from("."), Path::to_path_buf)
            .join("provider-pane-snapshots");
        let fallback: Vec<EvidenceIdentity> = identities
            .iter()
            .filter(|identity| unreadable.contains(&identity.row_id))
            .cloned()
            .collect();
        let mut read_pane = |session: &str, pane: &str| pane_read_via_mux(session, pane, 80, 10);
        let (pane_records, pane_refusals) = collect_pane_evidence(
            &fallback,
            &mux_by_row,
            now_s,
            &snapshot_root,
            &mut read_pane,
            &reset_timezone,
        );
        let successful: std::collections::HashSet<&str> =
            pane_records.iter().map(|r| r.row_id.as_str()).collect();
        refusals.retain(|item| {
            !(PANE_FALLBACK.contains(&reason_of(item).as_str())
                && successful.contains(row_of(item).as_str()))
        });
        records.extend(pane_records.iter().cloned());
        refusals.extend(pane_refusals);
    }
    let mut counts: std::collections::BTreeMap<String, i64> = std::collections::BTreeMap::new();
    for item in &refusals {
        *counts.entry(reason_of(item)).or_insert(0) += 1;
    }
    if records.is_empty() && !refusals.is_empty() {
        return json!({
            "instrument": "unknown",
            "breakers": [],
            "counts": counts,
            "refusals": refusals,
        });
    }
    let mut report = measure_and_persist(&records, now_s, None, &policy);
    if !refusals.is_empty() {
        let mut merged = report
            .get("refusals")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        merged.extend(refusals);
        report["refusals"] = Value::Array(merged);
        if !report.get("counts").is_some_and(Value::is_object) {
            report["counts"] = json!({});
        }
        for (reason, count) in counts {
            report["counts"][reason] = json!(count);
        }
    }
    report
}

/// `<state_dir>/recovery/provider-outages.json`.
pub fn journal_path() -> PathBuf {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    crate::agents_config::state_dir(&cwd)
        .unwrap_or_else(|| PathBuf::from(".fno"))
        .join("recovery")
        .join("provider-outages.json")
}
