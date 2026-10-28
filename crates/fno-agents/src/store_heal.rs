//! The registry-miss healer: resolve a session token against each harness's
//! own store, and adopt the one store-only session it names as an orphaned
//! row. The registry is a cache of reality, not a gate in front of it.
//!
//! Four rules keep it from guessing:
//! - only a session-shaped token (8 alphanumerics, a UUID, `ses_...`) is
//!   probed, so a plain unknown name still fails as before;
//! - more than one matching session refuses with the candidate list;
//! - a store row proves the session exists, never that it runs, so the
//!   adopted row is `orphaned`;
//! - a store that cannot be read refuses a short token: a partial answer
//!   cannot prove it unique.
//!
//! `fno agents heal-token` prints the healed row; the lifecycle commands
//! (logs, attach, resume, trace) call [`heal_all_sources`] in process.

use crate::client_commands::py_repr_str;
use crate::identity::session_handle_tier;
use crate::paths::AgentsHome;
use crate::spawn_context::session_identity_key;
use serde_json::Value;
use std::io::BufRead;
use std::path::{Path, PathBuf};

/// `fno agents rm` stamps removed sessions here, beside the registry; the
/// healer refuses to re-adopt them inside the grace window.
const RM_TOMBSTONE_FILENAME: &str = "rm_tombstones.json";
const RM_TOMBSTONE_GRACE_SECS: i64 = 86_400;
/// Transcript lines read before a session's `cwd` must have appeared, so a
/// probe never streams a large log.
const CWD_SCAN_LINES: usize = 50;

const HEAL_MISS_EXIT: i32 = 13;
const HEAL_AMBIGUOUS_EXIT: i32 = 3;
const HEAL_UNAVAILABLE_EXIT: i32 = 12;

/// Why a token cannot resolve to one session. A clean miss is `Ok(None)`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum HealError {
    /// More than one candidate, or a uniqueness the stores cannot prove.
    Ambiguous(String),
    /// The identity evidence itself is unreadable.
    Unavailable(String),
}

impl HealError {
    pub(crate) fn message(&self) -> &str {
        match self {
            HealError::Ambiguous(m) | HealError::Unavailable(m) => m,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct StoreHit {
    harness: &'static str,
    session_id: String,
    cwd: String,
    /// The per-session transcript the probe found; `None` for opencode,
    /// whose store is one shared database.
    transcript: Option<PathBuf>,
}

fn is_opencode_id(token: &str) -> bool {
    token
        .strip_prefix("ses_")
        .is_some_and(|rest| !rest.is_empty() && rest.bytes().all(|b| b.is_ascii_alphanumeric()))
}

fn is_short_token(token: &str) -> bool {
    token.len() == 8 && token.bytes().all(|b| b.is_ascii_alphanumeric())
}

fn env_dir(name: &str) -> Option<PathBuf> {
    std::env::var(name)
        .ok()
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
}

fn home_dir() -> PathBuf {
    PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| ".".to_string()))
}

fn codex_sessions_dir() -> PathBuf {
    if let Some(dir) = env_dir("FNO_CODEX_SESSIONS_DIR") {
        return dir;
    }
    if let Some(codex_home) = env_dir("CODEX_HOME") {
        let raw = codex_home.to_string_lossy().into_owned();
        let expanded = match raw.strip_prefix("~/") {
            Some(rest) => home_dir().join(rest),
            None => codex_home,
        };
        return expanded.join("sessions");
    }
    home_dir().join(".codex").join("sessions")
}

fn opencode_storage_dir() -> PathBuf {
    env_dir("FNO_OPENCODE_STORAGE_DIR").unwrap_or_else(|| {
        home_dir()
            .join(".local")
            .join("share")
            .join("opencode")
            .join("storage")
    })
}

/// Every file under `root` the filter accepts. An absent root is empty; any
/// directory that cannot be listed is an error, never an empty answer, so a
/// uniqueness check never reads a partial corpus as a complete one.
fn scan_files(
    root: &Path,
    max_depth: Option<usize>,
    include: &dyn Fn(&str) -> bool,
) -> std::io::Result<Vec<PathBuf>> {
    match std::fs::metadata(root) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e),
        Ok(meta) if !meta.is_dir() => {
            return Err(std::io::Error::other(format!(
                "not a directory: {}",
                root.display()
            )))
        }
        Ok(_) => {}
    }
    let mut found = Vec::new();
    let mut stack = vec![(root.to_path_buf(), 0usize)];
    while let Some((dir, depth)) = stack.pop() {
        for entry in std::fs::read_dir(&dir)? {
            let entry = entry?;
            let kind = entry.file_type()?;
            if kind.is_dir() {
                if max_depth.is_none_or(|max| depth < max) {
                    stack.push((entry.path(), depth + 1));
                }
                continue;
            }
            if kind.is_file() && include(&entry.file_name().to_string_lossy()) {
                found.push(entry.path());
            }
        }
    }
    found.sort();
    Ok(found)
}

