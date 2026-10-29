//! The merge door's green carry (x-cd18). A green CI verdict recorded at a
//! PR head is a GitHub fact that never changes once read; when the only
//! change past that head is main merging in, the recorded verdict answers
//! for the new head too. The carry is one-sided: a non-ancestor recorded
//! head, a code diff that differs, or any probe miss leaves today's hold in
//! place, so a wrong carry needs an identical code diff AND an ancestor
//! relation, both checkable before the substitution.

use crate::authorized_merge::ChecksRead;
use crate::loopcheck::git_bounded;
use serde_json::Value;
use std::path::Path;

/// Memo name prefix for one PR, the payload-code naming
/// (reviews::payload_is_code): `@` cannot occur in a GitHub owner or repo
/// name, so PR 7 of `o/r` never shares a prefix with PR 1 of `o/r-7`.
pub(crate) fn memo_prefix(slug: &str, pr: u64) -> String {
    format!("{}@{pr}@", slug.replace('/', "--"))
}

/// `origin/main`-style base qualification, the FreshnessResolver fold: a
/// bare branch name may itself contain a slash, so only the `origin/`
/// prefix marks one of ours.
pub(crate) fn qualified_base(base_ref: &str) -> String {
    let base = base_ref.trim();
    if base.is_empty() {
        "origin/main".to_string()
    } else if base.starts_with("origin/") {
        base.to_string()
    } else {
        format!("origin/{base}")
    }
}

/// The recorded green rows for one PR, newest memo file first. `memo::write`
/// supersedes a PR's older heads, so one row is the common case; corrupt or
/// foreign files are skipped: the carry only rides rows this code wrote.
fn green_rows(cwd: &Path, prefix: &str) -> Vec<(String, Value)> {
    let Some(dir) = crate::agents_config::pr_status_cache_dir(cwd)
        .map(|d| d.join(crate::pr_status::memo::GREEN_VERDICT))
    else {
        return Vec::new();
    };
    let mut rows: Vec<(std::time::SystemTime, String, Value)> = std::fs::read_dir(&dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|entry| {
            let file = entry.file_name();
            let file = file.to_string_lossy().into_owned();
            if !file.starts_with(prefix) || !file.ends_with(".json") {
                return None;
            }
            let head = file[prefix.len()..file.len() - ".json".len()].to_string();
            let row =
                serde_json::from_str::<Value>(&std::fs::read_to_string(entry.path()).ok()?).ok()?;
            let modified = entry.metadata().ok()?.modified().ok()?;
            Some((modified, head, row))
        })
        .collect();
    rows.sort_by(|a, b| b.0.cmp(&a.0));
    rows.into_iter().map(|(_, head, row)| (head, row)).collect()
}

/// One recorded green head carries to `head` when it is an ancestor and the
/// PR's code diff against the recorded base is identical at both heads.
/// The identity is `--raw` blob lines: the three-dot merge-base moves with
/// the merged-in main, so main's own content never lands on the head side
/// and a comment-only main merge reads identical, while a real push or a
/// conflict resolution changes a head blob and reads different.
fn carries(cwd: &Path, green_head: &str, head: &str, row: &Value) -> bool {
    let Some(base) = row.get("base").and_then(Value::as_str) else {
        return false;
    };
    let Some(recorded) = row.get("patch_hash").and_then(Value::as_str) else {
        return false;
    };
    let Some(out) = git_bounded(
        "git",
        &["merge-base", "--is-ancestor", green_head, head],
        cwd,
    ) else {
        return false;
    };
    if !out.status.success() {
        return false;
    }
    let Some(identity) =
        crate::review_freshness::pr_code_diff_identity("git", cwd, &qualified_base(base), head)
    else {
        return false;
    };
    identity.hash == recorded
}

/// The carry: a pending read at the new head consults the recorded green
/// heads of the same PR. Every failure mode returns the input unchanged -
/// fail-open to today's hold, never a synthesized green from a read that
/// could not answer. `github_block` stays whatever the CURRENT payload
/// named: a ruleset hold is not a CI verdict and is never carried away.
pub(crate) fn carried_checks(
    cwd: &Path,
    pr: u64,
    head: Option<&str>,
    checks: ChecksRead,
) -> ChecksRead {
    if checks.verdict != "pending" {
        return checks;
    }
    let Some(head) = head.filter(|h| !h.is_empty()) else {
        return checks;
    };
    let Some(slug) = crate::pr_status::cache::git_slug(cwd) else {
        return checks;
    };
    for (green_head, row) in green_rows(cwd, &memo_prefix(&slug, pr)) {
        if !carries(cwd, &green_head, head, &row) {
            continue;
        }
        return ChecksRead {
            verdict: "green".to_string(),
            github_block: checks.github_block,
            optional_unresolved: row
                .get("optional_unresolved")
                .map(|raw| raw.as_i64().map(Some).unwrap_or(None)),
            rerun_recovered: row.get("rerun_recovered").and_then(Value::as_bool),
            rerun_failures: row.get("rerun_failures").and_then(|raw| {
                raw.as_array().map(|rows| {
                    rows.iter()
                        .filter_map(Value::as_str)
                        .map(str::to_owned)
                        .collect()
                })
            }),
        };
    }
    checks
}

#[cfg(test)]
mod tests;
