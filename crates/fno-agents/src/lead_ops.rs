//! The lead commands the Python `fno.lead` package answered last: the scope
//! drain, the last-run read, role expiry and escalation, plus the manifest and
//! wake-ledger ops the PR watcher's lead wake and the role commands reach
//! through the `lead` spawn-overlay kind. The term ledger page rides
//! `lead-rundown --ledger` (rundown.rs). Contract: docs/architecture/lead.md.

use crate::claude_ask::py_repr;
use crate::state::RegistryEntry;
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Both lead arms end a walk: the walk arm emits `loop_terminated`, the
/// in-session stop arm `termination`.
const TERMINAL_TYPES: [&str; 2] = ["loop_terminated", "termination"];

/// The walk's respawn budget when the manifest names none.
const DEFAULT_RESPAWN_CEILING: i64 = 4;

/// Byte bound on the `wake_times` line, not a rate bound: it stops a bypassed
/// ledger growing without limit.
const DEFAULT_KEEP: usize = 32;

/// The trailing window a wake stamp counts inside. A stamp exactly 24h old
/// is outside it.
const WAKE_WINDOW_S: i64 = 86_400;

const STAMP_FMT: &str = "%Y-%m-%dT%H:%M:%SZ";

fn cwd() -> PathBuf {
    std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
}

/// `24h` / `90m` / `7d` / `30s` / a bare second count -> seconds.
pub(crate) fn parse_window(window: &str) -> Result<i64, String> {
    let raw = window.trim();
    let digits: String = raw.chars().take_while(char::is_ascii_digit).collect();
    let unit = &raw[digits.len()..];
    let scale = match unit {
        "" | "s" => 1,
        "m" => 60,
        "h" => 3600,
        "d" => 86400,
        _ => 0,
    };
    match digits.parse::<i64>() {
        Ok(n) if scale > 0 => Ok(n * scale),
        _ => Err(format!(
            "unparseable window {}; use e.g. 24h, 90m, 7d",
            py_repr(window)
        )),
    }
}

fn parse_ts(raw: &str) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(&raw.replace('Z', "+00:00"))
        .ok()
        .map(|t| t.timestamp())
        .or_else(|| {
            chrono::NaiveDateTime::parse_from_str(raw, "%Y-%m-%dT%H:%M:%S%.f")
                .ok()
                .map(|t| t.and_utc().timestamp())
        })
}

fn format_stamp(secs: i64) -> String {
    chrono::DateTime::from_timestamp(secs, 0)
        .map(|t| t.format(STAMP_FMT).to_string())
        .unwrap_or_default()
}

/// True when a lead walk terminal landed inside the window. Every failure
/// answers false (no evidence); a corrupt line skips itself.
pub(crate) fn last_run_is_fresh(events: &Path, since_s: i64, now: i64) -> bool {
    let query = crate::event_store::EventQuery::of_types(&TERMINAL_TYPES);
    let text = match crate::event_store::journal_text_checked(events, &query) {
        Ok(text) => text,
        Err(_) => std::fs::read_to_string(events).unwrap_or_default(),
    };
    let mut newest: Option<i64> = None;
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || !TERMINAL_TYPES.iter().any(|t| line.contains(t)) {
            continue;
        }
        let Ok(event) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let kind = event.get("type").and_then(Value::as_str).unwrap_or("");
        if !TERMINAL_TYPES.contains(&kind) {
            continue;
        }
        if event.pointer("/data/driver").and_then(Value::as_str) != Some("lead") {
            continue;
        }
        if let Some(ts) = event.get("ts").and_then(Value::as_str).and_then(parse_ts) {
            newest = Some(newest.map_or(ts, |n| n.max(ts)));
        }
    }
    newest.is_some_and(|ts| now - ts <= since_s)
}

/// `fno inbox board --last-run [--since WINDOW]`: exit 0 when a lead walk
/// terminated inside the window, 1 when not, 2 on a bad window.
pub fn run_last_run(args: &[String]) -> i32 {
    let mut since = "24h".to_string();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--last-run" | "--user-command" => i += 1,
            "--since" if i + 1 < args.len() => {
                since = args[i + 1].clone();
                i += 2;
            }
            other => {
                eprintln!("fno inbox board: unknown flag {other}");
                return 2;
            }
        }
    }
    let window = match parse_window(&since) {
        Ok(window) => window,
        Err(e) => {
            eprintln!("{e}");
            return 2;
        }
    };
    let events = crate::paths::project_events_path();
    let fresh = last_run_is_fresh(&events, window, chrono::Utc::now().timestamp());
    println!(
        "last lead walk within {since}: {}",
        if fresh { "yes" } else { "no" }
    );
    if fresh {
        0
    } else {
        1
    }
}

// ---------------------------------------------------------------------------
// The scope drain
// ---------------------------------------------------------------------------

/// The graph rows the drain reads. Strict: an unreadable graph is an
/// error, never an empty scope a walk would read as drained.
pub(crate) fn scope_rows(cwd: &Path) -> Result<Vec<Value>, String> {
    let path = crate::org_board::scope::graph_json_path(cwd);
    crate::graph_store::read_rows_where_strict(
        &path,
        &crate::backlog::RowQuery {
            fields: Some(
                [
                    "id",
                    "type",
                    "parent",
                    "project",
                    "status",
                    "superseded_by",
                    "deferred_kind",
                    "completed_at",
                ]
                .into_iter()
                .map(str::to_string)
                .collect(),
            ),
            ..Default::default()
        },
    )
    .map_err(|e| e.to_string())
}

/// The node ids a role scope covers (`compile_territory`).
pub(crate) fn scope_ids(scope: &str, rows: &[Value], cwd: &Path) -> Result<Vec<String>, String> {
    let projects = crate::org_board::scope::project_map(cwd);
    let mut ids: Vec<String> = crate::territory::compile_scope_ids(scope, rows, &projects)?
        .into_iter()
        .collect();
    ids.sort();
    Ok(ids)
}

