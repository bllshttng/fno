//! GitHub facts that never change once read, kept on disk beside the status
//! cache so a refresh does not buy them again. A PR's file list is fixed at
//! one head, and a finished check run's annotations are fixed for good. Each
//! status refresh re-read both, and on 2026-10-22 the two reads were 41% of
//! the fleet's core spend.
//!
//! Each kind lives in its own subdirectory. The status cache prunes every
//! `{slug}-{pr}-` file in its own directory and never recurses, so a memo
//! there would be deleted on the next status write. The state sweep reaps
//! each kind's directory on the status cache's retention window.

use serde_json::Value;
use std::path::{Path, PathBuf};

pub(crate) const PAYLOAD_CODE: &str = "payload-code";
pub(crate) const RUN_ANNOTATIONS: &str = "run-annotations";
pub(crate) const KINDS: [&str; 2] = [PAYLOAD_CODE, RUN_ANNOTATIONS];

fn memo_path(cwd: &Path, kind: &str, name: &str) -> Option<PathBuf> {
    let dir = crate::agents_config::pr_status_cache_dir(cwd)?.join(kind);
    Some(dir.join(format!("{name}.json")))
}

/// The stored value, or None. A missing or corrupt memo is a miss, never an
/// error: the network read is the truth.
pub(crate) fn read(cwd: &Path, kind: &str, name: &str) -> Option<Value> {
    let text = std::fs::read_to_string(memo_path(cwd, kind, name)?).ok()?;
    serde_json::from_str(&text).ok()
}

/// Whether any memo of `kind` whose name starts with `prefix` holds `true`.
pub(crate) fn any_true(cwd: &Path, kind: &str, prefix: &str) -> bool {
    let Some(path) = memo_path(cwd, kind, prefix) else {
        return false;
    };
    let Some(dir) = path.parent() else {
        return false;
    };
    std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .any(|entry| {
            let file = entry.file_name();
            let file = file.to_string_lossy();
            file.starts_with(prefix)
                && file.ends_with(".json")
                && std::fs::read_to_string(entry.path())
                    .ok()
                    .and_then(|t| serde_json::from_str::<Value>(&t).ok())
                    == Some(Value::Bool(true))
        })
}

/// Store `value`, then delete the kind's other files that start with
/// `superseded` (an older head of the same PR). A failed write is ignored:
/// the next refresh reads the network again.
pub(crate) fn write(cwd: &Path, kind: &str, name: &str, value: &Value, superseded: Option<&str>) {
    let Some(path) = memo_path(cwd, kind, name) else {
        return;
    };
    let Some(dir) = path.parent() else {
        return;
    };
    if std::fs::create_dir_all(dir).is_err() {
        return;
    }
    let tmp = path.with_extension(format!("{}.tmp", std::process::id()));
    if std::fs::write(&tmp, value.to_string()).is_err() || std::fs::rename(&tmp, &path).is_err() {
        let _ = std::fs::remove_file(&tmp);
        return;
    }
    let Some(prefix) = superseded else {
        return;
    };
    let keep = format!("{name}.json");
    for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let file = entry.file_name();
        let file = file.to_string_lossy();
        if file.starts_with(prefix) && file.ends_with(".json") && file != keep {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_memo_round_trips_and_a_new_head_prunes_the_old_one() {
        let _guard = crate::pr_status::cache_env_lock();
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        std::env::set_var("FNO_PR_STATUS_CACHE_DIR", dir);
        let cwd = Path::new(".");
        assert!(read(cwd, PAYLOAD_CODE, "o--r@7@aaa").is_none());
        write(
            cwd,
            PAYLOAD_CODE,
            "o--r@7@aaa",
            &json!(true),
            Some("o--r@7@"),
        );
        write(
            cwd,
            PAYLOAD_CODE,
            "o--r@70@aaa",
            &json!(true),
            Some("o--r@70@"),
        );
        assert_eq!(read(cwd, PAYLOAD_CODE, "o--r@7@aaa"), Some(json!(true)));
        write(
            cwd,
            PAYLOAD_CODE,
            "o--r@7@bbb",
            &json!(true),
            Some("o--r@7@"),
        );
        assert!(read(cwd, PAYLOAD_CODE, "o--r@7@aaa").is_none());
        assert_eq!(read(cwd, PAYLOAD_CODE, "o--r@7@bbb"), Some(json!(true)));
        assert!(any_true(cwd, PAYLOAD_CODE, "o--r@7@"));
        assert!(!any_true(cwd, PAYLOAD_CODE, "o--r@8@"));
        // PR 70 shares the "o--r@7" text but not the "o--r@7@" prefix.
        assert_eq!(read(cwd, PAYLOAD_CODE, "o--r@70@aaa"), Some(json!(true)));
        std::fs::write(dir.join("payload-code/o--r@7@bbb.json"), "{not json").unwrap();
        assert!(read(cwd, PAYLOAD_CODE, "o--r@7@bbb").is_none());
        std::env::remove_var("FNO_PR_STATUS_CACHE_DIR");
    }
}
