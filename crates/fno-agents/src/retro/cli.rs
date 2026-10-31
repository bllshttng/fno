//! `fno backlog retro`: the three commands.
//!
//! `run` consumes the retro-pending sentinels `fno backlog reconcile` drops
//! for PRs merged outside the ship gate, plus the worktree's
//! `.fno/.triage-pending` fast path, and runs the shared routine for each.
//! A sentinel is removed only after a clean land, so a crash re-triages
//! instead of dropping work, and a partial harvest (gh down) keeps the
//! sentinel for retry. `drain-postmortems` runs the postmortem pass alone.
//! `sweep-carveouts` reads the whole carve-out ledger with no PR.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use super::harvest::{self, GhRunner};
use super::land::{
    resolve_mode, LandOptions, MODE_AUTONOMOUS, MODE_INTERACTIVE, OUTCOME_ACTIVE, OUTCOME_FAILED,
    OUTCOME_INBOX, OUTCOME_QUEUED,
};
use super::ledger::{self, BACKFILL_KIND, VALID_KINDS};
use super::routine::{self, TriageInput, TriageReport};
use super::sweep::{self, SweepSeams};
use super::{py_str, truthy};

const USAGE: &str = "Usage: fno backlog retro COMMAND [ARGS]...

  Consume retro-triage triggers; file left-out work as backlog nodes.

Commands:
  run                Consume retro-pending sentinels and file left-out work.
  drain-postmortems  Drain unconsumed postmortems only.
  sweep-carveouts    Sweep the whole carve-out ledger, PR-independent; dry run by default.";

const RUN_USAGE: &str = "Usage: fno backlog retro run [OPTIONS]

  Consume retro-pending sentinels and file left-out work.

Options:
  --node TEXT          Triage only this node's sentinel.
  --pr-number INTEGER  Harvest this merged PR even with no sentinel (the
                       manual-merge path). Carve-outs for the PR's session(s)
                       are harvested and consumed.
  --session-id TEXT    Scope the --pr-number carve-out harvest (repeatable).
  --repo TEXT          owner/name for the --pr-number review harvest.
                       Defaults to the current repo via `gh repo view`.
  --keep-going         Treat this harvest as a no-human run, so the keep-going
                       engine dispatches follow-ups. A no-op unless
                       keep_going.enabled is set.";

const SWEEP_USAGE: &str = "Usage: fno backlog retro sweep-carveouts [OPTIONS]

  Sweep the whole carve-out ledger, PR-independent; dry run by default.
  A row is consumed only once its work has a node or inbox line to live in.

Options:
  --apply          File and consume for real. Omitted, nothing is written.
  -k, --kind TEXT  Narrow to one kind (deferred | oos-bug). backfill is never
                   swept; it belongs to /fno:ship pr merged.
  --autonomous     Land filed nodes active instead of queued behind a
                   `fno backlog pick` ack.";

const DRAIN_USAGE: &str = "Usage: fno backlog retro drain-postmortems

  Drain unconsumed postmortems only: no sentinel triage, no carve-out
  harvest. Idempotent: consumed_at stamps make a re-drain a no-op.";

fn is_help(a: &str) -> bool {
    a == "--help" || a == "-h"
}

pub fn run(tail: &[String]) -> i32 {
    let Some(cmd) = tail.first() else {
        eprintln!("{USAGE}");
        return 2;
    };
    let rest = &tail[1..];
    match cmd.as_str() {
        c if is_help(c) => {
            println!("{USAGE}");
            0
        }
        "run" => cmd_run(rest),
        "drain-postmortems" => cmd_drain(rest),
        "sweep-carveouts" => cmd_sweep(rest),
        other => {
            eprintln!("Error: No such command '{other}'. Try `fno backlog retro --help`.");
            2
        }
    }
}

/// The two roots every command needs: the worktree (for COMPLETION.md and the
/// fast path) and the canonical checkout (a filed node outlives the
/// worktree it was captured in, and the carve-out ledger is shared).
fn roots() -> (PathBuf, PathBuf) {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let repo_root = crate::paths::worktree_repo_root(&cwd);
    let node_root = crate::paths::canonical_repo_root(&cwd).unwrap_or_else(|| repo_root.clone());
    (repo_root, node_root)
}

fn read_nodes() -> Result<Vec<Value>, String> {
    crate::graph_store::read_rows(&crate::backlog::settings::graph_path())
        .map_err(|e| e.to_string())
}

