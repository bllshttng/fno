//! Worktree overlap recording and recurrence reporting, ported from the
//! deleted `cli/src/fno/worktree_cli/overlaps.py`: one hidden write path fed
//! by the SessionStart carrier's observation, and one read-only report over
//! the machine-global journal. Characterized by the `overlap_record_*`,
//! `overlap_report_*` and `fold_*` goldens.

use super::{resolve_lenient, sorted_json};
use chrono::{DateTime, TimeZone, Utc};
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

/// Three distinct observations in a rolling 28-day window warrant a Stage 3
/// design review. Intentionally low: its only effect is recommending a pass.
pub const RECURRENCE_THRESHOLD: usize = 3;
pub const RECURRENCE_WINDOW_DAYS: i64 = 28;

/// The raw-journal read with honest coverage: a missing file is a valid
/// empty source (no_data); an unreadable file is unknown; a malformed line
/// makes partial. The store-committed leg reads back through the same
/// append_envelope boundary that wrote it.
pub fn read_overlap_events(
    journal: &Path,
) -> Result<(Vec<Value>, Map<String, Value>), Map<String, Value>> {
    let (text, missing) = match std::fs::read(journal) {
        Ok(bytes) => (String::from_utf8_lossy(&bytes).into_owned(), false),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => (String::new(), true),
        Err(e) => {
            let mut coverage = Map::new();
            coverage.insert("state".to_string(), Value::String("unknown".into()));
            coverage.insert("unreadable".to_string(), Value::Bool(true));
            coverage.insert(
                "path".to_string(),
                Value::String(journal.to_string_lossy().into_owned()),
            );
            coverage.insert(
                "error".to_string(),
                Value::String(format!("{}: {}", e.kind(), e)),
            );
            return Err(coverage);
        }
    };
    let (events, mut coverage) = read_overlap_events_from(&text, missing);
    coverage.insert(
        "path".to_string(),
        Value::String(journal.to_string_lossy().into_owned()),
    );
    Ok((events, coverage))
}

/// Python's tolerant ISO parse: fromisoformat with Z folded to +00:00; a
/// naive stamp reads UTC.
pub fn parse_event_ts(raw: Option<&str>) -> Option<DateTime<Utc>> {
    let raw = raw?;
    if raw.is_empty() {
        return None;
    }
    if let Ok(dt) = DateTime::parse_from_rfc3339(raw) {
        return Some(dt.with_timezone(&Utc));
    }
    // A naive stamp: no offset, read UTC like Python's replace(tzinfo=utc).
    let fmts = ["%Y-%m-%dT%H:%M:%S%.f", "%Y-%m-%dT%H:%M:%S"];
    for fmt in fmts {
        if let Ok(naive) = chrono::NaiveDateTime::parse_from_str(raw, fmt) {
            return Some(Utc.from_utc_datetime(&naive));
        }
    }
    None
}

/// Deduplicate observations by id and fold the rolling window. Pure; no I/O.
pub fn fold_overlap_events(
    events: &[Value],
    since_days: i64,
    now: DateTime<Utc>,
) -> Result<Value, String> {
    if since_days < 1 {
        return Err("since_days must be at least 1".to_string());
    }
    let cutoff = now - chrono::Duration::days(since_days);
    let mut by_id: std::collections::HashMap<String, ObsSlot> = Default::default();
    for e in events {
        let data = e
            .get("data")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        let Some(oid) = data.get("observation_id").and_then(Value::as_str) else {
            continue;
        };
        if oid.is_empty() {
            continue;
        }
        let ts = parse_event_ts(e.get("ts").and_then(Value::as_str));
        let slot = by_id.entry(oid.to_string()).or_insert_with(|| ObsSlot {
            observation_id: oid.to_string(),
            worktree_key: data.get("worktree_key").cloned().unwrap_or(Value::Null),
            observer: data
                .get("observer_session_id")
                .cloned()
                .unwrap_or(Value::Null),
            peer_count: data
                .get("peer_session_ids")
                .and_then(Value::as_array)
                .map(|a| a.len() as i64)
                .unwrap_or(0),
            first_ts: None,
            latest_ts: ts,
        });
        if let Some(ts) = ts {
            if slot.latest_ts.is_none() || ts < slot.latest_ts.unwrap() {
                slot.latest_ts = Some(ts);
            }
            if slot.first_ts.is_none() || slot.first_ts.unwrap() > ts {
                slot.first_ts = Some(ts);
            }
        }
    }
    fold_window(by_id, since_days, now, cutoff)
}

