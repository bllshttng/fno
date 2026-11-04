//! The `status-pr-info` op: the Python `fno do pr info` read, ported beside
//! its REST twins. The command's read is one REST pulls fetch; a refusal
//! classified CORE (the budget drained, not the request rate) hands the same
//! selector to `read_pr_view_fields`, whose brokered GraphQL fallback is the
//! stop gate's own machinery, and the reply maps onto the REST payload keys
//! so every consumer reads one shape whichever transport served it. The
//! Python command is a bare forward to this op: three net lines in cli/src/fno
//! carrying the ask, the exit codes, and no transport decision.
//!
//! On the CORE arm the payload also takes the two zero-REST-quota legs: the
//! head sha cross-checked (or served whole) from git over
//! `refs/pull/<n>/head`, and the changed-file list from a git fetch + a
//! merge-base diff. The field set for the fallback read adds
//! `statusCheckRollup`, so the ONE brokered GraphQL read also answers check
//! states (one targeted query, never a second). Every served
//! field names its source in `sources`, and every unserved field names its
//! reason in `missing` - no blank field without a reason.

use crate::pr_status::{map_mergeable, map_pr_state, rest_reason, RestReason};
use crate::pr_status_facts::{GhProbe, RealGhProbe};
use serde_json::{json, Value};
use std::path::Path;
use std::process::Command;
use std::time::Duration;

/// Per-spawn bound for the fallback read, the stop gate's own read bound.
/// `read_pr_view_fields` can spend up to three spawns (primary read, quota
/// probe, fallback retry), so the command forward's timeout must cover the
/// sum, not one spawn.
const FALLBACK_READ_TIMEOUT: Duration = Duration::from_secs(30);

/// The `status-pr-info` op receipt. `error` present means the read refused:
/// the command forward maps it to exit 4, and the payload's info keys stay
/// absent. Success answers the REST payload's keys, whichever transport
/// served the read.
pub(crate) fn run_op(payload: &Value) -> Value {
    let cwd_str = payload.get("cwd").and_then(Value::as_str).unwrap_or(".");
    let cwd = Path::new(cwd_str);
    let pr_arg = payload.get("pr").and_then(Value::as_u64);
    let repo = payload.get("repo").and_then(Value::as_str);

    if pr_arg == Some(0) {
        return json!({"pr": null, "error": "PR number must be positive"});
    }
    // One slug for every leg: the REST reads name it in their api paths and
    // the fallback pins it with --repo, so a fork checkout (an upstream
    // remote gh would rather pick) reads the same repository end to end.
    let slug = match resolve_slug(cwd, repo) {
        Ok(slug) => slug,
        Err(reason) => {
            return json!({"pr": pr_arg, "error": reason.text});
        }
    };

    let pr = match resolve_pr(pr_arg, cwd, &slug) {
        Ok(Some(pr)) => pr,
        Ok(None) => {
            return json!({
                "pr": null,
                "error": "no pull request exists for the current branch",
            });
        }
        Err(reason) => {
            // The resolve read hit the drained CORE budget: the branch
            // resolution itself moves to the GraphQL transport (the adapter
            // resolves the current branch when no selector is passed), and a
            // fallback that answers nothing hands back the REST refusal with
            // the fallback's cause appended.
            if reason.rate_limit_class == "core" {
                match fallback_payload(cwd, None, Some(&slug)) {
                    Ok(Some(info)) => return info,
                    // The fallback answering "no such PR" is the real
                    // answer: nothing rides on the drained REST read.
                    Ok(None) => {
                        return json!({
                            "pr": null,
                            "error": "no pull request exists for the current branch",
                        });
                    }
                    Err(cause) => {
                        return json!({
                            "pr": null,
                            "error": append_fallback_cause(&reason.text, cause),
                        });
                    }
                }
            }
            return json!({"pr": null, "error": reason.text});
        }
    };

    match fetch_rest(pr, cwd, &slug) {
        Ok(info) => info,
        Err(reason) => {
            if reason.rate_limit_class == "core" {
                match fallback_payload(cwd, Some(pr), Some(&slug)) {
                    Ok(Some(info)) => return info,
                    // The fallback answering "no such PR" is the real
                    // answer about this number.
                    Ok(None) => {
                        return json!({
                            "pr": pr,
                            "error": "no pull request matches the selector",
                        });
                    }
                    Err(cause) => {
                        return json!({
                            "pr": pr,
                            "error": append_fallback_cause(&reason.text, cause),
                        });
                    }
                }
            }
            json!({"pr": pr, "error": reason.text})
        }
    }
}

/// The command receipt's error text when the fallback also failed: the ORIGINAL
/// REST refusal first (its rate-limit wording is what downstream
/// classifiers read; the broker's own prose carries neither the marker nor a
/// park trigger), the fallback's cause appended so the operator sees that a
/// second transport ran and why it answered nothing.
fn append_fallback_cause(rest_reason_text: &str, cause: Option<String>) -> String {
    match cause {
        Some(cause) => format!("{rest_reason_text} | graphql fallback: {cause}"),
        None => rest_reason_text.to_string(),
    }
}