fn project_for(node_root: &Path) -> Option<String> {
    crate::backlog::create_cli::detect_project(node_root)
}

pub(crate) fn current_repo_slug(gh: &GhRunner) -> Option<String> {
    let args: Vec<String> = [
        "repo",
        "view",
        "--json",
        "nameWithOwner",
        "-q",
        ".nameWithOwner",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    let (rc, out, _) = gh(&args);
    let slug = out.trim();
    (rc == 0 && !slug.is_empty()).then(|| slug.to_string())
}

fn slug_from(payload: &Value) -> Option<String> {
    crate::backlog::pr_link::repo_slug_from_url(payload.get("pr_url").and_then(Value::as_str))
}

fn payload_pr(payload: &Value) -> i64 {
    match payload.get("pr_number") {
        Some(Value::Number(n)) => n.as_i64().unwrap_or(0),
        Some(Value::String(s)) => s.trim().parse().unwrap_or(0),
        _ => 0,
    }
}

fn read_payload(path: &Path) -> Option<Value> {
    serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()
}

/// Whether a sentinel belongs to `want` (owner/name). An unconstrained repo,
/// or a sentinel with no repo hint, never drops on repo grounds: PR numbers
/// are unique only within a repo, so a known mismatch must.
fn sentinel_repo_matches(payload: Option<&Value>, want: Option<&str>) -> bool {
    let Some(want) = want else { return true };
    match payload.and_then(slug_from) {
        Some(slug) => slug.eq_ignore_ascii_case(want),
        None => true,
    }
}

/// The session ids whose ledger rows own `pr` in `repo`. A broken ledger is
/// named on stderr; a clean miss stays quiet. Either way the caller reads no
/// owner and goes read-only.
fn pr_session_ids(ledger: &Path, pr: i64, repo: Option<&str>) -> Vec<String> {
    let (sessions, reason) = crate::ledger_axes::pr_sessions(Some(ledger), pr, repo);
    if crate::ledger_axes::reason_is_infra_failure(reason.as_deref()) {
        if let Some(r) = reason {
            eprintln!("WARN {r}");
        }
    }
    sessions
}

/// A plan's COMPLETION.md: inside a folder plan, beside a single-file plan
/// in `<plan>.artifacts/`. A relative path resolves against the repo root.
fn completion_md_for(plan_path: Option<&str>, repo_root: &Path) -> Option<PathBuf> {
    let p = Path::new(plan_path.filter(|p| !p.is_empty())?);
    let p = if p.is_absolute() {
        p.to_path_buf()
    } else {
        repo_root.join(p)
    };
    let cand = if p.is_dir() {
        p.join("COMPLETION.md")
    } else {
        PathBuf::from(format!("{}.artifacts", p.display())).join("COMPLETION.md")
    };
    cand.exists().then_some(cand)
}

fn emit_report(report: &TriageReport) {
    eprintln!(
        "(triaging PR #{}: carve-outs={} reviews={} deferred={})",
        report.pr_number, report.carveout_count, report.review_count, report.deferred_count
    );
    for w in &report.warnings {
        eprintln!("WARN {w}");
    }
    if report.readonly_carveout_count > 0 {
        eprintln!(
            "(PR #{}: {} carve-out(s) shown read-only - no owning session resolved for this PR, so they were NOT filed or consumed under this PR)",
            report.pr_number, report.readonly_carveout_count
        );
    }
    let mut queued_any = false;
    for r in &report.results {
        let id = r.node_id.as_deref().unwrap_or("None");
        match r.outcome.as_str() {
            OUTCOME_ACTIVE => println!("created {id} (active): {}", r.candidate.title),
            OUTCOME_QUEUED => {
                queued_any = true;
                println!("queued {id} for review: {}", r.candidate.title);
            }
            OUTCOME_INBOX => println!("inbox line (nit): {}", r.candidate.title),
            OUTCOME_FAILED => eprintln!(
                "FAILED to land: {} ({})",
                r.candidate.title,
                r.error.as_deref().unwrap_or("None")
            ),
            _ => {}
        }
    }
    for f in &report.followups {
        match f.outcome.as_str() {
            "dispatched" => println!("keep-going: dispatched {} for {}", f.arm, f.node_id),
            "failed" => eprintln!(
                "keep-going: {} dispatch failed for {} (left as a filed node)",
                f.arm, f.node_id
            ),
            _ => {}
        }
    }
    if !report.uncited.is_empty() {
        eprintln!(
            "skipped {} uncited candidate(s) (triage_uncited_candidate; no source cite)",
            report.uncited.len()
        );
    }
    if report.nothing_to_do() {
        println!("(PR #{}: no left-out work to triage)", report.pr_number);
    }
    if queued_any {
        println!("run `fno backlog pick` to review queued items.");
    }
}

struct Ctx<'a> {
    repo_root: &'a Path,
    node_root: &'a Path,
    carveout_root: &'a Path,
    ledger: PathBuf,
    local_slug: Option<String>,
    gh: &'a GhRunner,
}

