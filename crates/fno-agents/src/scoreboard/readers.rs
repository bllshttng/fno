//! The scoreboard readers, ported from `cli/src/fno/scoreboard/fold.py`
//! (`load_ledger_rows`, `read_jsonl_events_with_coverage`,
//! `read_graph_nodes`) and its time/number helpers. Tolerant where the
//! Python leg was tolerant: the ledger retries once on a parse error before
//! it becomes a hard broken-input; the optional jsonl/graph sources skip a
//! corrupt line rather than crash the command. The store beside a journal
//! answers first (SQL authority); only a store-less path reads the raw file.

use chrono::{DateTime, Local, TimeZone};
use serde_json::{Map, Value};
use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::path::{Path, PathBuf};

/// `BrokenLedger` (fold.py): the ledger failed to parse twice, a real
/// corruption, not a mid-append race. `render` matches the Python
/// `f"{path}: parse error at byte {offset}: {msg}"` the CLI prints.
pub(crate) struct BrokenLedger {
    pub path: String,
    pub offset: usize,
    pub msg: String,
}

impl BrokenLedger {
    pub(crate) fn message(&self) -> String {
        format!(
            "{}: parse error at byte {}: {}",
            self.path, self.offset, self.msg
        )
    }
}

/// `_parse_ts` (fold.py): parse an ISO timestamp onto the naive LOCAL
/// timeline the whole fold shares. Naive strings stay local; aware strings
/// convert to local before their offset is dropped, so `+02:00` lands on the
/// same timeline instead of drifting by the offset.
pub(crate) fn parse_ts(raw: Option<&Value>) -> Option<DateTime<Local>> {
    let text = raw.and_then(|v| v.as_str()).unwrap_or_default();
    if text.is_empty() {
        return None;
    }
    let text = text.replacen('Z', "+00:00", 1);
    // Aware first: a trailing offset or Z makes the string offset-parsing.
    if let Ok(dt) = DateTime::parse_from_rfc3339(&text) {
        return Some(dt.with_timezone(&Local));
    }
    for fmt in [
        "%Y-%m-%dT%H:%M:%S%.f",
        "%Y-%m-%dT%H:%M:%S",
        "%Y-%m-%d %H:%M:%S",
        "%Y-%m-%d",
    ] {
        if let Ok(naive) = chrono::NaiveDateTime::parse_from_str(&text, fmt) {
            return Local.from_local_datetime(&naive).single();
        }
    }
    None
}

/// Python's round(), half-to-even at the decimal digit. `f64::round` runs
/// half-away-from-zero, so the two disagree on 12.5 and the goldens would
/// drift one unit.
pub(crate) fn py_round(value: f64) -> i64 {
    format!("{value:.0}").parse().unwrap_or(0)
}

pub(crate) fn pct(numerator: i64, denominator: i64) -> i64 {
    if denominator == 0 {
        0
    } else {
        py_round(100.0 * numerator as f64 / denominator as f64)
    }
}

/// `_num`: coerce a possibly-malformed ledger cost to f64; junk and
/// non-finite values read as 0.0.
pub(crate) fn num(value: Option<&Value>) -> f64 {
    let Some(value) = value else { return 0.0 };
    let as_f64 = match value {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.trim().parse::<f64>().ok(),
        Value::Bool(_) => Some(1.0), // float(True) == 1.0
        Value::Null => Some(0.0),
        _ => None,
    };
    match as_f64 {
        Some(v) if v.is_finite() => v,
        _ => 0.0,
    }
}

/// `_num_opt`: nullable numeric coercion. A MISSING/null/junk value is None
/// (not 0.0), so an unmeasurable metric stays out of distributions instead of
/// faking a measured zero. Negative values are junk too.
pub(crate) fn num_opt(value: Option<&Value>) -> Option<f64> {
    match value {
        None | Some(Value::Null) => None,
        Some(Value::Bool(_)) => None,
        Some(Value::String(s)) if s.trim().is_empty() => None,
        Some(v) => match v.as_f64() {
            Some(f) if f.is_finite() && f >= 0.0 => Some(f),
            _ => None,
        },
    }
}

