//! The completion face of `fno backlog done`: PR, link and note metadata, the
//! branch auto-detect, the ledger backfill, and the external-backend close.
//! The bare node-id close stays in `workflows.rs`; both faces share the gates
//! here, so a flag choice never skips the merge or promise gate.

use serde_json::{json, Value};
use std::cell::RefCell;
use std::path::Path;
use std::process::Command;

use super::merge_evidence::{
    fill_ref_urls, node_pr_refs, query_pr_state, render_merge_evidence_failure,
    resolve_merge_evidence, Outcome,
};
use super::node_match::{resolve_id, MatchKind, NodeMatch};
use super::pr_link::{
    is_repo_slug, pr_number_from_url, pr_url_for_repo, pr_url_from_slug, repo_slug_from_url,
    resolve_current_repo_slug,
};
use super::promise::resolve_promise_evidence;
use super::settings;
use super::workflows::{
    active_backend_name, apply_completion_fields, canonical_post_close, cascade_close_parents,
    drive_audit, emit_event, release_claim_at_closure, reparent_live_children, reparent_receipt,
    rollup_from_ledger, stamp_and_graduate_plan, text_at, DoneArgs, Rollup,
};
use crate::backlog::mutate_single_row;
use crate::graph_store;

pub(super) const DONE_HELP: &str = "Usage: fno backlog done [TASK_ID] [OPTIONS]

Mark a node complete. TASK_ID is a node id or a title substring. Omit it to
detect the node from the git branch.

Close flags:
  -F, --force             Bypass the merge cross-check. Requires --reason.
  -R, --reason TEXT       Why the cross-check is bypassed.
      --skip-stamp        Skip the plan stamp and projection.
Completion flags:
  -p, --pr-number, --pr N PR number.
      --pr-url URL        PR URL. Pair it with --pr.
      --repo OWNER/NAME   The repo the PR lives in, when it is not this checkout.
  -l, --link, --url URL   Artifact URL. Sets artifact_url.
  -m, --note TEXT         Completion note. Sets completion_note.
      --backfill          Run only the ledger rollup. With no TASK_ID, sweep done nodes.
      --force-overwrite   Overwrite rollup fields instead of filling empty ones.
  -h, --help              Print help.

The close flags pair with --note only; the other completion flags are a separate path.
Reverse a close with `fno backlog reopen <id> --reason ...`.
Exit codes: 2 usage, 3 refused, 4 outage, 5 awaiting merge, 6 promise unmet.
";

/// The merge gate, the forced-close journal and the promise gate: the exit
/// contract every close path shares. `Ok` carries the evidencing PR url.
pub(super) fn gate_pipeline(
    task_id: &str,
    node: &Value,
    refs: &[(i64, Option<String>)],
    force: bool,
    reason: Option<&str>,
    cwd: Option<&str>,
) -> Result<Option<String>, i32> {
    if force && reason.is_none() {
        eprintln!(
            "Error: --force requires --reason TEXT (explain why the cross-check is bypassed)"
        );
        return Err(2);
    }
    let mut evidence_pr_url: Option<String> = None;
    if !refs.is_empty() && !force {
        let evidence = resolve_merge_evidence(refs, cwd);
        match evidence.outcome {
            Outcome::Merged => evidence_pr_url = evidence.pr_url.clone(),
            Outcome::AwaitingMerge => {
                let note = evidence
                    .error
                    .as_ref()
                    .map(|e| format!(" (note: {e})"))
                    .unwrap_or_default();
                eprintln!(
                    "awaiting merge: PR #{} is OPEN, not merged. {task_id} stays in_review \
                     and closes on merge (reconcile / merge-triggered advance). \
                     Use --force --reason TEXT for an early close.{note}",
                    evidence.open_pr_number.unwrap_or_default(),
                );
                return Err(5);
            }
            Outcome::Outage => {
                eprintln!(
                    "{}",
                    render_merge_evidence_failure(task_id, &evidence, "open")
                );
                return Err(evidence.exit_code());
            }
            Outcome::Refused => {
                let msg = evidence
                    .reason
                    .clone()
                    .unwrap_or_else(|| format!("PR #{}: no merged evidence", refs[0].0));
                if evidence.remedy.is_some() {
                    eprintln!(
                        "{}",
                        render_merge_evidence_failure(task_id, &evidence, "open")
                    );
                } else {
                    eprintln!(
                        "Refused: {task_id} cross-check failed: {msg}\nUse --force --reason TEXT to bypass."
                    );
                }
                emit_event(
                    "backlog_done_refused",
                    json!({"node_id": task_id, "pr_number": refs[0].0, "reason": msg}),
                );
                if let Some(space) = std::env::current_dir()
                    .ok()
                    .and_then(|cwd| crate::paths::space_dir_opt(&cwd))
                {
                    crate::friction::record_gate_refusal(
                        &space.join("events.jsonl"),
                        "done",
                        Some(task_id),
                        &msg,
                        &msg,
                    );
                }
                return Err(evidence.exit_code());
            }
        }
    }
    if force {
        let reason = reason.unwrap_or_default();
        if let Some((first_number, first_url)) = refs.first() {
            evidence_pr_url = first_url.clone();
            let pr_repo = repo_slug_from_url(first_url.as_deref());
            let state = query_pr_state(*first_number, pr_repo.as_deref(), cwd)
                .map(|(s, _)| s)
                .unwrap_or_else(|_| "UNKNOWN".into());
            eprintln!(
                "Warning: force-closing {task_id} (reason: {reason}). PR #{first_number} state={state}."
            );
            emit_event(
                "backlog_done_forced",
                json!({
                    "node_id": task_id,
                    "force_reason": reason,
                    "pr_number": first_number,
                    "pr_state": state,
                }),
            );
        } else {
            eprintln!(
                "Warning: force flag set on advisory node {task_id} (reason: {reason}); no PR refs to check."
            );
        }
    } else {
        check_promise(node, cwd, &[], None)?;
    }
    Ok(evidence_pr_url)
}

