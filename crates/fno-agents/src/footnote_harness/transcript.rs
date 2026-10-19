//! Where footnote sessions live, as the supervisor resolves it. The record
//! format and its reader are `crate::footnote_transcript`, shared with the
//! `footnote` binary; the writer lives in bllshttng/fnh `src/transcript.rs`.

use serde_json::Value;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use crate::footnote_transcript::HEARTBEAT_STALE_SECS;
pub use crate::footnote_transcript::{
    is_record_file, read_records, transcript_file, KNOWN_TYPES, SCHEMA_V,
};

/// A heartbeat lands every 30 s, so a live record holds one near its end.
/// The bound covers several spilled tool results written in between.
const HEARTBEAT_TAIL_BYTES: u64 = 256 * 1024;

/// Whether the record's newest heartbeat is younger than the stale bound.
/// False on a missing, unreadable or quiet record: that proves nothing.
pub fn heartbeat_fresh(record: &Path, now_secs: u64) -> bool {
    let Ok(mut file) = std::fs::File::open(record) else {
        return false;
    };
    let Ok(meta) = file.metadata() else {
        return false;
    };
    // No write inside the bound means no fresh heartbeat: skip the read.
    let written = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_secs());
    if now_secs.saturating_sub(written) >= HEARTBEAT_STALE_SECS {
        return false;
    }
    let len = meta.len();
    let mut tail = Vec::new();
    if file
        .seek(SeekFrom::Start(len.saturating_sub(HEARTBEAT_TAIL_BYTES)))
        .and_then(|_| file.read_to_end(&mut tail))
        .is_err()
    {
        return false;
    }
    String::from_utf8_lossy(&tail)
        .lines()
        .rev()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .find(|r| r["type"] == "heartbeat")
        .and_then(|r| r["ts"].as_str().and_then(ts_secs))
        .is_some_and(|at| now_secs.saturating_sub(at) < HEARTBEAT_STALE_SECS)
}

/// The cwd the record's first line, its header, names.
pub fn header_cwd(record: &Path) -> Option<PathBuf> {
    let file = std::fs::File::open(record).ok()?;
    let mut first = String::new();
    std::io::BufRead::read_line(&mut std::io::BufReader::new(file), &mut first).ok()?;
    let rec: Value = serde_json::from_str(&first).ok()?;
    rec["data"]["cwd"].as_str().map(PathBuf::from)
}

/// Any RFC3339 `ts`, offset included, as UTC epoch seconds.
fn ts_secs(ts: &str) -> Option<u64> {
    let at = chrono::DateTime::parse_from_rfc3339(ts).ok()?;
    u64::try_from(at.timestamp()).ok()
}

/// `~/.fno/sessions`, beside the spaces root so test fences on that root
/// cover it too.
pub fn sessions_root() -> PathBuf {
    let spaces = crate::paths::spaces_root();
    spaces
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or(spaces)
        .join("sessions")
}

/// The project key: the canonical checkout's space slug, so every worktree
/// of a repo shares one; `_none` outside a repo.
pub fn project_slug(cwd: &Path) -> String {
    crate::paths::canonical_repo_root(cwd)
        .map(|root| crate::paths::space_slug(&root))
        .unwrap_or_else(|| "_none".to_string())
}

pub fn session_dir(root: &Path, cwd: &Path, fno_id: &str) -> PathBuf {
    root.join(project_slug(cwd)).join(fno_id)
}

/// Find a session's directory under any project slug: the sidecar dir
/// whose record `<fno_id>.jsonl` exists beside it.
pub fn find_session_dir(root: &Path, fno_id: &str) -> Option<PathBuf> {
    std::fs::read_dir(root)
        .ok()?
        .flatten()
        .map(|e| e.path().join(fno_id))
        .find(|p| transcript_file(p, fno_id).is_file())
}