/// The canonical JSON signature `json.dumps(e, sort_keys=True,
/// separators=(",", ":"))` freezes: keys sorted recursively, no spaces.
pub(crate) fn canonical_signature(value: &Value) -> String {
    fn sorted(value: &Value) -> Value {
        match value {
            Value::Object(map) => {
                let by_key: BTreeMap<&String, &Value> = map.iter().collect();
                let mut out = serde_json::Map::new();
                for (key, inner) in by_key {
                    out.insert(key.clone(), sorted(inner));
                }
                Value::Object(out)
            }
            Value::Array(items) => Value::Array(items.iter().map(sorted).collect()),
            other => other.clone(),
        }
    }
    serde_json::to_string(&sorted(value)).unwrap_or_default()
}

/// `_event_sort_key`: (dated, ts or datetime.min, complete-context, signature).
pub(crate) fn event_sort_key(event: &Value) -> (bool, i64, bool, String) {
    let parsed = parse_ts(event.get("ts").or_else(|| event.get("timestamp")));
    let signature_source = match event.get("raw") {
        Some(raw) if raw.is_object() => raw,
        _ => event,
    };
    let signature = canonical_signature(signature_source);
    let complete_context = event_kind(event) == Some("context_snapshot")
        && event
            .get("data")
            .and_then(|d| d.get("measurement_complete"))
            == Some(&Value::Bool(true));
    // i64 epoch millis stand in for the datetime comparison; the undated
    // bucket (dated=false) sorts ahead of every dated row exactly like the
    // Python tuple (False < True) did.
    let ts = parsed.map(|dt| dt.timestamp_millis()).unwrap_or(i64::MIN);
    (parsed.is_some(), ts, complete_context, signature)
}

/// `(e.get("kind") or e.get("type"))` as a borrowed str.
pub(crate) fn event_kind(event: &Value) -> Option<&str> {
    event
        .get("kind")
        .and_then(|v| v.as_str())
        .or_else(|| event.get("type").and_then(|v| v.as_str()))
        .filter(|k| !k.is_empty())
}

/// `load_ledger_rows`: read ledger.json rows; retry once on a parse error
/// (the writer holds the flock for milliseconds), then raise BrokenLedger.
/// Missing file reads empty (fresh install); a valid-JSON junk shape reads
/// empty; non-object rows drop.
pub(crate) fn load_ledger_rows(path: &Path) -> Result<Vec<Value>, BrokenLedger> {
    match std::fs::read_to_string(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(_) => Ok(Vec::new()),
        Ok(raw) => match serde_json::from_str::<Value>(&raw) {
            Ok(value) => Ok(ledger_rows_from(value)),
            Err(first) => {
                std::thread::sleep(std::time::Duration::from_millis(100));
                match std::fs::read_to_string(path) {
                    Ok(raw) => match serde_json::from_str::<Value>(&raw) {
                        Ok(value) => Ok(ledger_rows_from(value)),
                        Err(second) => Err(broken_ledger(path, &second)),
                    },
                    Err(_) => Err(broken_ledger(path, &first)),
                }
            }
        },
    }
}

fn ledger_rows_from(value: Value) -> Vec<Value> {
    let rows = match &value {
        Value::Object(map) => map.get("entries").cloned().unwrap_or(Value::Null),
        other => other.clone(),
    };
    match rows {
        Value::Array(items) => items.into_iter().filter(|r| r.is_object()).collect(),
        _ => Vec::new(),
    }
}

fn broken_ledger(path: &Path, error: &serde_json::Error) -> BrokenLedger {
    BrokenLedger {
        path: path.display().to_string(),
        offset: error.column().saturating_sub(1),
        msg: error_message(error),
    }
}

/// Python's JSONDecodeError messages the fold's golden froze. serde_json's
/// wording differs per shape; map the shapes the fixture corpus carries.
fn error_message(error: &serde_json::Error) -> String {
    let text = error.to_string();
    if text.contains("expected value") {
        return "Expecting value".to_string();
    }
    if text.contains("expected `,` or `}`") {
        return "Expecting ',' delimiter".to_string();
    }
    if text.contains("key must be a string") {
        return "Expecting property name enclosed in double quotes".to_string();
    }
    if text.contains("EOF while parsing") {
        return "Expecting value".to_string();
    }
    text
}