struct ObsSlot {
    observation_id: String,
    worktree_key: Value,
    observer: Value,
    peer_count: Option<i64>,
    first_ts: Option<DateTime<Utc>>,
    latest_ts: Option<DateTime<Utc>>,
}

fn fold_window(
    by_id: std::collections::HashMap<String, ObsSlot>,
    since_days: i64,
    now: DateTime<Utc>,
    cutoff: DateTime<Utc>,
) -> Result<Value, String> {
    let mut in_window: Vec<&ObsSlot> = by_id
        .values()
        .filter(|o| {
            o.latest_ts
                .map(|ts| cutoff <= ts && ts <= now)
                .unwrap_or(false)
        })
        .collect();
    let distinct = in_window.len() as i64;
    let mut worktrees: std::collections::HashMap<String, i64> = Default::default();
    let mut observers: std::collections::BTreeSet<String> = Default::default();
    let mut first_ts: Option<DateTime<Utc>> = None;
    let mut latest_ts: Option<DateTime<Utc>> = None;
    for o in &in_window {
        if let Value::String(k) = &o.worktree_key {
            if !k.is_empty() {
                *worktrees.entry(k.clone()).or_insert(0) += 1;
            }
        }
        if let Value::String(s) = &o.observer {
            if !s.is_empty() {
                observers.insert(s.clone());
            }
        }
        if o.first_ts.is_some() && (first_ts.is_none() || first_ts.unwrap() > o.first_ts.unwrap()) {
            first_ts = o.first_ts;
        }
        if o.latest_ts.is_some()
            && (latest_ts.is_none() || latest_ts.unwrap() < o.latest_ts.unwrap())
        {
            latest_ts = o.latest_ts;
        }
    }
    fold_render(
        distinct, worktrees, observers, first_ts, latest_ts, by_id, since_days, cutoff,
    )
}

#[allow(clippy::too_many_arguments)]
fn fold_render(
    distinct: i64,
    worktrees: std::collections::HashMap<String, i64>,
    observers: std::collections::BTreeSet<String>,
    first_ts: Option<DateTime<Utc>>,
    latest_ts: Option<DateTime<Utc>>,
    by_id: std::collections::HashMap<String, ObsSlot>,
    since_days: i64,
    cutoff: DateTime<Utc>,
) -> Result<Value, String> {
    let in_window: Vec<(&ObsSlot, Option<DateTime<Utc>>)> = by_id
        .values()
        .filter(|o| {
            o.latest_ts
                .map(|ts| cutoff <= ts && ts <= now)
                .unwrap_or(false)
        })
        .map(|o| (o, o.latest_ts))
        .collect();
    let observations: Vec<Value> = {
        let mut list: Vec<(&ObsSlot, Option<DateTime<Utc>>)> = in_window;
        list.sort_by_key(|(o, _)| {
            o.latest_ts
                .unwrap_or_else(|| Utc.timestamp_opt(0, 0).unwrap())
        });
        list.iter()
            .map(|(o, _)| {
                json!({
                    "observation_id": o.observation_id,
                    "worktree_key": o.worktree_key,
                    "observer_session_id": o.observer,
                    "peer_count": o.peer_count,
                    "latest_ts": o.latest_ts.map(iso_out),
                })
            })
            .collect()
    };
    let per_worktree: serde_json::Map<String, Value> = worktrees
        .iter()
        .map(|(k, c)| (k.clone(), Value::Number((*c).into())))
        .collect();
    Ok(json!({
        "distinct_observations": distinct,
        "distinct_worktrees": worktrees.len() as i64,
        "distinct_observers": observers.len() as i64,
        "first_ts": first_ts.map(iso_out),
        "latest_ts": latest_ts.map(iso_out),
        "per_worktree": per_worktree,
        "recurrence_threshold": RECURRENCE_THRESHOLD,
        "window_days": since_days,
        "recurrence_threshold_met": distinct >= RECURRENCE_THRESHOLD,
        "observations": observations,
    }))
}