/// Scope nodes not closed for good, over ids already compiled. Closure is
/// `is_terminal_entry`, never a bare completed_at test.
pub(crate) fn undelivered_of(ids: &[String], rows: &[Value]) -> i64 {
    let ids: std::collections::HashSet<&str> = ids.iter().map(String::as_str).collect();
    rows.iter()
        .filter(|row| {
            row.get("id")
                .and_then(Value::as_str)
                .is_some_and(|id| ids.contains(id))
                && !crate::graph_store::is_terminal_entry(row)
        })
        .count() as i64
}

/// The term goal's own count. An uncompilable scope is an error, so a reader
/// that would end a term on zero refuses.
pub(crate) fn scope_undelivered(scope: &str, rows: &[Value], cwd: &Path) -> Result<i64, String> {
    Ok(undelivered_of(&scope_ids(scope, rows, cwd)?, rows))
}

fn drain_cache_file(cwd: &Path) -> Option<PathBuf> {
    crate::agents_config::state_dir(cwd).map(|root| root.join("cache").join("lead-drain.json"))
}

/// The cache identity: the store's own content version. A changed graph
/// misses by construction.
fn graph_ident(cwd: &Path) -> Option<String> {
    let path = crate::org_board::scope::graph_json_path(cwd);
    crate::backlog::version(&path)
        .ok()
        .filter(|v| !v.is_empty())
}

fn cache_load(cwd: &Path, scope: &str, ident: &str) -> Option<i64> {
    let text = std::fs::read_to_string(drain_cache_file(cwd)?).ok()?;
    let data: Value = serde_json::from_str(&text).ok()?;
    let row = data.get(scope)?;
    let key = json!(["sqlite", ident]);
    (row.get("ident") == Some(&key))
        .then(|| row.get("undelivered").and_then(Value::as_i64))
        .flatten()
}

/// `path` with `suffix` appended to its whole name: `x.json` -> `x.json.<pid>.tmp`.
fn sibling(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(suffix);
    PathBuf::from(name)
}

/// Best-effort atomic write; a failed write is dropped, because a cache
/// must never add a failure mode to the drain.
fn cache_store(cwd: &Path, scope: &str, ident: &str, undelivered: i64) {
    let Some(path) = drain_cache_file(cwd) else {
        return;
    };
    let Some(parent) = path.parent() else {
        return;
    };
    if std::fs::create_dir_all(parent).is_err() {
        return;
    }
    let mut data: Map<String, Value> = std::fs::read_to_string(&path)
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default();
    data.insert(
        scope.to_string(),
        json!({"ident": ["sqlite", ident], "undelivered": undelivered}),
    );
    let tmp = sibling(&path, &format!(".{}.tmp", std::process::id()));
    if std::fs::write(&tmp, Value::Object(data).to_string()).is_ok() {
        let _ = std::fs::rename(&tmp, &path);
    }
}

/// The drain read behind `fno agents org drain` and the walk's in-process
/// readers: `(undelivered, cached)`. The cache answers only while the store
/// version it recorded is still current; the count is stored only when the
/// version did not move across the read.
pub(crate) fn drain(scope: &str, cwd: &Path) -> Result<(i64, bool), String> {
    // A local graph left behind under an external tracker is not the scope:
    // its zero would read as drained and end the walk with work pending.
    if crate::graph_get::external_backend_selected() {
        return Err("tracker metadata unavailable under an external tracker backend".to_string());
    }
    let before = graph_ident(cwd);
    if let Some(ident) = &before {
        if let Some(cached) = cache_load(cwd, scope, ident) {
            return Ok((cached, true));
        }
    }
    let rows = scope_rows(cwd)?;
    let undelivered = scope_undelivered(scope, &rows, cwd)?;
    if let (Some(pre), Some(post)) = (&before, graph_ident(cwd)) {
        if *pre == post {
            cache_store(cwd, scope, &post, undelivered);
        }
    }
    Ok((undelivered, false))
}

/// `fno agents org drain SCOPE`: how many scope nodes are not done. An
/// unreadable graph exits 1 on purpose: a walk that cannot see the scope
/// must not read the failure as drained.
pub fn run_drain(args: &[String]) -> i32 {
    let scope = match args {
        [scope] if !scope.starts_with('-') => scope.clone(),
        _ => {
            eprintln!("Usage: fno agents org drain SCOPE");
            return 2;
        }
    };
    match drain(&scope, &cwd()) {
        // The receipt keeps the Python leg's json.dumps bytes: its key
        // order, and a space after every separator.
        Ok((undelivered, cached)) => {
            println!(
                "{{\"scope\": {}, \"undelivered\": {undelivered}{}}}",
                json!(scope),
                if cached { ", \"cached\": true" } else { "" }
            );
            0
        }
        Err(e) => {
            eprintln!("lead: drain for {} unreadable: {e}", py_repr(&scope));
            1
        }
    }
}

// ---------------------------------------------------------------------------
// The manifest and its wake ledger
// ---------------------------------------------------------------------------

/// The frontmatter fields, Python `parse_manifest`'s reading: a quoted value
/// decodes as JSON. An unreadable manifest reads as absent.
pub(crate) fn parse_manifest(path: &Path) -> Map<String, Value> {
    let mut out = Map::new();
    let Ok(text) = std::fs::read_to_string(path) else {
        return out;
    };
    for line in text.lines() {
        if line.trim() == "---" {
            continue;
        }
        let Some((key, raw)) = line.split_once(':') else {
            continue;
        };
        let raw = raw.trim();
        let value = if raw.starts_with('"') {
            serde_json::from_str::<Value>(raw)
                .ok()
                .filter(Value::is_string)
                .unwrap_or_else(|| Value::String(raw.trim_matches('"').to_string()))
        } else {
            Value::String(raw.to_string())
        };
        out.insert(key.trim().to_string(), value);
    }
    out
}

fn field<'a>(fields: &'a Map<String, Value>, key: &str) -> &'a str {
    fields.get(key).and_then(Value::as_str).unwrap_or("")
}