fn check_promise(
    node: &Value,
    cwd: Option<&str>,
    extra: &[(i64, Option<String>)],
    repo: Option<&str>,
) -> Result<(), i32> {
    let promise = resolve_promise_evidence(node, cwd, extra, repo);
    if !promise.satisfied() {
        if let Some(reason) = &promise.reason {
            eprintln!("{reason}");
        }
        return Err(promise.exit_code());
    }
    if let Some(warning) = &promise.warning {
        eprintln!("warning: {warning}");
    }
    Ok(())
}

/// Judge the direct parent's plan promise before the lock: the cascade runs
/// inside the mutation, where gh I/O must not. A parent never judged holds.
pub(super) fn warm_parent_promises(
    rows: &[Value],
    task_id: &str,
) -> super::promise::RollupVerdicts {
    let mut held = super::promise::RollupVerdicts::new();
    let candidates: Vec<Value> = rows
        .iter()
        .find(|e| text_at(e, "id") == Some(task_id))
        .and_then(|trigger| text_at(trigger, "parent"))
        .and_then(|pid| {
            rows.iter()
                .find(|e| text_at(e, "id") == Some(pid) && super::promise::rollup_holds_plan(e))
        })
        .cloned()
        .into_iter()
        .collect();
    let verdicts = super::promise::rollup_hold_set(&candidates, &mut |parent| {
        resolve_promise_evidence(
            parent,
            super::promise::gate_cwd_for(parent).as_deref(),
            &[],
            None,
        )
    });
    for (pid, verdict) in verdicts {
        if let Some(reason) = &verdict.reason {
            let first = reason.lines().next().unwrap_or("");
            eprintln!("warning: parent {pid} stays open: {first}");
        }
        held.insert(pid, verdict);
    }
    held
}

fn current_branch() -> Option<String> {
    let out = Command::new("git")
        .args(["branch", "--show-current"])
        .output()
        .ok()?;
    let branch = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (!branch.is_empty()).then_some(branch)
}

/// The current branch's PR over REST. A failed read is announced, a branch
/// with no PR is silent, and both answer no PR.
fn current_pr() -> (Option<i64>, Option<String>) {
    let Some(branch) = current_branch() else {
        return (None, None);
    };
    let Some(slug) = resolve_current_repo_slug(None) else {
        return (None, None);
    };
    let owner = slug.split('/').next().unwrap_or_default();
    let mut cmd = Command::new("gh");
    cmd.args([
        "api",
        &format!("repos/{slug}/pulls?state=all&head={owner}:{branch}&per_page=2"),
    ]);
    let out = match crate::gh_budget::output(cmd) {
        Ok(out) => out,
        Err(e) => {
            eprintln!("fno backlog done: the pull request read failed: {e}");
            return (None, None);
        }
    };
    if !out.status.success() {
        let text = String::from_utf8_lossy(&out.stderr);
        let detail: String = text.trim().chars().take(4096).collect();
        eprintln!("fno backlog done: the pull request read failed: {detail}");
        return (None, None);
    }
    let rows: Value = serde_json::from_slice(&out.stdout).unwrap_or(Value::Null);
    let first = rows.get(0);
    (
        first.and_then(|r| r.get("number")).and_then(Value::as_i64),
        first
            .and_then(|r| r.get("html_url"))
            .and_then(Value::as_str)
            .map(str::to_string),
    )
}