/// The numeric selector, else the current branch's PR over the REST
/// pulls-list read the Python resolver used. `Ok(None)` is the real-world
/// no-PR state; every refusal is classified, never swallowed.
fn resolve_pr(pr: Option<u64>, cwd: &Path, slug: &str) -> Result<Option<u64>, RestReason> {
    if let Some(pr) = pr {
        return Ok(Some(pr));
    }
    let branch = Command::new("git")
        .args(["branch", "--show-current"])
        .current_dir(cwd)
        .output()
        .map_err(|e| RestReason::new(format!("git branch read failed: {e}"), ""))?;
    if !branch.status.success() {
        return Err(RestReason::new("git branch read failed", ""));
    }
    let branch = String::from_utf8_lossy(&branch.stdout).trim().to_string();
    if branch.is_empty() {
        return Err(RestReason::new(
            "no current branch (detached HEAD), so no pull request to resolve",
            "",
        ));
    }
    let owner = slug.split('/').next().unwrap_or_default().to_string();
    let probe = RealGhProbe;
    let (ok, stdout, stderr) = probe
        .run_gh(
            cwd,
            &[
                "api".to_string(),
                format!("repos/{slug}/pulls?state=all&head={owner}:{branch}&per_page=2"),
            ],
        )
        .map_err(|e| RestReason::new(format!("gh spawn failed: {e}"), ""))?;
    if !ok {
        return Err(rest_reason(&probe, cwd, &stderr));
    }
    let rows: Value = serde_json::from_str(&stdout)
        .map_err(|e| RestReason::new(format!("pulls list returned unparseable output: {e}"), ""))?;
    let Some(rows) = rows.as_array() else {
        return Err(RestReason::new(
            "pulls list returned a JSON value that is not an array",
            "",
        ));
    };
    Ok(rows
        .first()
        .and_then(|row| row.get("number"))
        .and_then(Value::as_u64))
}

/// One REST pulls read mapped onto the command payload's keys, the byte shape
/// `fetch_pr_info_rest` returned. Every refusal is classified
/// (`rest_reason`), so the caller knows CORE from everything else.
fn fetch_rest(pr: u64, cwd: &Path, slug: &str) -> Result<Value, RestReason> {
    let probe = RealGhProbe;
    let (ok, stdout, stderr) = probe
        .run_gh(
            cwd,
            &["api".to_string(), format!("repos/{slug}/pulls/{pr}")],
        )
        .map_err(|e| RestReason::new(format!("gh spawn failed: {e}"), ""))?;
    if !ok {
        return Err(rest_reason(&probe, cwd, &stderr));
    }
    let pulls: Value = serde_json::from_str(&stdout).map_err(|e| {
        RestReason::new(
            format!("gh api pulls/<n> returned unparseable output: {e}"),
            "",
        )
    })?;
    let info = rest_payload(pr, &pulls)
        .ok_or_else(|| RestReason::new("gh api pulls/<n> carried a malformed body", ""))?;
    if let Some(head_ref) = info.get("head_ref").and_then(Value::as_str) {
        crate::law_pr_shape::stamp(cwd, slug, head_ref, &pulls);
    }
    Ok(info)
}

fn resolve_slug(cwd: &Path, repo: Option<&str>) -> Result<String, RestReason> {
    repo.map(str::to_string)
        .or_else(|| crate::backlog::pr_link::resolve_current_repo_slug(cwd.to_str()))
        .ok_or_else(|| RestReason::new("could not resolve owner/repo from the origin remote", ""))
}

/// The fallback read's field set: the info fallback's set plus
/// `statusCheckRollup`, so the ONE brokered GraphQL read the CORE arm is
/// allowed also answers check states (one targeted query, never a second).
/// `concat!` takes literals only, so the superset is spelled out; the drift
/// test below pins it against [`crate::loopcheck::PR_INFO_FALLBACK_FIELDS`].
const PR_INFO_SLICE_FIELDS: &str = "state,number,headRefName,headRefOid,mergeable,mergeStateStatus,baseRefName,author,url,body,mergedAt,mergeCommit,autoMergeRequest,statusCheckRollup";

/// The changed-file cap the git leg answers with; a longer list truncates
/// and the payload says so (`files_truncated`), never silently.
const FILES_CAP: usize = 500;

/// The per-check-row cap inside `checks.rows`; the count lives in `counts`.
const CHECK_ROWS_CAP: usize = 20;

