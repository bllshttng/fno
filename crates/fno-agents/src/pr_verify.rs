//! `fno-agents pr-verify --kind merged|reviews`: the two external-outcome
//! audits the auto-merge / external-review gates read, ported from
//! `cli/src/fno/pr/_verify.py`, which this verb replaces.
//!
//! `--kind merged` audits the GitHub merge-state: records the merge into the
//! state-file frontmatter when MERGED, blocks with a reason when the merge
//! cannot happen, and on OPEN + all-clean runs ONE bounded remediation (one
//! authorized-merge ask, then one 30s poll - never a retry loop). Exit 0
//! merged/degrade-open, 1 blocked-with-reason, 2 substrate failure.
//!
//! `--kind reviews` walks the PR review and comment history and flips
//! `external_review_passed` (exit 1) when a non-pending reviewer has no
//! qualifying reply: a PR-author reply strictly after that reviewer's latest
//! review that @-mentions them OR lands within 24 hours. Exit 0
//! honest/degrade-open, 1 flipped, 2 substrate failure.
//!
//! Every owner-mediated wording (the dispatch hold, the coverage gate) is the
//! Rust merge gate's own, so the audit and the merge it audits cannot
//! disagree; the Python leg's wordings for those two died with the leg.

use crate::authorized_merge::{PrFacts, ProbeOutcome};
use crate::gate_probes::{dispatch_hold, run_probe, RunProbe};
use serde_json::{json, Value};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;

/// The argv transport: `pr-verify --kind merged|reviews --pr-number <s>
/// --state-file <f>`. The PR number is a free-form string, exactly the
/// Python leg's stance: a URL or branch name must not become a parse error
/// after every precondition passed.
pub fn run(argv: &[String]) -> i32 {
    let mut kind = "";
    let mut pr = "";
    let state_file;
    {
        let mut state = "";
        let mut it = argv.iter().map(String::as_str);
        while let Some(arg) = it.next() {
            match arg {
                "--kind" => kind = it.next().unwrap_or(""),
                "--pr-number" => pr = it.next().unwrap_or(""),
                "--state-file" => state = it.next().unwrap_or(""),
                other => {
                    eprintln!("usage: fno-agents pr-verify --kind merged|reviews --pr-number <s> --state-file <f> (got {other})");
                    return 2;
                }
            }
        }
        state_file = state.to_string();
    }
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let probe = |args: &[&str], dir: &Path| {
        let mut cmd = Command::new(args[0]);
        cmd.args(&args[1..]).current_dir(dir);
        run_probe(cmd)
    };
    let mut out = std::io::stdout().lock();
    let mut err = std::io::stderr().lock();
    let journal_of = |root: &Path| crate::paths::events_path(root);
    let gh_present = crate::gate_probes::tool_on_path("gh");
    match kind {
        "merged" => verify_merged(
            pr,
            &state_file,
            &cwd,
            &probe,
            gh_present,
            &journal_of,
            &mut out,
            &mut err,
        ),
        "reviews" => verify_reviews(
            pr,
            &state_file,
            &cwd,
            &probe,
            gh_present,
            &journal_of,
            &mut out,
            &mut err,
        ),
        "" => {
            eprintln!("pr-verify: --kind required");
            2
        }
        other => {
            eprintln!("pr-verify: unknown kind: {other}");
            2
        }
    }
}

fn readable(path: &str) -> bool {
    std::fs::metadata(path).is_ok()
}

fn repo_root(cwd: &Path, run: RunProbe) -> PathBuf {
    match run(&["git", "rev-parse", "--show-toplevel"], cwd) {
        Some(r) if r.ok && !r.stdout.trim().is_empty() => PathBuf::from(r.stdout.trim()),
        _ => cwd.to_path_buf(),
    }
}