/// Fill the ledger rollup into a row. Fill-if-empty by default; `force`
/// overwrites session and points and trusts the ledger alone for the session,
/// so a sweep from a live session cannot stamp its own id on every node. Cost
/// rows dedupe on (session_id, timestamp). Returns the receipt tags.
fn apply_rollup(
    entry: &mut Value,
    rollup: &Rollup,
    env_session: Option<&str>,
    force: bool,
) -> Vec<String> {
    let mut tags = Vec::new();
    let obj = entry.as_object_mut().expect("row is an object");
    let new_sid = if force {
        rollup.session_id.as_deref()
    } else {
        env_session.or(rollup.session_id.as_deref())
    };
    let has_session = obj
        .get("session_id")
        .and_then(Value::as_str)
        .is_some_and(|s| !s.is_empty());
    if let Some(sid) = new_sid.filter(|s| !s.is_empty() && (!has_session || force)) {
        obj.insert("session_id".into(), json!(sid));
        tags.push(format!(
            "session={}",
            sid.chars().take(8).collect::<String>()
        ));
    }
    if !rollup.cost_sessions.is_empty() {
        let mut existing: Vec<Value> = obj
            .get("cost_sessions")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let key = |row: &Value| {
            (
                row.get("session_id").cloned(),
                row.get("timestamp").cloned(),
            )
        };
        let before = existing.len();
        for row in &rollup.cost_sessions {
            if !existing.iter().any(|e| key(e) == key(row)) {
                existing.push(row.clone());
            }
        }
        if existing.len() > before {
            let total: f64 = existing
                .iter()
                .filter_map(|s| s.get("cost_usd").and_then(Value::as_f64))
                .sum();
            let total = (total * 10000.0).round() / 10000.0;
            obj.insert("cost_sessions".into(), Value::Array(existing));
            obj.insert("cost_usd".into(), json!(total));
            tags.push(format!("${total:.2}"));
        }
    }
    if let Some(points) = &rollup.points {
        if force || obj.get("points").is_none_or(Value::is_null) {
            obj.insert("points".into(), points.clone());
            tags.push(format!("points={points}"));
        }
    }
    tags
}

fn live_rows(graph: &Path) -> Result<Vec<Value>, i32> {
    match graph_store::read_rows(graph) {
        Ok(rows) => Ok(rows
            .into_iter()
            .filter(|r| r.get("archived_at").is_none_or(Value::is_null))
            .collect()),
        Err(e) => {
            eprintln!("Error: the backlog graph could not be read: {e}");
            Err(1)
        }
    }
}

fn print_candidates(found: &NodeMatch) {
    for c in &found.candidates {
        eprintln!(
            "  {:<14} {:<9} {}",
            text_at(c, "id").unwrap_or(""),
            text_at(c, "status").unwrap_or("?"),
            text_at(c, "title").unwrap_or("")
        );
    }
}

fn run_backfill(args: &DoneArgs, graph: &Path, env_session: Option<&str>) -> i32 {
    if active_backend_name() != "graph" {
        eprintln!(
            "fno backlog done --backfill: refused - the sweep reads and writes the graph \
             store, which is not selected (rollups live in the footnote sidecar under an \
             external backend)"
        );
        return 1;
    }
    let entries = match live_rows(graph) {
        Ok(rows) => rows,
        Err(code) => return code,
    };
    let targets: Vec<String> = if let Some(query) = &args.id {
        let branch = current_branch();
        let found = resolve_id(Some(query), &entries, branch.as_deref());
        match found.kind {
            MatchKind::None => {
                eprintln!("fno backlog done --backfill: no match for '{query}'");
                return 2;
            }
            MatchKind::Ambiguous => {
                eprintln!(
                    "fno backlog done --backfill: {} candidates for '{query}':",
                    found.candidates.len()
                );
                print_candidates(&found);
                return 2;
            }
            _ => found.id.into_iter().collect(),
        }
    } else {
        entries
            .iter()
            .filter(|e| text_at(e, "status") == Some("done"))
            .filter_map(|e| text_at(e, "id").map(str::to_string))
            .collect()
    };
    if targets.is_empty() {
        println!("fno backlog done --backfill: no done nodes to backfill");
        return 0;
    }
    // Ledger I/O stays outside the store lock.
    let rollups: Vec<(String, Rollup)> = entries
        .iter()
        .filter_map(|e| {
            let id = text_at(e, "id")?;
            targets
                .iter()
                .any(|t| t == id)
                .then(|| (id.to_string(), rollup_from_ledger(e)))
        })
        .collect();
    let touched: RefCell<Vec<(String, Vec<String>)>> = RefCell::new(Vec::new());
    let applied = mutate_single_row(graph, "backlog_done_backfill", |rows| {
        touched.borrow_mut().clear();
        for row in rows.iter_mut() {
            let Some(id) = text_at(row, "id").map(str::to_string) else {
                continue;
            };
            let Some((_, rollup)) = rollups.iter().find(|(rid, _)| *rid == id) else {
                continue;
            };
            let tags = apply_rollup(row, rollup, env_session, args.force_overwrite);
            if !tags.is_empty() {
                touched.borrow_mut().push((id, tags));
            }
        }
        Ok(!touched.borrow().is_empty())
    });
    if let Err(e) = applied {
        eprintln!("Error: {e}");
        return 1;
    }
    let touched = touched.into_inner();
    if touched.is_empty() {
        println!(
            "fno backlog done --backfill: scanned {} node(s); nothing to fill (all fields already set or ledger silent).",
            targets.len()
        );
        return 0;
    }
    for (id, tags) in &touched {
        println!("  {id}: {}", tags.join("  "));
    }
    println!(
        "fno backlog done --backfill: updated {} / {} node(s)",
        touched.len(),
        targets.len()
    );
    0
}