/// The fallback: the same selector through `read_pr_view_fields`, whose
/// brokered GraphQL fallback this op borrows wholesale - the earned checks,
/// the quota probe, and the bounded spawns are the stop gate's, not a second
/// impl. The field set is the op's own plus `statusCheckRollup`
/// ([`PR_INFO_SLICE_FIELDS`]), and the slug the REST legs read pins the
/// repository with --repo, so gh's own remote priority cannot pick a
/// different one. The primary read inside it re-attempts the REST fetch that
/// just refused; that costs one refused request and buys one owner for the
/// whole transport decision. The binary resolves as the stop gate's does, so
/// a test injects a fake the same way. `Err(cause)` when the fallback
/// answered nothing and the caller appends the cause to the REST refusal it
/// keeps; `Ok(None)` when the fallback answered "no such PR", which is a
/// real answer, not a failure.
fn fallback_payload(
    cwd: &Path,
    pr: Option<u64>,
    repo: Option<&str>,
) -> Result<Option<Value>, Option<String>> {
    let gh_bin =
        std::env::var("FNO_LOOPCHECK_GH_BIN").unwrap_or_else(|_| "fno-gh-loopcheck".into());
    let selector = pr.map(|n| n.to_string());
    let read = crate::loopcheck::read_pr_view_fields(
        &gh_bin,
        cwd,
        selector.as_deref(),
        PR_INFO_SLICE_FIELDS,
        repo,
        FALLBACK_READ_TIMEOUT,
    );
    assemble_fallback(cwd, pr, read)
}

/// The CORE arm's payload assembly over the fallback read's outcome, split
/// from the spawn so a test drives it with a fabricated read. On a served
/// reply the GraphQL payload takes the checks summary, then the git legs:
/// the head sha is cross-checked against `refs/pull/<n>/head` (git's answer
/// wins and names its source), and the changed-file list comes from a git
/// fetch + merge-base diff. On a refused reply the git legs answer alone -
/// a partial payload (`partial: true`) with `head_sha` from git when the
/// ref resolves, `files` when the diff does, and every unserved field named
/// with its reason in `missing`. `Ok(None)` stays the real "no such PR"
/// answer; `Err(cause)` means even git answered nothing.
fn assemble_fallback(
    cwd: &Path,
    pr: Option<u64>,
    read: Result<Option<Value>, crate::loopcheck::GhReadError>,
) -> Result<Option<Value>, Option<String>> {
    let urls = remote_urls(cwd);
    match read {
        Ok(None) => Ok(None),
        Ok(Some(gh)) => {
            let mut info = match graphql_payload(&gh) {
                Some(info) => info,
                None => return Err(Some("the reply carried a malformed body".to_string())),
            };
            let mut sources = json!({});
            let mut missing = serde_json::Map::new();
            match checks_from_rollup(&gh) {
                Some(checks) => {
                    info["checks"] = checks;
                    sources["checks"] = json!("graphql statusCheckRollup");
                }
                None => {
                    missing.insert(
                        "checks".to_string(),
                        json!("the fallback reply carried no statusCheckRollup array"),
                    );
                }
            }
            if let Some(pr) = pr {
                if let Some(sha) = git_head_sha(cwd, pr, &urls) {
                    info["head_sha"] = json!(sha);
                    sources["head_sha"] = json!(format!("git refs/pull/{pr}/head"));
                }
                match git_files(cwd, pr, &urls, info.get("base_ref").and_then(Value::as_str)) {
                    Ok((files, truncated, base)) => {
                        info["files"] = json!(files);
                        if truncated {
                            info["files_truncated"] = json!(true);
                        }
                        sources["files"] = json!(format!("git diff {base}...refs/pull/{pr}/head"));
                    }
                    Err(reason) => {
                        missing.insert("files".to_string(), json!(reason));
                    }
                }
            }
            if !missing.is_empty() {
                info["missing"] = Value::Object(missing);
            }
            let served_git = !sources
                .as_object()
                .map(|map| map.is_empty())
                .unwrap_or(true);
            if served_git {
                info["sources"] = sources;
            }
            Ok(Some(info))
        }
        Err(err) => {
            let cause = err.cause_line();
            let Some(pr) = pr else {
                return Err(Some(cause));
            };
            let sha = git_head_sha(cwd, pr, &urls);
            let files = git_files(cwd, pr, &urls, None);
            if sha.is_none() && !matches!(&files, Ok(_)) {
                return Err(Some(cause));
            }
            let missing_reason = format!("graphql fallback failed: {cause}");
            let mut missing = serde_json::Map::new();
            for field in [
                "url",
                "body",
                "state",
                "head_ref",
                "base_ref",
                "mergeable",
                "merge_state_status",
                "merged_at",
                "merge_sha",
                "author",
                "auto_merge",
                "checks",
            ] {
                missing.insert(field.to_string(), json!(missing_reason));
            }
            let mut sources = serde_json::Map::new();
            let mut info = json!({
                "pr": pr,
                "partial": true,
                "transport": "git",
            });
            if let Some(sha) = sha {
                info["head_sha"] = json!(sha);
                sources.insert(
                    "head_sha".to_string(),
                    json!(format!("git refs/pull/{pr}/head")),
                );
                missing.remove("head_sha");
            }
            match files {
                Ok((files, truncated, base)) => {
                    info["files"] = json!(files);
                    if truncated {
                        info["files_truncated"] = json!(true);
                    }
                    sources.insert(
                        "files".to_string(),
                        json!(format!("git diff {base}...refs/pull/{pr}/head")),
                    );
                }
                Err(reason) => {
                    missing.insert("files".to_string(), json!(reason));
                }
            }
            info["missing"] = Value::Object(missing);
            info["sources"] = Value::Object(sources);
            Ok(Some(info))
        }
    }
}