fn manifest_int(fields: &Map<String, Value>, key: &str, default: i64) -> i64 {
    match field(fields, key) {
        "" => default,
        raw => raw.parse().unwrap_or(default),
    }
}

/// `(at_ceiling, ceiling)`: the respawn budget with the Rust walk's defaults.
pub(crate) fn respawn_budget(fields: &Map<String, Value>) -> (bool, i64) {
    let ceiling = manifest_int(fields, "respawn_ceiling", DEFAULT_RESPAWN_CEILING);
    (
        ceiling > 0 && manifest_int(fields, "respawn_count", 0) >= ceiling,
        ceiling,
    )
}

/// The stamps inside the trailing 24h, oldest first. An unparseable stamp
/// is dropped.
pub(crate) fn read_wakes(path: &Path, now: i64) -> Vec<i64> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    let mut stamps: Vec<i64> = text
        .lines()
        .map(str::trim)
        .filter_map(|line| line.strip_prefix("wake_times:"))
        .flat_map(|list| list.split(','))
        .map(|raw| raw.trim().trim_matches('"'))
        .filter(|raw| !raw.is_empty())
        .filter_map(|raw| chrono::NaiveDateTime::parse_from_str(raw, STAMP_FMT).ok())
        .map(|t| t.and_utc().timestamp())
        .filter(|ts| now - ts < WAKE_WINDOW_S)
        .collect();
    stamps.sort_unstable();
    stamps
}

/// Replace the `wake_times` line only; the caller holds the lock. A manifest
/// from before the field existed gets it after the last lead field.
fn rewrite_wake_times(path: &Path, stamps: &[i64]) -> Result<(), String> {
    let Ok(content) = std::fs::read_to_string(path) else {
        return Ok(());
    };
    let joined: Vec<String> = stamps.iter().map(|s| format_stamp(*s)).collect();
    let line = format!("wake_times: {}", joined.join(","));
    let mut lines: Vec<String> = content.lines().map(str::to_string).collect();
    if let Some(at) = lines
        .iter()
        .position(|l| l.trim().starts_with("wake_times:"))
    {
        lines[at] = line;
    } else {
        let anchor = lines.iter().rposition(|l| {
            matches!(
                l.split(':').next().unwrap_or("").trim(),
                "fno_id" | "respawn_count" | "respawn_ceiling"
            )
        });
        lines.insert(anchor.map_or(0, |a| a + 1), line);
    }
    let tmp = sibling(path, ".tmp");
    std::fs::write(&tmp, format!("{}\n", lines.join("\n")))
        .and_then(|()| std::fs::rename(&tmp, path))
        .map_err(|e| format!("cannot rewrite {}: {e}", path.display()))
}

/// Decide AND bill one wake under the manifest lock, so two overlapping
/// ticks cannot both dispatch: `(refusal, count)`, refusal empty on allow.
/// A ceiling of 0 is the unbounded spelling.
pub(crate) fn admit_wake(
    path: &Path,
    now: i64,
    ceiling: i64,
    debounce_s: i64,
    keep: usize,
) -> Result<(&'static str, usize), String> {
    if !path.exists() {
        // No ledger, no wake; the walk construction names a missing manifest.
        return Ok(("debounce", 0));
    }
    crate::lead_state::with_manifest_lock(path, || {
        let mut stamps = read_wakes(path, now);
        if stamps.last().is_some_and(|last| now - last < debounce_s) {
            return Ok(("debounce", stamps.len()));
        }
        if ceiling > 0 && stamps.len() as i64 >= ceiling {
            return Ok(("ceiling", stamps.len()));
        }
        stamps.push(now);
        // The ledger must hold what the gate may count, or a ceiling above
        // the keep could never be reached.
        let keep = if ceiling > 0 {
            keep.max(ceiling as usize)
        } else {
            keep
        };
        let start = stamps.len().saturating_sub(keep.max(1));
        let kept = &stamps[start..];
        rewrite_wake_times(path, kept)?;
        Ok(("", kept.len()))
    })
}

/// An unconditional bill for a caller that already decided.
fn bill_wake(path: &Path, now: i64, keep: usize) -> Result<usize, String> {
    if !path.exists() {
        return Ok(0);
    }
    crate::lead_state::with_manifest_lock(path, || {
        let mut stamps = read_wakes(path, now);
        stamps.push(now);
        stamps.sort_unstable();
        let start = stamps.len().saturating_sub(keep.max(1));
        rewrite_wake_times(path, &stamps[start..])?;
        Ok(stamps.len() - start)
    })
}

/// Remove one scope's manifest under its lock. With an expected session, a
/// manifest naming another session stays: a successor armed mid-expiry owns
/// it. Best-effort; false names the refusal or a failure.
pub(crate) fn remove_manifest(root: &Path, scope: &str, expected: Option<&str>) -> bool {
    let Ok(path) = crate::lead_state::manifest_path(root, scope) else {
        return false;
    };
    crate::lead_state::with_manifest_lock(&path, || {
        if let Some(expected) = expected.filter(|e| !e.is_empty()) {
            if field(&parse_manifest(&path), "harness_session_id") != expected {
                return Ok(false);
            }
        }
        match std::fs::remove_file(&path) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.to_string()),
            _ => Ok(true),
        }
    })
    .unwrap_or(false)
}

/// The manifest naming `session` as holder, with `role_scope` set to its
/// file stem: the record a register re-stamps a stripped row from.
fn manifest_for_session(root: &Path, session: &str) -> Option<Map<String, Value>> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(root.join("leads"))
        .ok()?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "md"))
        .collect();
    files.sort();
    files.into_iter().find_map(|path| {
        let mut fields = parse_manifest(&path);
        if field(&fields, "harness_session_id").trim() != session {
            return None;
        }
        let stem = path.file_stem()?.to_string_lossy().into_owned();
        fields.insert("role_scope".into(), Value::String(stem));
        Some(fields)
    })
}