/// Triage one payload, from a sentinel or a synthetic `--pr-number`
/// request. Returns `(report, clean)`: clean means a complete harvest that
/// fully landed. Carve-outs a clean run processed are consumed here.
fn process_payload(
    payload: &Value,
    ctx: &Ctx,
    existing: &[Value],
    comments: Option<Vec<Value>>,
) -> (TriageReport, bool) {
    let pr = payload_pr(payload);
    let mode = resolve_mode(Some(payload));
    let repo_slug = slug_from(payload);
    let completion_md = completion_md_for(
        payload.get("plan_path").and_then(Value::as_str),
        ctx.repo_root,
    );

    let mut session_ids: Option<Vec<String>> = match payload.get("session_ids") {
        Some(Value::Array(a)) if !a.is_empty() => Some(a.iter().map(|s| py_str(Some(s))).collect()),
        _ => truthy(payload.get("session_id")).then(|| vec![py_str(payload.get("session_id"))]),
    };
    let mut readonly = truthy(payload.get("carveouts_readonly"));
    // An unscoped harvest would drain the whole shared ledger onto this PR.
    // Resolve the owning sessions; no owner, or no real PR, reads read-only.
    if session_ids.is_none() && !readonly {
        let resolved = if pr > 0 {
            pr_session_ids(&ctx.ledger, pr, repo_slug.as_deref())
        } else {
            Vec::new()
        };
        if resolved.is_empty() {
            readonly = true;
        } else {
            session_ids = Some(resolved);
        }
    }

    // Derive the addressed and skipped sets from live PR state before the
    // review harvest, or every implemented finding is re-filed.
    let mut derive_warnings = Vec::new();
    let mut resolved_unavailable = false;
    let mut resolved_ids = HashSet::new();
    let mut skipped_ids = HashSet::new();
    let mut commit_dates = None;
    let mut author = None;
    let comments = match comments {
        Some(c) => Some(c),
        None => {
            let (threads, unavailable) = harvest::fetch_review_thread_state(
                pr,
                repo_slug.as_deref(),
                ctx.gh,
                &mut derive_warnings,
            );
            if unavailable {
                // Without the latest resolved state an implemented finding
                // cannot be told from a declined one: harvest no reviews and
                // keep the sentinel for a retry.
                resolved_unavailable = true;
                Some(Vec::new())
            } else {
                resolved_ids = harvest::addressed_ids_from_threads(&threads);
                let (rows, _) = harvest::fetch_skipped_rows(
                    pr,
                    repo_slug.as_deref(),
                    ctx.gh,
                    &mut derive_warnings,
                );
                skipped_ids = harvest::skipped_ids_from_rows(&rows, &threads);
                let (dates, _) = harvest::fetch_pr_commit_dates(
                    pr,
                    repo_slug.as_deref(),
                    ctx.gh,
                    &mut derive_warnings,
                );
                commit_dates = Some(dates);
                author = harvest::fetch_pr_author(
                    pr,
                    repo_slug.as_deref(),
                    ctx.gh,
                    &mut derive_warnings,
                );
                None
            }
        }
    };

    // Attribute to this checkout's project only when the PR belongs to it:
    // the sentinel dir is global and can hold other repos' sentinels.
    let is_local = match (&repo_slug, &ctx.local_slug) {
        (Some(a), Some(b)) => a.eq_ignore_ascii_case(b),
        _ => true,
    };
    let project = if is_local {
        project_for(ctx.node_root)
    } else {
        None
    };
    let keep_going = mode == MODE_AUTONOMOUS && super::keep_going::enabled(Some(ctx.repo_root));
    let node_cwd = ctx.node_root.to_string_lossy().into_owned();
    let mut land = LandOptions::live(mode, project, Some(node_cwd));
    land.dedup_entries = Some(existing);
    let mut report = routine::triage_pr(
        TriageInput {
            carveout_root: ctx.carveout_root,
            pr_number: pr,
            session_ids,
            completion_md,
            comments,
            resolved_ids,
            skipped_ids,
            commit_dates,
            author_login: author,
            existing_nodes: existing,
            gh: ctx.gh,
            repo: repo_slug,
            carveouts_readonly: readonly,
            origin_node_id: payload
                .get("node_id")
                .and_then(Value::as_str)
                .map(str::to_string),
            keep_going,
            project_root: Some(ctx.repo_root),
        },
        land,
    );
    if !derive_warnings.is_empty() {
        derive_warnings.append(&mut report.warnings);
        report.warnings = derive_warnings;
    }
    emit_report(&report);

    let clean = !report.partial() && !report.failed() && !resolved_unavailable;
    if clean && !report.harvested_carveout_ids.is_empty() {
        // Best-effort, never blocking trigger removal, but never silent: a
        // short consume churns the ids back in next run.
        let want: HashSet<&String> = report.harvested_carveout_ids.iter().collect();
        let removed = ledger::consume_carveouts(ctx.carveout_root, &report.harvested_carveout_ids);
        if removed < want.len() {
            eprintln!(
                "WARN consume_carveouts removed only {removed}/{} processed carve-out id(s) from {}; the remainder may be re-harvested next run",
                want.len(),
                ledger::ledger_path(ctx.carveout_root).display()
            );
        }
    }
    (report, clean)
}