/// The PR url a completion stamps. Fails closed: a url-less number names no
/// repo, and PR numbers collide across repos.
fn pr_url_to_stamp(
    pr: i64,
    args: &DoneArgs,
    auto_url: Option<&str>,
    node: &Value,
) -> Result<String, i32> {
    if args.pr_url.is_some() && args.repo.is_some() {
        eprintln!("fno backlog done: --repo and --pr-url both name a repo; pass one, not both");
        return Err(2);
    }
    if let Some(repo) = args.repo.as_deref().filter(|r| !is_repo_slug(r)) {
        eprintln!(
            "fno backlog done: --repo '{repo}' is not an owner/name slug (expected <owner>/<repo>, e.g. fnodotdev/fno)"
        );
        return Err(2);
    }
    if let Some(url) = &args.pr_url {
        if repo_slug_from_url(Some(url)).is_none() {
            eprintln!(
                "fno backlog done: --pr-url '{url}' is not a GitHub PR url (expected https://github.com/<owner>/<repo>/pull/<n>)"
            );
            return Err(2);
        }
        let named = pr_number_from_url(Some(url));
        if named != Some(pr) {
            eprintln!(
                "fno backlog done: --pr-url names PR #{}, not #{pr} - a row pointing at two different PRs matches neither.",
                named.unwrap_or_default()
            );
            return Err(2);
        }
        return Ok(url.clone());
    }
    if let Some(repo) = &args.repo {
        let url = pr_url_from_slug(repo.trim(), pr);
        eprintln!("note: stamped --repo {} {url}", repo.trim());
        return Ok(url);
    }
    if let Some(url) = auto_url {
        return Ok(url.to_string());
    }
    let cwd = text_at(node, "cwd");
    let Some(url) = pr_url_for_repo(pr, cwd) else {
        eprintln!(
            "fno backlog done: cannot resolve the repo for PR #{pr} - refusing to stamp an \
             unattributable pr_number. Fix with either `gh auth login` or \
             `--pr-url https://github.com/<owner>/<repo>/pull/{pr}`."
        );
        return Err(2);
    };
    // The cwd derivation is a guess: it never re-stamps a cross-repo move.
    let recorded = repo_slug_from_url(text_at(node, "pr_url"));
    let derived = repo_slug_from_url(Some(&url));
    if let (Some(recorded), Some(derived)) = (recorded, derived) {
        if !recorded.eq_ignore_ascii_case(&derived) {
            eprintln!(
                "fno backlog done: derived {url} names repo {derived}, but this node's recorded \
                 pr_url names {recorded} - refusing to re-stamp a cross-repo move on a cwd \
                 derivation. Assert it with --repo {derived} (or a full --pr-url url)."
            );
            return Err(2);
        }
    }
    Ok(url)
}

/// The merge gate's ref set: an explicit --pr replaces the node's refs, and
/// --repo fills a url-less ref with the repo-derived url, so the read lands
/// in the named repo instead of the checkout's. A recorded url stays
/// authoritative; --repo is for the PR the checkout cannot see.
fn gate_refs_with_repo(
    node: &Value,
    pr: Option<i64>,
    repo: Option<&str>,
) -> Vec<(i64, Option<String>)> {
    let refs = match pr {
        Some(n) => vec![(n, text_at(node, "pr_url").map(str::to_string))],
        None => node_pr_refs(node),
    };
    fill_ref_urls(refs, repo)
}

