//! The recoverable lane: recent Codex rollouts absent from the registry.
//!
//! The scanner never collapses damaged evidence into a clean zero. Readable
//! candidates stay visible on an incomplete scan so dry output can name them,
//! while a writing caller refuses the whole batch.

use super::{Verdict, RECOVERABLE};
use crate::client_commands::py_repr_str;
use crate::paths::AgentsHome;
use crate::research::deliverable::os_error_text;
use crate::resume_receipt::model::py_parse_error;
use crate::session_truth::{record_parser, tail_records};
use crate::store_heal::{adopt_codex_rollout, codex_meta, codex_sessions_dir, scan_files};
use serde_json::{json, Map, Value};
use std::io::BufRead;
use std::path::{Path, PathBuf};

const MAX_RECOVERY_SINCE_S: f64 = 30.0 * 24.0 * 3600.0;
const MARKER_CHARS: usize = 200;

/// One recent Codex rollout absent from the registry snapshot.
#[derive(Clone, Debug, PartialEq)]
pub struct RecoverableRollout {
    pub session_id: String,
    pub cwd: String,
    pub rollout_path: PathBuf,
    pub mtime: f64,
    pub transcript_usable: bool,
    pub last_event_at: Option<String>,
    pub last_turn_marker: Option<String>,
    pub unusable_reason: Option<String>,
}

/// Strict recovery candidates plus evidence that the scan covered its inputs.
#[derive(Clone, Debug, PartialEq)]
pub struct Scan {
    pub recoverable: Vec<RecoverableRollout>,
    pub complete: bool,
    pub scanned_count: usize,
    pub malformed_count: usize,
    pub unreadable_count: usize,
    pub failures: Vec<String>,
}

impl Scan {
    pub fn usable_recoverable_count(&self) -> usize {
        self.recoverable
            .iter()
            .filter(|row| row.transcript_usable)
            .count()
    }
}

/// The fleet row a recoverable candidate stands for: a Codex row, `orphaned`.
#[derive(Clone, Debug, PartialEq)]
pub struct RecoverableRow {
    pub row_id: String,
    pub cwd: String,
}

fn now_epoch() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

/// Parse a positive, bounded Codex recovery age such as `24h`.
pub fn parse_recovery_since(value: &str) -> Result<f64, String> {
    let raw = value.trim().to_lowercase();
    let invalid = || {
        format!(
            "invalid since duration {}; use a positive value with s, m, h, or d",
            py_repr_str(value)
        )
    };
    let unit = raw.chars().last().ok_or_else(invalid)?;
    let scale = match unit {
        's' => 1.0,
        'm' => 60.0,
        'h' => 3600.0,
        'd' => 86400.0,
        _ => return Err(invalid()),
    };
    let number = &raw[..raw.len() - unit.len_utf8()];
    let digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    let well_formed = match number.split_once('.') {
        Some((whole, fraction)) => digits(whole) && digits(fraction),
        None => digits(number),
    };
    if !well_formed {
        return Err(invalid());
    }
    let amount: f64 = number.parse().map_err(|_| invalid())?;
    let seconds = amount * scale;
    if seconds <= 0.0 || seconds > MAX_RECOVERY_SINCE_S {
        return Err(format!(
            "since duration {} is outside the bounded 1s-30d recovery window",
            py_repr_str(value)
        ));
    }
    Ok(seconds)
}

/// The path the failure names: the first component that will not stat, under
/// its already-resolved parent. Python's strict resolve reports that one, and
/// `canonicalize` does not say which component failed.
fn first_failing_prefix(candidate: &Path) -> PathBuf {
    let mut walked = PathBuf::new();
    for part in candidate.components() {
        walked.push(part);
        if let Err(exc) = std::fs::symlink_metadata(&walked) {
            // ENOTDIR blames the file standing where a directory belongs.
            if exc.raw_os_error() == Some(20) {
                walked.pop();
            }
            return walked;
        }
        if let Ok(resolved) = std::fs::canonicalize(&walked) {
            walked = resolved;
        }
    }
    walked
}