/// Merge `fields` into a JSON sidecar under the manifest-lock helper, so two
/// overlapping ticks never lose each other's key. A corrupt sidecar reads
/// as empty.
pub(crate) fn update_sidecar(path: &Path, fields: &Map<String, Value>) -> Result<(), String> {
    crate::lead_state::with_manifest_lock(path, || {
        let mut data: Map<String, Value> = std::fs::read_to_string(path)
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default();
        for (k, v) in fields {
            data.insert(k.clone(), v.clone());
        }
        let tmp = sibling(path, &format!(".{}.tmp", std::process::id()));
        std::fs::write(&tmp, Value::Object(data).to_string())
            .and_then(|()| std::fs::rename(&tmp, path))
            .map_err(|e| format!("cannot write {}: {e}", path.display()))
    })
}

/// The `lead` spawn-overlay kind: the manifest, wake-ledger and scope ops
/// the Python callers of the retired `fno.lead` helpers reach. Every op
/// answers an object; a refusal is `{"error": ...}`.
pub fn overlay_kind(payload: &Value) -> Result<Value, String> {
    let s = |k: &str| payload.get(k).and_then(Value::as_str).unwrap_or("");
    let path = || PathBuf::from(s("path"));
    let now = || {
        payload
            .get("now")
            .and_then(Value::as_str)
            .and_then(parse_ts)
            .unwrap_or_else(|| chrono::Utc::now().timestamp())
    };
    let int = |k: &str, default: i64| payload.get(k).and_then(Value::as_i64).unwrap_or(default);
    let keep = || int("keep", DEFAULT_KEEP as i64).max(1) as usize;
    let root = || match s("root") {
        "" => crate::paths::space_dir(&match s("owner_cwd") {
            "" => cwd(),
            owner => PathBuf::from(owner),
        }),
        root => PathBuf::from(root),
    };
    let answer = match payload.get("op").and_then(Value::as_str) {
        Some("path") => match crate::lead_state::manifest_path(&root(), s("scope")) {
            Ok(p) => json!({"path": p.display().to_string()}),
            Err(e) => json!({"error": e}),
        },
        Some("read") => {
            let fields = parse_manifest(&path());
            let (at_ceiling, ceiling) = respawn_budget(&fields);
            json!({"fields": fields, "at_respawn_ceiling": at_ceiling, "respawn_ceiling": ceiling})
        }
        Some("remove") => {
            let expected = payload.get("expected_session").and_then(Value::as_str);
            json!({"removed": remove_manifest(&root(), s("scope"), expected)})
        }
        Some("for-session") => json!({"fields": manifest_for_session(&root(), s("session_id"))}),
        Some("admit-wake") => match admit_wake(
            &path(),
            now(),
            int("ceiling", 32),
            int("debounce_s", 900),
            keep(),
        ) {
            Ok((refusal, count)) => json!({"refusal": refusal, "count": count}),
            Err(e) => json!({"error": e}),
        },
        Some("bill-wake") => match bill_wake(&path(), now(), keep()) {
            Ok(count) => json!({"count": count}),
            Err(e) => json!({"error": e}),
        },
        Some("read-wakes") => {
            let stamps: Vec<String> = read_wakes(&path(), now())
                .into_iter()
                .map(format_stamp)
                .collect();
            json!({"stamps": stamps})
        }
        Some("scope") => {
            let here = match s("cwd") {
                "" => cwd(),
                c => PathBuf::from(c),
            };
            match scope_rows(&here).and_then(|rows| {
                let ids = scope_ids(s("scope"), &rows, &here)?;
                let undelivered = undelivered_of(&ids, &rows);
                Ok(json!({"ids": ids, "undelivered": undelivered}))
            }) {
                Ok(v) => v,
                Err(e) => json!({"error": e}),
            }
        }
        Some("last-run") => json!({"fresh": last_run_is_fresh(
            &path(),
            int("since_s", 86_400),
            now(),
        )}),
        // The caller's own store when it names one: a configured graph path
        // must not key its memo on the default store's version.
        Some("graph-ident") => json!({"ident": match s("path") {
            "" => graph_ident(&cwd()),
            p => crate::backlog::version(Path::new(p))
                .ok()
                .filter(|v| !v.is_empty()),
        }}),
        // The term reader over one scope, the manifests under the owner's
        // space and the alias map from this checkout.
        Some("term") => {
            let state = crate::lead_state::lead_state_in_repo(
                &root(),
                &cwd(),
                Some(s("scope")).filter(|v| !v.is_empty()),
                None,
                None,
                &crate::paths::AgentsHome::from_env().registry_json(),
            );
            serde_json::to_value(state).map_err(|e| e.to_string())?
        }
        Some("sidecar-update") => {
            let fields = payload
                .get("fields")
                .and_then(Value::as_object)
                .cloned()
                .unwrap_or_default();
            match update_sidecar(&path(), &fields) {
                Ok(()) => json!({"ok": true}),
                Err(e) => json!({"error": e}),
            }
        }
        other => {
            return Err(format!(
                "lead: unknown op {other:?}; expected path|read|remove|for-session|admit-wake|\
                 bill-wake|read-wakes|scope|last-run|graph-ident|term|sidecar-update"
            ))
        }
    };
    Ok(answer)
}

// ---------------------------------------------------------------------------
// done: expire a role
// ---------------------------------------------------------------------------

fn row_session(row: &RegistryEntry) -> Option<String> {
    [&row.harness_session_id, &row.cc_session_id]
        .into_iter()
        .flatten()
        .find(|s| !s.is_empty())
        .cloned()
        .or_else(|| (!row.short_id.is_empty()).then(|| row.short_id.clone()))
}

fn vacate(row: &mut RegistryEntry) {
    row.role_level = None;
    row.role_scope = None;
    row.role_grantor = None;
}