/// The completion flags, the fuzzy query, and the branch auto-detect.
pub(super) fn run_rich(args: &DoneArgs) -> i32 {
    let graph = settings::graph_path();
    let env_session = std::env::var("CLAUDECODE_SESSION_ID")
        .ok()
        .filter(|s| !s.is_empty());
    if args.backfill {
        return run_backfill(args, &graph, env_session.as_deref());
    }
    if active_backend_name() != "graph" {
        if args.pr.is_some()
            || args.pr_url.is_some()
            || args.link.is_some()
            || args.note.is_some()
            || args.repo.is_some()
        {
            eprintln!(
                "fno backlog done: --pr/--pr-url/--link/--note/--repo record graph-backend completion \
                 metadata and cannot be recorded under an external tracker backend"
            );
            return 2;
        }
        let Some(id) = &args.id else {
            eprintln!(
                "fno backlog done: an external tracker backend needs an explicit node id \
                 (branch auto-detect is footnote-metadata-based)"
            );
            return 2;
        };
        return done_via_seam(id, false, false, None, None);
    }
    let entries = match live_rows(&graph) {
        Ok(rows) => rows,
        Err(code) => return code,
    };
    let branch = current_branch();
    let found = resolve_id(args.id.as_deref(), &entries, branch.as_deref());
    match found.kind {
        MatchKind::None => {
            let target = args
                .id
                .as_deref()
                .or(branch.as_deref())
                .unwrap_or("<no input>");
            eprintln!("fno backlog done: no match for '{target}'");
            if !found.note.is_empty() {
                eprintln!("  ({})", found.note);
            }
            return 2;
        }
        MatchKind::Ambiguous => {
            eprintln!(
                "fno backlog done: {} candidates for '{}':",
                found.candidates.len(),
                args.id.as_deref().unwrap_or("")
            );
            print_candidates(&found);
            return 2;
        }
        _ => {}
    }
    if args.pr_url.is_some() && args.pr.is_none() {
        eprintln!("fno backlog done: --pr-url requires --pr");
        return 2;
    }
    // The slug check from pr_url_to_stamp, fired before any gate read: with
    // --repo alone the gates consume the slug before the stamp path runs.
    if let Some(repo) = args.repo.as_deref().filter(|r| !is_repo_slug(r)) {
        eprintln!(
            "fno backlog done: --repo '{repo}' is not an owner/name slug (expected <owner>/<repo>, e.g. fnodotdev/fno)"
        );
        return 2;
    }
    let node_id = found.id.clone().unwrap_or_default();
    let Some(node) = entries
        .iter()
        .find(|e| text_at(e, "id") == Some(node_id.as_str()))
    else {
        eprintln!("fno backlog done: no match for '{node_id}'");
        return 2;
    };
    let domain = text_at(node, "domain").unwrap_or("code").to_string();
    let (mut pr, mut auto_url) = (args.pr, None);
    if domain == "code" && pr.is_none() && args.link.is_none() && args.note.is_none() {
        (pr, auto_url) = current_pr();
    }
    if domain != "code" && pr.is_none() && args.link.is_none() && args.note.is_none() {
        eprintln!(
            "fno backlog done: {node_id} is domain={domain}; pass --link, --note, or --pr to mark it done."
        );
        return 2;
    }
    let already_done = text_at(node, "status") == Some("done")
        || node.get("completed_at").is_some_and(|v| !v.is_null());
    let cwd = text_at(node, "cwd").map(str::to_string);

    // The merge gate asks "is THIS pr merged", so an explicit --pr replaces
    // the refs and is scoped to the node's repo. The promise gate asks how
    // many ships landed, so it unions the pr onto the refs under the pr's own
    // url: a multi-repo split may land the new PR in another repo. An already
    // closed node is a metadata update, gated when it first closed.
    let mut merged = false;
    if !already_done {
        let gate_refs = gate_refs_with_repo(node, pr, args.repo.as_deref());
        if !gate_refs.is_empty() {
            let evidence = resolve_merge_evidence(&gate_refs, cwd.as_deref());
            match evidence.outcome {
                Outcome::Merged => merged = true,
                Outcome::AwaitingMerge => {
                    let note = evidence
                        .error
                        .as_ref()
                        .map(|e| format!(" (note: {e})"))
                        .unwrap_or_default();
                    eprintln!(
                        "awaiting merge: PR #{} is OPEN, not merged. {node_id} stays in_review and closes on merge (reconcile / merge-triggered advance).{note}",
                        evidence.open_pr_number.unwrap_or_default()
                    );
                    return evidence.exit_code();
                }
                Outcome::Outage => {
                    eprintln!(
                        "{}",
                        render_merge_evidence_failure(&node_id, &evidence, "open")
                    );
                    return evidence.exit_code();
                }
                Outcome::Refused => {
                    if evidence.remedy.is_some() {
                        eprintln!(
                            "{}",
                            render_merge_evidence_failure(&node_id, &evidence, "open")
                        );
                    } else {
                        eprintln!(
                            "Refused: {node_id} cross-check failed: {}",
                            evidence.reason.clone().unwrap_or_default()
                        );
                    }
                    return evidence.exit_code();
                }
            }
        }
        let extra: Vec<(i64, Option<String>)> = pr
            .map(|n| {
                (
                    n,
                    args.pr_url
                        .clone()
                        .or_else(|| args.repo.as_deref().map(|r| pr_url_from_slug(r.trim(), n)))
                        .or_else(|| auto_url.clone())
                        .or_else(|| text_at(node, "pr_url").map(str::to_string)),
                )
            })
            .into_iter()
            .collect();
        if let Err(code) = check_promise(node, cwd.as_deref(), &extra, args.repo.as_deref()) {
            return code;
        }
    }
    let held = warm_parent_promises(&entries, &node_id);
    let pr_url = match pr {
        Some(n) => match pr_url_to_stamp(n, args, auto_url.as_deref(), node) {
            Ok(url) => Some(url),
            Err(code) => return code,
        },
        None => None,
    };
    let rollup = rollup_from_ledger(node);
    let now = graph_store::now_isoformat();

    let collision: RefCell<Option<String>> = RefCell::new(None);
    let tags: RefCell<Vec<String>> = RefCell::new(Vec::new());
    let moved: RefCell<Vec<(String, Option<String>)>> = RefCell::new(Vec::new());
    let cascade: RefCell<Vec<String>> = RefCell::new(Vec::new());
    let after: RefCell<Option<Value>> = RefCell::new(None);
    let applied = mutate_single_row(&graph, "backlog_done", |rows| {
        let Some(idx) = rows
            .iter()
            .position(|e| text_at(e, "id") == Some(node_id.as_str()))
        else {
            return Ok(false);
        };
        let raced = text_at(&rows[idx], "status") == Some("done");
        {
            let obj = rows[idx].as_object_mut().expect("row is an object");
            if let Some(n) = pr {
                obj.insert("pr_number".into(), json!(n));
                obj.insert("pr_url".into(), json!(pr_url));
            }
            if let Some(link) = &args.link {
                obj.insert("artifact_url".into(), json!(link));
            }
            if let Some(note) = &args.note {
                obj.insert("completion_note".into(), json!(note));
            }
        }
        if raced {
            let first = text_at(&rows[idx], "completed_at")
                .unwrap_or("")
                .to_string();
            collision.replace(Some(first));
            if args.force_overwrite {
                tags.replace(apply_rollup(
                    &mut rows[idx],
                    &rollup,
                    env_session.as_deref(),
                    true,
                ));
            }
            return Ok(true);
        }
        apply_completion_fields(&mut rows[idx], merged);
        tags.replace(apply_rollup(
            &mut rows[idx],
            &rollup,
            env_session.as_deref(),
            args.force_overwrite,
        ));
        after.replace(Some(rows[idx].clone()));
        moved.replace(reparent_live_children(rows, &node_id));
        cascade.replace(cascade_close_parents(rows, &node_id, &held));
        Ok(true)
    });
    if let Err(e) = applied {
        eprintln!("Error: {e}");
        return 1;
    }
    if let Some(first) = collision.into_inner() {
        emit_event(
            "done_race_collision",
            json!({"node_id": node_id, "first_completed_at": first, "second_attempt_at": now}),
        );
        let at = if first.is_empty() {
            String::new()
        } else {
            format!(" at {first}")
        };
        eprintln!(
            "fno backlog done: {node_id} already done{at}; metadata updates applied; collision event emitted"
        );
        println!("fno backlog done: {node_id} -> already done (metadata updated)");
        return 0;
    }
    let moved = moved.into_inner();
    if !moved.is_empty() {
        println!("fno backlog done: {}", reparent_receipt(&moved));
    }
    let cascade = cascade.into_inner();
    let row = after.into_inner().unwrap_or_else(|| node.clone());
    canonical_post_close(
        &graph,
        &node_id,
        text_at(&row, "plan_path"),
        text_at(&row, "session_id"),
        &cascade,
        false,
        pr_url.as_deref(),
    );
    for id in std::iter::once(node_id.as_str()).chain(cascade.iter().map(String::as_str)) {
        release_claim_at_closure(id);
    }
    let mut bits = vec![format!("domain={domain}")];
    if let Some(n) = pr {
        bits.push(format!("PR #{n}"));
        bits.push(format!("pr_url={}", pr_url.as_deref().unwrap_or("")));
    }
    if let Some(link) = &args.link {
        bits.push(format!("link={link}"));
    }
    if let Some(note) = &args.note {
        bits.push(format!("note='{note}'"));
    }
    bits.extend(tags.into_inner());
    println!("fno backlog done: {node_id} -> done  {}", bits.join("  "));
    drive_audit(&node_id);
    0
}