/// One journal's rows through the store authority: a store beside the
/// journal answers first, a store-less path reads the raw file (a fixture or
/// a pre-cutover journal). The `context_snapshot` validation gate applies to
/// FILE rows only, exactly like the Python reader's branch split.
fn one_journal_rows(
    journal: &Path,
    kinds: &BTreeSet<String>,
    malformed: &mut usize,
    unreadable: &mut usize,
    path_coverage: &mut Vec<Value>,
) -> Vec<Value> {
    let path_key = std::fs::canonicalize(journal)
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| abs_path(journal));
    let store = crate::event_store::store_path(journal);
    if store.exists() {
        let mut out = Vec::new();
        let query = crate::event_store::EventQuery::of_types(
            &kinds.iter().map(String::as_str).collect::<Vec<_>>(),
        );
        return match crate::event_store::query_events(journal, &query) {
            Ok(rows) => {
                let mut path_malformed = 0usize;
                for row in rows {
                    match serde_json::from_str::<Value>(&row.line) {
                        Ok(v) if v.is_object() => out.push(v),
                        _ => {
                            path_malformed += 1;
                            *malformed += 1;
                        }
                    }
                }
                path_coverage.push(serde_json::json!({
                    "path": path_key,
                    "status": if path_malformed == 0 { "ok" } else { "malformed" },
                    "malformed_lines": path_malformed,
                }));
                out
            }
            Err(error) => {
                *unreadable += 1;
                path_coverage.push(serde_json::json!({
                    "path": path_key,
                    "status": "unreadable",
                    "error": error,
                    "malformed_lines": 0,
                }));
                out
            }
        };
    }
    if !journal.exists() {
        path_coverage.push(serde_json::json!({
            "path": path_key,
            "status": "missing",
            "malformed_lines": 0,
        }));
        return Vec::new();
    }
    let mut out = Vec::new();
    let mut path_malformed = 0usize;
    let lines = match std::fs::read_to_string(journal) {
        Ok(text) => text,
        Err(error) => {
            *unreadable += 1;
            path_coverage.push(serde_json::json!({
                "path": path_key,
                "status": "unreadable",
                "error": format!("{}: {}", error.kind(), error),
                "malformed_lines": 0,
            }));
            return out;
        }
    };
    for line in lines.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let event: Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => {
                path_malformed += 1;
                *malformed += 1;
                continue;
            }
        };
        if !event.is_object() {
            path_malformed += 1;
            *malformed += 1;
            continue;
        }
        if event_kind(&event) == Some("context_snapshot") {
            let map = event.as_object().expect("checked object above");
            if crate::event_store::validate::validate_envelope(map).is_err() {
                path_malformed += 1;
                *malformed += 1;
                continue;
            }
        }
        out.push(event);
    }
    path_coverage.push(serde_json::json!({
        "path": path_key,
        "status": if path_malformed == 0 { "ok" } else { "malformed" },
        "malformed_lines": path_malformed,
    }));
    out
}

fn abs_path(path: &Path) -> String {
    if path.is_absolute() {
        path.display().to_string()
    } else {
        std::env::current_dir()
            .map(|cwd| cwd.join(path).display().to_string())
            .unwrap_or_else(|_| path.display().to_string())
    }
}

/// `read_jsonl_events_with_coverage`: dedupe by canonical signature across
/// paths, sort by the event key, and carry input-integrity coverage beside
/// the rows. A kind matches `kind` or `type`, whichever is set first.
pub(crate) fn read_events_with_coverage(paths: &[PathBuf], kinds: &[&str]) -> (Vec<Value>, Value) {
    let kinds: BTreeSet<String> = kinds.iter().map(|k| k.to_string()).collect();
    let mut out: Vec<Value> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    let mut seen_paths: HashSet<String> = HashSet::new();
    let mut malformed = 0usize;
    let mut unreadable = 0usize;
    let mut path_coverage: Vec<Value> = Vec::new();
    for path in paths {
        let path_key = std::fs::canonicalize(path)
            .map(|p| p.display().to_string())
            .unwrap_or_else(|_| abs_path(path));
        if !seen_paths.insert(path_key.clone()) {
            continue;
        }
        for event in one_journal_rows(
            path,
            &kinds,
            &mut malformed,
            &mut unreadable,
            &mut path_coverage,
        ) {
            if !kinds.contains(event_kind(&event).unwrap_or_default()) {
                continue;
            }
            let signature = canonical_signature(&event);
            if seen.insert(signature) {
                out.push(event);
            }
        }
    }
    out.sort_by(|a, b| event_sort_key(a).cmp(&event_sort_key(b)));
    let coverage = serde_json::json!({
        "complete": malformed == 0 && unreadable == 0,
        "malformed_lines": malformed,
        "unreadable_paths": unreadable,
        "paths": path_coverage,
    });
    (out, coverage)
}

