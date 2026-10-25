//! The agy (Antigravity) transcript source. The store lives under
//! `~/.gemini/antigravity-cli/`: `conversation_summaries.db` (one sqlite row
//! per conversation), `conversations/<id>.db` (the transcript artifact) and
//! `history.jsonl` (the typed-input log).
//!
//! v1 boundary: the conversation bodies are protobuf blobs in the
//! per-conversation `steps` table with no named schema in this repo, so a
//! strings-based reader would guess; only the named records (summaries and
//! typed-input history) map to turns. Typed text and text the mux injected
//! share the row shape, so every turn reads the classifier's residual
//! (`unknown`), never `operator`. The store is only ever read.

use crate::opencode_transcript::push_user_row;
use crate::provenance::{
    claude_shaped_tool_uses, claude_shaped_turns, within_window, SessionFile, TranscriptSource,
    Turn,
};
use rusqlite::OpenFlags;
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

/// `~/.gemini/antigravity-cli`, the Antigravity CLI store root.
fn stores_root() -> PathBuf {
    match std::env::var_os("HOME") {
        Some(home) => PathBuf::from(home).join(".gemini").join("antigravity-cli"),
        None => PathBuf::from("."),
    }
}

/// The three store paths: summaries db, typed-input log, conversations dir.
pub(crate) fn agy_stores() -> (PathBuf, PathBuf, PathBuf) {
    let root = stores_root();
    (
        root.join("conversation_summaries.db"),
        root.join("history.jsonl"),
        root.join("conversations"),
    )
}

fn open_read_only(path: &Path) -> Result<rusqlite::Connection, String> {
    let conn = match rusqlite::Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY) {
        Ok(conn) => conn,
        // A daemon's clean exit can leave the header WAL with the -shm sidecar
        // gone; the plain read-only open refuses that state and the immutable
        // retry reads the checkpointed file.
        Err(e) => crate::graph_store::open_read_only_wal_safe(path)
            .ok_or_else(|| format!("{}: {e}", path.display()))?,
    };
    conn.busy_timeout(std::time::Duration::from_secs(2))
        .map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(conn)
}

fn mtime_secs(path: &Path) -> u64 {
    std::fs::metadata(path)
        .ok()
        .and_then(|m| m.modified().ok())
        .and_then(|m| m.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

const SUMMARIES_SQL: &str = "SELECT conversation_id, workspace_uris FROM conversation_summaries";

/// The agy transcript source. `roots` scopes the fold to the requested
/// projects (a conversation lists when any of its workspace URIs sits under
/// a root), `None` reads every conversation.
pub(crate) struct AgySource {
    pub(crate) summaries_db: PathBuf,
    pub(crate) history: PathBuf,
    pub(crate) conversations_dir: PathBuf,
    pub(crate) roots: Option<Vec<PathBuf>>,
}

impl AgySource {
    /// The summaries db must open read-only and answer its query; the
    /// typed-input log must be readable. The first failure names the path.
    pub(crate) fn probe(&self) -> Result<(), String> {
        let conn = open_read_only(&self.summaries_db)?;
        conn.prepare(SUMMARIES_SQL)
            .map_err(|e| format!("{}: {e}", self.summaries_db.display()))?;
        if !self.history.is_file() {
            return Err(format!(
                "no agy typed-input log at {}",
                self.history.display()
            ));
        }
        Ok(())
    }
}

impl TranscriptSource for AgySource {
    fn harness(&self) -> &'static str {
        "agy"
    }

    fn sessions(&self, days: u64) -> Vec<SessionFile> {
        let now = std::time::SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let mut out = Vec::new();
        let Ok(conn) = open_read_only(&self.summaries_db) else {
            return out;
        };
        let Ok(mut stmt) = conn.prepare(SUMMARIES_SQL) else {
            return out;
        };
        let Ok(rows) = stmt
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?))
            })
            .map_err(|e| e.to_string())
        else {
            return out;
        };
        for row in rows.flatten() {
            let (id, workspace_uris) = row;
            let db = self.conversations_dir.join(format!("{id}.db"));
            let Ok(meta) = std::fs::metadata(&db) else {
                continue;
            };
            if !meta.is_file() {
                continue;
            }
            // The conversation writes through WAL, so the db file's stamp
            // lags until a checkpoint; the -wal stamp keeps an active
            // conversation inside a windowed fold.
            let mtime = mtime_secs(&db).max(mtime_secs(
                &self.conversations_dir.join(format!("{id}.db-wal")),
            ));
            if !within_window(mtime, days, now) {
                continue;
            }
            let workspaces = workspace_uris
                .as_deref()
                .and_then(|u| serde_json::from_str::<Value>(u).ok())
                .and_then(|v| v.as_array().cloned())
                .unwrap_or_default()
                .iter()
                .filter_map(|u| u.as_str())
                .filter_map(|u| u.strip_prefix("file://"))
                .map(PathBuf::from)
                .collect::<Vec<_>>();
            let in_scope = match &self.roots {
                None => true,
                Some(roots) => workspaces
                    .iter()
                    .any(|p| roots.iter().any(|r| p.starts_with(r))),
            };
            if !in_scope {
                continue;
            }
            out.push(SessionFile {
                path: db,
                session_id: id,
                mtime,
                size: meta.len(),
            });
        }
        out.sort_by(|a, b| b.mtime.cmp(&a.mtime));
        out
    }

    /// One conversation's typed-input rows rendered as claude-shaped JSONL,
    /// re-read from the history log the store names.
    fn read(&self, file: &SessionFile) -> String {
        let Ok(raw) = std::fs::read_to_string(&self.history) else {
            return String::new();
        };
        render(&raw, &file.session_id)
    }

    fn turns(&self, raw: &str) -> Vec<Turn> {
        claude_shaped_turns(raw)
    }

    fn tool_uses(&self, raw: &str) -> usize {
        claude_shaped_tool_uses(raw)
    }
}

/// history.jsonl rows for one conversation rendered as claude-shaped user
/// rows; rows without a matching conversationId render nothing.
fn render(raw: &str, session_id: &str) -> String {
    let mut out = String::new();
    for line in raw.lines() {
        let Ok(row) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if row.get("conversationId").and_then(|v| v.as_str()) != Some(session_id) {
            continue;
        }
        let Some(text) = row.get("display").and_then(|v| v.as_str()) else {
            continue;
        };
        let ts = row.get("timestamp").and_then(|v| v.as_i64()).unwrap_or(0);
        push_user_row(&mut out, ts, text);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn summaries_reader_reads_a_wal_store_whose_shm_is_gone() {
        // The exact state a daemon's clean exit leaves: the header stays WAL
        // while the checkpoint removed the -wal and -shm sidecars. A plain
        // read-only open refuses it; the immutable retry must answer.
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("conversation_summaries.db");
        let conn = rusqlite::Connection::open(&db).unwrap();
        conn.execute_batch(
            "PRAGMA journal_mode=WAL;\n\
             CREATE TABLE conversation_summaries (conversation_id TEXT, workspace_uris TEXT);\n\
             INSERT INTO conversation_summaries VALUES ('c-1', '[]');\n\
             PRAGMA wal_checkpoint(TRUNCATE);",
        )
        .unwrap();
        drop(conn);
        assert!(!dir.path().join("conversation_summaries.db-shm").exists());
        let conn =
            open_read_only(&db).expect("the immutable retry must open the checkpointed store");
        let n: i64 = conn
            .query_row("SELECT COUNT(*) FROM conversation_summaries", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(n, 1);
    }
}
