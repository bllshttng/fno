//! The paginated REST reads behind `status-pr-list`, `status-pr-files` and
//! the `status-quota-graphql` op: the `fno.pr._rest` legs the pr package
//! deletion orphans in Python. Every gh read rides the GhProbe seam so tests
//! run offline; the row shapes are the Python functions' shapes.

use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::pr_status_facts::GhProbe;

/// The exempt `gh api rate_limit` read's GraphQL bucket: `(remaining,
/// reset-ISO)`, both None on any failure (the `_rest.graphql_remaining`
/// port). The exempt read admits nothing and spends no quota, so it runs
/// unadmitted.
pub(crate) fn graphql_remaining<P: GhProbe>(
    probe: &P,
    cwd: &Path,
) -> (Option<i64>, Option<String>) {
    let args = vec!["api".to_string(), "rate_limit".to_string()];
    let (ok, stdout, _stderr) = match probe.run_gh(cwd, &args) {
        Ok(triple) => triple,
        Err(_) => return (None, None),
    };
    if !ok {
        return (None, None);
    }
    let parsed: Value = match serde_json::from_str(&stdout) {
        Ok(v) => v,
        Err(_) => return (None, None),
    };
    let remaining = parsed
        .pointer("/resources/graphql/remaining")
        .and_then(Value::as_i64);
    let reset = parsed
        .pointer("/resources/graphql/reset")
        .and_then(Value::as_i64);
    match (remaining, reset) {
        (Some(remaining), Some(reset)) => {
            let iso = crate::pr_status::quota::iso_utc(reset);
            (Some(remaining), Some(iso))
        }
        _ => (None, None),
    }
}

/// `owner/repo` from the git origin remote, or a reason that names its
/// subject (the `_rest._slug_or_reason` port; the shared resolver is the
/// pr_info one).
pub(crate) fn slug_or_reason(cwd: Option<&Path>, repo: Option<&str>) -> Result<String, String> {
    if let Some(repo) = repo.filter(|r| !r.is_empty()) {
        return Ok(repo.to_string());
    }
    let resolved = cwd_or_dot(cwd);
    match crate::backlog::pr_link::resolve_current_repo_slug(resolved.to_str()) {
        Some(slug) if !slug.is_empty() => Ok(slug),
        _ => Err("could not resolve owner/repo from the origin remote".to_string()),
    }
}

/// The current branch's PR number over the REST pulls-list read (the
/// `_rest.resolve_current_pr_number_rest` port). The gh read rides the
/// real probe; the git branch read is local.
pub(crate) fn resolve_current_pr_number(
    cwd: Option<&Path>,
    repo: Option<&str>,
) -> Result<u64, String> {
    let slug = slug_or_reason(cwd, repo)?;
    let resolved = cwd_or_dot(cwd);
    if !slug.contains('/') {
        return Err(format!("not an owner/repo slug: {slug}"));
    }
    let branch = std::process::Command::new("git")
        .args(["branch", "--show-current"])
        .current_dir(&resolved)
        .output()
        .map_err(|e| format!("could not resolve the current git branch: {e}"))?;
    if !branch.status.success() {
        return Err("could not resolve the current git branch".to_string());
    }
    let branch = String::from_utf8_lossy(&branch.stdout).trim().to_string();
    if branch.is_empty() {
        return Err("could not resolve the current git branch".to_string());
    }
    let owner = slug.split('/').next().unwrap_or_default();
    let probe = crate::pr_status_facts::RealGhProbe;
    let (ok, stdout, stderr) = probe
        .run_gh(
            &resolved,
            &[
                "api".to_string(),
                format!("repos/{slug}/pulls?state=all&head={owner}:{branch}&per_page=2"),
            ],
        )
        .map_err(|e| format!("gh spawn failed: {e}"))?;
    if !ok {
        return Err(crate::pr_status::rest_reason(&probe, &resolved, &stderr).text);
    }
    let payload: Value = serde_json::from_str(&stdout)
        .map_err(|e| format!("gh api pulls list returned unparseable output: {e}"))?;
    let rows = payload.as_array().ok_or_else(|| {
        "gh api pulls list returned a JSON value that is not an array".to_string()
    })?;
    let number = rows
        .first()
        .and_then(|row| row.get("number"))
        .and_then(Value::as_u64);
    number.ok_or_else(|| format!("no pull requests found for branch {branch}"))
}

fn cwd_or_dot(cwd: Option<&Path>) -> PathBuf {
    cwd.map(PathBuf::from)
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")))
}

