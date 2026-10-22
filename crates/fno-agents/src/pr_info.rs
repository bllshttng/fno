//! The `status-pr-info` op: the Python `fno do pr info` read, ported beside
//! its REST twins. The command's read is one REST pulls fetch; a refusal
//! classified CORE (the budget drained, not the request rate) hands the same
//! selector to `read_pr_view_fields`, whose brokered GraphQL fallback is the
//! stop gate's own machinery, and the reply maps onto the REST payload keys
//! so every consumer reads one shape whichever transport served it. The
//! Python command is a bare forward to this op: three net lines in cli/src/fno
//! carrying the ask, the exit codes, and no transport decision.

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
    rest_payload(pr, &pulls)
        .ok_or_else(|| RestReason::new("gh api pulls/<n> carried a malformed body", ""))
}

fn resolve_slug(cwd: &Path, repo: Option<&str>) -> Result<String, RestReason> {
    repo.map(str::to_string)
        .or_else(|| crate::backlog::pr_link::resolve_current_repo_slug(cwd.to_str()))
        .ok_or_else(|| RestReason::new("could not resolve owner/repo from the origin remote", ""))
}

/// The fallback: the same selector through `read_pr_view_fields`, whose
/// brokered GraphQL fallback this op borrows wholesale - the earned checks,
/// the quota probe, and the bounded spawns are the stop gate's, not a second
/// impl. The field set is the op's own (the info payload maps four fields
/// the gate's set never asks for), and the slug the REST legs read pins the
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
    match crate::loopcheck::read_pr_view_fields(
        &gh_bin,
        cwd,
        selector.as_deref(),
        crate::loopcheck::PR_INFO_FALLBACK_FIELDS,
        repo,
        FALLBACK_READ_TIMEOUT,
    ) {
        Ok(Some(gh)) => match graphql_payload(&gh) {
            Some(info) => Ok(Some(info)),
            None => Err(Some("the reply carried a malformed body".to_string())),
        },
        Ok(None) => Ok(None),
        Err(err) => Err(Some(err.cause_line())),
    }
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
            append_fallback_cause("the CORE refusal", None),
            "the CORE refusal"
        );
    }
}