fn s_of(v: &Value, key: &str) -> String {
    v.get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

fn fetch_pr_state(pr: &str, cwd: &Path, run: RunProbe) -> Option<Value> {
    let out = run(
        &[
            "gh",
            "pr",
            "view",
            pr,
            "--json",
            "state,mergedAt,isDraft,reviewDecision,statusCheckRollup,headRefOid",
        ],
        cwd,
    )?;
    if !out.ok || out.stdout.trim().is_empty() {
        return None;
    }
    serde_json::from_str(&out.stdout).ok()
}

// ---------------------------------------------------------------------------
// verify --kind merged
// ---------------------------------------------------------------------------

#[allow(clippy::too_many_arguments)]
fn verify_merged(
    pr: &str,
    state_file: &str,
    cwd: &Path,
    run: RunProbe,
    gh_present: bool,
    journal_of: &dyn Fn(&Path) -> PathBuf,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> i32 {
    if pr.is_empty() {
        let _ = writeln!(err, "verify-pr-merged: --pr-number required");
        return 2;
    }
    if state_file.is_empty() || !readable(state_file) {
        let _ = writeln!(
            err,
            "verify-pr-merged: --state-file unreadable: {state_file}"
        );
        return 2;
    }
    if !gh_present {
        let _ = writeln!(err, "verify-pr-merged: gh missing; degrading open");
        return 0;
    }
    let repo_root = repo_root(cwd, run);
    let journal = journal_of(&repo_root);
    let Some(pr_json) = fetch_pr_state(pr, cwd, run) else {
        let _ = writeln!(err, "verify-pr-merged: gh pr view failed; degrading open");
        return 0;
    };
    let state = s_of(&pr_json, "state");
    let merged_at = s_of(&pr_json, "mergedAt");
    let is_draft = pr_json
        .get("isDraft")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let review_decision = s_of(&pr_json, "reviewDecision");

    if state == "MERGED" {
        return if record_merge(state_file, pr, &merged_at, err) {
            let _ = writeln!(out, "verify-pr-merged: PR #{pr} MERGED at {merged_at}");
            0
        } else {
            let _ = writeln!(
                err,
                "verify-pr-merged: record_merge failed (state lock or write error); state file may be stale"
            );
            2
        };
    }
    if state == "CLOSED" {
        emit_audit(
            &journal,
            state_file,
            pr,
            "pr_closed_without_merge",
            Some(json!({ "pr_state": "CLOSED" })),
        );
        let _ = writeln!(
            out,
            "pr_closed_without_merge: PR #{pr} is CLOSED (not merged) \u{2014} investigate before re-promising"
        );
        return 1;
    }
    if state != "OPEN" {
        let _ = writeln!(
            err,
            "verify-pr-merged: unknown PR state: {state}; degrading open"
        );
        return 0;
    }

    // This remediation path merges directly, so the sanctioned merge verb
    // cannot protect it. Read the same plan hold the merge gate reads before
    // any merge precondition can flow into remediation.
    let number = pr.parse::<u64>().unwrap_or(0);
    let facts = PrFacts {
        number,
        ..Default::default()
    };
    if let ProbeOutcome::Refused(reason) = dispatch_hold(cwd, &facts) {
        let _ = writeln!(out, "merge_held: PR #{pr}: {reason}");
        return 1;
    }

    if is_draft {
        emit_audit(
            &journal,
            state_file,
            pr,
            "pr_is_draft",
            Some(json!({ "pr_state": "OPEN" })),
        );
        let _ = writeln!(
            out,
            "pr_is_draft: PR #{pr} is in draft state; un-draft before auto-merge can run"
        );
        return 1;
    }
    if review_decision == "CHANGES_REQUESTED" {
        emit_audit(
            &journal,
            state_file,
            pr,
            "review_changes_requested",
            Some(json!({ "review_decision": "CHANGES_REQUESTED" })),
        );
        let _ = writeln!(
            out,
            "review_changes_requested: PR #{pr} has CHANGES_REQUESTED; address the review before auto-merge"
        );
        return 1;
    }

    // Same posture as the merge verb: the checks precondition applies only
    // when require_checks_pass is on, and PENDING is not failing - a
    // still-running check flows into the bounded remediation.
    if crate::agents_config::auto_merge_require_checks_pass(cwd) {
        let payload = json!({
            "op": "status-ci",
            "cwd": cwd.to_string_lossy(),
            "pr": number,
            "drop_coverage": true,
        });
        let (code, stdout, _stderr) = crate::pr_status::status_ci(&payload);
        if code != 0 {
            let _ = writeln!(err, "verify-pr-merged: required checks unreadable");
            return 1;
        }
        let rows: Vec<Value> = serde_json::from_str(stdout.trim()).unwrap_or_default();
        let failing: Vec<String> = rows
            .iter()
            .filter(|r| r.get("bucket").and_then(Value::as_str) == Some("fail"))
            .map(|r| s_of(r, "name"))
            .filter(|n| !n.is_empty())
            .collect();
        if !failing.is_empty() {
            let failing_csv = failing.join(",");
            emit_audit(
                &journal,
                state_file,
                pr,
                "required_checks_failing",
                Some(json!({ "failing_checks": failing_csv })),
            );
            let _ = writeln!(out, "required_checks_failing: {failing_csv}");
            return 1;
        }
    }

    // The coverage gate the merge verb itself answers, so the audit and the
    // merge it audits cannot disagree. No review lane configured reads
    // COVERED inside the gate itself.
    let (blocker, _waiver) =
        crate::merge_gates::coverage_gate(&crate::authorized_merge::RealProbes, cwd, number);
    if let Some(blocker) = blocker {
        let line = format!("{}: {}", blocker.code, blocker.detail);
        emit_audit(
            &journal,
            state_file,
            pr,
            "coverage_gate_refused",
            Some(json!({ "line": line })),
        );
        let _ = writeln!(out, "coverage_gate_refused: {line}");
        return 1;
    }

    // All preconditions clean.
    if crate::agents_config::auto_merge_remediation(cwd) == "verify_only" {
        emit_audit(
            &journal,
            state_file,
            pr,
            "remediation_disabled",
            Some(json!({ "remediation": "verify_only" })),
        );
        let _ = writeln!(
            out,
            "remediation_disabled: PR #{pr} is mergeable but config.auto_merge.remediation: verify_only - merge manually or flip the setting"
        );
        return 1;
    }

    bounded_remediation(
        pr, state_file, cwd, &journal, number, &pr_json, run, out, err,
    )
}

/// Single authorized-merge ask + single 30s poll (anti-thrash; the verb
/// executes - no --auto, no --delete-branch flag on the ask).
#[allow(clippy::too_many_arguments)]
#[allow(clippy::too_many_arguments)]
fn bounded_remediation(
    pr: &str,
    state_file: &str,
    cwd: &Path,
    journal: &Path,
    number: u64,
    state_read: &Value,
    run: RunProbe,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> i32 {
    let strategy = crate::agents_config::auto_merge_strategy(cwd);
    // The head THIS verb's gate covered, never whatever the local checkout
    // points at; the state read carried it beside the rollup.
    let mut payload = json!({
        "cwd": cwd.to_string_lossy(),
        "pr": number,
        "effect": "merge",
        "auto_merge_source": "verify-remediation",
        "require_checks": crate::agents_config::auto_merge_require_checks_pass(cwd),
        "decide_only": false,
        "authority": "manifest",
        "accept_flake": false,
    });
    let covered_head = s_of(state_read, "headRefOid");
    if !covered_head.is_empty() {
        payload["covered_head"] = json!(covered_head);
    }
    let (code, stdout, stderr) =
        crate::authorized_merge::run_authorized_merge_capture(&[payload.to_string()]);
    let receipt: Value = serde_json::from_str(stdout.trim()).unwrap_or(json!({}));
    let owner_unreadable = receipt.get("outcome").is_none();
    let outcome = if !owner_unreadable {
        s_of(&receipt, "outcome")
    } else {
        // An unread authorization is UNKNOWN, never a clear answer.
        "unknown".to_string()
    };
    let detail = if receipt.get("detail").is_some() {
        s_of(&receipt, "detail")
    } else if owner_unreadable && code != 0 {
        if stderr.trim().is_empty() {
            format!("authorized-merge exited {code}")
        } else {
            format!("authorized-merge exited {code}: {}", stderr.trim())
        }
    } else {
        "no detail".to_string()
    };

    if outcome == "merged" {
        // This verb's name is its contract, so the owner's receipt is not the
        // last word: re-read the PR, and give a lagging read ONE bounded poll
        // before reporting. Never a retry loop.
        for attempt in 0..2 {
            let fresh = fetch_pr_state(pr, cwd, run);
            let fresh_state = fresh.as_ref().map(|j| s_of(j, "state")).unwrap_or_default();
            if fresh_state == "MERGED" {
                let merged_at = fresh
                    .as_ref()
                    .map(|j| s_of(j, "mergedAt"))
                    .unwrap_or_default();
                record_merge(state_file, pr, &merged_at, err);
                remote_delete_cleanup(number, cwd, run, err);
                let _ = writeln!(out, "verify-pr-merged: PR #{pr} MERGED at {merged_at}");
                return 0;
            }
            if attempt == 0 {
                std::thread::sleep(std::time::Duration::from_secs(30));
            }
        }
        let final_state = fetch_pr_state(pr, cwd, run)
            .map(|j| s_of(&j, "state"))
            .unwrap_or_default();
        emit_audit(
            journal,
            state_file,
            pr,
            "merge_attempt_did_not_complete",
            Some(json!({ "final_state": final_state, "detail": detail })),
        );
        let _ = writeln!(
            out,
            "merge_attempt_did_not_complete: PR #{pr} still {final_state} after the merge + 30s poll"
        );
        return 1;
    }

    if outcome == "unknown" {
        emit_audit(
            journal,
            state_file,
            pr,
            "merge_state_unreadable",
            Some(json!({ "detail": detail })),
        );
        let _ = writeln!(out, "merge_state_unreadable: PR #{pr} {detail}");
        return 2;
    }

    if outcome == "failed" {
        let lowered = detail.to_lowercase();
        let freshness_hit = [
            "freshness",
            "stale state",
            "state file",
            "state-file mtime",
            "git-protection",
        ]
        .iter()
        .any(|tok| lowered.contains(tok));
        if freshness_hit {
            emit_audit(
                journal,
                state_file,
                pr,
                "merge_blocked_by_freshness_cap",
                Some(json!({ "stderr_token": "freshness_cap" })),
            );
            let _ = writeln!(
                out,
                "merge_blocked_by_freshness_cap: git-protection.py blocked the merge (state-file mtime > 1h). Bypass it from a shell with gh pr merge {pr} --{strategy}."
            );
            return 1;
        }
        emit_audit(
            journal,
            state_file,
            pr,
            "merge_attempt_failed",
            Some(json!({ "detail": detail })),
        );
        let _ = writeln!(out, "merge_attempt_failed: {detail}");
        return 1;
    }

    emit_audit(
        journal,
        state_file,
        pr,
        "merge_attempt_did_not_complete",
        Some(json!({ "outcome": outcome, "detail": detail })),
    );
    let _ = writeln!(
        out,
        "merge_attempt_did_not_complete: PR #{pr} {outcome}: {detail}"
    );
    1
}

/// Post-merge remote-branch delete, warn-only: the same `gh api -X DELETE`
/// against the PR's verified base repo the merge verb runs, split from the
/// merge so cleanup can never fail one. Never deletes the local branch.
fn remote_delete_cleanup(number: u64, cwd: &Path, run: RunProbe, err: &mut dyn Write) {
    if !crate::agents_config::auto_merge_delete_branch_on_merge(cwd) {
        return;
    }
    let read = run(
        &[
            "gh",
            "api",
            "--jq",
            r#"[.head.ref // "", .head.repo.full_name // "", .base.repo.full_name // ""] | @tsv"#,
            &format!("repos/{{{{owner}}}}/{{{{repo}}}}/pulls/{number}"),
        ],
        cwd,
    );
    let fields: Vec<String> = read
        .filter(|r| r.ok)
        .map(|r| r.stdout.trim().split('\t').map(str::to_string).collect())
        .unwrap_or_default();
    let branch = fields.first().map(String::as_str).unwrap_or_default();
    let head_repo = fields.get(1).map(String::as_str).unwrap_or_default();
    let base_repo = fields.get(2).map(String::as_str).unwrap_or_default();
    if branch.is_empty() || base_repo.is_empty() {
        let _ = writeln!(
            err,
            "verify-pr-merged: post-merge cleanup failed: remote branch name unreadable (non-fatal)"
        );
        return;
    }
    // A fork owns its head branch: deleting a same-named base branch would be
    // data loss, so a cross-repo head is a silent no-delete terminal (the
    // queue-merge finisher emits no failure event on an empty sink).
    if !head_repo.is_empty() && head_repo != base_repo {
        return;
    }
    let deleted = run(
        &[
            "gh",
            "api",
            "--method",
            "DELETE",
            &format!("repos/{base_repo}/git/refs/heads/{branch}"),
        ],
        cwd,
    );
    if deleted.map(|r| r.ok).unwrap_or(false) {
        return;
    }
    let _ = writeln!(
        err,
        "verify-pr-merged: post-merge cleanup failed (non-fatal)"
    );
}

/// The `merge-remote-delete` op on the authorized-merge door: the queue-merge
/// finisher's transport (pr port 7). The same gate and warn-only posture the
/// verify flow's cleanup leg holds; the receipt's `cleanup` word carries the
/// outcome the Python caller emitted before.
pub(crate) fn remote_delete_cleanup_op(payload: &Value) -> String {
    let pr = payload.get("pr").and_then(Value::as_u64).unwrap_or(0);
    let cwd = std::path::PathBuf::from(payload.get("cwd").and_then(Value::as_str).unwrap_or("."));
    let mut sink: Vec<u8> = Vec::new();
    let run = |args: &[&str], dir: &std::path::Path| -> Option<crate::gate_probes::Run> {
        let mut cmd = std::process::Command::new(args[0]);
        cmd.args(&args[1..]).current_dir(dir);
        crate::gate_probes::run_probe(cmd)
    };
    remote_delete_cleanup(pr, &cwd, &run, &mut sink);
    serde_json::json!({"cleanup": String::from_utf8_lossy(&sink).into_owned()}).to_string()
}

// ---------------------------------------------------------------------------
// verify --kind reviews
// ---------------------------------------------------------------------------

#[allow(clippy::too_many_arguments)]
fn verify_reviews(
    pr: &str,
    state_file: &str,
    cwd: &Path,
    run: RunProbe,
    gh_present: bool,
    journal_of: &dyn Fn(&Path) -> PathBuf,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> i32 {
    if pr.is_empty() {
        let _ = writeln!(err, "verify-review-replies: --pr-number required");
        return 2;
    }
    if state_file.is_empty() || !readable(state_file) {
        let _ = writeln!(
            err,
            "verify-review-replies: --state-file unreadable: {state_file}"
        );
        return 2;
    }
    if !gh_present {
        let _ = writeln!(err, "verify-review-replies: gh missing; degrading open");
        return 0;
    }
    let repo_root = repo_root(cwd, run);
    let journal = journal_of(&repo_root);

    let repo_read = run(
        &[
            "gh",
            "repo",
            "view",
            "--json",
            "nameWithOwner",
            "--jq",
            ".nameWithOwner",
        ],
        cwd,
    );
    let repo_slug = repo_read
        .filter(|r| r.ok)
        .map(|r| r.stdout.trim().to_string())
        .unwrap_or_default();
    if repo_slug.is_empty() {
        let _ = writeln!(
            err,
            "verify-review-replies: gh repo view failed; degrading open"
        );
        return 0;
    }

    let reviews = gh_api_json(
        run,
        cwd,
        &[
            &format!("/repos/{repo_slug}/pulls/{pr}/reviews"),
            "--jq",
            r#"[ .[] | select(.state != "PENDING") | {login: .user.login, submitted_at: .submitted_at} ]"#,
        ],
    );
    let Some(reviews) = reviews else {
        let _ = writeln!(
            err,
            "verify-review-replies: gh api reviews failed; degrading open"
        );
        return 0;
    };

    // Collapse to one entry per reviewer with their newest submitted_at.
    let mut reviewer_map: Vec<(String, String)> = Vec::new();
    let mut sorted: Vec<&Value> = reviews
        .as_array()
        .map(|a| a.iter().collect())
        .unwrap_or_default();
    sorted.sort_by(|a, b| {
        let ka = a.get("submitted_at").and_then(Value::as_str).unwrap_or("");
        let kb = b.get("submitted_at").and_then(Value::as_str).unwrap_or("");
        kb.cmp(ka)
    });
    for r in sorted {
        let login = s_of(r, "login");
        let submitted = s_of(r, "submitted_at");
        if !login.is_empty() && !reviewer_map.iter().any(|(l, _)| l == &login) {
            reviewer_map.push((login, submitted));
        }
    }
    if reviewer_map.is_empty() {
        // No non-pending reviewers - nothing to audit.
        return 0;
    }

    let author_read = run(
        &[
            "gh",
            "pr",
            "view",
            pr,
            "--json",
            "author",
            "--jq",
            ".author.login",
        ],
        cwd,
    );
    let mut pr_author = author_read
        .filter(|r| r.ok)
        .map(|r| r.stdout.trim().to_string())
        .unwrap_or_default();
    if pr_author == "null" {
        pr_author.clear();
    }
    if pr_author.is_empty() {
        let _ = writeln!(
            err,
            "verify-review-replies: could not fetch PR author; degrading open"
        );
        return 0;
    }

    let issue_comments = gh_api_json(
        run,
        cwd,
        &[
            &format!("/repos/{repo_slug}/issues/{pr}/comments"),
            "--jq",
            r#"[ .[] | {login: .user.login, created_at: .created_at, body: .body} ]"#,
        ],
    )
    .and_then(|v| v.as_array().cloned())
    .unwrap_or_default();
    let review_comments = gh_api_json(
        run,
        cwd,
        &[
            &format!("/repos/{repo_slug}/pulls/{pr}/comments"),
            "--jq",
            r#"[ .[] | {login: .user.login, created_at: .created_at, body: .body} ]"#,
        ],
    )
    .and_then(|v| v.as_array().cloned())
    .unwrap_or_default();
    let mut all_comments = issue_comments;
    all_comments.extend(review_comments);

    let mut missing: Vec<String> = Vec::new();
    for (login, last_at) in &reviewer_map {
        if login.is_empty() || last_at.is_empty() {
            continue;
        }
        if !has_qualifying_reply(&all_comments, login, last_at, &pr_author) {
            missing.push(login.clone());
        }
    }
    if missing.is_empty() {
        return 0;
    }

    // One transcript_audit_failed event per missing reviewer.
    let ts = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();
    let nonce = read_field(state_file, "provenance_nonce");
    let sid = read_field(state_file, "session_id");
    for reviewer in &missing {
        let last_at = reviewer_map
            .iter()
            .find(|(l, _)| l == reviewer)
            .map(|(_, t)| t.clone())
            .unwrap_or_default();
        emit_event(
            &journal,
            &json!({
                "ts": ts,
                "type": "transcript_audit_failed",
                "source": "hook",
                "data": {
                    "gate": "external_review_passed",
                    "reason": "missing_reply_to_reviewer",
                    "pr_number": pr,
                    "reviewer": reviewer,
                    "last_review_at": last_at,
                    "nonce": nonce,
                    "session_id": sid,
                }
            }),
            &format!("missing_reply:{reviewer}"),
        );
    }

    let _ = writeln!(
        out,
        "verify-review-replies: external_review_passed flipped back to false \u{2014} missing replies to: {}",
        missing.join(" ")
    );
    1
}

fn gh_api_json(run: RunProbe, cwd: &Path, args: &[&str]) -> Option<Value> {
    let mut argv: Vec<&str> = vec!["gh", "api"];
    argv.extend_from_slice(args);
    let out = run(&argv, cwd)?;
    if !out.ok || out.stdout.trim().is_empty() {
        return None;
    }
    serde_json::from_str(&out.stdout).ok()
}

/// The qualifying-reply predicate. A comment qualifies when (a) authored by
/// the PR author, (b) created strictly after the reviewer's latest review,
/// AND (c) it @-mentions the reviewer OR was posted within 24h of that
/// review. Loosening any clause reopens the gate-forgery hole (PR #234).
fn has_qualifying_reply(comments: &[Value], login: &str, last_at: &str, pr_author: &str) -> bool {
    let Some(last_dt) = parse_ts(last_at) else {
        // A malformed/empty reviewer timestamp cannot anchor the window; the
        // reviewer reads as un-replied-to rather than crashing.
        return false;
    };
    let window = chrono::Duration::hours(24);
    for c in comments {
        if s_of(c, "login") != pr_author {
            continue;
        }
        let created = s_of(c, "created_at");
        if created.is_empty() {
            continue;
        }
        let Some(c_dt) = parse_ts(&created) else {
            continue;
        };
        if c_dt <= last_dt {
            continue;
        }
        let body = s_of(c, "body");
        if body.contains(&format!("@{login}")) || (c_dt - last_dt) <= window {
            return true;
        }
    }
    false
}

fn parse_ts(s: &str) -> Option<chrono::DateTime<chrono::FixedOffset>> {
    chrono::DateTime::parse_from_rfc3339(&s.replace('Z', "+00:00")).ok()
}

// ---------------------------------------------------------------------------
// shared: state-file recording, audit events
// ---------------------------------------------------------------------------

/// Read `field:` from the state file's frontmatter (first match, dequoted).
fn read_field(state_file: &str, field: &str) -> String {
    let Ok(text) = std::fs::read_to_string(state_file) else {
        return String::new();
    };
    for ln in text.lines() {
        let prefix = format!("{field}:");
        if let Some(rest) = ln.strip_prefix(&prefix) {
            return dequote(rest.trim());
        }
    }
    String::new()
}

fn dequote(val: &str) -> String {
    let b = val.as_bytes();
    if val.len() >= 2
        && ((b[0] == b'"' && b[val.len() - 1] == b'"')
            || (b[0] == b'\'' && b[val.len() - 1] == b'\''))
    {
        return val[1..val.len() - 1].to_string();
    }
    val.to_string()
}

/// Append merged_prs (idempotent) + set merged_at in the frontmatter,
/// atomically: pre-scan for an existing merged_at line so we never both
/// insert and replace, write a sibling tempfile, rename over. Under the
/// 30-second dir mutex on the shared `.lock.d` convention.
fn record_merge(state_file: &str, pr: &str, merged_at: &str, err: &mut dyn Write) -> bool {
    let lock_dir = PathBuf::from(format!("{state_file}.lock.d"));
    let timeout = std::time::Duration::from_secs(30);
    let Some(token) = crate::claims::acquire_dir_mutex(&lock_dir, timeout, false) else {
        let _ = writeln!(err, "verify-pr-merged: state-file lock timeout");
        return false;
    };
    let result = (|| {
        let Ok(text) = std::fs::read_to_string(state_file) else {
            return false;
        };
        let lines: Vec<String> = text.lines().map(str::to_string).collect();
        // Pre-scan: does an existing merged_at line live inside the frontmatter?
        let mut fm_count = 0;
        let mut existing_merged_at: Option<usize> = None;
        for (i, ln) in lines.iter().enumerate() {
            if ln.trim_end() == "---" {
                fm_count += 1;
                if fm_count == 2 {
                    break;
                }
                continue;
            }
            if fm_count == 1 && ln.starts_with("merged_at:") {
                existing_merged_at = Some(i);
            }
        }
        let mut out: Vec<String> = Vec::new();
        let mut saw_merged_prs = false;
        let mut in_fm = false;
        let mut fm_seen = 0;
        for (i, ln) in lines.iter().enumerate() {
            if ln.trim_end() == "---" {
                fm_seen += 1;
                in_fm = fm_seen == 1;
                if fm_seen == 2 && existing_merged_at.is_none() {
                    out.push(format!("merged_at: \"{merged_at}\""));
                }
                out.push(ln.clone());
                continue;
            }
            if in_fm && ln.starts_with("merged_prs:") {
                // `merged_prs: [a, b]` -> append when absent.
                let inner = ln
                    .split_once('[')
                    .and_then(|(_, rest)| rest.split_once(']'))
                    .map(|(body, _)| body.trim().to_string());
                if let Some(body) = inner {
                    let mut existing: Vec<String> = body
                        .split(',')
                        .map(str::trim)
                        .filter(|s| !s.is_empty())
                        .map(str::to_string)
                        .collect();
                    if !existing.iter().any(|x| x == pr) {
                        existing.push(pr.to_string());
                    }
                    out.push(format!("merged_prs: [{}]", existing.join(", ")));
                } else {
                    out.push(ln.clone());
                }
                saw_merged_prs = true;
                continue;
            }
            if in_fm && existing_merged_at == Some(i) {
                out.push(format!("merged_at: \"{merged_at}\""));
                continue;
            }
            out.push(ln.clone());
        }
        if !saw_merged_prs {
            out.push(format!("merged_prs: [{pr}]"));
        }
        // Sibling tempfile + atomic rename.
        let path = Path::new(state_file);
        let dir = path.parent().unwrap_or(Path::new("."));
        let tmp = dir.join(format!(".target-state.{}.tmp", std::process::id()));
        let mut body = out.join("\n");
        if text.ends_with('\n') {
            body.push('\n');
        }
        if std::fs::write(&tmp, body).is_err() {
            let _ = std::fs::remove_file(&tmp);
            return false;
        }
        std::fs::rename(&tmp, path).is_ok()
    })();
    crate::claims::release_dir_mutex(&lock_dir, &token);
    result
}

/// Append a transcript_audit_failed event (best-effort; never fatal).
fn emit_audit(
    journal: &Path,
    state_file: &str,
    pr_number: &str,
    reason: &str,
    extra: Option<Value>,
) {
    let nonce = read_field(state_file, "provenance_nonce");
    let sid = read_field(state_file, "session_id");
    let ts = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();
    let mut data = json!({
        "gate": "auto_merge_outcome",
        "reason": reason,
        "pr_number": pr_number,
        "nonce": nonce,
        "session_id": sid,
    });
    if let Some(extra) = extra {
        if let (Some(base), Some(add)) = (data.as_object_mut(), extra.as_object()) {
            for (k, v) in add {
                base.insert(k.clone(), v.clone());
            }
        }
    }
    let event = json!({
        "ts": ts,
        "type": "transcript_audit_failed",
        "source": "hook",
        "data": data,
    });
    emit_event(journal, &event, reason);
}

/// Commit through the native event store; the store judges at commit. A
/// failure is one stderr line, never a failed verdict.
fn emit_event(journal: &Path, event: &Value, reason: &str) {
    let envelope = event.to_string();
    if let Err(e) = crate::event_store::append_envelope(&journal, &envelope, None) {
        eprintln!("pr-verify: event store commit failed (reason={reason}): {e}");
    }
}

#[cfg(test)]
mod tests;
