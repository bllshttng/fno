//! main's CI verdict: the workflow-run reduction the lead check-in renders,
//! the same token the merge gate (`authorized_merge`) reads behind a short
//! TTL row cache, and the red-run shape accessor both share.

use serde_json::{json, Value};
use std::path::Path;

/// Per-workflow verdict inputs: (name, newest run, newest completed run).
/// Row position is never trusted - the branch listing has served stale rows
/// first - `created_at` alone picks the newest of each. Made pub(crate) for
/// ci_front's per-sha verdict, so one fold answers both surfaces and two
/// folds never disagree.
pub(crate) fn fold_page<'a>(
    acc: &mut Vec<(&'a str, &'a Value, Option<&'a Value>)>,
    runs: impl Iterator<Item = &'a Value>,
) {
    for run in runs {
        let name = run.get("name").and_then(Value::as_str).unwrap_or("");
        if name.is_empty() {
            continue;
        }
        let created = run.get("created_at").and_then(Value::as_str).unwrap_or("");
        let completed = run.get("status").and_then(Value::as_str) == Some("completed");
        match acc.iter_mut().find(|(seen, _, _)| *seen == name) {
            None => acc.push((name, run, completed.then_some(run))),
            Some((_, newest, newest_completed)) => {
                if created
                    > newest
                        .get("created_at")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                {
                    *newest = run;
                }
                if completed {
                    let is_newest = match *newest_completed {
                        Some(c) => {
                            created > c.get("created_at").and_then(Value::as_str).unwrap_or("")
                        }
                        None => true,
                    };
                    if is_newest {
                        *newest_completed = Some(run);
                    }
                }
            }
        }
    }
}

/// How far back the check-in reports an old-commit failure: past this the
/// failure is archaeology, not a live signal, and the un-paginated branch
/// page would bury it anyway.
const STALE_REPORT_WINDOW_DAYS: i64 = 14;