/// The postmortem drain: `(failed, harvested)`. Self-contained: a failure
/// here never sinks the sentinel loop.
fn postmortem_pass(node_root: &Path) -> (bool, usize) {
    let Some(dir) = super::postmortem::postmortems_dir(node_root) else {
        eprintln!("WARN postmortem harvest: no state dir resolves for the postmortems");
        return (true, 0);
    };
    let existing = read_nodes().unwrap_or_default();
    let mut land = LandOptions::live(
        MODE_INTERACTIVE,
        project_for(node_root),
        Some(node_root.to_string_lossy().into_owned()),
    );
    land.dedup_entries = Some(&existing);
    let pm = routine::triage_postmortems(&dir, &existing, &land, &routine::stamp_now);
    if pm.harvested > 0 {
        println!(
            "postmortems: {} unconsumed -> {} node(s), {} inbox, {} archived, {} dupe(s), {} failed; {} stamped consumed_at",
            pm.harvested,
            pm.count("node"),
            pm.count("inbox"),
            pm.count("archived"),
            pm.count("dupe"),
            pm.count("failed"),
            pm.stamped
        );
    }
    for w in &pm.warnings {
        eprintln!("WARN postmortem: {w}");
    }
    (pm.failed(), pm.harvested)
}

fn cmd_drain(rest: &[String]) -> i32 {
    if rest.iter().any(|a| is_help(a)) {
        println!("{DRAIN_USAGE}");
        return 0;
    }
    if let Some(extra) = rest.first() {
        eprintln!("Error: Got unexpected extra argument ({extra})");
        return 2;
    }
    let (_, node_root) = roots();
    let (failed, harvested) = postmortem_pass(&node_root);
    if harvested == 0 && !failed {
        println!("postmortems: 0 unconsumed");
    }
    i32::from(failed)
}

/// Unharvested non-backfill carve-outs; 0 on any failure.
fn pending_carveout_count(root: &Path) -> usize {
    ledger::read_carveouts(root, None)
        .map(|rows| {
            rows.iter()
                .filter(|r| r.get("kind").and_then(Value::as_str) != Some(BACKFILL_KIND))
                .count()
        })
        .unwrap_or(0)
}