/// Resolve the exact checkout scope before touching the registry.
pub fn resolve_recovery_cwd(value: Option<&str>) -> Result<PathBuf, String> {
    let value = value.filter(|v| !v.is_empty());
    let candidate: PathBuf = match value {
        Some(v) => crate::plans_path::expanduser(v),
        None => {
            std::env::current_dir().map_err(|exc| format!("cwd could not be resolved: {exc}"))?
        }
    };
    let label = match value {
        Some(v) => py_repr_str(v),
        None => py_repr_str(&candidate.display().to_string()),
    };
    let resolved = std::fs::canonicalize(&candidate).map_err(|exc| {
        let absolute = std::env::current_dir()
            .map(|cwd| cwd.join(&candidate))
            .unwrap_or_else(|_| candidate.clone());
        let named = first_failing_prefix(&absolute);
        format!(
            "cwd {label} could not be resolved: {}",
            os_error_text(&exc, &named)
        )
    })?;
    if !resolved.is_dir() {
        return Err(format!("cwd {label} is not a directory"));
    }
    Ok(resolved)
}

/// `str(uuid.UUID(value)) == value`: the lowercase hyphenated form, exactly.
pub fn is_canonical_full_uuid(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.len() == 36
        && bytes.iter().enumerate().all(|(index, byte)| match index {
            8 | 13 | 18 | 23 => *byte == b'-',
            _ => byte.is_ascii_digit() || (b'a'..=b'f').contains(byte),
        })
}

/// `" ".join(text.split())`, whose separators include the four ASCII
/// information separators Rust's whitespace class leaves out.
fn collapse_whitespace(text: &str) -> String {
    text.split(|c: char| c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c))
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

/// `(usable, last_event_at, last_turn_marker, unusable_reason)`. A transcript
/// that cannot be read reads the same as one with no turn in it.
fn rollout_usability(
    rollout_path: &Path,
) -> (bool, Option<String>, Option<String>, Option<String>) {
    let unusable = |reason: &str| (false, None, None, Some(reason.to_string()));
    let Some(parse) = record_parser("codex") else {
        return unusable("no_readable_transcript_turn");
    };
    let records = tail_records(rollout_path, 1, parse).unwrap_or_default();
    let Some(last) = records.last() else {
        return unusable("no_readable_transcript_turn");
    };
    let marker: String = collapse_whitespace(&last.text)
        .chars()
        .take(MARKER_CHARS)
        .collect();
    if marker.is_empty() {
        return unusable("no_readable_transcript_turn");
    }
    let Some(timestamp) = last.timestamp.as_deref().filter(|t| !t.is_empty()) else {
        return unusable("transcript_timestamp_unreadable");
    };
    let Ok(parsed) = chrono::DateTime::parse_from_rfc3339(timestamp) else {
        return unusable("transcript_timestamp_unreadable");
    };
    let last_event_at = parsed
        .with_timezone(&chrono::Utc)
        .format("%Y-%m-%dT%H:%M:%SZ")
        .to_string();
    (true, Some(last_event_at), Some(marker), None)
}

/// `UnicodeDecodeError.__str__` for a line that is not UTF-8.
fn utf8_error_text(bytes: &[u8], error: &std::str::Utf8Error) -> String {
    let position = error.valid_up_to();
    let byte = bytes.get(position).copied().unwrap_or(0);
    let tail = bytes.len() - position;
    match error.error_len() {
        None if tail > 1 => format!(
            "'utf-8' codec can't decode bytes in position {position}-{}: unexpected end of data",
            bytes.len() - 1
        ),
        None => format!(
            "'utf-8' codec can't decode byte 0x{byte:02x} in position {position}: \
             unexpected end of data"
        ),
        Some(_) => {
            let reason = if (0xc2..=0xf4).contains(&byte) {
                "invalid continuation byte"
            } else {
                "invalid start byte"
            };
            format!("'utf-8' codec can't decode byte 0x{byte:02x} in position {position}: {reason}")
        }
    }
}

fn incomplete_at_root(
    failures: Vec<String>,
    malformed_count: usize,
    unreadable_count: usize,
) -> Scan {
    Scan {
        recoverable: Vec::new(),
        complete: false,
        scanned_count: 0,
        malformed_count,
        unreadable_count,
        failures,
    }
}

