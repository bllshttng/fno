//! The main_revert arm: when main stays red, revert the culprit merge first
//! and fix forward second. Opt-in per repo (`main_repair.auto_revert`). A red
//! head that outlives `main_repair.revert_after_minutes` and is a PR merge
//! over a green parent is the culprit: the arm files a `main-repair` node,
//! opens the revert PR the merge gate lets through a red main, and asks the
//! culprit's author to re-land with the fix. What one read cannot prove is
//! refused, never guessed. One `main_revert_check` row per red head is the
//! arm's memory, so a restart never opens a second revert.

use std::collections::HashSet;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{json, Map, Value};

use crate::paths::AgentsHome;

const INTERVAL: Duration = Duration::from_secs(60);
const WINDOW_MS: i64 = 7 * 24 * 3_600_000;
const TIMEOUT: Duration = Duration::from_secs(120);
pub(crate) const CHECK_EVENT: &str = "main_revert_check";
const FINAL: &[&str] = &["opened", "ambiguous", "flaky", "failed"];

#[derive(Default)]
pub struct Arm {
    last_tick: Mutex<Option<Instant>>,
    in_flight: Arc<AtomicBool>,
}

#[derive(Debug, PartialEq)]
pub(crate) enum Decision {
    Ambiguous(String),
    Flaky,
    Revert(u64),
}

fn short(sha: &str) -> String {
    sha.chars().take(8).collect()
}

/// The red head is the culprit only when it is a PR merge whose first parent
/// ran green: any other parent verdict means an earlier commit may be the
/// first red, and a cancelled run can hide which one.
pub(crate) fn decide(
    subject: &str,
    parent_bucket: &str,
    failing: &[String],
    flakes: &HashSet<String>,
) -> Decision {
    if parent_bucket != "pass" {
        return Decision::Ambiguous(format!("the parent reads {parent_bucket}, not green"));
    }
    let pr = subject
        .strip_prefix("Merge pull request #")
        .map(|rest| {
            rest.chars()
                .take_while(char::is_ascii_digit)
                .collect::<String>()
        })
        .and_then(|digits| digits.parse().ok());
    let Some(pr) = pr else {
        return Decision::Ambiguous("the red head is not a PR merge".into());
    };
    // main reads red on a cancelled run too (a runner outage): with no
    // failing check, the merge broke nothing.
    if failing.is_empty() {
        return Decision::Ambiguous("no failing check at the red head".into());
    }
    if failing.iter().all(|f| flakes.contains(f)) {
        return Decision::Flaky;
    }
    Decision::Revert(pr)
}

/// When this repo's `head` was first seen red, and whether a final outcome
/// already closed it.
pub(crate) fn fold(rows: &[Value], slug: &str, head: &str) -> (Option<i64>, bool) {
    let (mut first, mut closed) = (None::<i64>, false);
    for row in rows {
        let field = |k: &str| row.pointer(&format!("/data/{k}")).and_then(Value::as_str);
        if field("slug") == Some(slug) && field("head") == Some(head) {
            let ts = crate::ci_cadence::ts_ms(row).unwrap_or(i64::MAX);
            first = Some(first.map_or(ts, |f| f.min(ts)));
            closed |= FINAL.contains(&field("outcome").unwrap_or(""));
        }
    }
    (first, closed)
}

fn gh_json(root: &Path, path: &str) -> Result<Value, String> {
    let raw = crate::pr_push::gh_api("gh", root, path, &[])?;
    serde_json::from_str(&raw).map_err(|e| format!("unparseable {path}: {e}"))
}