fn iso_out(dt: DateTime<Utc>) -> String {
    dt.to_rfc3339_opts(chrono::SecondsFormat::AutoSi, false)
}

/// `Path.resolve()` for the two key fields: a Git-directory path collapses
/// to one canonical absolute identity.
fn normalize_key(raw: Option<&Value>) -> String {
    match raw {
        Some(Value::String(s)) if !s.is_empty() => {
            resolve_lenient(Path::new(s)).to_string_lossy().into_owned()
        }
        _ => String::new(),
    }
}

/// Build the event envelope the validator will admit: peers sorted unique,
/// the digest recomputed, the observer never its own peer.
#[allow(clippy::too_many_arguments)]
fn build_overlap_event(
    observer: &str,
    peers: &[Value],
    repository_key: &str,
    worktree_key: &str,
    live_window_seconds: i64,
    now: DateTime<Utc>,
) -> Result<Value, String> {
    if observer.is_empty() {
        return Err("worktree_overlap_observed observer_session_id cannot be empty".to_string());
    }
    if peers.is_empty()
        || peers
            .iter()
            .any(|p| !p.is_string() || p.as_str().unwrap_or("").is_empty())
    {
        return Err(
            "worktree_overlap_observed peer_session_ids must be a non-empty \
             list of non-empty strings"
                .to_string(),
        );
    }
    if repository_key.is_empty() {
        return Err("worktree_overlap_observed repository_key cannot be empty".to_string());
    }
    if worktree_key.is_empty() {
        return Err("worktree_overlap_observed worktree_key cannot be empty".to_string());
    }
    if live_window_seconds <= 0 {
        return Err(
            "worktree_overlap_observed live_window_seconds must be a positive \
             integer"
                .to_string(),
        );
    }
    let mut sorted_peers: Vec<String> = peers
        .iter()
        .filter_map(|p| p.as_str().map(str::to_string))
        .collect();
    sorted_peers.sort();
    sorted_peers.dedup();
    if sorted_peers.iter().any(|p| p == observer) {
        return Err("worktree_overlap_observed observer cannot be its own peer".to_string());
    }
    let peers_json: Vec<Value> = sorted_peers
        .iter()
        .map(|p| Value::String(p.clone()))
        .collect();
    let blob = serde_json::to_string(&json!([repository_key, worktree_key, observer, peers_json]))
        .map_err(|e| e.to_string())?;
    let mut hasher = Sha256::new();
    Digest::update(&mut hasher, blob.as_bytes());
    let oid = format!("{:x}", Sha256::finalize(hasher));
    Ok(json!({
        "type": "worktree_overlap_observed",
        "source": "hook",
        "ts": iso_out(now),
        "data": {
            "observation_id": oid,
            "repository_key": repository_key,
            "worktree_key": worktree_key,
            "observer_session_id": observer,
            "peer_session_ids": peers_json,
            "live_window_seconds": live_window_seconds,
        },
    }))
}

/// The pinned test clock (`FNO_TEST_NOW`, RFC3339) or real UTC now - the
/// record op's one determinism hook, read once per call.
fn effective_now() -> DateTime<Utc> {
    if let Ok(raw) = std::env::var("FNO_TEST_NOW") {
        if let Ok(dt) = DateTime::parse_from_rfc3339(raw.trim()) {
            return dt.with_timezone(&Utc);
        }
    }
    Utc::now()
}