fn emit_vacated(
    home: &crate::paths::AgentsHome,
    scope: &str,
    row: Option<&RegistryEntry>,
    holder_session: Option<&str>,
    cause: &str,
) {
    let events = crate::events::EventEmitter::new(home.events_jsonl(), "agents");
    let _ = events.emit(
        "agent_role_vacated",
        &json!({
            "scope": scope,
            "level": row.and_then(|r| r.role_level),
            "holder": row.map(|r| r.name.clone()),
            "holder_session": row.map_or(holder_session.map(str::to_string), |r| r.harness_session_id.clone()),
            "grantor": row.and_then(|r| r.role_grantor.clone()),
            "cause": cause,
            "successor": Value::Null,
        }),
    );
}

fn refuse(code: i32, text: &str) -> i32 {
    eprintln!("{text}");
    code
}

/// `fno agents org done [--scope SCOPE]`: vacate the role's row, then clear
/// its manifest. The departure half of the lifecycle: a lead ending its term
/// calls it so a successor arms without --force.
pub fn run_done(args: &[String]) -> i32 {
    let mut scope = String::new();
    match args {
        [] => {}
        [flag, value] if flag == "--scope" => scope = value.clone(),
        _ => return refuse(2, "Usage: fno agents org done [--scope SCOPE]"),
    }
    let here = cwd();
    let projects: HashMap<String, String> =
        crate::org_board::scope::project_map(&here).unwrap_or_default();
    if !scope.trim().is_empty() {
        // Rows store the canonical form, so the compares below must not
        // depend on member order or aliases.
        scope = crate::loop_lead::territory_members(&scope, &projects).join(",");
    }
    let home = crate::paths::AgentsHome::from_env();
    let registry_path = home.registry_json();
    let load = || crate::state::load_registry(&registry_path).map(|r| r.entries);
    let caller =
        match crate::spawn_promote::calling_row(&home) {
            crate::spawn_promote::Calling::Unreadable(_)
            | crate::spawn_promote::Calling::Unregistered(_) => return refuse(
                2,
                "lead: cannot verify the caller's role: this session carries an agent identity \
                 the registry does not resolve to a row, and expiring a role requires a holder. \
                 Run /fno-me or retry, or ask your lead to expire it.",
            ),
            crate::spawn_promote::Calling::Human => None,
            crate::spawn_promote::Calling::Agent(row, _) => Some(*row),
        };
    let answers = |held: Option<&str>| {
        held.is_some_and(|h| crate::loop_lead::team_answers_to(h, &scope, &projects))
    };
    let holder_name: String = match &caller {
        None => {
            if scope.trim().is_empty() {
                return refuse(
                    2,
                    "lead: a terminal with no session identity holds no role of its own; pass --scope <territory> \
                     to expire that role.",
                );
            }
            let rows = match load() {
                Ok(rows) => rows,
                Err(e) => return refuse(1, &format!("lead: role expire failed: {e}")),
            };
            let live: Vec<&RegistryEntry> = rows
                .iter()
                .filter(|r| !crate::lead_state::is_terminal(r))
                .collect();
            // One member of a set role is not a role: vacating nothing would
            // still print a false expiry receipt.
            if !live
                .iter()
                .any(|r| r.role_scope.as_deref() == Some(scope.as_str()))
            {
                if let Some(row) = live.iter().find(|r| answers(r.role_scope.as_deref())) {
                    let held = row.role_scope.as_deref().unwrap_or("");
                    return refuse(
                        2,
                        &format!(
                            "lead: refusing to expire {}: it is one member of {}'s role over {}. \
                             Expire the whole role with --scope {held}, or narrow it with fno \
                             agents org promote {} --scope <the remaining epics>.",
                            py_repr(&scope),
                            row.name,
                            py_repr(held),
                            row.name
                        ),
                    );
                }
            }
            String::new()
        }
        Some(me) => {
            let own = me.role_scope.clone();
            if scope.trim().is_empty() {
                match &own {
                    Some(own) => scope = own.clone(),
                    None => {
                        return refuse(
                            2,
                            "lead: this session holds no role, so there is nothing to expire.",
                        )
                    }
                }
            } else if own.as_deref() != Some(scope.as_str()) {
                let rows = match load() {
                    Ok(rows) => rows,
                    Err(e) => return refuse(1, &format!("lead: role expire failed: {e}")),
                };
                let mine: Vec<&RegistryEntry> = rows
                    .iter()
                    .filter(|r| r.role_scope.as_deref() == Some(scope.as_str()))
                    .collect();
                let teams: Vec<crate::territory::Team> = rows
                    .iter()
                    .filter(|r| !crate::lead_state::is_terminal(r))
                    .filter_map(|r| {
                        Some(crate::territory::Team {
                            scope: crate::territory::canonical_scope(r.role_scope.as_deref()?),
                            level: r.role_level?.min(255) as u8,
                            holder: r.name.clone(),
                            holder_session: r.harness_session_id.clone(),
                        })
                    })
                    .collect();
                let level = mine
                    .iter()
                    .find_map(|r| r.role_level)
                    .filter(|l| *l != 0)
                    .or_else(|| crate::loop_lead::derived_scope_level(&scope, &projects));
                let presider = crate::team_reap::presiding_holder(level, &scope, &teams, &here);
                let has_live = mine.iter().any(|r| !crate::lead_state::is_terminal(r));
                let member = rows.iter().any(|r| {
                    answers(r.role_scope.as_deref())
                        && r.role_scope.as_deref() != Some(scope.as_str())
                });
                if has_live || member || presider.as_deref() != Some(me.name.as_str()) {
                    return refuse(
                        2,
                        &format!(
                            "lead: refusing to expire {}: this session's role is {}, and an \
                             agent expires only its own role. Call `fno agents lead done` with \
                             no --scope, or ask the lead over that territory.",
                            py_repr(&scope),
                            own.as_deref().map_or("None".to_string(), py_repr)
                        ),
                    );
                }
            }
            if own.as_deref() == Some(scope.as_str()) {
                me.name.clone()
            } else {
                String::new()
            }
        }
    };

    // Snapshot the manifest's own session BEFORE vacating: removal compares
    // it under the manifest lock, so a successor promoted mid-vacate survives.
    let owner_root = crate::paths::space_dir(&match caller.as_ref().map(|c| c.cwd.as_str()) {
        Some(c) if !c.is_empty() => PathBuf::from(c),
        _ => here.clone(),
    });
    let expired_session: Option<String> = crate::lead_state::manifest_path(&owner_root, &scope)
        .ok()
        .map(|p| field(&parse_manifest(&p), "harness_session_id").to_string())
        .filter(|s| !s.is_empty());

    // Vacate the row BEFORE the file, under the registry lock: a scope that
    // moved to a successor mid-call is refused here.
    let attended_named = holder_name.is_empty();
    let expect = caller.as_ref().map(|c| (c.name.clone(), row_session(c)));
    let mut vacated_rows: Vec<RegistryEntry> = Vec::new();
    let mut refused: Option<&str> = None;
    let write = crate::state::update_registry(&registry_path, |reg| {
        if attended_named && caller.is_some() {
            if reg.entries.iter().any(|r| {
                r.role_scope.as_deref() == Some(scope.as_str())
                    && !crate::lead_state::is_terminal(r)
            }) {
                refused = Some("a live holder promoted mid-expiry");
            }
            return;
        }
        for row in reg.entries.iter_mut() {
            if row.role_scope.as_deref() != Some(scope.as_str()) {
                continue;
            }
            if attended_named {
                if crate::lead_state::is_terminal(row) {
                    continue;
                }
            } else if expect.as_ref() != Some(&(row.name.clone(), row_session(row))) {
                continue;
            }
            vacated_rows.push(row.clone());
            vacate(row);
            if !attended_named {
                break;
            }
        }
    });
    if let Some(reason) = refused {
        return refuse(1, &format!("lead: role expire failed: {reason}"));
    }
    if let Err(e) = write {
        return refuse(1, &format!("lead: role expire failed: {e}"));
    }
    let vacated = !vacated_rows.is_empty();
    if !vacated && !attended_named {
        return refuse(
            1,
            &format!(
                "lead: refusing to expire {}: this row no longer holds it (the role moved or was \
                 already vacated), so the manifest on disk may belong to a successor. Re-read \
                 with `fno agents team` before expiring anything.",
                py_repr(&scope)
            ),
        );
    }
    let cause = if attended_named {
        "expired"
    } else {
        "stepped_down"
    };
    for row in &vacated_rows {
        if let Some(session) = row.harness_session_id.as_deref() {
            crate::retired_lead::write(&home, session, &scope, "org-done");
        }
        // The row write is the authority: this fires even if the manifest
        // removal below then fails. One event per row, so a split role
        // records every holder.
        emit_vacated(&home, &scope, Some(row), None, cause);
    }
    if !remove_manifest(&owner_root, &scope, expired_session.as_deref()) {
        return refuse(
            1,
            &format!(
                "lead: row vacated, but the manifest for {} could not be removed: the file no \
                 longer names the session this expiry snapshotted, so a successor promoted \
                 mid-expiry likely owns it now. Re-read with `fno agents team` before touching \
                 anything.",
                py_repr(&scope)
            ),
        );
    }
    if !vacated {
        // No live holder: the manifest clear is the whole vacate.
        emit_vacated(
            &home,
            &scope,
            None,
            expired_session.as_deref(),
            "orphan_manifest",
        );
    }
    println!("lead: role expired: {scope}");
    println!(
        "{}",
        if vacated {
            "row role: vacated; manifest: cleared"
        } else {
            "row role: no live holder found; manifest: cleared"
        }
    );
    0
}