/// The remote URLs the git legs read: origin, then upstream (a fork checkout
/// convention). A checkout with neither has no git leg.
fn remote_urls(cwd: &Path) -> Vec<String> {
    ["origin", "upstream"]
        .iter()
        .filter_map(|remote| {
            Command::new("git")
                .args(["remote", "get-url", remote])
                .current_dir(cwd)
                .output()
                .ok()
                .filter(|out| out.status.success())
                .map(|out| String::from_utf8_lossy(&out.stdout).trim().to_string())
                .filter(|url| !url.is_empty())
        })
        .collect()
}

/// The PR head sha from git alone: `git ls-remote <url> refs/pull/<n>/head`
/// costs no REST quota. The first URL whose remote carries the ref wins.
fn git_head_sha(cwd: &Path, pr: u64, urls: &[String]) -> Option<String> {
    let want = format!("refs/pull/{pr}/head");
    for url in urls {
        let Some(out) = Command::new("git")
            .args(["ls-remote", url, &want])
            .current_dir(cwd)
            .output()
            .ok()
        else {
            continue;
        };
        if !out.status.success() {
            continue;
        }
        let line = String::from_utf8_lossy(&out.stdout);
        if let Some(first) = line.lines().next() {
            let sha = first.split('\t').next().unwrap_or("").trim().to_string();
            if sha.len() == 40 && sha.chars().all(|c| c.is_ascii_hexdigit()) {
                return Some(sha);
            }
        }
    }
    None
}

/// The changed-file list from git alone: fetch `refs/pull/<n>/head` into
/// FETCH_HEAD (a transient scratch file, never a ref), then a three-dot
/// name-only diff against the PR's base branch - `origin/<base>` when the
/// checkout has it, else `origin/main`. `Err` names what the leg lacked.
fn git_files(
    cwd: &Path,
    pr: u64,
    urls: &[String],
    base_ref: Option<&str>,
) -> Result<(Vec<String>, bool, String), String> {
    let url = urls.first().ok_or_else(|| {
        "no origin or upstream remote, so the git leg cannot read the PR ref".to_string()
    })?;
    let spec = format!("refs/pull/{pr}/head");
    let fetch = Command::new("git")
        .args(["fetch", "--no-tags", url, &spec])
        .current_dir(cwd)
        .output()
        .map_err(|e| format!("git fetch failed to spawn: {e}"))?;
    if !fetch.status.success() {
        return Err(format!(
            "git fetch of {spec} failed: {}",
            String::from_utf8_lossy(&fetch.stderr).trim()
        ));
    }
    let base = base_ref
        .and_then(|name| local_base(cwd, name))
        .or_else(|| local_base(cwd, "main"))
        .ok_or_else(|| "no local origin/<base> or origin/main to diff against".to_string())?;
    let diff = Command::new("git")
        .args(["diff", "--name-only", &format!("{base}...FETCH_HEAD")])
        .current_dir(cwd)
        .output()
        .map_err(|e| format!("git diff failed to spawn: {e}"))?;
    if !diff.status.success() {
        return Err(format!(
            "git diff against {base} failed: {}",
            String::from_utf8_lossy(&diff.stderr).trim()
        ));
    }
    let mut files: Vec<String> = String::from_utf8_lossy(&diff.stdout)
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(str::to_string)
        .collect();
    let truncated = files.len() > FILES_CAP;
    files.truncate(FILES_CAP);
    Ok((files, truncated, base))
}

/// `origin/<name>` when the checkout tracks that branch locally.
fn local_base(cwd: &Path, name: &str) -> Option<String> {
    let candidate = format!("origin/{name}");
    let out = Command::new("git")
        .args(["rev-parse", "--verify", "--quiet", &candidate])
        .current_dir(cwd)
        .output()
        .ok()?;
    if out.status.success() && !out.stdout.is_empty() {
        Some(candidate)
    } else {
        None
    }
}