/// Python's `int()` on the machine-mode payload: numbers truncate, bools
/// read 0/1, numeric strings parse, everything else is the coercion error
/// the record reason carries verbatim.
fn coerce_live_window(raw: Option<&Value>) -> Result<i64, String> {
    let raw = raw.unwrap_or(&Value::Number(serde_json::Number::from(120)));
    match raw {
        Value::Number(n) => Ok(n.as_f64().map(|f| f as i64).unwrap_or(i64::MAX)),
        Value::Bool(b) => Ok(i64::from(*b)),
        Value::String(s) => s
            .trim()
            .parse::<i64>()
            .map_err(|_| format!("invalid literal for int() with base 10: {s:?}")),
        Value::Null => Err(
            "int() argument must be a string, a bytes-like object or a real \
             number, not 'NoneType'"
                .to_string(),
        ),
        Value::Array(_) => Err(
            "int() argument must be a string, a bytes-like object or a real \
             number, not 'list'"
                .to_string(),
        ),
        Value::Object(_) => Err(
            "int() argument must be a string, a bytes-like object or a real \
             number, not 'dict'"
                .to_string(),
        ),
    }
}

/// The record result: always exit 0; recording and fold status ride the JSON.
pub fn overlap_record(journal: &Path, stdin_text: &str, since_days: i64) -> Value {
    let now = effective_now();
    let mut result = json!({"recorded": false, "fold": {"state": "no_data"}});
    let parsed: Value = match serde_json::from_str(stdin_text.trim()) {
        Ok(v) => v,
        Err(_) => Value::Null,
    };
    let Some(obj) = parsed.as_object() else {
        return finish_record(result, "invalid-input");
    };
    // Only the non-empty-list shape reads invalid-input here; per-element
    // and field validity are the schema builder's refusals, one tier later.
    let Some(peer_list) = obj.get("peer_session_ids").and_then(Value::as_array) else {
        return finish_record(result, "invalid-input");
    };
    if peer_list.is_empty() {
        return finish_record(result, "invalid-input");
    }
    let peers = peer_list.clone();
    let repository_key = normalize_key(obj.get("repository_common_dir"));
    let worktree_key = normalize_key(obj.get("worktree_git_dir"));
    let live_window_seconds = match coerce_live_window(obj.get("live_window_seconds")) {
        Ok(v) => v,
        Err(msg) => return finish_record(result, &format!("schema-rejected: {msg}")),
    };
    let event = match build_overlap_event(
        obj.get("observer_session_id")
            .and_then(Value::as_str)
            .unwrap_or(""),
        &peers,
        &repository_key,
        &worktree_key,
        live_window_seconds,
        now,
    ) {
        Ok(ev) => ev,
        Err(msg) => return finish_record(result, &format!("schema-rejected: {msg}")),
    };
    let envelope = event.to_string();
    match crate::event_store::append_envelope(journal, &envelope, None) {
        Ok(_) => {}
        Err(msg) => {
            let reason = if msg.starts_with(crate::event_store::VALIDATE_PREFIX) {
                format!("schema-rejected: {msg}")
            } else {
                format!("io-error: {msg}")
            };
            return finish_record(result, &reason);
        }
    }
    result["recorded"] = Value::Bool(true);
    result["observation_id"] = event["data"]["observation_id"].clone();
    match read_overlap_events(journal) {
        Err(coverage) => {
            // The read error's message: the stored error, else the path,
            // else the bare word - str(OverlapReadError) in the Python leg.
            let detail = coverage
                .get("error")
                .or_else(|| coverage.get("path"))
                .and_then(Value::as_str)
                .unwrap_or("unreadable");
            result["fold"] = json!({"state": "unknown", "error": detail});
        }
        Ok((events, coverage)) => match fold_overlap_events(&events, since_days, now) {
            Ok(folded) => {
                let mut merged = serde_json::Map::new();
                merged.insert(
                    "state".to_string(),
                    coverage.get("state").cloned().unwrap_or(Value::Null),
                );
                if let Some(fold_obj) = folded.as_object() {
                    for (k, v) in fold_obj {
                        merged.insert(k.clone(), v.clone());
                    }
                }
                result["fold"] = Value::Object(merged);
            }
            Err(_) => {
                result["fold"] = json!({"state": "unknown", "error": "since_days < 1"});
            }
        },
    }
    result
}

fn finish_record(mut result: Value, reason: &str) -> Value {
    result["record_reason"] = Value::String(reason.to_string());
    result
}