/// The head's subject line and its first parent's bucket for `workflow`.
fn read_head(root: &Path, workflow: &str, head: &str) -> Result<(String, String), String> {
    let commit = gh_json(root, &format!("repos/{{owner}}/{{repo}}/commits/{head}"))?;
    let subject = commit["commit"]["message"].as_str().unwrap_or("");
    let subject = subject.lines().next().unwrap_or("").to_string();
    let parents = commit["parents"].as_array().cloned().unwrap_or_default();
    // A squash or a direct push has one parent: never a merge to revert.
    let (2, Some(parent)) = (
        parents.len(),
        parents.first().and_then(|p| p["sha"].as_str()),
    ) else {
        return Ok((String::new(), "none".into()));
    };
    let path = format!("repos/{{owner}}/{{repo}}/actions/runs?head_sha={parent}&event=push");
    let runs = gh_json(root, &path)?;
    let bucket = runs["workflow_runs"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|r| r["name"].as_str() == Some(workflow))
        .max_by_key(|r| r["created_at"].as_str().unwrap_or("").to_string())
        .map_or("none", crate::pr_push::rest_bucket);
    Ok((subject, bucket.into()))
}

fn failing_checks(root: &Path, sha: &str) -> Result<Vec<String>, String> {
    let rows = crate::pr_push::read_checks_rows("gh", root, sha)?;
    let latest = crate::check_supersession::latest_per_name(&Value::Array(rows));
    Ok(latest
        .as_array()
        .into_iter()
        .flatten()
        .filter(|r| r["bucket"].as_str() == Some("fail"))
        .filter_map(|r| r["name"].as_str().map(str::to_string))
        .collect())
}

fn run(cwd: &Path, bin: &str, args: &[&str]) -> Result<String, String> {
    match crate::pr_push::run_labeled("main-revert", bin, args, cwd, TIMEOUT)? {
        (true, out, _) => Ok(out.trim().to_string()),
        (false, _, err) => Err(format!("{bin} {}: {}", args.join(" "), err.trim())),
    }
}

/// File the repair node, open the revert PR bound to it, tell the culprit.
fn open_revert(root: &Path, sha: &str, pr: u64, workflow: &str) -> Result<Value, String> {
    let title = format!("Revert PR {pr}: main red at {}", short(sha));
    let why = format!(
        "main went red in {workflow} at {}, the merge of PR {pr}.",
        short(sha)
    );
    let receipt = run(
        root,
        "fno",
        &["backlog", "idea", &title, "-J", "--details", &why],
    )?;
    let node = serde_json::from_str::<Value>(&receipt).ok();
    let node = node.and_then(|v| v["id"].as_str().map(str::to_string));
    let node = node.ok_or_else(|| format!("backlog idea printed no id: {receipt}"))?;
    // Past this point a failure names the filed node, so the row points at
    // the repair node left without a PR.
    let opened = revert_for(root, sha, pr, &node, &title, &why);
    opened.map_err(|e| format!("repair node {node} has no PR: {e}"))
}

fn revert_for(
    root: &Path,
    sha: &str,
    pr: u64,
    node: &str,
    title: &str,
    why: &str,
) -> Result<Value, String> {
    let tag = format!("tags+={}", crate::authorized_merge::MAIN_REPAIR_TAG);
    run(root, "fno", &["backlog", "update", node, &tag])?;
    let branch = format!("bugfix/{node}-revert-pr{pr}");
    let tree = std::env::temp_dir().join(format!("fno-main-revert-{node}"));
    let tree_s = tree.to_string_lossy().into_owned();
    run(root, "git", &["fetch", "origin", "main"])?;
    // The red verdict is a cached read: a main that moved since may already
    // carry the fix, and a revert on top of it would undo a green main.
    let fetched = run(root, "git", &["rev-parse", "origin/main"])?;
    if fetched != sha {
        return Err(format!(
            "main moved to {} since the red read",
            short(&fetched)
        ));
    }
    let add = ["worktree", "add", "-b", &branch, &tree_s, "origin/main"];
    run(root, "git", &add)?;
    let body = format!("{why} This reverts that merge so main turns green in one CI run. The author of #{pr} re-lands it with the fix.\n\nFixes {node}");
    let create = [
        "pr", "create", "--base", "main", "--head", &branch, "--title", &title, "--body", &body,
    ];
    let url = run(&tree, "git", &["revert", "-m", "1", "--no-edit", sha])
        .and_then(|_| run(&tree, "git", &["push", "-u", "origin", &branch]))
        .and_then(|_| run(&tree, "gh", &create));
    let _ = run(root, "git", &["worktree", "remove", "--force", &tree_s]);
    let url = url?;
    let ask = format!("{why} The fno daemon opened a revert: {url}. Please re-land this change with the fix in a new PR.");
    let comments = format!("repos/{{owner}}/{{repo}}/issues/{pr}/comments");
    let _ = run(
        root,
        "gh",
        &["api", &comments, "-f", &format!("body={ask}")],
    );
    let owner = crate::authorized_merge::pr_bound_entry(root, pr);
    if let Some(owner) = owner.and_then(|e| crate::graph_store::entry_id(&e).map(str::to_string)) {
        let _ = run(root, "fno", &["backlog", "note", &owner, &ask]);
    }
    Ok(json!({"pr": pr, "node": node, "url": url}))
}

