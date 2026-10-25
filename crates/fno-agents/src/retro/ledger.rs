//! The carve-out ledger, as the retro passes read and consume it.
//!
//! The ledger lives in the repo's space (`<space>/carveouts.jsonl`), with a
//! one-time migration from the legacy `<repo>/.fno/carveouts.jsonl`. Every
//! rewriter takes the sibling `carveouts.jsonl.lock.d` mkdir mutex the
//! Python capture writer also takes, and replaces the file atomically, so a
//! torn write never loses the rows it was not touching.

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