/// The global journal: the parity pin `FNO_TEST_JOURNAL`, else the state
/// root's events.jsonl (paths.global_events_json's config-free arm).
fn journal_path() -> PathBuf {
    if let Ok(pin) = std::env::var("FNO_TEST_JOURNAL") {
        if !pin.is_empty() {
            return PathBuf::from(pin);
        }
    }
    super::policy::events_journal()
}

/// The report over the deduplicated journal; exit 0 for a valid empty or
/// complete read, 1 for unknown or partial, so incomplete evidence never
/// reads as zero.
pub fn overlaps_report(since_days: i64) -> (Value, i32) {
    if since_days < 1 {
        eprintln!("worktree overlaps: --since must be at least 1 (got {since_days})");
        let report = json!({
            "state": "unknown",
            "since_days": since_days,
            "error": "since_days < 1",
        });
        return (report, 1);
    }
    let (events, coverage) = match read_overlap_events(&journal_path()) {
        Ok(pair) => pair,
        Err(coverage) => {
            let report = json!({
                "state": "unknown",
                "coverage": Value::Object(coverage),
                "distinct_observations": null,
            });
            return (report, 1);
        }
    };
    let now = effective_now();
    let folded = match fold_overlap_events(&events, since_days, now) {
        Ok(f) => f,
        Err(_) => json!({}),
    };
    let mut report = json!({
        "state": coverage.get("state").cloned().unwrap_or(Value::Null),
        "since_days": since_days,
        "coverage": {
            "path": coverage.get("path").cloned().unwrap_or(Value::Null),
            "malformed_lines": coverage.get("malformed_lines").cloned().unwrap_or(Value::Null),
            "journal_lines": events.len() as i64,
        },
    });
    if let (Some(rep), Some(fold)) = (report.as_object_mut(), folded.as_object()) {
        for (k, v) in fold {
            rep.insert(k.clone(), v.clone());
        }
    }
    let code = match report.get("state") {
        Some(Value::String(s)) if s == "no_data" || s == "complete" => 0,
        _ => 1,
    };
    (report, code)
}

/// The human rendering. per_worktree walks the map alphabetically where the
/// Python leg walked Counter.most_common(); the corpus only pins the single
/// worktree shape, so the deterministic order is the documented choice.
pub fn render_overlaps_text(report: &Value) -> String {
    let state = report.get("state").and_then(Value::as_str).unwrap_or("");
    let cov = report.get("coverage").and_then(Value::as_object);
    let mut lines: Vec<String> = Vec::new();
    match state {
        "no_data" => lines.push(format!(
            "no worktree overlap observations in the last {} days (journal absent at {})",
            report
                .get("since_days")
                .and_then(Value::as_i64)
                .map(|v| v.to_string())
                .unwrap_or_else(|| "28".to_string()),
            cov.and_then(|c| c.get("path"))
                .and_then(Value::as_str)
                .unwrap_or("the overlap journal"),
        )),
        "unknown" => {
            let reason = report
                .get("error")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .unwrap_or("overlap journal unreadable");
            lines.push(format!(
                "unknown: {reason} at {} ({} malformed lines)",
                cov.and_then(|c| c.get("path"))
                    .and_then(Value::as_str)
                    .unwrap_or("the overlap journal"),
                cov.and_then(|c| c.get("malformed_lines"))
                    .and_then(Value::as_i64)
                    .unwrap_or(0),
            ));
        }
        _ => {
            let n = report
                .get("distinct_observations")
                .and_then(Value::as_i64)
                .unwrap_or(0);
            let window = report
                .get("window_days")
                .and_then(Value::as_i64)
                .unwrap_or(28);
            lines.push(format!(
                "{n} distinct worktree overlap observation(s) in the last {window} days"
            ));
            let dw = report
                .get("distinct_worktrees")
                .and_then(Value::as_i64)
                .unwrap_or(0);
            let observers_n = report
                .get("distinct_observers")
                .and_then(Value::as_i64)
                .unwrap_or(0);
            lines.push(format!("  worktrees: {dw}  observers: {observers_n}"));
            let first = report.get("first_ts").and_then(Value::as_str);
            let latest = report.get("latest_ts").and_then(Value::as_str);
            if first.is_some() || latest.is_some() {
                lines.push(format!(
                    "  first: {}  latest: {}",
                    first.unwrap_or("None"),
                    latest.unwrap_or("None"),
                ));
            }
            if let Some(per) = report.get("per_worktree").and_then(Value::as_object) {
                for (wt, count) in per {
                    lines.push(format!("  {}x  {}", count.as_i64().unwrap_or(0), wt,));
                }
            }
            let malformed = cov
                .and_then(|c| c.get("malformed_lines"))
                .and_then(Value::as_i64)
                .unwrap_or(0);
            if state == "partial" || malformed > 0 {
                lines.push(format!(
                    "  coverage: PARTIAL - {malformed} malformed journal line(s) skipped \
                     (counts may understate)"
                ));
            }
            if report
                .get("recurrence_threshold_met")
                .and_then(Value::as_bool)
                .unwrap_or(false)
            {
                lines.push(format!(
                    "  recurrence reached {n}/{}: a Stage 3 worktree-write-lock design node \
                     is now warranted.",
                    report
                        .get("recurrence_threshold")
                        .and_then(Value::as_i64)
                        .unwrap_or(RECURRENCE_THRESHOLD as i64),
                ));
            }
            lines.push("  report: fno agents workspace worktree overlaps --since 28 --json");
        }
    }
    lines.join("\n")
}