/// One repo: the outcome word and its row fields, `None` when nothing is owed.
fn check_repo(
    root: &Path,
    slug: &str,
    rows: &[Value],
    flakes: &HashSet<String>,
    now_ms: i64,
) -> Option<(&'static str, Map<String, Value>)> {
    let delay =
        crate::agents_config::config_value_deep(root, &["main_repair", "revert_after_minutes"]);
    let delay_ms = delay.and_then(|v| v.as_integer()).unwrap_or(10) * 60_000;
    let token = crate::main_ci::main_ci_reading_cached(root).ok()?;
    let (workflow, head) = crate::main_ci::main_ci_red_run(&token)?;
    let (first, closed) = fold(rows, slug, &head);
    let mut fields = Map::from_iter([
        ("head".into(), json!(head)),
        ("workflow".into(), json!(workflow)),
    ]);
    match first {
        _ if closed => return None,
        None => return Some(("seen", fields)),
        Some(first) if now_ms.saturating_sub(first) < delay_ms => return None,
        Some(_) => {}
    }
    let read = read_head(root, &workflow, &head)
        .and_then(|(subject, parent)| Ok((subject, parent, failing_checks(root, &head)?)));
    let (outcome, extra) = match read {
        Err(reason) => ("unreadable", json!({"reason": reason})),
        Ok((subject, parent, failing)) => match decide(&subject, &parent, &failing, flakes) {
            Decision::Ambiguous(reason) => ("ambiguous", json!({"reason": reason})),
            Decision::Flaky => ("flaky", json!({"failing": failing})),
            Decision::Revert(pr) => match open_revert(root, &head, pr, &workflow) {
                Ok(opened) => ("opened", opened),
                Err(reason) => ("failed", json!({"pr": pr, "reason": reason})),
            },
        },
    };
    fields.extend(extra.as_object().cloned().unwrap_or_default());
    Some((outcome, fields))
}

pub(crate) fn run_pass(home: &AgentsHome) -> Result<(), String> {
    let on = |root: &String| {
        let key = crate::agents_config::config_value_deep(
            Path::new(root),
            &["main_repair", "auto_revert"],
        );
        key.and_then(|v| v.as_bool()) == Some(true)
    };
    let roots = crate::daemon::worktree_sweep::registry_repo_roots(home);
    let roots: Vec<String> = roots.into_iter().filter(on).collect();
    if roots.is_empty() {
        return Ok(());
    }
    let now_ms = crate::ci_cadence::millis_now();
    let path = crate::daemon::global_events_path(home);
    let query = crate::event_store::EventQuery {
        since_ms: Some(now_ms - WINDOW_MS),
        ..crate::event_store::EventQuery::of_types(&[CHECK_EVENT])
    };
    let text = crate::event_store::journal_text_checked(&path, &query)?;
    let rows: Vec<Value> = text
        .lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect();
    let emitter = crate::events::EventEmitter::new(path.clone(), "daemon");
    let mut slugs = HashSet::new();
    for root in roots {
        let root = Path::new(&root);
        // Two clones of one repo are one main.
        let slug = crate::pr_status::cache::git_slug(root).filter(|s| slugs.insert(s.clone()));
        let Some(slug) = slug else { continue };
        let flakes = crate::ci_cadence::flake_checks(&path, root);
        if let Some((outcome, mut fields)) = check_repo(root, &slug, &rows, &flakes, now_ms) {
            fields.insert("slug".into(), json!(slug));
            fields.insert("outcome".into(), json!(outcome));
            let landed = emitter.emit_fields(CHECK_EVENT, fields);
            landed.map_err(|e| format!("{CHECK_EVENT} row did not land: {e}"))?;
        }
    }
    Ok(())
}