/// One rollout's first line classified. `Err` is the failure sentence and
/// whether it counts as malformed (else unreadable).
fn read_meta(path: &Path) -> Result<(String, String), (String, bool)> {
    let shown = path.display();
    let file = std::fs::File::open(path).map_err(|exc| {
        (
            format!(
                "rollout read failed: {shown}: {}",
                os_error_text(&exc, path)
            ),
            false,
        )
    })?;
    let mut bytes: Vec<u8> = Vec::new();
    std::io::BufReader::new(file)
        .read_until(b'\n', &mut bytes)
        .map_err(|exc| {
            (
                format!(
                    "rollout read failed: {shown}: {}",
                    os_error_text(&exc, path)
                ),
                false,
            )
        })?;
    let line = std::str::from_utf8(&bytes).map_err(|exc| {
        (
            format!(
                "rollout decode failed: {shown}: {}",
                utf8_error_text(&bytes, &exc)
            ),
            true,
        )
    })?;
    let schema = |why: &str| (format!("rollout schema failed: {shown}: {why}"), true);
    let record: Value = serde_json::from_str(line).map_err(|exc| {
        (
            format!(
                "rollout JSON failed: {shown}: {}",
                py_parse_error(line, &exc)
            ),
            true,
        )
    })?;
    if record.get("type").and_then(Value::as_str) != Some("session_meta") {
        return Err(schema("missing session_meta"));
    }
    let payload = record
        .get("payload")
        .and_then(Value::as_object)
        .ok_or_else(|| schema("payload is not an object"))?;
    let session_id = payload
        .get("id")
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty())
        .ok_or_else(|| schema("id is not a string"))?;
    if !is_canonical_full_uuid(session_id) {
        return Err(schema("id is not a canonical full UUID"));
    }
    let cwd = payload
        .get("cwd")
        .and_then(Value::as_str)
        .ok_or_else(|| schema("cwd is not a string"))?;
    Ok((session_id.to_string(), cwd.to_string()))
}

/// Recent exact-cwd Codex rollouts missing from one registry snapshot. `None`
/// roots resolve the live store and the shared registry.
pub fn scan_recoverable_codex_rollouts(
    cwd: &Path,
    recency_s: f64,
    sessions_dir: Option<&Path>,
    registry_path: Option<&Path>,
    now_s: Option<f64>,
) -> Scan {
    let root: PathBuf = sessions_dir
        .map(Path::to_path_buf)
        .unwrap_or_else(codex_sessions_dir);
    let target_cwd = cwd.display().to_string();
    let cutoff = now_s.unwrap_or_else(now_epoch) - recency_s;
    let mut failures: Vec<String> = Vec::new();
    let mut malformed_count = 0usize;
    let mut unreadable_count = 0usize;

    let registry: PathBuf = registry_path
        .map(Path::to_path_buf)
        .unwrap_or_else(AgentsHome::shared_registry_json);
    let mut registered_ids: Vec<String> = Vec::new();
    match crate::state::load_registry_with_counts(&registry) {
        Err(exc) => failures.push(format!(
            "registry load failed: {}: {exc}",
            registry.display()
        )),
        Ok((loaded, raw_rows)) => {
            if loaded.schema_version > crate::state::REGISTRY_SCHEMA_VERSION
                || raw_rows != loaded.entries.len()
            {
                failures.push(format!("registry load incomplete: {}", registry.display()));
            }
            registered_ids = loaded
                .entries
                .into_iter()
                .filter(|entry| entry.harness.as_deref() == Some("codex"))
                .filter_map(|entry| entry.harness_session_id)
                .filter(|sid| !sid.is_empty())
                .collect();
        }
    }

    if !std::fs::metadata(&root).is_ok_and(|meta| meta.is_dir()) {
        failures.push(format!("sessions root unreadable: {}", root.display()));
        return incomplete_at_root(failures, malformed_count, unreadable_count + 1);
    }

    let rollout_paths = match scan_files(&root, None, &|name| {
        name.starts_with("rollout-") && name.ends_with(".jsonl")
    }) {
        Ok(paths) => paths,
        Err(exc) => {
            failures.push(format!(
                "sessions root enumeration failed: {}: {}",
                root.display(),
                os_error_text(&exc, &root)
            ));
            return incomplete_at_root(failures, malformed_count, unreadable_count + 1);
        }
    };

    let mut dated: Vec<(f64, PathBuf)> = Vec::new();
    for rollout_path in &rollout_paths {
        match std::fs::metadata(rollout_path).and_then(|meta| meta.modified()) {
            Err(exc) => {
                failures.push(format!(
                    "rollout stat failed: {}: {}",
                    rollout_path.display(),
                    os_error_text(&exc, rollout_path)
                ));
                unreadable_count += 1;
            }
            Ok(modified) => {
                let mtime = modified
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs_f64())
                    .unwrap_or(0.0);
                if mtime >= cutoff {
                    dated.push((mtime, rollout_path.clone()));
                }
            }
        }
    }
    dated.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));

    let mut recoverable: Vec<RecoverableRollout> = Vec::new();
    let mut seen: Vec<String> = Vec::new();
    for (mtime, rollout_path) in dated {
        let (session_id, rollout_cwd) = match read_meta(&rollout_path) {
            Ok(meta) => meta,
            Err((failure, malformed)) => {
                failures.push(failure);
                if malformed {
                    malformed_count += 1;
                } else {
                    unreadable_count += 1;
                }
                continue;
            }
        };
        if seen.contains(&session_id) {
            continue;
        }
        seen.push(session_id.clone());
        if rollout_cwd != target_cwd || registered_ids.contains(&session_id) {
            continue;
        }
        let (transcript_usable, last_event_at, last_turn_marker, unusable_reason) =
            rollout_usability(&rollout_path);
        recoverable.push(RecoverableRollout {
            session_id,
            cwd: rollout_cwd,
            rollout_path,
            mtime,
            transcript_usable,
            last_event_at,
            last_turn_marker,
            unusable_reason,
        });
    }

    Scan {
        recoverable,
        complete: failures.is_empty(),
        scanned_count: rollout_paths.len(),
        malformed_count,
        unreadable_count,
        failures,
    }
}