/// The paginated pulls listing (`_rest.list_prs_rest` port): rows reduced to
/// {number, state} through the shared mapper. `Err` is a loud refusal, never
/// an absent answer; the bool rides true when the ceiling cut a full last
/// page (the Python leg logged a warning for the same condition).
pub(crate) fn list_pulls<P: GhProbe>(
    probe: &P,
    cwd: &Path,
    slug: &str,
    state: &str,
    per_page: usize,
    max_pages: usize,
) -> Result<(Vec<Value>, bool), String> {
    let mut rows: Vec<Value> = Vec::new();
    for page in 1..=max_pages {
        let (ok, stdout, stderr) = probe
            .run_gh(
                cwd,
                &[
                    "api".to_string(),
                    format!("repos/{slug}/pulls?state={state}&per_page={per_page}&page={page}"),
                ],
            )
            .map_err(|e| format!("gh spawn failed: {e}"))?;
        if !ok {
            return Err(crate::pr_status::rest_reason(probe, cwd, &stderr).text);
        }
        let payload: Value = serde_json::from_str(&stdout).map_err(|e| {
            format!("gh api pulls list page {page} returned unparseable output: {e}")
        })?;
        let Some(rows_page) = payload.as_array() else {
            return Err(format!(
                "gh api pulls list page {page} was not a JSON array"
            ));
        };
        for row in rows_page {
            let Some(number) = row.get("number").and_then(Value::as_u64) else {
                continue;
            };
            let state_word = crate::pr_status::map_pr_state(row);
            rows.push(json!({
                "number": number,
                "state": state_word,
            }));
        }
        if rows_page.len() < per_page {
            return Ok((rows, false));
        }
    }
    Ok((rows, true))
}

/// The paginated changed-file paths read (`_rest.fetch_pr_file_paths_rest`
/// port): up to 30 pages of 100, fail closed at GitHub's 3,000-file cap.
pub(crate) fn pr_file_paths<P: GhProbe>(
    probe: &P,
    cwd: &Path,
    slug: &str,
    pr: u64,
) -> Result<Vec<String>, String> {
    let mut paths: Vec<String> = Vec::new();
    for page in 1..=30 {
        let (ok, stdout, stderr) = probe
            .run_gh(
                cwd,
                &[
                    "api".to_string(),
                    format!("repos/{slug}/pulls/{pr}/files?per_page=100&page={page}"),
                ],
            )
            .map_err(|e| format!("gh spawn failed: {e}"))?;
        if !ok {
            return Err(crate::pr_status::rest_reason(probe, cwd, &stderr).text);
        }
        let payload: Value = serde_json::from_str(&stdout).map_err(|e| {
            format!("gh api pull files page {page} returned unparseable output: {e}")
        })?;
        let Some(rows_page) = payload.as_array() else {
            return Err(format!(
                "gh api pull files page {page} returned a value that is not an array"
            ));
        };
        let mut page_paths: Vec<String> = Vec::new();
        for (index, row) in rows_page.iter().enumerate() {
            match row
                .get("filename")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
            {
                Some(filename) => page_paths.push(filename.to_string()),
                None => {
                    return Err(format!(
                        "gh api pull files page {page} carried malformed row {index}"
                    ));
                }
            }
        }
        if rows_page.len() < 100 {
            let mut all = paths;
            all.extend(page_paths);
            return Ok(all);
        }
        paths.extend(page_paths);
    }
    Err("gh api pull files reached GitHub's 3,000-file cap without a short page".to_string())
}

/// The `status-pr-files` op receipt: every changed-file path for one PR,
/// resolved from cwd when the payload carries no slug.
pub(crate) fn files_op(payload: &Value) -> Value {
    let Some(pr) = payload.get("pr").and_then(Value::as_u64) else {
        return json!({"error": "status-pr-files needs a pr"});
    };
    let cwd = PathBuf::from(payload.get("cwd").and_then(Value::as_str).unwrap_or("."));
    let slug = match payload
        .get("slug")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
    {
        Some(slug) => slug.to_string(),
        None => match crate::backlog::pr_link::resolve_current_repo_slug(cwd.to_str()) {
            Some(slug) => slug,
            None => {
                return json!({"error": "status-pr-files cannot resolve the repo slug from cwd"})
            }
        },
    };
    let probe = crate::pr_status_facts::RealGhProbe;
    match pr_file_paths(&probe, &cwd, &slug, pr) {
        Ok(paths) => json!({"paths": paths}),
        Err(reason) => json!({"error": reason}),
    }
}

/// The `status-pr-list` op receipt: one paginated open-PR listing per repo,
/// the sweep's shape ({number, state} rows, a truncated marker at the
/// ceiling).
pub(crate) fn list_op(payload: &Value) -> String {
    let Some(slug) = payload.get("slug").and_then(Value::as_str) else {
        return json!({"error": "status-pr-list needs a slug"}).to_string();
    };
    let cwd = PathBuf::from(payload.get("cwd").and_then(Value::as_str).unwrap_or("."));
    let state = payload
        .get("state")
        .and_then(Value::as_str)
        .unwrap_or("open");
    let per_page = payload
        .get("per_page")
        .and_then(Value::as_u64)
        .unwrap_or(100) as usize;
    let max_pages = payload
        .get("max_pages")
        .and_then(Value::as_u64)
        .unwrap_or(20) as usize;
    let probe = crate::pr_status_facts::RealGhProbe;
    match list_pulls(&probe, &cwd, slug, state, per_page, max_pages) {
        Ok((rows, truncated)) => {
            let mut out = json!({"rows": rows, "truncated": truncated});
            if let Some(details) = payload
                .get("details")
                .and_then(Value::as_bool)
                .filter(|d| *d)
            {
                out["details"] = json!(details);
            }
            out.to_string()
        }
        Err(reason) => json!({"error": reason}).to_string(),
    }
}