/// The session's own recorded cwd, or "" when it never recorded one.
fn transcript_cwd(path: &Path) -> String {
    let Ok(file) = std::fs::File::open(path) else {
        return String::new();
    };
    for line in std::io::BufReader::new(file)
        .split(b'\n')
        .take(CWD_SCAN_LINES)
    {
        let Ok(bytes) = line else {
            return String::new();
        };
        let Ok(record) = serde_json::from_slice::<Value>(&bytes) else {
            continue;
        };
        if let Some(cwd) = record.get("cwd").and_then(Value::as_str) {
            if !cwd.is_empty() {
                return cwd.to_string();
            }
        }
    }
    String::new()
}

/// Claude's transcript store, the one `claude -r` reads. The filename IS the
/// session UUID, so a short token matches by prefix.
fn probe_claude(token: &str) -> std::io::Result<Vec<StoreHit>> {
    if is_opencode_id(token) {
        return Ok(Vec::new());
    }
    let needle = token.to_ascii_lowercase();
    let root = crate::mail_cli::send::resolve::projects_dir();
    let found = scan_files(&root, Some(1), &|name| {
        name.ends_with(".jsonl") && name.to_ascii_lowercase().contains(&needle)
    })?;
    let mut hits: Vec<StoreHit> = Vec::new();
    for path in found {
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        if name.contains(".sync-conflict-") {
            continue;
        }
        let sid = name.trim_end_matches(".jsonl").to_string();
        if session_handle_tier(token, &sid).is_some() && !hits.iter().any(|h| h.session_id == sid) {
            hits.push(StoreHit {
                harness: "claude",
                cwd: transcript_cwd(&path),
                session_id: sid,
                transcript: Some(path),
            });
        }
    }
    Ok(hits)
}

/// A rollout's `session_meta` line 1: `(id, cwd)`, or `None` when the file
/// carries no readable meta.
fn codex_meta(path: &Path) -> Option<(String, String)> {
    let file = std::fs::File::open(path).ok()?;
    let mut first = String::new();
    std::io::BufReader::new(file).read_line(&mut first).ok()?;
    let record: Value = serde_json::from_str(&first).ok()?;
    if record.get("type").and_then(Value::as_str) != Some("session_meta") {
        return None;
    }
    let payload = record.get("payload")?.as_object()?;
    let sid = payload.get("id")?.as_str().filter(|s| !s.is_empty())?;
    let cwd = payload.get("cwd").and_then(Value::as_str).unwrap_or("");
    Some((sid.to_string(), cwd.to_string()))
}

fn probe_codex(token: &str) -> std::io::Result<Vec<StoreHit>> {
    if is_opencode_id(token) {
        return Ok(Vec::new());
    }
    let found = scan_files(&codex_sessions_dir(), None, &|name| {
        name.starts_with("rollout-") && name.ends_with(".jsonl")
    })?;
    let mut hits: Vec<StoreHit> = Vec::new();
    for path in found {
        let (sid, cwd) = codex_meta(&path).ok_or_else(|| {
            std::io::Error::other(format!(
                "unreadable codex session candidate: {}",
                path.display()
            ))
        })?;
        if session_handle_tier(token, &sid).is_none() || hits.iter().any(|h| h.session_id == sid) {
            continue;
        }
        hits.push(StoreHit {
            harness: "codex",
            session_id: sid,
            cwd,
            transcript: Some(path),
        });
    }
    Ok(hits)
}