/// The non-destructive watchdog payload over one scan. `session_id` narrows
/// the scan itself, so a later apply adopts only the selected session.
pub fn sweep_from_scan(
    cwd: &Path,
    mut scan: Scan,
    now_s: f64,
    session_id: Option<&str>,
) -> (Value, Vec<RecoverableRow>, Scan) {
    if let Some(selected) = session_id {
        scan.recoverable.retain(|row| row.session_id == selected);
    }
    let mut rows: Vec<RecoverableRow> = Vec::new();
    let mut verdicts: Vec<Verdict> = Vec::new();
    for candidate in &scan.recoverable {
        rows.push(RecoverableRow {
            row_id: candidate.session_id.clone(),
            cwd: candidate.cwd.clone(),
        });
        let shown = candidate.rollout_path.display();
        let basis = if candidate.transcript_usable {
            format!("Codex rollout {shown} is absent from the registry")
        } else {
            format!(
                "Codex rollout {shown} is unusable: {}",
                candidate
                    .unusable_reason
                    .as_deref()
                    .filter(|r| !r.is_empty())
                    .unwrap_or("transcript_unusable")
            )
        };
        let action = if candidate.transcript_usable {
            "adopt"
        } else {
            "refuse"
        };
        verdicts.push(Verdict {
            row_id: candidate.session_id.clone(),
            name: crate::identity::canonical_handle(&candidate.session_id),
            state: "orphaned".to_string(),
            verdict: RECOVERABLE.to_string(),
            basis,
            action: action.to_string(),
            agent: "codex".to_string(),
        });
    }
    let counts = if scan.complete {
        json!({ RECOVERABLE: verdicts.len() })
    } else {
        json!({})
    };
    let generated_at = chrono::DateTime::from_timestamp(now_s.floor() as i64, 0)
        .map(|t| t.format("%Y-%m-%dT%H:%M:%SZ").to_string())
        .unwrap_or_default();
    let mut payload = Map::new();
    payload.insert("generated_at".to_string(), json!(generated_at));
    payload.insert("verdicts".to_string(), json!(verdicts));
    payload.insert("counts".to_string(), counts);
    payload.insert("warnings".to_string(), json!(scan.failures));
    payload.insert("complete".to_string(), json!(scan.complete));
    payload.insert("scanned_count".to_string(), json!(scan.scanned_count));
    payload.insert("malformed_count".to_string(), json!(scan.malformed_count));
    payload.insert("unreadable_count".to_string(), json!(scan.unreadable_count));
    payload.insert("cwd".to_string(), json!(cwd.display().to_string()));
    if let Some(selected) = session_id {
        payload.insert("selected_session_id".to_string(), json!(selected));
    }
    if scan.complete {
        let usable = scan.usable_recoverable_count();
        payload.insert("recoverable_count".to_string(), json!(verdicts.len()));
        payload.insert("usable_recoverable_count".to_string(), json!(usable));
        payload.insert(
            "unusable_recoverable_count".to_string(),
            json!(scan.recoverable.len() - usable),
        );
    }
    (Value::Object(payload), rows, scan)
}