// ---------------------------------------------------------------------------
// escalate: tell the presiding role, or the operator
// ---------------------------------------------------------------------------

/// The escalation channel's question key: the first 12 hex of sha256 over
/// the sorted unique ids joined by newlines (`stale_escalate.dedupe_key`).
fn dedupe_key(ids: &[String]) -> String {
    let mut ids: Vec<&str> = ids.iter().map(String::as_str).collect();
    ids.sort_unstable();
    ids.dedup();
    let digest = Sha256::digest(ids.join("\n").as_bytes());
    digest.iter().take(6).map(|b| format!("{b:02x}")).collect()
}

/// The escalating session: the live target manifest's `fno_id` (legacy
/// `session_id`), else `$CLAUDECODE_SESSION_ID`.
fn escalating_session(here: &Path) -> Option<String> {
    let manifest = crate::state_path::resolve("target-state", here)
        .and_then(|p| std::fs::read_to_string(p).ok())
        .unwrap_or_default();
    ["fno_id", "session_id"]
        .into_iter()
        .find_map(|k| crate::claude_adopt::manifest_field(&manifest, k))
        .filter(|s| s != "null")
        .or_else(|| {
            std::env::var("CLAUDECODE_SESSION_ID")
                .ok()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
        })
}

/// `(summary, scope)` of the term verdict; a failed read still records,
/// as `unreadable ...`, so escalation never goes silent.
fn verdict_read(here: &Path, scope: &str) -> (String, Option<String>) {
    let exe = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("fno-agents"));
    let mut cmd = std::process::Command::new(exe);
    cmd.args(["lead-history", "--verdict", "--cwd"]).arg(here);
    if !scope.is_empty() {
        cmd.args(["--scope", scope]);
    }
    cmd.arg("--json");
    let out = match cmd.output() {
        Ok(out) => out,
        Err(e) => return (format!("unreadable {e}"), None),
    };
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        let text = String::from_utf8_lossy(&out.stdout);
        let detail = if err.is_empty() { text } else { err };
        let detail: String = detail.trim().chars().take(200).collect();
        return (
            format!(
                "unreadable native exit {}: {detail}",
                out.status.code().unwrap_or(-1)
            ),
            None,
        );
    }
    match serde_json::from_slice::<Value>(&out.stdout) {
        Ok(payload) => (
            payload
                .get("summary")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
            payload
                .get("scope")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .map(str::to_string),
        ),
        Err(e) => (format!("unreadable {e}"), None),
    }
}