/// opencode's store: the SQLite database when it exists, else the legacy
/// `storage/session` JSON tree.
fn probe_opencode(token: &str) -> std::io::Result<Vec<StoreHit>> {
    if !(is_opencode_id(token) || is_short_token(token)) {
        return Ok(Vec::new());
    }
    let storage = opencode_storage_dir();
    let db = storage
        .parent()
        .map(|p| p.join("opencode.db"))
        .unwrap_or_else(|| PathBuf::from("opencode.db"));
    if !std::fs::exists(&db)? {
        let found = scan_files(&storage.join("session"), Some(1), &|name| {
            name.ends_with(".json") && name.contains(token)
        })?;
        let mut hits = Vec::new();
        for path in found {
            let info: Option<(String, String)> = std::fs::read_to_string(&path)
                .ok()
                .and_then(|text| serde_json::from_str::<Value>(&text).ok())
                .and_then(|data| {
                    let sid = data
                        .get("id")?
                        .as_str()
                        .filter(|s| !s.is_empty())?
                        .to_string();
                    let cwd = data
                        .get("directory")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string();
                    Some((sid, cwd))
                });
            let (sid, cwd) = info.ok_or_else(|| {
                std::io::Error::other(format!(
                    "unreadable opencode session candidate: {}",
                    path.display()
                ))
            })?;
            if session_handle_tier(token, &sid).is_some() {
                hits.push(StoreHit {
                    harness: "opencode",
                    session_id: sid,
                    cwd,
                    transcript: None,
                });
            }
        }
        return Ok(hits);
    }
    // Read-only: a live opencode holds this database open in WAL mode, and
    // observing it must not perturb it.
    let connection = rusqlite::Connection::open_with_flags(
        &db,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(std::io::Error::other)?;
    let mut statement = connection
        .prepare(
            "SELECT id, directory FROM session \
             WHERE id = ?1 OR substr(id, -8) = ?1 OR substr(id, 1, 8) = ?1",
        )
        .map_err(std::io::Error::other)?;
    let rows = statement
        .query_map([token], |row| {
            Ok((
                row.get::<_, Option<String>>(0).ok().flatten(),
                row.get::<_, Option<String>>(1).ok().flatten(),
            ))
        })
        .map_err(std::io::Error::other)?;
    let mut hits = Vec::new();
    for row in rows {
        let (sid, directory) = row.map_err(std::io::Error::other)?;
        let Some(sid) = sid.filter(|s| !s.is_empty()) else {
            continue;
        };
        if session_handle_tier(token, &sid).is_some() {
            hits.push(StoreHit {
                harness: "opencode",
                session_id: sid,
                cwd: directory.unwrap_or_default(),
                transcript: None,
            });
        }
    }
    Ok(hits)
}

fn sorted_hits(hits: &[StoreHit]) -> Vec<&StoreHit> {
    let mut sorted: Vec<&StoreHit> = hits.iter().collect();
    sorted.sort_by(|a, b| (a.harness, &a.session_id).cmp(&(b.harness, &b.session_id)));
    sorted
}

/// Every harness store's answer for `token`, complete or refused: an
/// unreadable store cannot prove a short token unique.
fn complete_store_hits(token: &str) -> Result<Vec<StoreHit>, HealError> {
    let token = token.trim();
    if !crate::resolve_tier::is_session_shaped(token) {
        return Ok(Vec::new());
    }
    let mut hits: Vec<StoreHit> = Vec::new();
    let mut failed: Vec<&str> = Vec::new();
    let probes: [(&str, fn(&str) -> std::io::Result<Vec<StoreHit>>); 3] = [
        ("claude", probe_claude),
        ("codex", probe_codex),
        ("opencode", probe_opencode),
    ];
    for (name, probe) in probes {
        match probe(token) {
            Ok(found) => hits.extend(found),
            Err(_) => failed.push(name),
        }
    }
    let mut distinct: Vec<StoreHit> = Vec::new();
    for hit in hits {
        if session_handle_tier(token, &hit.session_id).is_some()
            && !distinct
                .iter()
                .any(|d| d.harness == hit.harness && d.session_id == hit.session_id)
        {
            distinct.push(hit);
        }
    }
    if !failed.is_empty() {
        let candidates = if distinct.is_empty() {
            String::new()
        } else {
            format!(
                " Visible candidates: {}.",
                sorted_hits(&distinct)
                    .iter()
                    .map(|h| format!("{} ({})", h.session_id, h.harness))
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        };
        return Err(HealError::Ambiguous(format!(
            "token {} could not be checked for cross-store uniqueness because these harness \
stores were unreadable: {}. {candidates} Use the full session id.",
            py_repr_str(token),
            failed.join(", ")
        )));
    }
    Ok(distinct)
}

/// The adopted session's newest timestamped transcript entry as a UTC
/// stamp; the file mtime when no entry carries a timestamp. `None` when no
/// per-session transcript exists (opencode's shared store has no honest
/// per-session stamp).
fn transcript_last_write(hit: &StoreHit) -> Option<String> {
    let path = hit.transcript.as_ref()?;
    let file = std::fs::File::open(path).ok()?;
    let mut newest: Option<i64> = None;
    for line in std::io::BufReader::new(file).split(b'\n') {
        let Ok(bytes) = line else {
            break;
        };
        let Ok(record) = serde_json::from_slice::<Value>(&bytes) else {
            continue;
        };
        let stamp = record
            .get("timestamp")
            .and_then(Value::as_str)
            .and_then(|t| chrono::DateTime::parse_from_rfc3339(t).ok())
            .map(|t| t.timestamp());
        if let Some(epoch) = stamp {
            newest = Some(newest.map_or(epoch, |n| n.max(epoch)));
        }
    }
    let epoch = match newest {
        Some(epoch) => epoch,
        None => std::fs::metadata(path)
            .ok()?
            .modified()
            .ok()?
            .duration_since(std::time::UNIX_EPOCH)
            .ok()?
            .as_secs() as i64,
    };
    chrono::DateTime::<chrono::Utc>::from_timestamp(epoch, 0)
        .map(|t| t.format("%Y-%m-%dT%H:%M:%SZ").to_string())
}

/// The session's in-window `fno agents rm` stamp, if any.
fn recent_rm_tombstone(harness: &str, session_id: &str, registry_path: &Path) -> bool {
    let Some(parent) = registry_path.parent() else {
        return false;
    };
    let Ok(text) = std::fs::read_to_string(parent.join(RM_TOMBSTONE_FILENAME)) else {
        return false;
    };
    let Ok(Value::Array(rows)) = serde_json::from_str::<Value>(&text) else {
        return false;
    };
    let now = chrono::Utc::now().timestamp();
    rows.iter().any(|row| {
        row.get("harness").and_then(Value::as_str) == Some(harness)
            && row.get("session_id").and_then(Value::as_str) == Some(session_id)
            && row
                .get("removed_at")
                .and_then(Value::as_f64)
                .is_some_and(|t| (now as f64) - t <= RM_TOMBSTONE_GRACE_SECS as f64)
    })
}

/// The session that adopts the row vouches for it (it never spawned it).
fn adopting_session(adopted: &str) -> Option<String> {
    let get = |name: &str| std::env::var(name).ok();
    crate::spawn_context::resolve_self_identity(&get, None, None, &AgentsHome::from_env())
        .session_id
        .filter(|sid| !sid.is_empty() && sid != adopted)
}

fn collision(kind: &str, token: &str, action: &str, session_id: &str, tail: &str) -> HealError {
    HealError::Ambiguous(format!(
        "{kind} {} collision while {action} session {}; {tail}",
        py_repr_str(token),
        py_repr_str(session_id)
    ))
}

/// Register one store hit as an adopted orphan: refresh the session's own
/// row in place, else mint one under the canonical handle. A failed write
/// still returns a synthesized row, because reaching the session beats the
/// roster row.
///
/// No `FNO_AGENT_SELF` refusal here: that guard stops a worker minting a
/// second row for itself, and an adopted session is never the caller.
fn adopt(hit: &StoreHit, registry_path: &Path, token: &str) -> Result<Value, HealError> {
    use crate::state::RegistryEntry;
    if recent_rm_tombstone(hit.harness, &hit.session_id, registry_path) {
        return Err(HealError::Ambiguous(format!(
            "session {} ({}) was removed by `fno agents rm` inside the grace window ({}h); it \
is not re-adopted from the harness store. Re-spawn the work, or wait out the window to adopt it \
deliberately.",
            hit.session_id,
            hit.harness,
            RM_TOMBSTONE_GRACE_SECS / 3600
        )));
    }
    // claude's transport key is the 8-hex jobId, not the full UUID.
    let short_id: String = if hit.harness == "claude" {
        hit.session_id.chars().take(8).collect()
    } else {
        String::new()
    };
    let last_message_at = transcript_last_write(hit);
    let adopted_by = adopting_session(&hit.session_id);
    let harness = hit.harness.to_string();
    let sid = hit.session_id.clone();
    let written = crate::state::update_registry(registry_path, |registry| {
        let entries = &mut registry.entries;
        if let Some(index) = entries.iter().position(|e| {
            e.harness.as_deref() == Some(harness.as_str())
                && e.harness_session_id.as_deref() == Some(sid.as_str())
        }) {
            if !short_id.is_empty()
                && entries[index].short_id != short_id
                && entries.iter().enumerate().any(|(i, e)| {
                    i != index
                        && crate::register::address_taken(std::slice::from_ref(e), &short_id, false)
                })
            {
                return Err(collision(
                    "transport short id",
                    &short_id,
                    "refreshing",
                    &sid,
                    "use the full session id",
                ));
            }
            let row = &mut entries[index];
            if row.status != crate::AgentStatus::Live {
                row.status = crate::AgentStatus::Orphaned;
            }
            row.cwd = hit.cwd.clone();
            if !short_id.is_empty() {
                row.short_id = short_id.clone();
            }
            if row.origin.is_none() {
                row.origin = Some("adopted".to_string());
            }
            if last_message_at.is_some() {
                row.last_message_at = last_message_at.clone();
            }
            crate::register::apply_session_manifest_role(row, &sid, &hit.cwd);
            return Ok(row.name.clone());
        }
        let generated = crate::identity::canonical_handle(&sid);
        if crate::register::address_taken(entries, &generated, true) {
            return Err(collision(
                "canonical handle",
                &generated,
                "registering",
                &sid,
                "use the full session id directly",
            ));
        }
        if !short_id.is_empty() && crate::register::address_taken(entries, &short_id, false) {
            return Err(collision(
                "transport short id",
                &short_id,
                "registering",
                &sid,
                "use the full session id",
            ));
        }
        let mut name = generated.clone();
        let mut suffix = 2;
        while crate::register::address_taken(entries, &name, false) {
            name = format!("{generated}-{suffix}");
            suffix += 1;
        }
        let mut fresh = RegistryEntry::new(
            Some(sid.clone()),
            crate::state::Lineage::unproven(
                "adopted from the harness store: adoption carries no parent edge",
            ),
        );
        fresh.name = name.clone();
        fresh.harness = Some(harness.clone());
        fresh.cwd = hit.cwd.clone();
        fresh.status = crate::AgentStatus::Orphaned;
        fresh.origin = Some("adopted".to_string());
        fresh.short_id = short_id.clone();
        fresh.last_message_at = last_message_at.clone();
        fresh.adopted_by_session = adopted_by.clone();
        fresh.created_at = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
        crate::register::apply_session_manifest_role(&mut fresh, &sid, &hit.cwd);
        entries.push(fresh);
        Ok(name)
    });
    match written {
        Ok(Ok(name)) => {
            // Read the row back so write-time fields (the minted fno_id)
            // reach the caller exactly as stored.
            let stored = crate::client_commands::load_registry_entries(registry_path)
                .ok()
                .and_then(|rows| {
                    rows.into_iter()
                        .find(|r| r.get("name").and_then(Value::as_str) == Some(name.as_str()))
                });
            stored.ok_or_else(|| {
                HealError::Unavailable(format!(
                    "adopted {} but could not read its row back",
                    py_repr_str(&sid)
                ))
            })
        }
        Ok(Err(refusal)) => Err(refusal),
        Err(exc) => {
            eprintln!(
                "WARN: resolved {} from the {} store but could not register it ({exc}); the row \
will appear on a later resolution.",
                py_repr_str(token),
                hit.harness
            );
            let mut synthetic = RegistryEntry::new(
                Some(sid.clone()),
                crate::state::Lineage::unproven(
                    "adopted from the harness store: adoption carries no parent edge",
                ),
            );
            synthetic.name = crate::identity::canonical_handle(&sid);
            synthetic.harness = Some(harness);
            synthetic.cwd = hit.cwd.clone();
            synthetic.status = crate::AgentStatus::Orphaned;
            synthetic.origin = Some("adopted".to_string());
            synthetic.short_id = short_id;
            synthetic.adopted_by_session = adopted_by;
            serde_json::to_value(synthetic)
                .map_err(|e| HealError::Unavailable(format!("adopted row did not serialize: {e}")))
        }
    }
}

/// Adopt the one session `token` names from the harness stores. `Ok(None)`
/// when the token is not session-shaped or no store knows it.
pub(crate) fn heal_from_store(
    token: &str,
    registry_path: &Path,
) -> Result<Option<Value>, HealError> {
    let hits = complete_store_hits(token)?;
    match hits.len() {
        0 => Ok(None),
        1 => adopt(&hits[0], registry_path, token).map(Some),
        n => Err(HealError::Ambiguous(format!(
            "token {} matches {n} sessions across harness stores: {}. Disambiguate with the full \
session id.",
            py_repr_str(token),
            sorted_hits(&hits)
                .iter()
                .map(|h| format!("{} ({})", h.session_id, h.harness))
                .collect::<Vec<_>>()
                .join(", ")
        ))),
    }
}

fn registry_label(row: &Value) -> (String, String, String) {
    let name = row
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let harness = row
        .get("harness")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let sid = row
        .get("harness_session_id")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| name.clone());
    (sid, harness, name)
}

fn ambiguous_across(token: &str, candidates: Vec<String>) -> HealError {
    HealError::Ambiguous(format!(
        "token {} is ambiguous across {} sessions: {}. Disambiguate with the full session id.",
        py_repr_str(token),
        candidates.len(),
        candidates.join(", ")
    ))
}

/// A registry hit must stay unique against the persisted friendly aliases.
fn ensure_unique_across_aliases(row: &Value, token: &str) -> Result<(), HealError> {
    let alias_ids = crate::mail_cli::send::resolve::alias_to_session_ids(token).map_err(|_| {
        HealError::Unavailable(format!(
            "persisted alias map unreadable; cannot resolve {} uniquely",
            py_repr_str(token)
        ))
    })?;
    let registry_key = row
        .get("harness_session_id")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(session_identity_key);
    let mut foreign: Vec<String> = alias_ids
        .into_iter()
        .filter(|sid| registry_key.as_deref() != Some(session_identity_key(sid).as_str()))
        .collect();
    foreign.sort();
    foreign.dedup();
    if foreign.is_empty() {
        return Ok(());
    }
    let (sid, harness, name) = registry_label(row);
    let mut candidates = vec![format!("{sid} ({harness}, registry name={name})")];
    candidates.extend(foreign.iter().map(|s| format!("{s} (persisted alias)")));
    Err(ambiguous_across(token, candidates))
}

/// A registry hit must stay unique against store-only sessions.
fn ensure_unique_across_stores(row: &Value, token: &str) -> Result<(), HealError> {
    let (sid, harness, name) = registry_label(row);
    let registry_key = row
        .get("harness_session_id")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(session_identity_key);
    let mut foreign: Vec<StoreHit> = Vec::new();
    for hit in complete_store_hits(token)? {
        let key = session_identity_key(&hit.session_id);
        let same = hit.harness == harness && registry_key.as_deref() == Some(key.as_str());
        let seen = foreign
            .iter()
            .any(|f| f.harness == hit.harness && session_identity_key(&f.session_id) == key);
        if !same && !seen {
            foreign.push(hit);
        }
    }
    if foreign.is_empty() {
        return Ok(());
    }
    let mut candidates = vec![format!("{sid} ({harness}, registry name={name})")];
    candidates.extend(
        sorted_hits(&foreign)
            .iter()
            .map(|h| format!("{} ({}, harness store)", h.session_id, h.harness)),
    );
    Err(ambiguous_across(token, candidates))
}

/// Resolve `token` against the registry at `registry_path` and every harness
/// store as one namespace. A full session id and an ordinary name answer from
/// the registry; a short session-shaped hit must stay unique across aliases
/// and stores; a registry miss may adopt one unique store hit. An ambiguous
/// registry is never healed: that would pick the winner it refused to pick.
pub(crate) fn heal_all_sources(
    token: &str,
    registry_path: &Path,
) -> Result<Option<Value>, HealError> {
    use crate::client_commands::{find_agent_entry, ResolveError};
    use crate::resolve_tier::entry_session_tier;
    let rows = crate::client_commands::load_registry_entries(registry_path).map_err(|e| {
        HealError::Unavailable(format!(
            "registry unreadable ({e}); cannot resolve {}",
            py_repr_str(token)
        ))
    })?;
    match find_agent_entry(&rows, token) {
        Ok(row) => {
            if entry_session_tier(row, token) != Some(0) {
                ensure_unique_across_aliases(row, token)?;
                if crate::resolve_tier::is_session_shaped(token) {
                    ensure_unique_across_stores(row, token)?;
                }
            }
            Ok(Some(row.clone()))
        }
        Err(ResolveError::Ambiguous(message)) => Err(HealError::Ambiguous(message)),
        Err(ResolveError::NotFound(_)) => heal_from_store(token, registry_path),
    }
}

/// `fno agents heal-token <token> [--registry FILE] [--all-sources]`: the
/// healed row as one JSON line. Exit 13 on a miss, 3 with the candidates on
/// stderr when ambiguous, 12 when identity evidence is unreadable.
pub fn run_heal_token(args: &[String]) -> i32 {
    let mut token: Option<String> = None;
    let mut registry: Option<PathBuf> = None;
    let mut all_sources = false;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--registry" => match it.next() {
                Some(v) => registry = Some(PathBuf::from(v)),
                None => {
                    eprintln!("error: --registry wants a registry file");
                    return 2;
                }
            },
            "--all-sources" => all_sources = true,
            // Retired: healing is machine-wide; accepted as a silent no-op.
            "--cross-project" => {}
            "-h" | "--help" => {
                println!("usage: fno agents heal-token <token> [--registry FILE] [--all-sources]");
                return 0;
            }
            other if other.starts_with('-') => {
                eprintln!("heal-token: unknown argument {other:?}");
                return 2;
            }
            other if token.is_none() => token = Some(other.to_string()),
            other => {
                eprintln!("heal-token: unexpected argument {other:?}");
                return 2;
            }
        }
    }
    let Some(token) = token else {
        eprintln!("error: missing token");
        return 2;
    };
    let registry = registry.unwrap_or_else(AgentsHome::shared_registry_json);
    let outcome = if all_sources {
        heal_all_sources(&token, &registry)
    } else {
        heal_from_store(&token, &registry)
    };
    match outcome {
        Ok(Some(row)) => {
            println!("{row}");
            0
        }
        Ok(None) => HEAL_MISS_EXIT,
        Err(err) => {
            eprintln!("{}", err.message());
            match err {
                HealError::Unavailable(_) if all_sources => HEAL_UNAVAILABLE_EXIT,
                _ => HEAL_AMBIGUOUS_EXIT,
            }
        }
    }
}
