//! The wake-identity tombstone: one JSON file beside the registry mapping a
//! stopped worker's harness session uuid to the registry identity it last
//! held (name, `fno_id`, the `spawned_by_*` parent edge and the node), so a
//! revival comes back as the session it was. The stamp lives in the
//! registry write's removal accounting, so every drop door records it with
//! no caller change; the stop seams stamp too, for a wake that reads while
//! the registry itself is unreadable (`stamp_from_row`). A wake that finds
//! neither row nor tombstone reads the transcript's naming records before
//! the alias (`lookup_transcript`). One consumer
//! (`reentry::revival_identity`). No expiry: an identity is the
//! worker-to-node join for the uuid's lifetime; the per-uuid dedupe plus
//! the record cap bound the file.

use crate::claude_ask::ClaudeHome;
use crate::daemon::now_epoch_secs;
use crate::paths::AgentsHome;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

/// Records kept after a write trims. One per distinct stopped worker uuid;
/// a fleet week is a few hundred, so the cap never trims live history.
const CAP: usize = 512;

const FILENAME: &str = "wake_names.json";

fn path(home: &AgentsHome) -> PathBuf {
    home.root().join(FILENAME)
}

/// The identity a revival carries: the row's name plus every join the
/// board, mail and ledger read. A record from before the carry holds a
/// name only; no field is ever invented for an old record.
#[derive(Debug, Clone, PartialEq, Default)]
pub(crate) struct Carried {
    pub name: String,
    pub fno_id: Option<String>,
    pub spawned_by_session: Option<String>,
    pub spawned_by_harness: Option<String>,
    pub spawned_by_cwd: Option<String>,
    pub node: Option<String>,
}

fn clean(value: &Option<String>) -> Option<&str> {
    value.as_deref().map(str::trim).filter(|v| !v.is_empty())
}

impl Carried {
    /// The name-only answer (transcript rung and alias): no recorded
    /// identity keeps a fresh id and no parent.
    pub(crate) fn named(name: impl Into<String>) -> Self {
        Carried {
            name: name.into(),
            ..Default::default()
        }
    }

    /// The identity of a registry row; a blank value reads as absent.
    pub(crate) fn from_row(entry: &crate::state::RegistryEntry) -> Self {
        let field = |value: &Option<String>| clean(value).map(str::to_string);
        Carried {
            name: entry.name.clone(),
            fno_id: field(&entry.fno_id),
            spawned_by_session: field(&entry.spawned_by_session),
            spawned_by_harness: field(&entry.spawned_by_harness),
            spawned_by_cwd: field(&entry.spawned_by_cwd),
            node: field(&entry.node),
        }
    }

    fn to_row_json(&self, session_id: &str, at: i64) -> Value {
        let mut row = json!({ "session_id": session_id, "name": self.name, "at": at });
        for (key, value) in [
            ("fno_id", &self.fno_id),
            ("spawned_by_session", &self.spawned_by_session),
            ("spawned_by_harness", &self.spawned_by_harness),
            ("spawned_by_cwd", &self.spawned_by_cwd),
            ("node", &self.node),
        ] {
            if let Some(v) = clean(value) {
                row[key] = json!(v);
            }
        }
        row
    }
}

/// Stamp `session_id`'s identity. Best-effort: a failed write costs a
/// later wake its identity, never the stop itself (the stop arms ignore
/// the result, the same stance the rm tombstone takes).
pub(crate) fn record_at(
    file: &Path,
    session_id: &str,
    carried: &Carried,
    now: i64,
) -> Result<(), String> {
    if session_id.trim().is_empty() || carried.name.trim().is_empty() {
        return Ok(());
    }
    crate::rm_tombstone::update_json_array(file, |mut entries| {
        entries.retain(|row| row.get("session_id").and_then(Value::as_str) != Some(session_id));
        entries.push(carried.to_row_json(session_id, now));
        while entries.len() > CAP {
            entries.remove(0);
        }
        entries
    })
}