fn cmd_sweep(rest: &[String]) -> i32 {
    let mut apply = false;
    let mut autonomous = false;
    let mut kind: Option<String> = None;
    let mut i = 0;
    while i < rest.len() {
        let a = rest[i].as_str();
        match a {
            _ if is_help(a) => {
                println!("{SWEEP_USAGE}");
                return 0;
            }
            "--apply" => apply = true,
            "--autonomous" => autonomous = true,
            "--kind" | "-k" => {
                i += 1;
                let Some(v) = rest.get(i) else {
                    eprintln!("Error: Option '{a}' requires an argument.");
                    return 2;
                };
                kind = Some(v.clone());
            }
            _ if a.starts_with("--kind=") => kind = Some(a["--kind=".len()..].to_string()),
            _ => {
                eprintln!("Error: No such option: {a}");
                return 2;
            }
        }
        i += 1;
    }
    let sweepable: Vec<&str> = VALID_KINDS
        .iter()
        .copied()
        .filter(|k| *k != BACKFILL_KIND)
        .collect();
    if let Some(k) = kind.as_deref() {
        if !sweepable.contains(&k) {
            let detail = if k == BACKFILL_KIND {
                " (backfill belongs to /fno:ship pr merged's backfill slot and is never swept)"
            } else {
                ""
            };
            eprintln!(
                "retro: invalid --kind '{k}' (expected one of: {}){detail}",
                sweepable.join(", ")
            );
            return 2;
        }
    }
    let (_, node_root) = roots();
    // Without the node store there is no dedup, and filing blind is the
    // failure this sweep exists to prevent.
    let nodes = match read_nodes() {
        Ok(n) => n,
        Err(e) => {
            eprintln!("retro: cannot read the graph for dedup ({e}); refusing to sweep");
            return 1;
        }
    };
    let mode = if autonomous {
        MODE_AUTONOMOUS
    } else {
        MODE_INTERACTIVE
    };
    let mut land = LandOptions::live(
        mode,
        project_for(&node_root),
        Some(node_root.to_string_lossy().into_owned()),
    );
    land.anchor_scan = None;
    land.dedup_entries = Some(&nodes);
    let report = sweep::sweep_carveouts(
        &node_root,
        &nodes,
        apply,
        kind.as_deref(),
        &SweepSeams::live(land),
    );
    for line in sweep::render_sweep(&report) {
        println!("{line}");
    }
    for w in &report.warnings {
        eprintln!("WARN {w}");
    }
    if !apply && !report.items.is_empty() {
        println!();
        println!("re-run with --apply to file and consume.");
    }
    i32::from(report.failed())
}

#[derive(Default)]
struct RunArgs {
    node: Option<String>,
    pr: Option<i64>,
    sessions: Vec<String>,
    repo: Option<String>,
    keep_going: bool,
}

fn parse_run(rest: &[String]) -> Result<Option<RunArgs>, String> {
    let mut args = RunArgs::default();
    let mut i = 0;
    while i < rest.len() {
        let raw = rest[i].as_str();
        if is_help(raw) {
            return Ok(None);
        }
        if raw == "--keep-going" {
            args.keep_going = true;
            i += 1;
            continue;
        }
        let (flag, inline) = match raw.split_once('=') {
            Some((f, v)) if f.starts_with("--") => (f, Some(v.to_string())),
            _ => (raw, None),
        };
        let mut value = || -> Result<String, String> {
            if let Some(v) = inline.clone() {
                return Ok(v);
            }
            i += 1;
            rest.get(i)
                .cloned()
                .ok_or_else(|| format!("Error: Option '{flag}' requires an argument."))
        };
        match flag {
            "--node" => args.node = Some(value()?),
            "--pr-number" | "--pr" => {
                let v = value()?;
                args.pr = Some(v.trim().parse().map_err(|_| {
                    format!("Error: Invalid value for '--pr-number': '{v}' is not a valid integer.")
                })?);
            }
            "--session-id" | "--session" => args.sessions.push(value()?),
            "--repo" => args.repo = Some(value()?),
            _ => return Err(format!("Error: No such option: {raw}")),
        }
        i += 1;
    }
    Ok(Some(args))
}