/// `read_graph_nodes`: the tolerant whole-graph read. A strict read failure
/// reads empty; the fold's coverage-honesty lines take it from there.
pub(crate) fn read_graph_nodes(graph_db: &Path) -> Vec<Value> {
    crate::graph_store::read_rows_where(graph_db, &crate::backlog::RowQuery::default())
        .unwrap_or_default()
}

/// The graph rows every view and the classifier share. Unfiltered, so the
/// in-process classifier sees exactly the rows the keeper's own read ships.
/// One `config.paths.<key>` override read the way Python `_resolve` does for
/// these files: `~/` expands against HOME, everything else rides verbatim,
/// and a relative value anchors under `~/.fno` at the call site.
fn config_path_override(cwd: &Path, key: &str) -> Option<PathBuf> {
    let raw = crate::agents_config::config_lookup(cwd, &["paths", key])?
        .as_str()?
        .to_string();
    if raw.is_empty() {
        return None;
    }
    if let Some(rest) = raw.strip_prefix("~/") {
        let home = std::env::var_os("HOME")?;
        return Some(PathBuf::from(home).join(rest));
    }
    Some(PathBuf::from(raw))
}

pub(crate) fn graph_db_path() -> PathBuf {
    // Python `graph_json()` anchors on `state_dir()` and the store is the db
    // sibling: the FNO_STATE_DIR seal wins, then the config `state_dir` key,
    // else `~/.fno/`.
    if let Ok(dir) = std::env::var("FNO_STATE_DIR") {
        if !dir.is_empty() {
            return PathBuf::from(dir).join("db").join("graph.db");
        }
    }
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let state_dir = crate::agents_config::config_lookup(&cwd, &["state_dir"])
        .and_then(|v| v.as_str().map(String::from))
        .filter(|s| !s.is_empty())
        .map(|raw| match raw.strip_prefix("~/") {
            Some(rest) => match std::env::var_os("HOME") {
                Some(home) => PathBuf::from(home).join(rest),
                None => PathBuf::from(raw),
            },
            None => PathBuf::from(raw),
        })
        .unwrap_or_else(|| {
            let home = crate::paths::AgentsHome::from_env();
            home.root()
                .parent()
                .map(Path::to_path_buf)
                .unwrap_or_else(|| PathBuf::from(".fno"))
        });
    state_dir.join("db").join("graph.db")
}

/// The state-root files the fold reads beside the ledger: the live journal
/// and its ephemeral sibling (`events.jsonl` + `.ephemeral`).
pub(crate) fn events_paths(ledger_path: &Path) -> Vec<PathBuf> {
    let parent = ledger_path
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_default();
    vec![
        parent.join("events.jsonl"),
        parent.join("events.jsonl.ephemeral"),
    ]
}

/// `(event["data"] or {})` as a borrowed map.
pub(crate) fn event_data(event: &Value) -> Map<String, Value> {
    event
        .get("data")
        .and_then(|d| d.as_object())
        .cloned()
        .unwrap_or_default()
}

/// Python's `datetime.max` stand-in: undated loop_check fires sort LAST.
pub(crate) fn datetime_max() -> DateTime<Local> {
    Local
        .with_ymd_and_hms(9999, 12, 31, 23, 59, 59)
        .single()
        .unwrap()
}

/// Python `paths.ledger_json()`: the FNO_STATE_DIR seal wins first, then an
/// absolute `config.paths.ledger_json` override (a relative one anchors
/// under `~/.fno`), else the pinned global file.
pub(crate) fn ledger_path() -> PathBuf {
    if let Ok(dir) = std::env::var("FNO_STATE_DIR") {
        if !dir.is_empty() {
            return PathBuf::from(dir).join("ledger.json");
        }
    }
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    if let Some(override_path) = config_path_override(&cwd, "ledger_json") {
        if override_path.is_absolute() {
            return override_path;
        }
        let home = crate::paths::AgentsHome::from_env();
        return match home.root().parent() {
            Some(fno_dir) => fno_dir.join(override_path),
            None => PathBuf::from(".fno").join(override_path),
        };
    }
    let home = crate::paths::AgentsHome::from_env();
    home.root()
        .parent()
        .map(|p| p.join("ledger.json"))
        .unwrap_or_else(|| PathBuf::from(".fno/ledger.json"))
}