/// The holder of the role above the escalating session's own, or None.
fn presiding_lead(home: &crate::paths::AgentsHome, session: &str, here: &Path) -> Option<String> {
    let registry = crate::state::load_registry(&home.registry_json()).ok()?;
    let own = registry.entries.iter().find(|r| {
        r.harness_session_id
            .as_deref()
            .is_some_and(|sid| crate::claims::same_session_id(sid, session))
    })?;
    let scope = own.role_scope.as_deref().filter(|s| !s.is_empty())?;
    let level = own.role_level?;
    let teams = crate::territory::live_teams(&home.registry_json()).ok()?;
    crate::team_reap::presiding_holder(Some(level), scope, &teams, here)
}

fn escalation(
    ids: &[String],
    key: &str,
    reason: &str,
    readings: (
        Option<bool>,
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
    ),
) -> crate::lead_escalation::EscalationAnswer {
    let (live, unknown_reason, verdict, scope, session_id) = readings;
    crate::lead_escalation::render(&crate::lead_escalation::EscalationRequest {
        stalled: ids.to_vec(),
        key: key.to_string(),
        reason: reason.to_string(),
        live,
        unknown_reason,
        verdict,
        scope,
        session_id,
        route: crate::lead_escalation::route_for(&cwd()),
        route_cwd: None,
    })
}

/// The sender stamp on an escalation's mail to the presiding holder.
const ESCALATE_SENDER: &str = "fno/lead-escalate";

/// Mail the presiding holder; true once the durable envelope lands. The
/// daemon's mail-wake arm delivers it, and the operator question is already
/// recorded, so a holder that never reads it still leaves a record.
fn mail_presiding(holder: &str, ids: &[String], key: &str, reason: &str, verdict: &str) -> bool {
    let answer = escalation(
        ids,
        key,
        reason,
        (None, None, Some(verdict.to_string()), None, None),
    );
    let Some(message) = answer.mail.filter(|_| answer.ok) else {
        return false;
    };
    crate::lead_mail::send_to_name(ESCALATE_SENDER, holder, &message).is_ok()
}