/// Scan the live store and build the payload.
pub fn run_recoverable_sweep(
    cwd: &Path,
    recency_s: f64,
    now_s: Option<f64>,
    session_id: Option<&str>,
) -> (Value, Vec<RecoverableRow>, Scan) {
    let now = now_s.unwrap_or_else(now_epoch);
    let scan = scan_recoverable_codex_rollouts(cwd, recency_s, None, None, Some(now));
    sweep_from_scan(cwd, scan, now, session_id)
}

/// The evidence a re-read still vouches for: `(last_event_at, last_turn_marker)`.
type Evidence = (Option<String>, Option<String>);

/// Re-read the rollout. `Err` is `(reason, detail)`: the transcript no longer
/// matches what the scan saw, so adoption must not stand on it.
fn transcript_readback(candidate: &RecoverableRollout) -> Result<Evidence, (String, String)> {
    let refuse = |reason: &str, detail: String| Err((reason.to_string(), detail));
    let shown = candidate.rollout_path.display();
    if !candidate.transcript_usable {
        let reason = candidate
            .unusable_reason
            .as_deref()
            .filter(|r| !r.is_empty())
            .unwrap_or("no_readable_transcript_turn");
        return refuse(
            "transcript_unusable",
            format!("transcript_unusable: {reason}"),
        );
    }
    if !candidate.rollout_path.is_file() {
        return refuse(
            "transcript_unavailable",
            format!("rollout vanished before adoption: {shown}"),
        );
    }
    let expected = (candidate.session_id.clone(), candidate.cwd.clone());
    if codex_meta(&candidate.rollout_path) != Some(expected) {
        return refuse(
            "transcript_changed",
            format!("transcript_changed: rollout identity changed: {shown}"),
        );
    }
    let (usable, last_event_at, last_turn_marker, unusable_reason) =
        rollout_usability(&candidate.rollout_path);
    if !usable {
        return refuse(
            "transcript_changed",
            format!(
                "transcript_changed: {}",
                unusable_reason.as_deref().unwrap_or("transcript_unusable")
            ),
        );
    }
    if last_event_at != candidate.last_event_at || last_turn_marker != candidate.last_turn_marker {
        return refuse(
            "transcript_changed",
            "transcript_changed: last event evidence no longer matches the scan".to_string(),
        );
    }
    Ok((last_event_at, last_turn_marker))
}

fn is_adopted_row(entry: &crate::state::RegistryEntry, candidate: &RecoverableRollout) -> bool {
    entry.harness.as_deref() == Some("codex")
        && entry.harness_session_id.as_deref() == Some(candidate.session_id.as_str())
        && entry.cwd == candidate.cwd
        && entry.origin.as_deref() == Some("adopted")
}