/// The checks summary from the fallback reply's `statusCheckRollup`: the
/// status read's own classifiers (generic-CI filter, supersession, verdict)
/// over the same array, plus per-check rows capped at [`CHECK_ROWS_CAP`].
/// `None` when the reply carried no rollup array at all - GitHub's own empty
/// rollup answers with `verdict: "unknown"`, which is a real answer.
fn checks_from_rollup(gh: &Value) -> Option<Value> {
    let rollup = gh.get("statusCheckRollup")?.as_array()?;
    let generic = crate::pr_status::generic_ci_rollup(rollup);
    let (verdict, _code, counts) = crate::pr_status::verdict_for(&generic);
    // Rows read the same deduped set the counts read: supersession keeps the
    // latest row per name, so a stale ci row never sits beside its rerun.
    let deduped = crate::check_supersession::latest_per_name(&Value::Array(generic.clone()));
    let deduped = deduped.as_array().cloned().unwrap_or_default();
    let rows: Vec<Value> = deduped
        .iter()
        .take(CHECK_ROWS_CAP)
        .map(|check| {
            let name = {
                let n = check.get("name").and_then(Value::as_str).unwrap_or("");
                if n.is_empty() {
                    check.get("context").and_then(Value::as_str).unwrap_or("")
                } else {
                    n
                }
            };
            let status = check.get("status").and_then(Value::as_str).unwrap_or("");
            let conclusion = check
                .get("conclusion")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .map(str::to_uppercase)
                .or_else(|| {
                    Some(
                        check
                            .get("state")
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .to_uppercase(),
                    )
                })
                .unwrap_or_default();
            let state = if !conclusion.is_empty() {
                conclusion
            } else if !status.is_empty() && !status.eq_ignore_ascii_case("COMPLETED") {
                "PENDING".to_string()
            } else {
                "UNKNOWN".to_string()
            };
            json!({"name": name, "state": state})
        })
        .collect();
    Some(json!({
        "verdict": verdict,
        "counts": counts,
        "rows": rows,
        "rows_truncated": deduped.len() > CHECK_ROWS_CAP,
    }))
}

/// The REST pulls body mapped onto the command payload's keys, the twin of
/// `fetch_pr_info_rest`'s return: state and mergeability through the shared
/// mappers, the head sha load-bearing, auto_merge passed through whole.
fn rest_payload(pr: u64, pulls: &Value) -> Option<Value> {
    let head = pulls.get("head")?;
    let base = pulls.get("base")?;
    let sha = head.get("sha")?.as_str()?;
    if sha.is_empty() {
        return None;
    }
    let head_ref = head.get("ref")?.as_str()?;
    let base_ref = base.get("ref")?.as_str()?;
    let merged_at = pulls.get("merged_at").and_then(Value::as_str);
    let merge_sha = pulls.get("merge_commit_sha").and_then(Value::as_str);
    Some(json!({
        "pr": pr,
        "url": pulls.get("html_url").and_then(Value::as_str),
        "body": pulls.get("body").and_then(Value::as_str).unwrap_or(""),
        "state": map_pr_state(pulls),
        "head_sha": sha,
        "head_ref": head_ref,
        "base_ref": base_ref,
        "mergeable": map_mergeable(pulls.get("mergeable").unwrap_or(&Value::Null)),
        "merge_state_status": pulls.get("mergeable_state").and_then(Value::as_str),
        "merged_at": merged_at,
        "merge_sha": merge_sha,
        "author": pulls.pointer("/user/login").and_then(Value::as_str),
        "auto_merge": pulls.get("auto_merge").cloned().unwrap_or(Value::Null),
    }))
}