/// One verdict token for the check-in line, from the workflow-run history on
/// main, judged at the branch head only: the verdict reduces rows whose
/// head sha equals `head_sha` - `at_sha` carries those push runs, and the
/// branch page is filtered to the head and to push-event rows to SEED which
/// workflows join the verdict at all: a schedule-event nightly release with
/// no push row of its own never reads main red while the head's push CI is
/// green. Once a workflow is seeded, a later completed run of that SAME
/// workflow at the SAME head sha supersedes an earlier push run regardless
/// of its own trigger: a concurrency group shared across triggers
/// can cancel the push run in favor of, say, a schedule run of the identical
/// workflow and sha, and the cancelled run must not outlive that newer
/// completion. A run on an OLDER commit never sets the verdict, however
/// fresh the workflow: a release run that failed or was cancelled days ago
/// on an old sha must not read main red while the head is clean. Such
/// failures ride the token's `stale` list instead (a cancelled older run is
/// a superseded one and rides nowhere) and the check-in names
/// each on its own line with its age. A stale failure a newer completed run
/// of the same `workflow_id` answered is resolved, not live, and rides
/// nowhere too: an older red outlived by a newer pass is history. While the
/// head's own run is still in flight the verdict reads pending and the
/// NEWEST stale failure promotes onto the verdict line (`pending (cli-ci at
/// <sha>)`) instead of riding the stale list - the previous head's red is
/// the live signal while the head is unverified; with no head rows at all
/// nothing promotes. When nothing promotes either, the pending verdict
/// names the newest settled run on main (`pending (last settled: green
/// cli-ci at <sha>, <1h old)`) so a coalescing burst reads as a queue with
/// a known floor rather than an open wedge; a cancel is no verdict and
/// never answers. Per workflow with head rows the
/// newest COMPLETED run decides - red on fail or cancel, naming the
/// workflow and the sha it ran on - except that a cancelled run whose
/// workflow already has a newer in-flight run on the same sha reduces to
/// that newer run's verdict, pending included: the rerun is the live one,
/// and a red read on the cancelled predecessor holds every merge for the
/// length of the rerun. A workflow whose newest head run is
/// still in flight reads pending; no head rows at all reads pending, never
/// green. The stale list reports within a bounded window (`now` minus
/// [`STALE_REPORT_WINDOW_DAYS`]): the branch page is one un-paginated read,
/// so an unbounded stale list would silently expire at the 100-row page
/// bound instead of at a named age.
fn main_ci_token_from_pages<'a>(
    head_sha: &str,
    at_sha: &'a [Value],
    on_main: &'a [Value],
    now: chrono::DateTime<chrono::Utc>,
) -> (Value, Option<&'a Value>) {
    let at_head = |run: &Value| run.get("head_sha").and_then(Value::as_str) == Some(head_sha);
    // The verdict is main's push CI at the head. A schedule-event nightly
    // release failing at the head is a release-engineering concern, never
    // main red while the head's push CI is green; only rows the API lists as
    // push events seed a workflow into the verdict.
    let is_push = |run: &Value| run.get("event").and_then(Value::as_str) == Some("push");
    let mut head: Vec<(&'a str, &'a Value, Option<&'a Value>)> = Vec::new();
    fold_page(&mut head, at_sha.iter());
    fold_page(
        &mut head,
        on_main.iter().filter(|run| at_head(run) && is_push(run)),
    );
    // A later same-sha completed run of an ALREADY-seeded workflow supersedes
    // an earlier push run's conclusion, from any trigger: a
    // concurrency group shared across triggers can cancel the push run in
    // favor of, say, a schedule run of the identical workflow and sha, and
    // the cancelled run must not outlive a newer completion of that same
    // workflow. Only `newest_completed` moves here, never the `newest`
    // pointer the in-flight check below reads - so an in-flight nightly of a
    // workflow that already finished its push run never holds the verdict
    // pending, and a workflow with no push row at the head (a pure-schedule
    // release) never joins the verdict through this pass. Matched by the
    // run's stable `workflow_id`, never the display `name` alone: two
    // different workflow files can share a `name`, and a completed run of
    // one must never supersede the other's seeded push run.
    for run in on_main.iter().filter(|run| at_head(run) && !is_push(run)) {
        let name = run.get("name").and_then(Value::as_str).unwrap_or("");
        if name.is_empty() || run.get("status").and_then(Value::as_str) != Some("completed") {
            continue;
        }
        let Some((_, newest, newest_completed)) =
            head.iter_mut().find(|(seen, _, _)| *seen == name)
        else {
            continue;
        };
        let wf_id = run.get("workflow_id").and_then(Value::as_u64);
        if wf_id.is_none() || wf_id != newest.get("workflow_id").and_then(Value::as_u64) {
            continue;
        }
        let created = run.get("created_at").and_then(Value::as_str).unwrap_or("");
        let is_newer = match *newest_completed {
            Some(c) => created > c.get("created_at").and_then(Value::as_str).unwrap_or(""),
            None => true,
        };
        if is_newer {
            *newest_completed = Some(run);
        }
    }
    // A cancelled run on an older commit is not a failure: main's
    // concurrency group cancels the pending run a newer push replaces. Those
    // rows never join the older fold, so the newest run that reached a
    // verdict decides each workflow's stale entry.
    let mut older: Vec<(&'a str, &'a Value, Option<&'a Value>)> = Vec::new();
    fold_page(
        &mut older,
        on_main
            .iter()
            .filter(|run| !at_head(run) && crate::pr_push::rest_bucket(run) != "cancel"),
    );
    let window_start = now - chrono::Duration::days(STALE_REPORT_WINDOW_DAYS);
    // A stale failure a newer completed run of the same workflow answered is
    // resolved, not live: an older red outlived by a newer pass is history
    // the check-in must not keep printing. Matched by the stable
    // `workflow_id` like the supersede passes above, never the display
    // `name` alone, and fail-closed - a row pair the API gives no id for
    // never de-stales.
    let resolved_by_newer_pass = |run: &Value| {
        let (Some(wf_id), Some(created)) = (
            run.get("workflow_id").and_then(Value::as_u64),
            run.get("created_at").and_then(Value::as_str),
        ) else {
            return false;
        };
        at_sha.iter().chain(on_main.iter()).any(|other| {
            crate::pr_push::rest_bucket(other) == "pass"
                && other.get("workflow_id").and_then(Value::as_u64) == Some(wf_id)
                && other
                    .get("created_at")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    > created
        })
    };
    let mut stale: Vec<Value> = older
        .iter()
        .filter_map(|(_, _, completed)| *completed)
        .filter(|run| crate::pr_push::rest_bucket(run) == "fail")
        .filter(|run| !resolved_by_newer_pass(run))
        .filter(|run| {
            run.get("created_at")
                .and_then(Value::as_str)
                .and_then(|raw| chrono::DateTime::parse_from_rfc3339(raw).ok())
                .map(|ts| ts.with_timezone(&chrono::Utc) >= window_start)
                .unwrap_or(true)
        })
        .map(|run| {
            json!({
                "workflow": run.get("name").and_then(Value::as_str).unwrap_or("unknown"),
                "sha": run.get("head_sha").and_then(Value::as_str).unwrap_or("unknown"),
                "created_at": run.get("created_at").and_then(Value::as_str).unwrap_or(""),
            })
        })
        .collect();
    let red = head
        .iter()
        .filter_map(|(_, _, completed)| *completed)
        .filter(|run| matches!(crate::pr_push::rest_bucket(run), "fail" | "cancel"))
        .max_by_key(|run| run.get("created_at").and_then(Value::as_str).unwrap_or(""));
    // A cancelled red pick whose workflow already has a newer in-flight run
    // on the same sha is a rerun victim, not a verdict: it reduces to that
    // newer run's verdict, pending included. Only a cancel demotes - a
    // failure stays red no matter what queues behind it - and the newer run
    // must match the stable `workflow_id`, never the display `name` alone:
    // two workflow files can share a name. Push rows ride the fold's
    // `newest` pointer; a rerun from another trigger lands on the branch
    // page outside the fold, so it is scanned directly.
    let red_superseded_inflight = red
        .filter(|run| crate::pr_push::rest_bucket(run) == "cancel")
        .is_some_and(|run| {
            let name = run.get("name").and_then(Value::as_str).unwrap_or("");
            let Some(wf_id) = run.get("workflow_id").and_then(Value::as_u64) else {
                return false;
            };
            let created = run.get("created_at").and_then(Value::as_str).unwrap_or("");
            let newer_inflight = |other: &Value| {
                other.get("status").and_then(Value::as_str) != Some("completed")
                    && other.get("name").and_then(Value::as_str).unwrap_or("") == name
                    && other.get("workflow_id").and_then(Value::as_u64) == Some(wf_id)
                    && other
                        .get("created_at")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        > created
            };
            head.iter().any(|(_, newest, _)| newer_inflight(newest))
                || on_main
                    .iter()
                    .any(|other| at_head(other) && !is_push(other) && newer_inflight(other))
        });
    let red = if red_superseded_inflight { None } else { red };
    let inflight = red_superseded_inflight
        || head.iter().any(|(_, newest, _)| {
            newest.get("status").and_then(Value::as_str) != Some("completed")
        });
    let mut token = match red {
        Some(run) => json!({
            "verdict": "red",
            "workflow": run.get("name").and_then(Value::as_str).unwrap_or("unknown"),
            "sha": run.get("head_sha").and_then(Value::as_str).unwrap_or("unknown"),
        }),
        None => {
            json!({ "verdict": if head.is_empty() || inflight { "pending" } else { "green" } })
        }
    };
    // While the head's own run is still in flight the newest stale failure
    // is the live signal, not archaeology: promote it onto the verdict line
    // (`pending (cli-ci at 40bde08)`) and leave the rest to the stale list.
    // A red verdict never promotes - it already names its run - and main
    // with no head rows at all keeps its stale list, since nothing is in
    // flight to read the promotion against.
    if red.is_none() && inflight && !stale.is_empty() {
        let newest = stale
            .iter()
            .enumerate()
            .max_by_key(|(_, run)| run.get("created_at").and_then(Value::as_str).unwrap_or(""))
            .map(|(idx, _)| idx)
            .expect("stale is non-empty");
        let run = stale.swap_remove(newest);
        let fields = token.as_object_mut().expect("token is built as an object");
        if let Some(workflow) = run.get("workflow") {
            fields.insert("workflow".into(), workflow.clone());
        }
        if let Some(sha) = run.get("sha") {
            fields.insert("sha".into(), sha.clone());
        }
    }
    // A pending verdict with nothing promoted names the newest settled run:
    // once a red is resolved by a newer pass the line would read bare
    // `pending` for the length of the next coalescing burst, with nothing
    // saying main was green an hour ago. A cancel is no verdict and never
    // answers, and the promotion above owns the line when a live red rides
    // it, so this only fires on the bare word.
    if token.get("verdict").and_then(Value::as_str) == Some("pending")
        && token.get("workflow").is_none()
    {
        let settled = at_sha
            .iter()
            .chain(on_main.iter())
            .filter(|run| matches!(crate::pr_push::rest_bucket(run), "pass" | "fail"))
            .max_by_key(|run| run.get("created_at").and_then(Value::as_str).unwrap_or(""));
        if let Some(run) = settled {
            let created = run.get("created_at").and_then(Value::as_str).unwrap_or("");
            token
                .as_object_mut()
                .expect("token is built as an object")
                .insert(
                    "last_settled".into(),
                    json!({
                        "workflow": run.get("name").and_then(Value::as_str).unwrap_or("unknown"),
                        "sha": run.get("head_sha").and_then(Value::as_str).unwrap_or("unknown"),
                        "verdict": if crate::pr_push::rest_bucket(run) == "pass" {
                            "green"
                        } else {
                            "red"
                        },
                        "created_at": created,
                        "age": age_word(created, now),
                    }),
                );
        }
    }
    if !stale.is_empty() {
        token
            .as_object_mut()
            .expect("token is built as an object")
            .insert("stale".into(), Value::Array(stale));
    }
    (token, red)
}

/// The incident attachment: a red verdict whose run ended `cancel` reads the
/// run's jobs for the never-got-a-runner shape, then confirms against the
/// shared status-page seam before the explanation rides the token. Every
/// read is fail-open - a jobs or status-site failure leaves the token
/// exactly as the fold built it.
fn attach_incident(cwd: &Path, token: &mut Value, red: Option<&Value>) {
    let Some(run) = red else {
        return;
    };
    if run.get("conclusion").and_then(Value::as_str) != Some("cancel") {
        return;
    }
    let Some(id) = run.get("id").and_then(Value::as_u64) else {
        return;
    };
    let Ok(raw) = crate::pr_push::gh_api(
        "gh",
        cwd,
        &format!("repos/{{owner}}/{{repo}}/actions/runs/{id}/jobs?per_page=100"),
        &[],
    ) else {
        return;
    };
    let Ok(jobs_page) = serde_json::from_str::<Value>(&raw) else {
        return;
    };
    let jobs = jobs_page.get("jobs").and_then(Value::as_array);
    let Some(true) = jobs.map(|jobs| crate::gh_incident::cancelled_no_runner(jobs)) else {
        return;
    };
    let incidents = crate::pr_status::seams::platform_incident();
    if incidents.is_null() {
        return;
    }
    token
        .as_object_mut()
        .expect("token is built as an object")
        .insert("platform_incident".into(), incidents);
}

/// The `main ci:` line body: a red verdict names the workflow and sha, a
/// pending verdict names them too when the previous head's red promotes
/// onto it, a pending verdict with nothing promoted names the newest
/// settled run with its verdict, workflow, sha and age, a plain verdict is
/// the word alone, and legacy cached string tokens pass through untouched.
pub(crate) fn main_ci_render(v: Option<&Value>) -> String {
    match v {
        Some(Value::Object(o)) => {
            let field = |k: &str| o.get(k).and_then(Value::as_str).unwrap_or("unknown");
            let base = match o.get("last_settled") {
                Some(s) => {
                    let sf = |k: &str| s.get(k).and_then(Value::as_str).unwrap_or("unknown");
                    format!(
                        "{} (last settled: {} {} at {}, {})",
                        field("verdict"),
                        sf("verdict"),
                        sf("workflow"),
                        sf("sha"),
                        sf("age")
                    )
                }
                None => match o.get("workflow") {
                    Some(_) => format!(
                        "{} ({} at {})",
                        field("verdict"),
                        field("workflow"),
                        field("sha")
                    ),
                    None => field("verdict").to_string(),
                },
            };
            match o.get("platform_incident") {
                Some(incidents) => {
                    format!("{base} {}", crate::gh_incident::incident_line(incidents))
                }
                None => base,
            }
        }
        other => crate::lead_checkin::dash(other),
    }
}

/// The age word shared by the stale lines and the last-settled field: hours
/// under two days, days at and past it, and a named shape when the row
/// carries no readable stamp.
fn age_word(created_at: &str, now: chrono::DateTime<chrono::Utc>) -> String {
    chrono::DateTime::parse_from_rfc3339(created_at)
        .map(|ts| {
            let hours = (now - ts.with_timezone(&chrono::Utc)).num_hours();
            if hours >= 48 {
                format!("{}d old", hours / 24)
            } else if hours >= 1 {
                format!("{}h old", hours)
            } else {
                "<1h old".to_string()
            }
        })
        .unwrap_or_else(|_| "age unknown".to_string())
}

/// The check-in's stale-failure lines: one per old-commit failure the token
/// carries, each naming the workflow, the sha and the run's age. Legacy
/// string tokens and tokens without a stale list emit nothing.
pub(crate) fn main_ci_stale_lines(
    v: Option<&Value>,
    now: chrono::DateTime<chrono::Utc>,
) -> Vec<String> {
    let Some(stale) = v.and_then(|t| t.get("stale")).and_then(Value::as_array) else {
        return Vec::new();
    };
    stale
        .iter()
        .map(|run| {
            let field = |k: &str| run.get(k).and_then(Value::as_str).unwrap_or("unknown");
            let age = age_word(field("created_at"), now);
            format!(
                "main ci stale: {} at {} ({})",
                field("workflow"),
                field("sha"),
                age
            )
        })
        .collect()
}

pub(crate) fn r_main_ci() -> Result<Value, String> {
    let cwd = std::env::current_dir().map_err(|e| e.to_string())?;
    main_ci_reading(&cwd)
}

/// The live reading for any cwd: the lead check-in renders it per beat, and
/// the merge gate reads it behind the TTL cache below.
pub(crate) fn main_ci_reading(cwd: &Path) -> Result<Value, String> {
    // Judge at main's current head: the verdict reduces only rows whose head
    // sha equals the branch head, so the head's own push runs are read
    // directly, the branch page contributes its head-sha push-event rows
    // only, and its older rows only ever feed the stale
    // list, never the verdict.
    let head_raw = crate::pr_push::gh_api("gh", &cwd, "repos/{owner}/{repo}/branches/main", &[])
        .map_err(|error| {
            format!(
                "gh api failed: {}",
                crate::lead_checkin::gh_error_cause(&error)
            )
        })?;
    let head_sha = serde_json::from_str::<Value>(&head_raw)
        .ok()
        .and_then(|page| {
            page.pointer("/commit/sha")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .filter(|sha| !sha.is_empty())
        .ok_or_else(|| "gh api branches/main returned no head sha".to_string())?;
    // One un-paginated page per read: the listings grow with the repo's age,
    // and a paginated read would walk the whole history on every beat. The
    // branch page's head-sha rows join the verdict fold; its older rows feed
    // the stale list only.
    let read_runs = |query: &str| -> Result<Vec<Value>, String> {
        let raw = crate::pr_push::gh_api("gh", &cwd, query, &[]).map_err(|error| {
            format!(
                "gh api failed: {}",
                crate::lead_checkin::gh_error_cause(&error)
            )
        })?;
        let runs = serde_json::from_str::<Value>(&raw)
            .ok()
            .and_then(|page| page.get("workflow_runs").and_then(Value::as_array).cloned())
            .unwrap_or_default();
        Ok(runs)
    };
    let at_sha = read_runs(&format!(
        "repos/{{owner}}/{{repo}}/actions/runs?head_sha={head_sha}&event=push&per_page=100"
    ))?;
    let mut on_main = read_runs("repos/{owner}/{repo}/actions/runs?branch=main&per_page=100")?;
    // The verdict only needs the head rows, but the stale report owes the
    // whole window: when the newest 100 rows end inside it, a burst has
    // pushed in-window failures onto page 2. One extra page, and no more -
    // a burst past two pages outruns the stale report, never the verdict.
    let window_start = chrono::Utc::now() - chrono::Duration::days(STALE_REPORT_WINDOW_DAYS);
    let covers_window = on_main
        .iter()
        .filter_map(|run| {
            run.get("created_at")
                .and_then(Value::as_str)
                .and_then(|raw| chrono::DateTime::parse_from_rfc3339(raw).ok())
        })
        .min()
        .map(|oldest| oldest.with_timezone(&chrono::Utc) <= window_start)
        .unwrap_or(true);
    if !covers_window {
        on_main.extend(read_runs(
            "repos/{owner}/{repo}/actions/runs?branch=main&per_page=100&page=2",
        )?);
    }
    let (mut token, red) =
        main_ci_token_from_pages(&head_sha, &at_sha, &on_main, chrono::Utc::now());
    attach_incident(cwd, &mut token, red);
    // Best effort: a store fault must never fail the CI read it rides on.
    if let Some(repo) = crate::finalize::slug_from_git_remote(cwd) {
        let _ = crate::main_ci_transition::record(
            &crate::paths_cli::global_events_json(cwd),
            &repo,
            &head_sha,
            &token,
        );
    }
    Ok(token)
}

/// The one red run a main-ci token names, as (workflow, head sha). The word
/// tokens (`green`, `pending`) and any other shape are not red: a pending
/// main is not red. Lives beside the token's producer so the shape is read
/// in one module.
pub(crate) fn main_ci_red_run(token: &Value) -> Option<(String, String)> {
    if token.get("verdict").and_then(Value::as_str) != Some("red") {
        return None;
    }
    let field = |k: &str| token.get(k).and_then(Value::as_str).unwrap_or("unknown");
    Some((field("workflow").to_string(), field("sha").to_string()))
}

/// How long a cached main verdict answers before the live read runs again.
const MAIN_CI_CACHE_TTL_S: u64 = 60;

/// [`main_ci_reading`] behind the gh-facts row store, TTL'd: the merge gate
/// and the preview walk read main's verdict on every pass (every status
/// receipt included), far more often than the verdict changes. A stale,
/// missing or corrupt row falls through to the live read, which stays the
/// truth; a failed live read is an `Err`, never a manufactured red.
pub(crate) fn main_ci_reading_cached(cwd: &Path) -> Result<Value, String> {
    let root = crate::gh_cache::rows_root(cwd);
    let slug = crate::finalize::slug_from_git_remote(cwd);
    let (Some(root), Some(slug)) = (root, slug) else {
        return main_ci_reading(cwd);
    };
    let fresh = crate::gh_cache::read_row(
        &root,
        "main-ci",
        &slug,
        None,
        Some(MAIN_CI_CACHE_TTL_S),
        crate::gh_cache::now_secs(),
    );
    if let Some(token) = fresh.get("row").and_then(|r| r.get("token")) {
        return Ok(token.clone());
    }
    let token = main_ci_reading(cwd)?;
    // The token rides an object row because the row store only holds
    // objects; the bare `green`/`pending` words must cache too, or the
    // common case pays the live read on every pass.
    crate::gh_cache::write_row(
        &root,
        "main-ci",
        &slug,
        None,
        &json!({ "token": token.clone() }),
    );
    Ok(token)
}

/// The `fno do pr status main` receipt for one reading: a JSON line in the
/// PR status shape and its exit code, 0 green, 1 red, 2 pending, 3 unknown,
/// 4 read failed. A watch on main arms this, never a newest-run listing: the
/// newest row is one workflow, and a red sibling hides behind its pass.
fn status_main_receipt(reading: Result<Value, String>) -> (Value, i32) {
    match reading {
        Ok(token) => {
            let verdict = token
                .get("verdict")
                .and_then(Value::as_str)
                .unwrap_or("unknown")
                .to_string();
            let code = match verdict.as_str() {
                "green" => 0,
                "red" => 1,
                "pending" => 2,
                _ => 3,
            };
            let line = main_ci_render(Some(&token));
            (
                json!({"pr": "main", "verdict": verdict, "green": code == 0, "main_ci": line}),
                code,
            )
        }
        Err(reason) => (
            json!({"pr": "main", "verdict": "error", "green": false, "reason": reason}),
            4,
        ),
    }
}

/// `fno do pr status main`: main's head verdict across every workflow, the
/// same reduction the check-in's `main ci:` line and the merge gate read.
/// `refresh` skips the 60s row cache for one live read.
pub(crate) fn run_status_main(cwd: &Path, refresh: bool) -> i32 {
    let reading = if refresh {
        main_ci_reading(cwd)
    } else {
        main_ci_reading_cached(cwd)
    };
    let (receipt, code) = status_main_receipt(reading);
    println!("{receipt}");
    code
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The workflow-run rows an `/actions/runs` page carries, in the fields
    /// the reducer reads; row order is free.
    fn wf_run(name: &str, sha: &str, status: &str, conclusion: &str, created: &str) -> Value {
        wf_run_event(name, sha, status, conclusion, created, "push")
    }

    /// The same row with an explicit trigger event: push rows set the
    /// verdict, anything else (the nightly's `schedule`) never does.
    fn wf_run_event(
        name: &str,
        sha: &str,
        status: &str,
        conclusion: &str,
        created: &str,
        event: &str,
    ) -> Value {
        serde_json::json!({
            "name": name, "head_sha": sha, "status": status,
            "conclusion": conclusion, "created_at": created, "event": event,
        })
    }

    /// The same row with an explicit `workflow_id`: the stable id a
    /// non-push row must match before it can supersede an already-seeded
    /// workflow's newest completed run - two different workflow files can
    /// share a display `name`, so the id is what tells them apart.
    fn wf_run_with_id(
        name: &str,
        sha: &str,
        status: &str,
        conclusion: &str,
        created: &str,
        event: &str,
        workflow_id: u64,
    ) -> Value {
        let mut run = wf_run_event(name, sha, status, conclusion, created, event);
        run.as_object_mut()
            .expect("wf_run_event builds an object")
            .insert("workflow_id".into(), serde_json::json!(workflow_id));
        run
    }

    /// The reducer at the tests' fixed clock, 2026-09-30T12:00:00Z, so the
    /// stale window's boundary never drifts under a hardcoded date.
    fn token_at(head_sha: &str, at_sha: &[Value], on_main: &[Value]) -> Value {
        main_ci_token_from_pages(
            head_sha,
            at_sha,
            on_main,
            chrono::DateTime::parse_from_rfc3339("2026-09-30T12:00:00Z")
                .unwrap()
                .with_timezone(&chrono::Utc),
        )
        .0
    }

    /// The filed bug's shape: the branch page led with a stale green from
    /// five days before the head while the newer cli-ci run had failed.
    /// Row position is never trusted, so the stale-first page still reads
    /// red; the position-trusting reader read green here.
    #[test]
    fn main_ci_reads_red_when_the_branch_page_leads_with_a_stale_green() {
        let on_main = vec![
            wf_run(
                "cli-ci",
                "stale",
                "completed",
                "success",
                "2026-09-23T05:52:00Z",
            ),
            wf_run(
                "guards",
                "stale",
                "completed",
                "success",
                "2026-09-23T05:52:00Z",
            ),
            wf_run(
                "cli-ci",
                "fresh",
                "completed",
                "failure",
                "2026-09-28T07:30:00Z",
            ),
        ];
        assert_eq!(
            token_at("fresh", &[], &on_main),
            serde_json::json!({"verdict": "red", "workflow": "cli-ci", "sha": "fresh"})
        );
    }

    /// Main's concurrency group cancels the pending run a newer push
    /// replaces. That cancel on an older commit is never a stale failure, and
    /// it never hides an older failure of the same workflow either.
    #[test]
    fn a_superseded_cancel_on_an_older_commit_is_not_stale() {
        let on_main = vec![
            wf_run(
                "cli-ci",
                "a1",
                "completed",
                "failure",
                "2026-09-29T08:00:00Z",
            ),
            wf_run(
                "cli-ci",
                "a2",
                "completed",
                "cancelled",
                "2026-09-29T09:00:00Z",
            ),
            wf_run(
                "guards",
                "a2",
                "completed",
                "cancelled",
                "2026-09-29T09:00:00Z",
            ),
            wf_run(
                "cli-ci",
                "head",
                "completed",
                "success",
                "2026-09-29T10:00:00Z",
            ),
        ];
        let stale = vec![serde_json::json!({
            "workflow": "cli-ci", "sha": "a1", "created_at": "2026-09-29T08:00:00Z",
        })];
        assert_eq!(
            token_at("head", &[], &on_main),
            serde_json::json!({"verdict": "green", "stale": stale})
        );
    }

    /// Codex's finding on this fix (PR #3484): two different workflow files
    /// can share a display `name`. A completed schedule-only run of one
    /// (`workflow_id` 99) must never supersede the other's seeded, cancelled
    /// push run (`workflow_id` 42) just because both happen to be named
    /// "cli-ci" - the verdict stays red on the genuinely cancelled push run.
    #[test]
    fn a_different_workflow_sharing_the_same_name_never_supersedes() {
        let at_sha = vec![wf_run_with_id(
            "cli-ci",
            "head",
            "completed",
            "cancelled",
            "2026-09-28T07:00:00Z",
            "push",
            42,
        )];
        let on_main = vec![
            wf_run_with_id(
                "cli-ci",
                "head",
                "completed",
                "cancelled",
                "2026-09-28T07:00:00Z",
                "push",
                42,
            ),
            wf_run_with_id(
                "cli-ci",
                "head",
                "completed",
                "success",
                "2026-09-28T07:30:00Z",
                "schedule",
                99,
            ),
        ];
        assert_eq!(
            token_at("head", &at_sha, &on_main),
            serde_json::json!({"verdict": "red", "workflow": "cli-ci", "sha": "head"})
        );
    }

    /// The pending-rerun case the completed-supersede pass never covered:
    /// cli-ci's newest completed run at the head is a cancel and its newer
    /// rerun on the same sha is still queued. The verdict rides the rerun -
    /// pending - never red on the cancelled predecessor.
    #[test]
    fn a_cancelled_run_with_a_pending_rerun_on_the_same_sha_reads_pending() {
        let at_sha = vec![
            wf_run_with_id(
                "cli-ci",
                "head",
                "completed",
                "cancelled",
                "2026-10-18T13:00:00Z",
                "push",
                42,
            ),
            wf_run_with_id(
                "cli-ci",
                "head",
                "queued",
                "",
                "2026-10-18T14:00:00Z",
                "push",
                42,
            ),
        ];
        assert_eq!(
            token_at("head", &at_sha, &[]),
            serde_json::json!({"verdict": "pending"})
        );
    }

    /// A rerun from another trigger (a re-dispatch still in flight) demotes
    /// the cancel too: the workflow has a newer live run at the head, so the
    /// verdict is that run's pending, not the predecessor's red.
    #[test]
    fn a_pending_rerun_from_another_trigger_also_reads_pending() {
        let at_sha = vec![wf_run_with_id(
            "cli-ci",
            "head",
            "completed",
            "cancelled",
            "2026-10-18T13:00:00Z",
            "push",
            42,
        )];
        let on_main = vec![
            wf_run_with_id(
                "cli-ci",
                "head",
                "completed",
                "cancelled",
                "2026-10-18T13:00:00Z",
                "push",
                42,
            ),
            wf_run_with_id(
                "cli-ci",
                "head",
                "in_progress",
                "",
                "2026-10-18T14:00:00Z",
                "workflow_dispatch",
                42,
            ),
        ];
        assert_eq!(
            token_at("head", &at_sha, &on_main),
            serde_json::json!({"verdict": "pending"})
        );
    }

    /// A queued run of a DIFFERENT workflow sharing the display name never
    /// answers for the cancelled one: the verdict stays red on the cancel,
    /// the same `workflow_id` discrimination the completed-supersede pass
    /// applies.
    #[test]
    fn a_pending_rerun_of_a_different_workflow_sharing_the_name_demotes_nothing() {
        let at_sha = vec![wf_run_with_id(
            "cli-ci",
            "head",
            "completed",
            "cancelled",
            "2026-10-18T13:00:00Z",
            "push",
            42,
        )];
        let on_main = vec![wf_run_with_id(
            "cli-ci",
            "head",
            "queued",
            "",
            "2026-10-18T14:00:00Z",
            "push",
            99,
        )];
        assert_eq!(
            token_at("head", &at_sha, &on_main),
            serde_json::json!({"verdict": "red", "workflow": "cli-ci", "sha": "head"})
        );
    }

    /// A failure is a verdict, never a rerun victim: a failed run stays red
    /// even while a newer run queues behind it. Only a cancel demotes.
    #[test]
    fn a_failed_run_stays_red_even_with_a_newer_pending_run() {
        let at_sha = vec![
            wf_run_with_id(
                "cli-ci",
                "head",
                "completed",
                "failure",
                "2026-10-18T13:00:00Z",
                "push",
                42,
            ),
            wf_run_with_id(
                "cli-ci",
                "head",
                "queued",
                "",
                "2026-10-18T14:00:00Z",
                "push",
                42,
            ),
        ];
        assert_eq!(
            token_at("head", &at_sha, &[]),
            serde_json::json!({"verdict": "red", "workflow": "cli-ci", "sha": "head"})
        );
    }

    /// The filed check-in bug: a failed `workflow_dispatch` release run on
    /// an older sha read as `main ci: red` while the head's push CI was
    /// green. The verdict answers the head's push rows alone; the old
    /// failure rides the `stale` list, and with no head rows at all the
    /// verdict reads pending, never red.
    #[test]
    fn a_failed_dispatch_on_an_older_sha_never_reads_red() {
        let at_sha = vec![wf_run(
            "cli-ci",
            "fresh",
            "completed",
            "success",
            "2026-09-30T11:00:00Z",
        )];
        let on_main = vec![
            wf_run_event(
                "release",
                "old",
                "completed",
                "failure",
                "2026-09-28T11:00:00Z",
                "workflow_dispatch",
            ),
            wf_run(
                "cli-ci",
                "fresh",
                "completed",
                "success",
                "2026-09-30T11:00:00Z",
            ),
        ];
        assert_eq!(
            token_at("fresh", &at_sha, &on_main),
            serde_json::json!({
                "verdict": "green",
                "stale": [{
                    "workflow": "release", "sha": "old",
                    "created_at": "2026-09-28T11:00:00Z",
                }],
            })
        );
        assert_eq!(
            token_at(
                "fresh",
                &[],
                &[wf_run_event(
                    "release",
                    "old",
                    "completed",
                    "failure",
                    "2026-09-28T11:00:00Z",
                    "workflow_dispatch",
                )],
            ),
            serde_json::json!({
                "verdict": "pending",
                "stale": [{
                    "workflow": "release", "sha": "old",
                    "created_at": "2026-09-28T11:00:00Z",
                }],
            })
        );
    }

    /// The stale specimen: cli-ci failed at 54ba82b, then passed at 2753e49,
    /// the new head. The older red is resolved - a newer completed pass of
    /// the same workflow rides it off the stale list entirely.
    #[test]
    fn a_stale_failure_a_newer_pass_of_the_same_workflow_resolved_rides_nowhere() {
        let at_sha = vec![wf_run_with_id(
            "cli-ci",
            "2753e49",
            "completed",
            "success",
            "2026-09-30T11:05:00Z",
            "push",
            42,
        )];
        let on_main = vec![
            wf_run_with_id(
                "cli-ci",
                "54ba82b",
                "completed",
                "failure",
                "2026-09-29T20:00:00Z",
                "push",
                42,
            ),
            wf_run_with_id(
                "cli-ci",
                "2753e49",
                "completed",
                "success",
                "2026-09-30T11:05:00Z",
                "push",
                42,
            ),
        ];
        assert_eq!(
            token_at("2753e49", &at_sha, &on_main),
            serde_json::json!({"verdict": "green"})
        );
    }

    /// The resolution is workflow-scoped and id-matched: a newer pass of a
    /// DIFFERENT workflow file sharing the display name answers nothing, and
    /// a candidate the API gives no `workflow_id` never de-stales.
    #[test]
    fn a_newer_pass_of_a_different_workflow_sharing_the_name_resolves_nothing() {
        let on_main = vec![
            wf_run_with_id(
                "cli-ci",
                "old",
                "completed",
                "failure",
                "2026-09-29T20:00:00Z",
                "push",
                42,
            ),
            wf_run_with_id(
                "cli-ci",
                "head",
                "completed",
                "success",
                "2026-09-30T11:00:00Z",
                "push",
                99,
            ),
        ];
        assert_eq!(
            token_at("head", &[], &on_main),
            serde_json::json!({
                "verdict": "green",
                "stale": [{
                    "workflow": "cli-ci", "sha": "old",
                    "created_at": "2026-09-29T20:00:00Z",
                }],
            })
        );
    }

    /// The pending specimen: cli-ci failed at 40bde08 (the previous head)
    /// and the newer head 6889efa is still running. The verdict is pending
    /// and the line names the previous head's red instead of leaving it to
    /// archaeology, while the merge gate still reads no red.
    #[test]
    fn a_red_run_on_the_previous_head_rides_the_pending_line_while_the_head_runs() {
        let at_sha = vec![wf_run_with_id(
            "cli-ci",
            "6889efa",
            "in_progress",
            "",
            "2026-09-30T11:30:00Z",
            "push",
            42,
        )];
        let on_main = vec![
            wf_run_with_id(
                "cli-ci",
                "40bde08",
                "completed",
                "failure",
                "2026-09-30T11:05:00Z",
                "push",
                42,
            ),
            wf_run_with_id(
                "cli-ci",
                "6889efa",
                "in_progress",
                "",
                "2026-09-30T11:30:00Z",
                "push",
                42,
            ),
        ];
        let token = token_at("6889efa", &at_sha, &on_main);
        assert_eq!(
            token,
            serde_json::json!({
                "verdict": "pending", "workflow": "cli-ci", "sha": "40bde08",
            })
        );
        assert_eq!(main_ci_red_run(&token), None);
    }

    /// The bare-pending specimen this fix closes: the previous head's red
    /// was resolved by a newer pass, the head's own run is still queued, and
    /// the stale list is empty. The line names the newest settled run
    /// instead of reading bare `pending` for the length of a coalescing
    /// burst, and the merge gate keeps reading no red.
    #[test]
    fn a_pending_head_with_a_resolved_red_names_the_newest_settled_run() {
        let at_sha = vec![wf_run_with_id(
            "cli-ci",
            "head",
            "queued",
            "",
            "2026-09-30T10:00:00Z",
            "push",
            42,
        )];
        let on_main = vec![
            wf_run_with_id(
                "cli-ci",
                "prev",
                "completed",
                "failure",
                "2026-09-30T08:00:00Z",
                "push",
                42,
            ),
            wf_run_with_id(
                "cli-ci",
                "mid",
                "completed",
                "success",
                "2026-09-30T11:00:00Z",
                "push",
                42,
            ),
        ];
        let token = token_at("head", &at_sha, &on_main);
        assert_eq!(
            token,
            serde_json::json!({
                "verdict": "pending",
                "last_settled": {
                    "workflow": "cli-ci", "sha": "mid", "verdict": "green",
                    "created_at": "2026-09-30T11:00:00Z", "age": "1h old",
                },
            })
        );
        assert_eq!(main_ci_red_run(&token), None);
    }

    /// A cancelled run is no verdict: under a burst of cancels the newest
    /// settled run beneath them is the last pass or failure, never the
    /// cancel. This is 2026-10-30's shape: seven queued main runs cancelled
    /// around a settled pass.
    #[test]
    fn a_cancelled_run_never_answers_the_last_settled_field() {
        let at_sha = vec![wf_run_with_id(
            "cli-ci",
            "head",
            "queued",
            "",
            "2026-09-30T10:00:00Z",
            "push",
            42,
        )];
        let on_main = vec![
            wf_run_with_id(
                "cli-ci",
                "c1",
                "completed",
                "cancelled",
                "2026-09-30T09:00:00Z",
                "push",
                42,
            ),
            wf_run_with_id(
                "cli-ci",
                "mid",
                "completed",
                "success",
                "2026-09-30T08:00:00Z",
                "push",
                42,
            ),
        ];
        let token = token_at("head", &at_sha, &on_main);
        assert_eq!(
            token,
            serde_json::json!({
                "verdict": "pending",
                "last_settled": {
                    "workflow": "cli-ci", "sha": "mid", "verdict": "green",
                    "created_at": "2026-09-30T08:00:00Z", "age": "4h old",
                },
            })
        );
    }

    /// The promotion owns the line when a live red rides it: the stale red
    /// is named at the top level and no last-settled field is attached.
    #[test]
    fn a_promoted_red_owns_the_line_and_no_last_settled_is_attached() {
        let at_sha = vec![wf_run_with_id(
            "cli-ci",
            "head",
            "queued",
            "",
            "2026-09-30T10:00:00Z",
            "push",
            42,
        )];
        let on_main = vec![wf_run_with_id(
            "cli-ci",
            "prev",
            "completed",
            "failure",
            "2026-09-30T08:00:00Z",
            "push",
            42,
        )];
        let token = token_at("head", &at_sha, &on_main);
        assert_eq!(
            token,
            serde_json::json!({"verdict": "pending", "workflow": "cli-ci", "sha": "prev"})
        );
        assert!(token.get("last_settled").is_none());
    }

    /// The render names the last settled run with its verdict, workflow, sha
    /// and age; a bare pending word renders untouched; and a settled-red
    /// verdict carries no last-settled field to render.
    #[test]
    fn the_render_names_the_last_settled_run_on_a_pending_line() {
        let pending = serde_json::json!({
            "verdict": "pending",
            "last_settled": {"workflow": "cli-ci", "sha": "mid",
                "verdict": "green", "created_at": "2026-09-30T11:00:00Z", "age": "<1h old"},
        });
        assert_eq!(
            main_ci_render(Some(&pending)),
            "pending (last settled: green cli-ci at mid, <1h old)".to_string()
        );
        assert_eq!(
            main_ci_render(Some(&serde_json::json!({"verdict": "pending"}))),
            "pending".to_string()
        );
        assert!(token_at("h", &[], &[]).get("last_settled").is_none());
    }

    /// Promotion moves the NEWEST failure onto the line and leaves the rest
    /// to the stale list: the previous head's red is live, the older one is
    /// archaeology with its own age line.
    #[test]
    fn the_pending_line_promotes_the_newest_failure_and_leaves_the_rest_stale() {
        let at_sha = vec![wf_run_with_id(
            "cli-ci",
            "head",
            "queued",
            "",
            "2026-09-30T10:00:00Z",
            "push",
            42,
        )];
        let on_main = vec![
            wf_run_event(
                "release",
                "old1",
                "completed",
                "failure",
                "2026-09-28T08:00:00Z",
                "workflow_dispatch",
            ),
            wf_run_with_id(
                "cli-ci",
                "prev",
                "completed",
                "failure",
                "2026-09-30T08:00:00Z",
                "push",
                42,
            ),
        ];
        assert_eq!(
            token_at("head", &at_sha, &on_main),
            serde_json::json!({
                "verdict": "pending", "workflow": "cli-ci", "sha": "prev",
                "stale": [{
                    "workflow": "release", "sha": "old1",
                    "created_at": "2026-09-28T08:00:00Z",
                }],
            })
        );
    }

    /// The rendered line names the workflow and sha; a plain verdict is the
    /// word alone, legacy string tokens pass through untouched, and a live
    /// platform incident rides the explanation beside the verdict. The
    /// casualty discrimination feeding the attachment lives here too: only a
    /// cancelled, never-started, runnerless job set is a casualty - a
    /// started job (manual or timeout cancel), a failure conclusion, and an
    /// empty job set are causes.
    #[test]
    fn main_ci_render_names_the_failed_workflow_and_sha() {
        let red = serde_json::json!({"verdict": "red", "workflow": "cli-ci", "sha": "a1"});
        assert_eq!(main_ci_render(Some(&red)), "red (cli-ci at a1)".to_string());
        assert_eq!(
            main_ci_render(Some(&serde_json::json!({"verdict": "green"}))),
            "green".to_string()
        );
        assert_eq!(
            main_ci_render(Some(&Value::String("green".into()))),
            "green".to_string()
        );
        assert_eq!(main_ci_render(None), "-".to_string());
        let casualty = serde_json::json!({
            "name": "cargo audit", "status": "completed", "conclusion": "cancelled",
            "runner_name": null, "started_at": null, "completed_at": "2026-10-05T19:48:00Z",
        });
        assert!(crate::gh_incident::cancelled_no_runner(&[casualty.clone()]));
        let ran_then_cancelled = serde_json::json!({
            "name": "cli-ci", "status": "completed", "conclusion": "cancelled",
            "runner_name": null, "started_at": "2026-10-05T19:27:00Z",
        });
        assert!(!crate::gh_incident::cancelled_no_runner(&[
            ran_then_cancelled
        ]));
        let test_failure = serde_json::json!({
            "name": "rollup", "status": "completed", "conclusion": "failure",
            "runner_name": null, "started_at": "2026-10-05T19:27:00Z",
        });
        assert!(!crate::gh_incident::cancelled_no_runner(&[test_failure]));
        assert!(!crate::gh_incident::cancelled_no_runner(&[]));
        let incidents = serde_json::json!([
            {"name": "Incident with Actions", "status": "investigating",
             "created_at": "2026-10-05T19:11:58Z", "updated_at": "2026-10-05T21:32:31Z"},
        ]);
        let mut with_incident = red.as_object().unwrap().clone();
        with_incident.insert("platform_incident".into(), incidents.clone());
        assert_eq!(
            main_ci_render(Some(&Value::Object(with_incident))),
            "red (cli-ci at a1) GitHub Actions incident: investigating since 2026-10-05T21:32:31Z"
                .to_string()
        );
        assert_eq!(
            crate::gh_incident::incident_line(&Value::Array(vec![])),
            "GitHub Actions incident: unknown since unknown"
        );
    }

    /// The merge gate's main-verdict read is the TTL-cached one: a fresh row
    /// answers (with the freshness stamp) and no live gh read runs behind it,
    /// the red-run shape is read in one accessor, and an expired row is a
    /// miss for the row op. The row store rides `FNO_GH_FACTS_DIR` so no
    /// test touches a real state root; the variable is process-global but
    /// nothing else in this binary reads it.
    #[test]
    fn the_merge_gates_main_verdict_read_is_ttl_cached() {
        let root = std::env::temp_dir().join(format!("fno-main-ci-cache-{}", std::process::id()));
        std::env::set_var("FNO_GH_FACTS_DIR", &root);
        let repo = Path::new(env!("CARGO_MANIFEST_DIR"));
        let slug = crate::finalize::slug_from_git_remote(repo).expect("crate lives in a repo");
        let token = serde_json::json!({"verdict": "red", "workflow": "cli-ci", "sha": "a1"});
        assert!(crate::gh_cache::write_row(
            &root,
            "main-ci",
            &slug,
            None,
            &json!({ "token": token })
        ));
        let got = main_ci_reading_cached(repo).unwrap();
        assert_eq!(got["verdict"], json!("red"));
        assert_eq!(got["workflow"], json!("cli-ci"));
        assert_eq!(got["sha"], json!("a1"));
        assert!(crate::gh_cache::write_row(
            &root,
            "main-ci",
            &slug,
            None,
            &json!({ "token": json!("green") })
        ));
        assert_eq!(main_ci_reading_cached(repo).unwrap(), json!("green"));
        assert_eq!(
            main_ci_red_run(&got),
            Some(("cli-ci".to_string(), "a1".to_string()))
        );
        assert_eq!(main_ci_red_run(&Value::String("pending".into())), None);
        let kind_dir = root.join("main-ci");
        let path = std::fs::read_dir(&kind_dir)
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        let mut row =
            serde_json::from_str::<Value>(&std::fs::read_to_string(&path).unwrap()).unwrap();
        row["ts"] = json!(0.0);
        std::fs::write(&path, row.to_string()).unwrap();
        assert_eq!(
            crate::gh_cache::read_row(
                &root,
                "main-ci",
                &slug,
                None,
                Some(60),
                crate::gh_cache::now_secs()
            )["row"],
            Value::Null
        );
        std::env::remove_var("FNO_GH_FACTS_DIR");
        std::fs::remove_dir_all(&root).ok();
    }

    /// `pr status main` reads every workflow at the head sha. The filed
    /// trap: a newest-run probe returned a passing sibling and stayed
    /// silent while rust-ci was red. A queued sibling with no check run
    /// yet holds the verdict pending, never green. A failed read exits 4.
    #[test]
    fn status_main_reads_red_when_one_workflow_fails_beside_green_ones() {
        let mut runs = vec![wf_run(
            "rust-ci",
            "h",
            "completed",
            "failure",
            "2026-09-30T10:00:00Z",
        )];
        for name in ["cli-ci", "guards", "internal-refs", "stacked-base-guard"] {
            runs.push(wf_run(
                name,
                "h",
                "completed",
                "success",
                "2026-09-30T10:05:00Z",
            ));
        }
        let (receipt, code) = status_main_receipt(Ok(token_at("h", &runs, &[])));
        assert_eq!((receipt["verdict"].as_str(), code), (Some("red"), 1));
        assert_eq!(receipt["main_ci"], "red (rust-ci at h)");

        runs[0] = wf_run("rust-ci", "h", "queued", "", "2026-09-30T10:00:00Z");
        let (receipt, code) = status_main_receipt(Ok(token_at("h", &runs, &[])));
        assert_eq!((receipt["verdict"].as_str(), code), (Some("pending"), 2));

        let (receipt, code) = status_main_receipt(Err("gh api failed".into()));
        assert_eq!((receipt["verdict"].as_str(), code), (Some("error"), 4));
    }
}
