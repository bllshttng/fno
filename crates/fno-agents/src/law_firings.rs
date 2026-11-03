//! The per-law firing log: one JSONL row each time a stage door fires a law.
//!
//! ```json
//! {"ts": "<RFC 3339>", "law": "d-1234abcd", "stage": "blueprint"}
//! ```
//!
//! A firing-count guard reads [`count_since`] against the instant the law was
//! recorded. Appends are single `write` calls on an `O_APPEND` handle, so
//! parallel sessions interleave whole rows. A write that fails is dropped: a
//! lost row under-counts a guard by one, and a stage door must never fail
//! because its log did.

use serde_json::{json, Value};
use std::io::Write;
use std::path::{Path, PathBuf};

const LOG_FILE: &str = "state/law-firings.jsonl";

pub fn default_path() -> PathBuf {
    crate::decision_index::default_state_path(LOG_FILE)
}

/// Appends one row per fired law id.
pub fn record(path: &Path, laws: &[&str], stage: &str, now: chrono::DateTime<chrono::Utc>) {
    if laws.is_empty() {
        return;
    }
    let ts = now.to_rfc3339();
    let mut buf = String::new();
    for law in laws {
        buf.push_str(&json!({"ts": ts, "law": law, "stage": stage}).to_string());
        buf.push('\n');
    }
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        let _ = f.write_all(buf.as_bytes());
    }
}

/// Rows for `law` stamped at or after `since`. A missing or unreadable log
/// counts zero; a damaged row is skipped.
pub fn count_since(path: &Path, law: &str, since: chrono::DateTime<chrono::Utc>) -> usize {
    let Ok(text) = std::fs::read_to_string(path) else {
        return 0;
    };
    text.lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .filter(|r| r.get("law").and_then(Value::as_str) == Some(law))
        .filter_map(|r| {
            let ts = r.get("ts")?.as_str()?;
            chrono::DateTime::parse_from_rfc3339(ts).ok()
        })
        .filter(|ts| ts.with_timezone(&chrono::Utc) >= since)
        .count()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Duration, Utc};

    #[test]
    fn counts_only_the_named_law_since_the_cutoff() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(LOG_FILE);
        let now = Utc::now();
        record(
            &path,
            &["d-aaaaaaaa"],
            "blueprint",
            now - Duration::hours(2),
        );
        record(&path, &["d-aaaaaaaa", "d-bbbbbbbb"], "blueprint", now);
        std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap()
            .write_all(b"not json\n")
            .unwrap();
        let cutoff = now - Duration::hours(1);
        assert_eq!(count_since(&path, "d-aaaaaaaa", cutoff), 1);
        assert_eq!(count_since(&path, "d-bbbbbbbb", cutoff), 1);
        assert_eq!(
            count_since(&path.with_extension("none"), "d-aaaaaaaa", cutoff),
            0
        );
    }
}