/// The `worktree-overlap-record` op: `--since N`, one observation on stdin,
/// always exit 0.
pub fn record_client(args: &[String]) -> i32 {
    let mut since = 28i64;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--since" if i + 1 < args.len() => {
                since = args[i + 1].parse().unwrap_or(28);
                i += 2;
            }
            "--stdin" => i += 1,
            other => {
                eprintln!("fno-agents worktree-overlap-record: unexpected argument {other}");
                return 2;
            }
        }
    }
    use std::io::Read;
    let mut stdin_text = String::new();
    let _ = std::io::stdin().read_to_string(&mut stdin_text);
    let result = overlap_record(&journal_path(), &stdin_text, since);
    println!("{}", super::sorted_json(&result));
    0
}

/// The `worktree-overlaps` op: `--since N --json`; text render otherwise.
pub fn report_client(args: &[String]) -> i32 {
    let mut since = 28i64;
    let mut as_json = false;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--since" if i + 1 < args.len() => {
                since = args[i + 1].parse().unwrap_or(28);
                i += 2;
            }
            "--json" | "-J" => {
                as_json = true;
                i += 1;
            }
            other => {
                eprintln!("fno-agents worktree-overlaps: unexpected argument {other}");
                return 2;
            }
        }
    }
    let (report, code) = overlaps_report(since);
    if as_json {
        println!("{}", super::sorted_json(&report));
    } else {
        println!("{}", render_overlaps_text(&report));
    }
    code
}

fn read_overlap_events_from(
    text: &str,
    missing: bool,
) -> Result<(Vec<Value>, Map<String, Value>), Map<String, Value>> {
    let mut coverage = Map::new();
    coverage.insert("state".to_string(), Value::String("no_data".into()));
    coverage.insert("malformed_lines".to_string(), Value::Number(0.into()));
    coverage.insert("unreadable".to_string(), Value::Bool(false));
    coverage.insert("path".to_string(), Value::String(String::new()));
    if missing {
        return Ok((Vec::new(), coverage));
    }
    let mut events = Vec::new();
    let mut malformed = 0u64;
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let parsed: Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => {
                malformed += 1;
                continue;
            }
        };
        if !parsed.is_object() {
            malformed += 1;
            continue;
        }
        if parsed.get("type").and_then(Value::as_str) == Some("worktree_overlap_observed") {
            let obj = parsed.as_object().unwrap();
            if crate::event_store::validate::validate_envelope(obj).is_err() {
                malformed += 1;
                continue;
            }
            events.push(parsed);
        }
    }
    coverage.insert(
        "malformed_lines".to_string(),
        Value::Number(malformed.into()),
    );
    let state = if malformed > 0 {
        "partial"
    } else if events.is_empty() {
        "no_data"
    } else {
        "complete"
    };
    coverage.insert("state".to_string(), Value::String(state.into()));
    Ok((events, coverage))
}