pub fn maybe_tick(arm: &Arm, home: AgentsHome) {
    {
        let mut last = arm.last_tick.lock().unwrap_or_else(|e| e.into_inner());
        let busy = last.is_some_and(|t| t.elapsed() < INTERVAL);
        if busy || arm.in_flight.swap(true, Ordering::SeqCst) {
            return;
        }
        *last = Some(Instant::now());
    }
    let flag = Arc::clone(&arm.in_flight);
    tokio::task::spawn_blocking(move || {
        let _gate = crate::daemon::SweepGate(flag);
        if let Err(error) = run_pass(&home) {
            eprintln!("main-revert: {error}");
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    const MERGE: &str = "Merge pull request #11 from o/feature/x-1";

    fn decide_with(subject: &str, parent: &str, failing: &[&str], flakes: &[&str]) -> Decision {
        let failing: Vec<String> = failing.iter().map(|s| s.to_string()).collect();
        let flakes: HashSet<String> = flakes.iter().map(|s| s.to_string()).collect();
        decide(subject, parent, &failing, &flakes)
    }

    #[test]
    fn a_red_merge_over_a_green_parent_is_reverted() {
        assert_eq!(
            decide_with(MERGE, "pass", &["unit"], &[]),
            Decision::Revert(11)
        );
    }

    #[test]
    fn what_one_read_cannot_prove_is_refused() {
        // An unsettled or red parent may hide the first red commit; a squash,
        // a direct push or a revert subject is no PR merge.
        let cases = [
            (MERGE, "cancel"),
            (MERGE, "fail"),
            (MERGE, "none"),
            ("feat: thing (#11)", "pass"),
            ("Revert \"Merge pull request #11\"", "pass"),
        ];
        for (subject, parent) in cases {
            let decision = decide_with(subject, parent, &["unit"], &[]);
            assert!(
                matches!(decision, Decision::Ambiguous(_)),
                "{subject} {parent}"
            );
        }
        // A red made only of cancelled runs names no failing check.
        let cancelled = decide_with(MERGE, "pass", &[], &[]);
        assert!(matches!(cancelled, Decision::Ambiguous(_)));
    }

    #[test]
    fn a_red_made_only_of_known_flakes_is_not_reverted() {
        assert_eq!(
            decide_with(MERGE, "pass", &["e2e"], &["e2e"]),
            Decision::Flaky
        );
        let real = decide_with(MERGE, "pass", &["e2e", "unit"], &["e2e"]);
        assert_eq!(real, Decision::Revert(11));
    }

    #[test]
    fn the_journal_names_first_sighting_and_closure_per_repo_head() {
        let row = |ts: &str, slug: &str, head: &str, outcome: &str| json!({"ts": ts, "data": {"slug": slug, "head": head, "outcome": outcome}});
        let rows = [
            row("2026-10-26T10:00:00Z", "o/r", "c2", "seen"),
            row("2026-10-26T10:11:00Z", "o/r", "c2", "opened"),
            row("2026-10-26T10:12:00Z", "o/r", "c3", "seen"),
            row("2026-10-26T10:13:00Z", "x/y", "c3", "opened"),
        ];
        let ts = |i: usize| crate::ci_cadence::ts_ms(&rows[i]);
        assert_eq!(fold(&rows, "o/r", "c2"), (ts(0), true));
        assert_eq!(fold(&rows, "o/r", "c3"), (ts(2), false));
        assert_eq!(fold(&rows, "o/r", "c4"), (None, false));
    }
}