fn door(payload: Value) -> Result<Value, String> {
    let out = crate::tracker::run_door(&payload);
    match out.get("error").and_then(Value::as_str) {
        Some(error) => Err(error.to_string()),
        None => Ok(out),
    }
}

/// The joined selection model under an external backend: the snapshot exactly
/// once, open items only. Selection fails closed: the message names the backend
/// and nothing falls back to the local graph.
pub(super) fn external_open_entries() -> Result<Vec<Value>, String> {
    let failed = |e: String| format!("tracker '{}' snapshot failed: {e}", active_backend_name());
    let out = door(json!({"tracker": "snapshot"})).map_err(failed)?;
    let entries = out
        .get("entries")
        .and_then(Value::as_array)
        .ok_or_else(|| failed("the answer carries no entries".to_string()))?;
    Ok(entries
        .iter()
        .filter(|e| text_at(e, "state") == Some("open"))
        .cloned()
        .collect())
}

/// The exact-id tracker read plus its sidecar. `Ok(None)` is not found.
pub(super) fn read_external(id: &str) -> Result<Option<(Value, Value)>, String> {
    let read = door(json!({"tracker": "read", "id": id}))?;
    if read.get("not_found").is_some() {
        return Ok(None);
    }
    let side = door(json!({"tracker": "sidecar-get", "id": id}))?;
    if side.get("not_found").is_some() {
        return Ok(None);
    }
    Ok(Some((read["node"].clone(), side["sidecar"].clone())))
}

