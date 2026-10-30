//! The carve-out ledger, as the retro passes read and consume it.
//!
//! The ledger lives in the repo's space (`<space>/carveouts.jsonl`), with a
//! one-time migration from the legacy `<repo>/.fno/carveouts.jsonl`. Every
//! writer takes the sibling `carveouts.jsonl.lock.d` mkdir mutex, and every
//! rewriter replaces the file atomically, so a torn write never loses the
//! rows it was not touching.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde_json::Value;

use super::classify::py_splitlines;
use super::py_str;

pub const BACKFILL_KIND: &str = "backfill";
pub const VALID_KINDS: [&str; 3] = ["deferred", "oos-bug", BACKFILL_KIND];
pub const CARVEOUTS_NAME: &str = "carveouts.jsonl";

/// The ledger under the canonical `root`, migrated from the checkout once.
pub fn ledger_path(root: &Path) -> PathBuf {
    let path = crate::paths::space_dir(root).join(CARVEOUTS_NAME);
    crate::paths::migrate_from_checkout(&root.join(".fno").join(CARVEOUTS_NAME), &path);
    path
}

/// Ledger rows in order, narrowed to `kind`. A malformed or non-object line
/// is skipped. A missing ledger is empty; a ledger that exists but cannot be
/// read is an error, never a silent empty.
pub fn read_carveouts(root: &Path, kind: Option<&str>) -> Result<Vec<Value>, String> {
    let path = ledger_path(root);
    if !path.exists() {
        return Ok(Vec::new());
    }
    let text = std::fs::read_to_string(&path)
        .map_err(|e| format!("cannot read carve-out ledger {}: {e}", path.display()))?;
    Ok(py_splitlines(&text)
        .into_iter()
        .filter_map(|line| serde_json::from_str::<Value>(line.trim()).ok())
        .filter(Value::is_object)
        .filter(|rec| kind.is_none_or(|k| rec.get("kind").and_then(Value::as_str) == Some(k)))
        .collect())
}

/// Hold the ledger's mkdir mutex for `f`; `None` when it cannot be taken
/// within 30 seconds.
fn with_lock<T>(path: &Path, f: impl FnOnce() -> T) -> Option<T> {
    let lock_dir = path.with_file_name(format!("{CARVEOUTS_NAME}.lock.d"));
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        match std::fs::create_dir(&lock_dir) {
            Ok(()) => break,
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                if Instant::now() >= deadline {
                    return None;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(_) => return None,
        }
    }
    let out = f();
    let _ = std::fs::remove_dir(&lock_dir);
    Some(out)
}

fn rewrite(path: &Path, lines: &[String]) -> std::io::Result<()> {
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(dir)?;
    let mut tmp = tempfile::Builder::new()
        .prefix(&format!("{CARVEOUTS_NAME}."))
        .suffix(".tmp")
        .tempfile_in(dir)?;
    let body = if lines.is_empty() {
        String::new()
    } else {
        format!("{}\n", lines.join("\n"))
    };
    tmp.write_all(body.as_bytes())?;
    tmp.as_file().sync_all()?;
    tmp.persist(path).map_err(|e| e.error)?;
    Ok(())
}

/// Append one row under the mutex. A failure is an error: a failed capture is
/// never a silent success.
pub fn append_row(root: &Path, row: &Value) -> Result<(), String> {
    let path = ledger_path(root);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    with_lock(&path, || {
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)?;
        writeln!(file, "{row}")
    })
    .ok_or_else(|| format!("carveouts.jsonl lock timeout: {}", path.display()))?
    .map_err(|e| e.to_string())
}

/// Why [`rewrite_row`] did not land. An absent id must not read as a failed
/// write: the first means the edit cannot apply, the second that it did not.
pub enum RewriteError {
    NotFound(String),
    Failed(String),
}

/// Edit the row named `id` in one locked rewrite and return it. Never creates
/// the row, since that would resurrect one `/fno:ship pr merged` consumed. A
/// malformed neighbouring line is kept verbatim.
pub fn rewrite_row(
    root: &Path,
    id: &str,
    edit: impl FnOnce(&mut serde_json::Map<String, Value>),
) -> Result<Value, RewriteError> {
    let path = ledger_path(root);
    if !path.exists() {
        return Err(RewriteError::NotFound(format!(
            "no carve-out ledger at {}",
            path.display()
        )));
    }
    with_lock(&path, || {
        let text = std::fs::read_to_string(&path).map_err(|e| {
            RewriteError::Failed(format!(
                "cannot read carve-out ledger {}: {e}",
                path.display()
            ))
        })?;
        let mut edit = Some(edit);
        let mut updated = None;
        let mut kept = Vec::new();
        for line in py_splitlines(&text) {
            let stripped = line.trim();
            if stripped.is_empty() {
                continue;
            }
            match serde_json::from_str::<Value>(stripped) {
                Ok(Value::Object(mut rec))
                    if rec.contains_key("id") && py_str(rec.get("id")) == id =>
                {
                    if let Some(edit) = edit.take() {
                        edit(&mut rec);
                    }
                    let rec = Value::Object(rec);
                    kept.push(rec.to_string());
                    updated = Some(rec);
                }
                _ => kept.push(stripped.to_string()),
            }
        }
        let Some(updated) = updated else {
            return Err(RewriteError::NotFound(format!(
                "{id} is not on the ledger (already resolved, or recorded under a different project root)"
            )));
        };
        rewrite(&path, &kept).map_err(|e| {
            RewriteError::Failed(format!(
                "cannot write carve-out ledger {}: {e}",
                path.display()
            ))
        })?;
        Ok(updated)
    })
    .unwrap_or_else(|| {
        Err(RewriteError::Failed(format!(
            "carveouts.jsonl lock timeout: {}",
            path.display()
        )))
    })
}

/// Remove `ids` from the ledger and return how many rows went. A malformed
/// line is kept. Best-effort: 0 on any failure, lock timeout included.
pub fn consume_carveouts(root: &Path, ids: &[String]) -> usize {
    let want: std::collections::HashSet<&str> = ids.iter().map(String::as_str).collect();
    if want.is_empty() {
        return 0;
    }
    let path = ledger_path(root);
    if !path.exists() {
        return 0;
    }
    with_lock(&path, || {
        let Ok(text) = std::fs::read_to_string(&path) else {
            return 0;
        };
        let mut kept = Vec::new();
        let mut removed = 0;
        for line in py_splitlines(&text) {
            let stripped = line.trim();
            if stripped.is_empty() {
                continue;
            }
            match serde_json::from_str::<Value>(stripped) {
                Ok(rec)
                    if want.contains(py_str(rec.get("id")).as_str()) && rec.get("id").is_some() =>
                {
                    removed += 1;
                }
                _ => kept.push(stripped.to_string()),
            }
        }
        if rewrite(&path, &kept).is_err() {
            return 0;
        }
        removed
    })
    .unwrap_or(0)
}