fn cmd_run(rest: &[String]) -> i32 {
    let args = match parse_run(rest) {
        Ok(Some(a)) => a,
        Ok(None) => {
            println!("{RUN_USAGE}");
            return 0;
        }
        Err(e) => {
            eprintln!("{e}");
            return 2;
        }
    };
    let (repo_root, node_root) = roots();
    let gh: &GhRunner = &harvest::default_gh_runner;
    // Two slugs with two jobs: the local one gates project attribution, the
    // `--repo` override scopes the sentinel filter and the synthetic url.
    let local_slug = current_repo_slug(gh);
    let current_slug = args.repo.clone().or_else(|| local_slug.clone());
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let ctx = Ctx {
        repo_root: &repo_root,
        node_root: &node_root,
        carveout_root: &node_root,
        ledger: crate::paths_cli::ledger_json(&cwd),
        local_slug,
        gh,
    };

    let sentinel_dir = crate::agents_config::state_dir(&cwd)
        .map(|root| crate::state_layout::place(&root, "retro-pending"));
    let mut candidates: Vec<PathBuf> = match (&args.node, &sentinel_dir) {
        (Some(node), Some(dir)) => {
            let p = dir.join(format!("{node}.json"));
            if p.exists() {
                vec![p]
            } else {
                Vec::new()
            }
        }
        (None, Some(dir)) => {
            let mut found: Vec<PathBuf> = std::fs::read_dir(dir)
                .map(|it| {
                    it.filter_map(Result::ok)
                        .map(|e| e.path())
                        .filter(|p| p.extension().is_some_and(|x| x == "json"))
                        .collect()
                })
                .unwrap_or_default();
            found.sort();
            found
        }
        _ => Vec::new(),
    };
    // The fast path pr-merge drops in the worktree: same shape, same
    // routine, so two triggers for one PR collapse to one node set.
    let fast_path = repo_root.join(".fno").join(".triage-pending");
    if args.node.is_none() && fast_path.exists() {
        candidates.push(fast_path);
    }
    // `--pr-number` harvests only that PR: number and, when known, repo.
    if let (Some(pr), None) = (args.pr, &args.node) {
        candidates.retain(|p| {
            let payload = read_payload(p);
            payload.as_ref().map(payload_pr).filter(|n| *n != 0) == Some(pr)
                && sentinel_repo_matches(payload.as_ref(), current_slug.as_deref())
        });
    }

    // The postmortem pass and the gate-escape summary ride the plain run
    // only; a targeted run skips both.
    let mut pm_failed = false;
    if args.node.is_none() && args.pr.is_none() {
        pm_failed = postmortem_pass(&node_root).0;
        let summary = super::gate_escape::summarize(&crate::paths::events_path(&node_root));
        for line in super::gate_escape::render(&summary) {
            eprintln!("{line}");
        }
    }

    // Counted at the end, after the triggers ran: an up-front count would
    // name rows this very run was about to harvest.
    let report_pending = || {
        let pending = pending_carveout_count(&node_root);
        if pending > 0 {
            println!(
                "carve-outs: {pending} left unharvested on the ledger after this run (no trigger covers them); preview with `fno backlog retro sweep-carveouts`"
            );
        }
    };

    if candidates.is_empty() && args.pr.is_none() {
        println!("(no retro-pending sentinels to triage)");
        report_pending();
        return i32::from(pm_failed);
    }

    let mut any_retained = pm_failed;
    for path in &candidates {
        // Reload per sentinel, so a node an earlier sentinel filed is seen.
        let existing = read_nodes().unwrap_or_default();
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let Some(payload) = read_payload(path).filter(Value::is_object) else {
            eprintln!("WARN sentinel {name}: unreadable or malformed payload");
            any_retained = true;
            continue;
        };
        let (_, clean) = process_payload(&payload, &ctx, &existing, None);
        if !(clean && std::fs::remove_file(path).is_ok()) {
            any_retained = true;
        }
    }

    // The synthetic harvest: no sentinel exists (a manual merge with no
    // node-to-PR link), so build one from the flags.
    if let Some(pr) = args.pr {
        let mut payload = json!({"pr_number": pr});
        // No owning session: the carve-out source is read-only, so another
        // session's work is never stamped onto an arbitrary PR.
        if !args.sessions.is_empty() {
            payload["session_ids"] = json!(args.sessions);
        } else {
            let owners = pr_session_ids(&ctx.ledger, pr, current_slug.as_deref());
            if owners.is_empty() {
                payload["carveouts_readonly"] = json!(true);
            } else {
                payload["session_ids"] = json!(owners);
            }
        }
        if let Some(slug) = &current_slug {
            payload["pr_url"] = json!(crate::backlog::pr_link::pr_url_from_slug(slug, pr));
        }
        if args.keep_going && super::keep_going::enabled(Some(&repo_root)) {
            payload["mode"] = json!(MODE_AUTONOMOUS);
        }
        let existing = read_nodes().unwrap_or_default();
        // Carve-out first: with no repo, reviews are unharvestable, so skip
        // review derivation rather than mark the run unavailable and never
        // consume the carve-outs.
        let comments = if current_slug.is_some() {
            None
        } else {
            Some(Vec::new())
        };
        let (_, clean) = process_payload(&payload, &ctx, &existing, comments);
        if !clean {
            any_retained = true;
        }
    }

    report_pending();
    i32::from(any_retained)
}