/// Close ancestors whose children are now all closed, the external twin of
/// the graph cascade. Best-effort: it never fails the close.
fn cascade_external_parents(child_id: &str) {
    let walk = || -> Result<(), String> {
        let listed = door(json!({"tracker": "list-open"}))?;
        let candidates = listed["candidates"].as_array().cloned().unwrap_or_default();
        let has_open_child = |id: &str| candidates.iter().any(|c| text_at(c, "parent") == Some(id));
        let read = door(json!({"tracker": "read", "id": child_id}))?;
        let mut cur = text_at(&read["node"], "parent").map(str::to_string);
        let mut seen: Vec<String> = Vec::new();
        while let Some(id) = cur.take() {
            if seen.contains(&id) || has_open_child(&id) {
                break;
            }
            seen.push(id.clone());
            let Ok(parent) = door(json!({"tracker": "read", "id": id})) else {
                break;
            };
            let node = &parent["node"];
            if text_at(node, "state") != Some("closed") {
                if let Err(e) = door(json!({"tracker": "close", "id": id})) {
                    eprintln!("warning: cascade close failed for {id}: {e}");
                    break;
                }
            }
            cur = text_at(node, "parent").map(str::to_string);
        }
        Ok(())
    };
    if let Err(e) = walk() {
        eprintln!("warning: cascade evaluation failed: {e}");
    }
}

/// The close under an external backend: the shared gates, the rollups into
/// the sidecar before the irreversible close, exactly one tracker close, and
/// success printed only after it returns. A failed close stays open.
pub(super) fn done_via_seam(
    task_id: &str,
    skip_stamp: bool,
    force: bool,
    reason: Option<&str>,
    note: Option<&str>,
) -> i32 {
    let (tnode, mut sc) = match read_external(task_id) {
        Ok(Some(pair)) => pair,
        Ok(None) => {
            eprintln!("Error: feature {task_id} not found");
            return 1;
        }
        Err(e) => {
            eprintln!("Error: the tracker read for {task_id} failed: {e}");
            return 1;
        }
    };
    if text_at(&tnode, "state") == Some("closed") {
        eprintln!("{task_id} is already done");
        return 0;
    }
    let field = |key: &str| sc.get(key).cloned().unwrap_or(Value::Null);
    let row = json!({
        "id": task_id,
        "title": tnode.get("title"),
        "cwd": field("cwd"),
        "plan_path": field("plan_path"),
        "pr_number": field("pr_number"),
        "pr_url": field("pr_url"),
        "additional_prs": field("additional_prs"),
        "sessions": field("sessions"),
        "contained_in": field("contained_in"),
    });
    let refs = node_pr_refs(&row);
    let evidence = match gate_pipeline(task_id, &row, &refs, force, reason, text_at(&row, "cwd")) {
        Ok(url) => url,
        Err(code) => return code,
    };
    let rollup = rollup_from_ledger(&row);
    if let Some(obj) = sc.as_object_mut() {
        if let Some(cost) = rollup.cost_usd {
            if obj.get("cost_usd").is_none_or(Value::is_null) {
                obj.insert("cost_usd".into(), json!(cost));
            }
        }
        if !rollup.cost_sessions.is_empty()
            && obj
                .get("cost_sessions")
                .and_then(Value::as_array)
                .is_none_or(Vec::is_empty)
        {
            obj.insert("cost_sessions".into(), json!(rollup.cost_sessions));
        }
        if evidence.is_some() && obj.get("pr_url").is_none_or(Value::is_null) {
            obj.insert("pr_url".into(), json!(evidence));
        }
        if let Some(note) = note.map(str::trim).filter(|n| !n.is_empty()) {
            obj.insert("completion_note".into(), json!(note));
        }
        obj.remove("id");
    }
    if let Err(e) = door(json!({"tracker": "sidecar-set", "id": task_id, "fields": sc})) {
        eprintln!("Error: the sidecar write for {task_id} failed: {e}");
        return 1;
    }
    if let Err(e) = door(json!({"tracker": "close", "id": task_id})) {
        eprintln!(
            "Error: external close failed for {task_id} on backend '{}': {e}\nThe item stays open; retry once the backend is available.",
            active_backend_name()
        );
        return 1;
    }
    cascade_external_parents(task_id);
    println!("Marked {task_id} done");
    release_claim_at_closure(task_id);
    if !skip_stamp {
        if let Some(plan) = sc.get("plan_path").and_then(Value::as_str) {
            stamp_and_graduate_plan(plan, evidence.as_deref(), None);
        }
    }
    0
}