/// The tombstoned identity `session_id` held when its row stopped, read from
/// the file beside `registry_path` (the daemon resolves the pair together,
/// the same way the rm tombstone lookup does). A missing or unreadable file
/// answers None: the caller's alias fallback applies.
pub(crate) fn lookup_beside(registry_path: &Path, session_id: &str) -> Option<Carried> {
    let file = registry_path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join(FILENAME);
    let raw = std::fs::read_to_string(file).ok()?;
    let entries: Vec<Value> = serde_json::from_str(&raw).ok()?;
    let field = |row: &Value, key: &str| {
        row.get(key)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .map(str::to_string)
    };
    entries.into_iter().rev().find_map(|row| {
        let id = row.get("session_id").and_then(Value::as_str)?;
        if id != session_id {
            return None;
        }
        let name = field(&row, "name")?;
        Some(Carried {
            name,
            fno_id: field(&row, "fno_id"),
            spawned_by_session: field(&row, "spawned_by_session"),
            spawned_by_harness: field(&row, "spawned_by_harness"),
            spawned_by_cwd: field(&row, "spawned_by_cwd"),
            node: field(&row, "node"),
        })
    })
}

/// The shape a spawnable name keeps: alphanumerics plus `-` `_` `.`. One
/// predicate for both filters that name a session (the relaunch name and
/// the wake-name transcript rung), so the two cannot drift.
pub(crate) fn spawn_safe_name(value: &str) -> bool {
    value
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

/// The stamp a registry-row DROP uses: the write's removal accounting (and
/// every stop seam that holds a row) records the identity here, the same
/// best-effort contract the stop seams honor - a failed write costs a
/// later wake its identity, never the drop.
pub(crate) fn stamp_from_row(home: &AgentsHome, entry: &crate::state::RegistryEntry) {
    let Some(sid) = entry
        .harness_session_id
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    else {
        return;
    };
    let _ = record_at(
        &path(home),
        sid,
        &Carried::from_row(entry),
        now_epoch_secs(),
    );
}

/// A transcript past this many bytes is skipped: the rung is best-effort,
/// and a wake must not pay a multi-MB scan to answer a name. Titles ride
/// normal records, so the cap only sheds pathological files.
const TRANSCRIPT_SCAN_CAP_BYTES: u64 = 8 * 1024 * 1024;

/// The last spawn-safe name this uuid's transcript carries: the newest
/// `custom-title` (customTitle) or `agent-name` (agentName) record whose
/// value is non-empty, not this session's own `wake-<handle>` alias, and
/// spawn-name shaped. The rung between the tombstone and the alias: a row
/// dropped before the tombstone existed still revives under the name its
/// transcript shows. An unreadable store answers None - best-effort by the
/// module contract.
pub(crate) fn lookup_transcript(claude_home: &ClaudeHome, session_id: &str) -> Option<String> {
    let alias = format!("wake-{}", crate::identity::canonical_handle(session_id));
    for projects in claude_home.project_dirs() {
        let Some(path) = crate::claude_transcript_paths::resolve_transcript(&projects, session_id)
        else {
            continue;
        };
        let Ok(meta) = std::fs::metadata(&path) else {
            continue;
        };
        if meta.len() > TRANSCRIPT_SCAN_CAP_BYTES {
            continue;
        }
        let Ok(file) = std::fs::File::open(&path) else {
            continue;
        };
        let mut name = None;
        for line in std::io::BufRead::lines(std::io::BufReader::new(file)).map_while(Result::ok) {
            let Ok(record) = serde_json::from_str::<Value>(&line) else {
                continue;
            };
            let value = match record.get("type").and_then(Value::as_str) {
                Some("custom-title") => record.get("customTitle").and_then(Value::as_str),
                Some("agent-name") => record.get("agentName").and_then(Value::as_str),
                _ => None,
            };
            let Some(value) = value
                .map(str::trim)
                .filter(|v| !v.is_empty() && *v != alias)
            else {
                continue;
            };
            // A spawn name only: the fallback's answer must be spawnable.
            if !spawn_safe_name(value) {
                continue;
            }
            name = Some(value.to_string());
        }
        if name.is_some() {
            return name;
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn staged() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let reg = dir.path().join("registry.json");
        (dir, reg)
    }

    /// AC1-HP: a stamp then read roundtrips the whole identity, with a
    /// re-stamp rewriting the record. AC2-EDGE: a record written before the
    /// carry shipped (name only) reads back with a name and no invented
    /// field.
    #[test]
    fn record_then_lookup_roundtrips_and_dedupes() {
        fn full(name: &str, fno: &str, sid: &str, cwd: &str) -> Carried {
            Carried {
                name: name.into(),
                fno_id: Some(fno.into()),
                spawned_by_session: Some(sid.into()),
                spawned_by_harness: Some("claude".into()),
                spawned_by_cwd: Some(cwd.into()),
                node: Some("x-1".into()),
            }
        }
        let (_dir, reg) = staged();
        let file = reg.parent().unwrap().join(FILENAME);
        let uuid = "aaaabbbb-cccc-dddd-eeee-ffff00001111";
        record_at(
            &file,
            uuid,
            &full("t-old", "f-1", "lead-1111", "/lead/dir"),
            1,
        )
        .unwrap();
        record_at(
            &file,
            uuid,
            &full("t-new", "f-2", "lead-2222", "/lead/dir2"),
            2,
        )
        .unwrap();
        let carried = lookup_beside(&reg, uuid).unwrap();
        assert_eq!(carried.name, "t-new", "the last stop's identity wins");
        assert_eq!(carried.fno_id.as_deref(), Some("f-2"));
        assert_eq!(carried.spawned_by_session.as_deref(), Some("lead-2222"));
        assert_eq!(carried.spawned_by_harness.as_deref(), Some("claude"));
        assert_eq!(carried.spawned_by_cwd.as_deref(), Some("/lead/dir2"));
        assert_eq!(carried.node.as_deref(), Some("x-1"), "the node joins too");
        // An empty uuid or name is refused at the write, so the alias
        // fallback answers instead of a blank record.
        record_at(&file, "", &Carried::named("t"), 3).unwrap();
        record_at(&file, uuid, &Carried::named("  "), 3).unwrap();
        // A pre-carry record (name only, as every drop stamped before the
        // carry) reads back with the name and nothing invented beside it.
        let legacy = json!({ "session_id": "bbbbcccc-0000-0000-0000-000000000001", "name": "t-legacy", "at": 1 });
        crate::rm_tombstone::update_json_array(&file, |mut entries| {
            entries.push(legacy);
            entries
        })
        .unwrap();
        let legacy_read = lookup_beside(&reg, "bbbbcccc-0000-0000-0000-000000000001").unwrap();
        assert_eq!(legacy_read.name, "t-legacy");
        assert_eq!(legacy_read.fno_id, None, "no field is invented");
        assert_eq!(legacy_read.spawned_by_session, None);
        assert_eq!(legacy_read.node, None);
        let raw = std::fs::read_to_string(&file).unwrap();
        let rows: Vec<Value> = serde_json::from_str(&raw).unwrap();
        assert_eq!(rows.len(), 2, "the uuid holds one record");
    }

    /// The transcript rung answers the last non-wake customTitle or
    /// agentName: the store that lost both the row and the tombstone still
    /// revives under the name the transcript shows.
    #[test]
    fn transcript_rung_answers_the_last_non_wake_name() {
        let home = tempfile::tempdir().unwrap();
        let claude_home = ClaudeHome::at(home.path());
        // The fixture plants the transcript where the production accessor
        // looks, so a store-layout move follows the accessor, not this test.
        let projects = claude_home.project_dirs()[0].join("work");
        std::fs::create_dir_all(&projects).unwrap();
        let uuid = "7ada6c8a-1111-2222-3333-444444444444";
        std::fs::write(
            projects.join(format!("{uuid}.jsonl")),
            concat!(
                "{\"type\":\"custom-title\",\"customTitle\":\"t-old-name\"}\n",
                "{\"type\":\"user\",\"message\":{}}\n",
                "{\"type\":\"agent-name\",\"agentName\":\"wake-7ada6c8a\"}\n",
                "{\"type\":\"agent-name\",\"agentName\":\"t-rename-later\"}\n",
            ),
        )
        .unwrap();
        assert_eq!(
            lookup_transcript(&claude_home, uuid).as_deref(),
            Some("t-rename-later"),
            "the last non-wake record wins; the wake alias is skipped",
        );
    }
}