/// gh's own pr-view JSON mapped onto the same REST payload keys, the twin of
/// the REST mapper: GraphQL names lower into the REST spellings
/// (mergeStateStatus lowercased, mergeCommit.oid into merge_sha,
/// autoMergeRequest into the REST auto_merge keys). The read asks for every
/// key this maps (PR_INFO_FALLBACK_FIELDS), so a mapped null is GitHub's
/// own null, never an unfetched key. The transport marker names the
/// provenance; consumers read named keys and ignore it.
fn graphql_payload(gh: &Value) -> Option<Value> {
    let number = gh.get("number")?.as_u64()?;
    let state_status = gh
        .get("mergeStateStatus")
        .and_then(Value::as_str)
        .map(|s| s.to_lowercase());
    let auto_merge = gh.get("autoMergeRequest");
    Some(json!({
        "pr": number,
        "url": gh.get("url").and_then(Value::as_str),
        "body": gh.get("body").and_then(Value::as_str).unwrap_or(""),
        "state": gh.get("state").and_then(Value::as_str),
        "head_sha": gh.get("headRefOid").and_then(Value::as_str),
        "head_ref": gh.get("headRefName").and_then(Value::as_str),
        "base_ref": gh.get("baseRefName").and_then(Value::as_str),
        "mergeable": gh.get("mergeable").and_then(Value::as_str),
        "merge_state_status": state_status,
        "merged_at": gh.get("mergedAt").and_then(Value::as_str),
        "merge_sha": gh
            .get("mergeCommit")
            .and_then(|c| c.get("oid"))
            .and_then(Value::as_str),
        "author": gh.pointer("/author/login").and_then(Value::as_str),
        "auto_merge": match auto_merge {
            Some(a) if a.is_object() => json!({
                "enabled_at": a.get("enabledAt").and_then(Value::as_str),
                "merge_method": a.get("mergeMethod").and_then(Value::as_str),
            }),
            _ => Value::Null,
        },
        "transport": "graphql",
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Both mappers land on the same REST keys: the pulls body through the
    /// shared state/mergeability mappers, the gh shape with the GraphQL
    /// names lowered (mergeStateStatus lowercased, mergeCommit.oid into
    /// merge_sha, autoMergeRequest into the REST auto_merge keys). A pulls
    /// body with no head sha refuses the mapper.
    #[test]
    fn mappers_map_both_transports_onto_the_rest_keys() {
        let pulls = serde_json::json!({
            "state": "open",
            "html_url": "https://github.test/o/r/pull/7",
            "body": "the body",
            "merged": null,
            "merged_at": null,
            "head": {"sha": "abc123", "ref": "feature/one"},
            "base": {"ref": "main"},
            "mergeable": true,
            "mergeable_state": "clean",
            "merge_commit_sha": null,
            "user": {"login": "jj"},
            "auto_merge": null,
        });
        let rest = rest_payload(7, &pulls).expect("a well-formed pulls body maps");
        assert_eq!(rest.get("state").and_then(Value::as_str), Some("OPEN"));
        assert_eq!(rest.get("head_sha").and_then(Value::as_str), Some("abc123"));
        assert_eq!(
            rest.get("mergeable").and_then(Value::as_str),
            Some("MERGEABLE")
        );
        assert_eq!(
            rest.get("merge_state_status").and_then(Value::as_str),
            Some("clean")
        );

        // A pulls body with no head sha refuses the mapper; the command reports
        // that as the malformed-body refusal.
        let shaless = serde_json::json!({"head": {}, "base": {"ref": "main"}});
        assert!(rest_payload(7, &shaless).is_none());

        // GraphQL shape in, REST keys out: names lower, oid becomes
        // merge_sha, the queue object becomes the REST auto_merge keys, and
        // the transport marker names the provenance.
        let gh = serde_json::json!({
            "state": "OPEN",
            "number": 7,
            "headRefName": "feature/one",
            "headRefOid": "def456",
            "mergeable": "MERGEABLE",
            "mergeStateStatus": "BEHIND",
            "baseRefName": "main",
            "author": {"login": "jj"},
            "url": "https://github.test/o/r/pull/7",
            "mergedAt": "2026-10-19T00:00:00Z",
            "mergeCommit": {"oid": "msh1"},
            "autoMergeRequest": {"enabledAt": "t", "mergeMethod": "MERGE"},
        });
        let mapped = graphql_payload(&gh).expect("a well-formed gh body maps");
        assert_eq!(
            mapped.get("head_sha").and_then(Value::as_str),
            Some("def456")
        );
        assert_eq!(
            mapped.get("merge_state_status").and_then(Value::as_str),
            Some("behind")
        );
        assert_eq!(
            mapped.get("merge_sha").and_then(Value::as_str),
            Some("msh1")
        );
        assert_eq!(
            mapped
                .pointer("/auto_merge/merge_method")
                .and_then(Value::as_str),
            Some("MERGE")
        );
        assert_eq!(
            mapped.get("transport").and_then(Value::as_str),
            Some("graphql")
        );
    }

    /// The op refuses with a named cause and spawns nothing when the ask is
    /// malformed or the cwd names no repository: `pr: null` on the resolve
    /// arm, the numeric pr on the fetch arm. The receipt's error text keeps
    /// the REST refusal first and appends a fallback cause after it.
    #[test]
    fn the_op_refuses_with_a_named_cause_and_no_read() {
        let out = run_op(&serde_json::json!({"pr": 0, "cwd": "."}));
        assert_eq!(out.get("pr"), Some(&serde_json::json!(null)));
        assert!(
            out.get("error")
                .and_then(Value::as_str)
                .unwrap_or("")
                .contains("positive"),
            "names the refusal: {out}"
        );

        // A cwd with no repository context: the slug refusal names the
        // missing context; never a causeless error.
        let dir = tempfile::tempdir().unwrap();
        let out = run_op(&serde_json::json!({
            "pr": 5,
            "cwd": dir.path().to_string_lossy(),
        }));
        let err = out.get("error").and_then(Value::as_str).unwrap_or("");
        assert!(
            err.contains("owner/repo"),
            "names the missing context: {err}"
        );

        // A fallback that answered nothing appends its cause after the REST
        // refusal; a causeless fallback leaves the REST refusal alone.
        let text = append_fallback_cause(
            "gh: API rate limit exceeded | this is the CORE REST quota",
            Some("pr_view: broker refused".to_string()),
        );
        assert!(
            text.starts_with("gh: API rate limit exceeded"),
            "the REST refusal leads: {text}"
        );
        assert!(
            text.contains("graphql fallback: pr_view: broker refused"),
            "the fallback cause follows: {text}"
        );
        assert_eq!(
            append_fallback_cause("the CORE arm appended the fallback cause", None),
            "the CORE arm appended the fallback cause"
        );
    }

    /// The slice field set extends the info fallback's set by exactly
    /// `statusCheckRollup`: the ONE brokered GraphQL read the CORE arm is
    /// allowed serves checks beside the view fields (one targeted query,
    /// never a second). Routing still rides `url`, and the rollup field
    /// itself is broker-only too.
    #[test]
    fn slice_fields_extend_the_info_set_by_the_rollup_field() {
        assert_eq!(
            PR_INFO_SLICE_FIELDS,
            format!(
                "{},statusCheckRollup",
                crate::loopcheck::PR_INFO_FALLBACK_FIELDS
            )
        );
        use crate::pr_status::adapter::METADATA_FIELDS;
        assert!(
            !METADATA_FIELDS.contains(&"statusCheckRollup"),
            "statusCheckRollup is broker-only"
        );
    }

    /// The rollup maps through the status read's own classifiers: the
    /// generic-CI filter drops the coverage/evals projections, supersession
    /// keeps the latest row per name, the verdict reads red on one settled
    /// failure, and rows carry name + state capped at 20.
    #[test]
    fn rollup_maps_to_verdict_counts_and_capped_rows() {
        let gh = serde_json::json!({
            "number": 7,
            "headRefOid": "abc",
            "statusCheckRollup": [
                {"__typename": "CheckRun", "name": "ci", "status": "COMPLETED", "conclusion": "SUCCESS"},
                {"__typename": "CheckRun", "name": "ci", "status": "COMPLETED", "conclusion": "FAILURE"},
                {"__typename": "CheckRun", "name": "lint", "status": "IN_PROGRESS"},
                {"__typename": "CheckRun", "name": "lint", "status": "IN_PROGRESS"},
                {"__typename": "StatusContext", "context": "fno/review-coverage", "state": "SUCCESS"},
                {"__typename": "StatusContext", "context": "fno/evals-gate", "state": "SUCCESS"},
                {"__typename": "StatusContext", "context": "fno/evals-gate", "state": "SUCCESS"}
            ]
        });
        let checks = checks_from_rollup(&gh).expect("a rollup array maps");
        assert_eq!(checks.get("verdict").and_then(Value::as_str), Some("red"));
        let counts = checks.get("counts").expect("counts ride");
        assert_eq!(counts.get("total").and_then(Value::as_i64), Some(3));
        assert_eq!(counts.get("pass").and_then(Value::as_i64), Some(1));
        assert_eq!(counts.get("fail").and_then(Value::as_i64), Some(1));
        assert_eq!(
            checks.get("rows").and_then(Value::as_array).map(Vec::len),
            Some(3)
        );
        assert_eq!(counts.get("pending").and_then(Value::as_i64), Some(1));
        assert_eq!(
            checks.get("rows_truncated").and_then(Value::as_bool),
            Some(false)
        );
        let dropped = checks_from_rollup(&serde_json::json!({"number": 7}));
        assert!(dropped.is_none(), "no rollup array, no checks key");
    }

    /// The git leg answers head sha and files with zero REST quota: a real
    /// temp repo whose origin carries `refs/pull/7/head`. A missing ref
    /// refuses with the ref name in the reason.
    #[test]
    fn git_leg_reads_head_and_files_over_refs_pull_head() {
        let dir = tempfile::tempdir().unwrap();
        let origin = dir.path().join("origin.git");
        let work = dir.path().join("work");
        std::fs::create_dir_all(&work).unwrap();
        let urls = vec![origin.to_string_lossy().into_owned()];
        let g = |args: &[&str]| {
            let out = Command::new("git")
                .args(args)
                .current_dir(&work)
                .env("GIT_AUTHOR_NAME", "t")
                .env("GIT_AUTHOR_EMAIL", "t@t")
                .env("GIT_COMMITTER_NAME", "t")
                .env("GIT_COMMITTER_EMAIL", "t@t")
                .output()
                .expect("git spawns");
            assert!(
                out.status.success(),
                "git {args:?} failed: {}",
                String::from_utf8_lossy(&out.stderr)
            );
        };
        // init origin bare
        let out = Command::new("git")
            .args(["init", "--bare", "-b", "main"])
            .arg(&origin)
            .output()
            .expect("git init spawns");
        assert!(out.status.success());
        g(&["init", "-b", "main"]);
        g(&["remote", "add", "origin", origin.to_str().unwrap()]);
        g(&["commit", "--allow-empty", "-m", "base"]);
        g(&["push", "-q", "origin", "main"]);
        g(&["checkout", "-b", "feature"]);
        std::fs::write(work.join("b.txt"), "x").unwrap();
        g(&["add", "b.txt"]);
        g(&["commit", "-q", "-m", "feature"]);
        g(&["push", "-q", "origin", "HEAD:refs/pull/7/head"]);
        g(&["fetch", "-q", "origin"]);

        let sha = git_head_sha(&work, 7, &urls).expect("the ref resolves");
        assert_eq!(sha, {
            let out = Command::new("git")
                .args(["rev-parse", "HEAD"])
                .current_dir(&work)
                .output()
                .unwrap();
            String::from_utf8_lossy(&out.stdout).trim().to_string()
        });
        assert_eq!(git_head_sha(&work, 8, &urls), None);

        let (files, truncated, base) =
            git_files(&work, 7, &urls, Some("main")).expect("the diff answers");
        assert_eq!(files, vec!["b.txt".to_string()]);
        assert!(!truncated);
        assert_eq!(base, "origin/main");
        let err = git_files(&work, 8, &urls, None).expect_err("a missing ref refuses");
        assert!(
            err.contains("refs/pull/8/head"),
            "the refusal names the ref: {err}"
        );
    }

    /// The CORE arm's assembly over a real temp repo: a served GraphQL
    /// reply takes the checks summary, the git head cross-check overrides
    /// head_sha and names its source, files stamps source or missing-reason.
    /// A refused read falls to the git legs alone: a partial payload whose
    /// served fields carry sources and unserved fields carry reasons.
    #[test]
    fn assemble_stamps_sources_and_partial_payload() {
        let dir = tempfile::tempdir().unwrap();
        let origin = dir.path().join("origin.git");
        let work = dir.path().join("work");
        std::fs::create_dir_all(&work).unwrap();
        let origin_arg = origin.to_string_lossy().into_owned();
        let run = |repo: &std::path::Path, args: &[&str]| {
            let out = Command::new("git")
                .args(args)
                .current_dir(repo)
                .env("GIT_AUTHOR_NAME", "t")
                .env("GIT_AUTHOR_EMAIL", "t@t")
                .env("GIT_COMMITTER_NAME", "t")
                .env("GIT_COMMITTER_EMAIL", "t@t")
                .output()
                .expect("git spawns");
            assert!(
                out.status.success(),
                "git {args:?} failed: {}",
                String::from_utf8_lossy(&out.stderr)
            );
        };
        let out = Command::new("git")
            .args(["init", "--bare", "-b", "main"])
            .arg(&origin)
            .output()
            .expect("git init spawns");
        assert!(out.status.success(), "bare init failed");
        run(&work, &["init", "-b", "main"]);
        run(&work, &["remote", "add", "origin", &origin_arg]);
        run(&work, &["commit", "--allow-empty", "-m", "base"]);
        run(&work, &["push", "-q", "origin", "main"]);
        run(&work, &["checkout", "-q", "-b", "feature"]);
        std::fs::write(work.join("b.txt"), "x").unwrap();
        run(&work, &["add", "b.txt"]);
        run(&work, &["commit", "-q", "-m", "feature"]);
        run(&work, &["push", "-q", "origin", "HEAD:refs/pull/7/head"]);
        run(&work, &["fetch", "-q", "origin"]);
        let git_sha = {
            let out = Command::new("git")
                .args(["rev-parse", "HEAD"])
                .current_dir(&work)
                .output()
                .expect("rev-parse spawns");
            String::from_utf8_lossy(&out.stdout).trim().to_string()
        };
        // Case A: GraphQL served - checks ride, git overrides the head sha,
        // files come from the git diff, sources name the git legs.
        let gh = serde_json::json!({
            "number": 7,
            "headRefOid": "f".repeat(40),
            "baseRefName": "main",
            "statusCheckRollup": [
                {"__typename": "CheckRun", "name": "ci", "status": "COMPLETED", "conclusion": "FAILURE"}
            ]
        });
        let info = assemble_fallback(&work, Some(7), Ok(Some(gh)))
            .ok()
            .flatten()
            .expect("a served reply assembles");
        assert_eq!(
            info.get("head_sha").and_then(Value::as_str),
            Some(git_sha.as_str())
        );
        assert_eq!(
            info.pointer("/sources/head_sha").and_then(Value::as_str),
            Some("git refs/pull/7/head")
        );
        assert_eq!(
            info.pointer("/checks/verdict").and_then(Value::as_str),
            Some("red")
        );
        assert_eq!(
            info.pointer("/sources/files").and_then(Value::as_str),
            Some("git diff origin/main...refs/pull/7/head")
        );
        assert!(
            info.get("files")
                .and_then(Value::as_array)
                .map(Vec::len)
                .unwrap_or(0)
                >= 1,
            "the diff leg serves files: {info}"
        );
        // Case B: the read refused - git answers alone.
        let read: Result<Option<Value>, crate::loopcheck::GhReadError> = Err(
            crate::loopcheck::GhReadError::for_test("pr_view", "bucket drained"),
        );
        let info = assemble_fallback(&work, Some(7), read)
            .ok()
            .flatten()
            .expect("git serves head + files");
        assert_eq!(info.get("partial").and_then(Value::as_bool), Some(true));
        assert_eq!(info.get("transport").and_then(Value::as_str), Some("git"));
        assert_eq!(
            info.get("head_sha").and_then(Value::as_str),
            Some(git_sha.as_str())
        );
        let missing = info
            .get("missing")
            .and_then(Value::as_object)
            .expect("missing map");
        assert!(
            missing
                .get("url")
                .and_then(Value::as_str)
                .unwrap_or("")
                .contains("graphql fallback failed"),
            "unserved fields name the cause: {missing:?}"
        );
        assert!(
            missing.get("head_sha").is_none(),
            "the served head is not missing"
        );
    }
}