/// Adopt one candidate through the shared store-hit writer, then re-read the
/// transcript. The follow-up path rides the adoption write itself: a second
/// write afterward leaves a window where the row stays registered with no
/// resume path. `Err` is the batch-level `adoption_failed` detail.
fn adopt_one(candidate: &RecoverableRollout, registry_path: &Path) -> Result<Value, String> {
    let session_id = candidate.session_id.as_str();
    let process_cwd = std::env::current_dir().map_err(|exc| exc.to_string())?;
    let state_root = crate::agents_config::state_dir(&process_cwd)
        .ok_or_else(|| "state root could not be resolved".to_string())?;
    let log_path = state_root
        .join("agents")
        .join(session_id)
        .join("output.jsonl")
        .display()
        .to_string();
    adopt_codex_rollout(
        session_id,
        &candidate.cwd,
        &candidate.rollout_path,
        registry_path,
        session_id,
        &log_path,
    )
    .map_err(|err| err.message().to_string())?;
    let entries = crate::state::load_registry(registry_path)
        .map_err(|exc| exc.to_string())?
        .entries;
    let exact: Vec<&crate::state::RegistryEntry> = entries
        .iter()
        .filter(|entry| is_adopted_row(entry, candidate))
        .collect();
    if exact.len() != 1 {
        return Err("registry did not contain exactly one adopted Codex row".to_string());
    }
    if exact[0].log_path.as_deref() != Some(log_path.as_str()) {
        return Err("adopted Codex row is missing its full-ID follow-up path".to_string());
    }
    match transcript_readback(candidate) {
        Err((reason, detail)) => {
            let removed = crate::state::update_registry(registry_path, |registry| {
                let before = registry.entries.len();
                registry
                    .entries
                    .retain(|entry| !is_adopted_row(entry, candidate));
                before - registry.entries.len()
            })
            .map_err(|exc| exc.to_string())?;
            if removed != 1 {
                return Err(
                    "registry rollback did not remove exactly one adopted Codex row".to_string(),
                );
            }
            Ok(json!({
                "session_id": session_id,
                "outcome": "refused",
                "reason": reason,
                "transcript_usable": false,
                "registry_rollback": "removed",
                "detail": detail,
            }))
        }
        Ok((last_event_at, last_turn_marker)) => Ok(json!({
            "session_id": session_id,
            "outcome": "applied",
            "transcript_usable": true,
            "last_event_at": last_event_at,
            "last_turn_marker": last_turn_marker,
            "registry_row_count": exact.len(),
            "detail": format!("adopted {session_id} handle={}", exact[0].name),
        })),
    }
}

/// [`apply_recoverable`] against an explicit registry, with the tick budget.
/// `should_apply` answering false defers every remaining candidate.
pub fn apply_recoverable_with(
    scan: &Scan,
    registry_path: &Path,
    should_apply: Option<&dyn Fn() -> bool>,
) -> Vec<Value> {
    if !scan.complete {
        let reason = if scan.failures.is_empty() {
            "coverage could not be established".to_string()
        } else {
            scan.failures.join("; ")
        };
        return vec![json!({
            "session_id": Value::Null,
            "outcome": "refused",
            "detail": format!("recovery coverage incomplete: {reason}"),
        })];
    }
    let mut results: Vec<Value> = Vec::new();
    for (index, candidate) in scan.recoverable.iter().enumerate() {
        let session_id = candidate.session_id.as_str();
        if should_apply.is_some_and(|allowed| !allowed()) {
            for remaining in &scan.recoverable[index..] {
                results.push(json!({
                    "session_id": remaining.session_id,
                    "outcome": "deferred",
                    "detail": "tick budget spent; retry on the next tick",
                }));
            }
            break;
        }
        if let Err((reason, detail)) = transcript_readback(candidate) {
            results.push(json!({
                "session_id": session_id,
                "outcome": "refused",
                "reason": reason,
                "transcript_usable": false,
                "detail": detail,
            }));
            continue;
        }
        // One vanished row never aborts the batch.
        results.push(
            adopt_one(candidate, registry_path).unwrap_or_else(|detail| {
                json!({
                    "session_id": session_id,
                    "outcome": "refused",
                    "reason": "adoption_failed",
                    "transcript_usable": true,
                    "detail": detail,
                })
            }),
        );
    }
    results
}

/// Adopt a complete scan into the shared registry through the store-hit writer.
pub fn apply_recoverable(scan: &Scan) -> Vec<Value> {
    apply_recoverable_with(scan, &AgentsHome::shared_registry_json(), None)
}

pub fn recovery_result_counts(results: &[Value]) -> Value {
    let count = |word: &str| {
        results
            .iter()
            .filter(|result| result.get("outcome").and_then(Value::as_str) == Some(word))
            .count()
    };
    json!({
        "applied": count("applied"),
        "refused": count("refused"),
        "deferred": count("deferred"),
    })
}