#[cfg(test)]
mod tests {
    use super::super::merge_evidence::{resolve_merge_evidence_with, PrReadError};
    use super::*;

    fn rollup() -> Rollup {
        Rollup {
            session_id: Some("ledger-session".into()),
            cost_usd: Some(1.5),
            cost_sessions: vec![json!({"session_id": "s1", "timestamp": "t1", "cost_usd": 1.5})],
            points: Some(json!(3)),
        }
    }

    #[test]
    fn a_rollup_fills_only_what_is_empty_and_dedupes_cost_rows() {
        let mut row = json!({"id": "x-1", "session_id": "kept", "points": 9});
        let tags = apply_rollup(&mut row, &rollup(), Some("env"), false);
        assert_eq!(row["session_id"], "kept");
        assert_eq!(row["points"], 9);
        assert_eq!(row["cost_usd"], 1.5);
        assert_eq!(tags, vec!["$1.50".to_string()]);
        assert!(apply_rollup(&mut row, &rollup(), Some("env"), false).is_empty());
    }

    fn rich(pr_url: Option<&str>, repo: Option<&str>) -> DoneArgs {
        let mut args = super::super::workflows::parse_done_args(&[]);
        args.pr_url = pr_url.map(str::to_string);
        args.repo = repo.map(str::to_string);
        args
    }

    #[test]
    fn the_stamp_refuses_a_mismatched_url_and_a_doubled_repo() {
        let node = json!({"id": "x-1"});
        let url = "https://github.com/o/r/pull/8";
        assert_eq!(
            pr_url_to_stamp(7, &rich(Some(url), None), None, &node),
            Err(2)
        );
        assert_eq!(
            pr_url_to_stamp(8, &rich(Some(url), Some("o/r")), None, &node),
            Err(2)
        );
        assert_eq!(
            pr_url_to_stamp(8, &rich(Some(url), None), None, &node),
            Ok(url.to_string())
        );
    }

    #[test]
    fn repo_alone_aims_the_merge_read_at_the_named_repo() {
        // A pr_number recorded without a url, its PR merged in a repo the
        // checkout cannot name: --repo alone must aim the gate read there.
        let node = json!({"id": "ab-11111111", "pr_number": 7});
        let refs = gate_refs_with_repo(&node, None, Some("fork/widget"));
        assert_eq!(
            refs,
            vec![(7, Some("https://github.com/fork/widget/pull/7".to_string()))]
        );
        let saw = RefCell::new(None);
        let ev = resolve_merge_evidence_with(
            &refs,
            None,
            &|n: i64, repo: Option<&str>, _cwd: Option<&str>| {
                *saw.borrow_mut() = repo.map(str::to_string);
                match repo {
                    Some("fork/widget") => Ok((
                        "MERGED".to_string(),
                        Some(format!("https://github.com/fork/widget/pull/{n}")),
                    )),
                    _ => Err(PrReadError::new("PR not found in that repo", "not_found")),
                }
            },
        );
        assert_eq!(ev.outcome, Outcome::Merged);
        assert_eq!(saw.into_inner().as_deref(), Some("fork/widget"));
    }

    #[test]
    fn a_recorded_url_keeps_precedence_over_repo() {
        // The recorded url is the node's own evidence; --repo fills only the
        // url-less refs, on both the recorded-refs and explicit --pr arms.
        let node = json!({
            "id": "ab-11111111",
            "pr_number": 7,
            "pr_url": "https://github.com/acme/widget/pull/7",
        });
        let refs = gate_refs_with_repo(&node, None, Some("fork/widget"));
        assert_eq!(
            refs,
            vec![(7, Some("https://github.com/acme/widget/pull/7".to_string()))]
        );
        let explicit = gate_refs_with_repo(&node, Some(7), Some("fork/widget"));
        assert_eq!(explicit, refs);
    }
}