/// `fno agents org escalate [SCOPE] [--stalled IDS] [--reason R]`: record one
/// operator question for the stalled set, and mail the presiding role when
/// one holds the territory above.
pub fn run_escalate(args: &[String]) -> i32 {
    let mut scope_arg = String::new();
    let mut stalled = String::new();
    let mut reason = "NoProgress".to_string();
    let mut i = 0;
    while i < args.len() {
        match (args[i].as_str(), args.get(i + 1)) {
            ("--stalled", Some(v)) => stalled = v.clone(),
            ("--reason" | "-R", Some(v)) => reason = v.clone(),
            (word, _) if !word.starts_with('-') && scope_arg.is_empty() => {
                scope_arg = word.to_string();
                i += 1;
                continue;
            }
            (other, _) => {
                return refuse(
                    2,
                    &format!(
                        "fno agents org escalate: unknown argument {other}\nUsage: fno agents org \
                         escalate [SCOPE] [--stalled IDS] [--reason R]"
                    ),
                )
            }
        }
        i += 2;
    }
    let mut ids: Vec<String> = stalled
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect();
    let here = cwd();
    let home = crate::paths::AgentsHome::from_env();
    let session = escalating_session(&here);
    // The caller's own liveness, read not asserted: unknown reads as dead in
    // the renderer, with the reason named.
    // With no escalating session the term reader asks who this process is.
    let (term_session, term_harness) = match &session {
        Some(sid) => (Some(sid.clone()), None),
        None => {
            let get = |k: &str| std::env::var(k).ok();
            let ident = crate::spawn_context::resolve_self_identity(&get, None, None, &home);
            (ident.session_id.filter(|s| !s.is_empty()), ident.harness)
        }
    };
    let (live, unknown_reason) = match term_session.as_deref() {
        Some(sid) => {
            let state = crate::lead_state::lead_state_in_repo(
                &crate::paths::space_dir(&here),
                &here,
                None,
                Some(sid),
                term_harness.as_deref(),
                &home.registry_json(),
            );
            (state.live, state.unknown_reason)
        }
        None => (
            None,
            Some("term_state unreadable: no session to read the term for".to_string()),
        ),
    };
    let (summary, verdict_scope) = verdict_read(&here, &scope_arg);
    // A degraded or unknown verdict has no stalled row, so the verdict is the
    // reading. The renderer refuses a converging one.
    if ids.is_empty() {
        let scope = verdict_scope.as_deref().unwrap_or(&scope_arg);
        ids = vec![format!("reading:verdict:{}", scope.replace(',', "+"))];
    }
    ids.sort();
    ids.dedup();
    let key = dedupe_key(&ids);
    let presiding = session
        .as_deref()
        .and_then(|sid| presiding_lead(&home, sid, &here));
    // Render BEFORE the fold: a refusal must exit while the channel is still
    // untouched, because the channel's empty branch closes open asks.
    let answer = escalation(
        &ids,
        &key,
        &reason,
        (
            live,
            unknown_reason,
            Some(summary.clone()),
            verdict_scope,
            session.clone(),
        ),
    );
    let (Some(question), true) = (answer.question.clone(), answer.ok) else {
        let message = answer
            .message
            .unwrap_or_else(|| "lead escalation refused".into());
        return refuse(1, &format!("lead: escalation failed: {message}"));
    };
    let marker = answer
        .marker
        .clone()
        .unwrap_or_else(|| "lead-escalation".to_string());
    let store = crate::provider_cap::questions_path(&home);
    let (outcome, qid) = match crate::fleet_task::reconcile(
        &store,
        &marker,
        &key,
        &here.display().to_string(),
        &question,
        "",
        false,
    ) {
        Ok(pair) => pair,
        Err(e) => return refuse(1, &format!("lead: escalation failed: {e}")),
    };
    let outcome = if outcome == "asked" {
        "recorded".to_string()
    } else {
        outcome
    };
    if let Some(nudge) = &answer.nudge {
        eprintln!("{nudge}");
    }
    let mailed = presiding
        .as_deref()
        .filter(|_| answer.mail_relay)
        .filter(|holder| mail_presiding(holder, &ids, &key, &reason, &summary));
    let note = mailed.map_or(String::new(), |h| format!(", mailed presiding lead {h}"));
    eprintln!("lead: {outcome}{note} {qid}");
    match mailed {
        Some(holder) => println!("lead:{holder}"),
        None => println!("operator:{qid}"),
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    const T0: i64 = 1_788_000_000;

    fn manifest(dir: &Path, body: &str) -> PathBuf {
        let path = dir.join("leads").join("epic-x.md");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, body).unwrap();
        path
    }

    fn lines(path: &Path) -> Vec<String> {
        std::fs::read_to_string(path)
            .unwrap()
            .lines()
            .map(str::to_string)
            .collect()
    }

    #[test]
    fn admit_refuses_inside_the_debounce_and_at_the_ceiling_then_bills() {
        let dir = tempfile::tempdir().unwrap();
        let path = manifest(dir.path(), "---\nfno_id: k-1\nrespawn_count: 0\n---\n");
        assert_eq!(admit_wake(&path, T0, 2, 600, 32).unwrap(), ("", 1));
        // The stamp lands after the last lead field, never at the top.
        assert_eq!(lines(&path)[3], format!("wake_times: {}", format_stamp(T0)));
        assert_eq!(
            admit_wake(&path, T0 + 60, 2, 600, 32).unwrap(),
            ("debounce", 1)
        );
        assert_eq!(admit_wake(&path, T0 + 700, 2, 600, 32).unwrap(), ("", 2));
        assert_eq!(
            admit_wake(&path, T0 + 1400, 2, 600, 32).unwrap(),
            ("ceiling", 2)
        );
        // A ceiling of 0 is unbounded; the keep then bounds the line alone.
        assert_eq!(admit_wake(&path, T0 + 2100, 0, 600, 1).unwrap(), ("", 1));
    }

    #[test]
    fn a_stamp_a_full_day_old_leaves_the_window() {
        let dir = tempfile::tempdir().unwrap();
        let body = format!(
            "---\nwake_times: {},{}\n---\n",
            format_stamp(T0),
            format_stamp(T0 + 60)
        );
        let path = manifest(dir.path(), &body);
        assert_eq!(read_wakes(&path, T0 + WAKE_WINDOW_S), vec![T0 + 60]);
        assert_eq!(bill_wake(&path, T0 + WAKE_WINDOW_S, 32).unwrap(), 2);
        let absent = dir.path().join("absent.md");
        assert_eq!(admit_wake(&absent, T0, 2, 0, 32).unwrap(), ("debounce", 0));
    }

    #[test]
    fn the_respawn_budget_defaults_to_four_like_the_walk() {
        let read = |body: &str| {
            let dir = tempfile::tempdir().unwrap();
            respawn_budget(&parse_manifest(&manifest(dir.path(), body)))
        };
        assert_eq!(read("respawn_count: 4\n"), (true, 4));
        assert_eq!(read("respawn_count: 3\n"), (false, 4));
        assert_eq!(read("respawn_count: 9\nrespawn_ceiling: 0\n"), (false, 0));
    }

    #[test]
    fn parse_manifest_decodes_a_quoted_value() {
        let dir = tempfile::tempdir().unwrap();
        let path = manifest(
            dir.path(),
            "---\nmodel: \"glm-5.3[1m]\"\nscope: epic-x\n---\n",
        );
        let fields = parse_manifest(&path);
        assert_eq!(field(&fields, "model"), "glm-5.3[1m]");
        assert_eq!(field(&fields, "scope"), "epic-x");
    }

    #[test]
    fn removal_keeps_a_manifest_another_session_holds() {
        let dir = tempfile::tempdir().unwrap();
        let path = manifest(dir.path(), "---\nharness_session_id: successor\n---\n");
        assert!(!remove_manifest(dir.path(), "epic-x", Some("expired")));
        assert!(path.exists());
        assert!(remove_manifest(dir.path(), "epic-x", Some("successor")));
        assert!(!path.exists());
        assert!(!remove_manifest(dir.path(), "../escape", None));
    }

    #[test]
    fn a_driven_but_unshipped_row_stays_undelivered() {
        // Assignment is not delivery: an in-progress row with a PR counts.
        let rows = vec![
            json!({"id": "x-root", "type": "epic", "status": "in_progress"}),
            json!({"id": "x-a", "parent": "x-root", "status": "in_progress", "pr": 1497}),
            json!({"id": "x-b", "parent": "x-root", "status": "done"}),
        ];
        let ids = vec!["x-a".to_string(), "x-b".to_string()];
        assert_eq!(undelivered_of(&ids, &rows), 1);
    }

    #[test]
    fn a_drain_under_an_external_tracker_refuses() {
        let dir = tempfile::tempdir().unwrap();
        let _guard = crate::claims::test_env_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        std::env::set_var("FNO_TRACKER_BACKEND", "github");
        let answer = drain("epic-x", dir.path());
        std::env::remove_var("FNO_TRACKER_BACKEND");
        let err = answer.expect_err("an external tracker has no local scope");
        assert!(err.contains("tracker metadata unavailable"), "{err}");
    }

    #[test]
    fn a_window_reads_its_unit() {
        assert_eq!(parse_window("90m"), Ok(5400));
        assert_eq!(parse_window("7d"), Ok(604_800));
        assert_eq!(parse_window("30"), Ok(30));
        assert_eq!(
            parse_window("4x"),
            Err("unparseable window '4x'; use e.g. 24h, 90m, 7d".to_string())
        );
    }

    #[test]
    fn the_dedupe_key_is_the_python_channel_key() {
        // sha256("x-1\nx-2")[:12], the key stale_escalate.dedupe_key minted.
        let ids = vec!["x-2".to_string(), "x-1".to_string(), "x-1".to_string()];
        assert_eq!(dedupe_key(&ids), "c16cde8900b4");
    }
}
