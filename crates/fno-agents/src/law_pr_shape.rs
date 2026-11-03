//! The PR-shape facts a rule filter reads: the labels on the session's PR and
//! its size in changed lines.
//!
//! A law read never calls GitHub. The `pr info` command already reads the
//! pulls body, so it stamps one row per branch here (`stamp`). The law reader
//! only reads that row (`session_shape`). A missing, corrupt or stale row is
//! `None`, and the filter then fails open like every other unanswerable fact.

use serde_json::{json, Value};
use std::path::Path;

const KIND: &str = "pr-shape";

/// A stamped row ages out after an hour. A push changes the size, and the
/// next `pr info` read restamps it, so the window only bounds how long an
/// unread PR keeps answering.
pub const TTL_S: u64 = 3600;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrShape {
    pub labels: Vec<String>,
    pub lines: u64,
}

fn branch_key(slug: &str, head_ref: &str) -> String {
    format!("{slug}@{head_ref}")
}

/// The shape of one pulls body: its label names and additions plus deletions.
pub fn shape_of(pulls: &Value) -> PrShape {
    let labels = pulls
        .get("labels")
        .and_then(Value::as_array)
        .map(|all| {
            all.iter()
                .filter_map(|l| l.get("name").and_then(Value::as_str))
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    let count = |key: &str| pulls.get(key).and_then(Value::as_u64).unwrap_or(0);
    PrShape {
        labels,
        lines: count("additions") + count("deletions"),
    }
}

/// Stamp the shape of the PR whose head is `head_ref`. Best effort: a write
/// that fails costs the filter its answer, never the caller its read.
pub fn stamp(cwd: &Path, slug: &str, head_ref: &str, pulls: &Value) {
    let Some(root) = crate::gh_cache::rows_root(cwd) else {
        return;
    };
    let shape = shape_of(pulls);
    let row = json!({"labels": shape.labels, "lines": shape.lines});
    crate::gh_cache::write_row(&root, KIND, &branch_key(slug, head_ref), None, &row);
}

fn read_shape(root: &Path, slug: &str, head_ref: &str, now: f64) -> Option<PrShape> {
    let hit = crate::gh_cache::read_row(
        root,
        KIND,
        &branch_key(slug, head_ref),
        None,
        Some(TTL_S),
        now,
    );
    let row = hit.get("row").filter(|r| r.is_object())?;
    Some(PrShape {
        labels: row
            .get("labels")?
            .as_array()?
            .iter()
            .filter_map(|l| l.as_str().map(str::to_string))
            .collect(),
        lines: row.get("lines")?.as_u64()?,
    })
}

/// The shape of the PR on `branch`, from the stamped row. No network read.
pub fn session_shape(cwd: &Path, branch: &str) -> Option<PrShape> {
    let root = crate::gh_cache::rows_root(cwd)?;
    let url = crate::finalize::git_capture(cwd, &["config", "--get", "remote.origin.url"])?;
    let slug = crate::backlog::pr_link::slug_from_remote(&url)?;
    read_shape(&root, &slug, branch, crate::gh_cache::now_secs())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pulls() -> Value {
        json!({
            "labels": [{"name": "paint"}, {"name": "ui"}],
            "additions": 120,
            "deletions": 30,
        })
    }

    #[test]
    fn shape_counts_additions_plus_deletions_and_names_every_label() {
        let shape = shape_of(&pulls());
        assert_eq!(shape.lines, 150);
        assert_eq!(shape.labels, vec!["paint", "ui"]);
        assert_eq!(shape_of(&json!({})).lines, 0);
    }

    #[test]
    fn a_stamped_row_reads_back_and_a_stale_or_absent_one_does_not() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let row = json!({"labels": ["paint"], "lines": 150});
        assert!(crate::gh_cache::write_row(
            root,
            KIND,
            &branch_key("o/r", "feature/x"),
            None,
            &row
        ));
        let now = crate::gh_cache::now_secs();
        let got = read_shape(root, "o/r", "feature/x", now).unwrap();
        assert_eq!(got.lines, 150);
        assert_eq!(got.labels, vec!["paint"]);
        assert!(read_shape(root, "o/r", "feature/y", now).is_none());
        assert!(read_shape(root, "o/r", "feature/x", now + TTL_S as f64 + 5.0).is_none());
    }
}
