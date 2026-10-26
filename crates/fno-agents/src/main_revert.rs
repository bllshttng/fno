//! The main_revert arm: when main stays red, revert the culprit merge first
//! and fix forward second.
//!
//! Opt-in per repo with `main_repair.auto_revert = true`. Each tick reads
//! main's verdict through the merge gate's own cached reduction. A red head
//! that stays red past `main_repair.revert_after_minutes` (default 10) is
//! walked first-parent back to its last green commit. The oldest red commit
//! after it is the culprit. When the culprit is a `Merge pull request #N`
//! commit whose failing checks are not all known flakes, the arm files a
//! node tagged `main-repair`, opens a revert PR bound to it, and asks the
//! culprit's author to re-land with the fix. The merge gate already lets a
//! `main-repair` node through a red main, so the revert needs one CI run.
//!
//! Anything the walk cannot prove is refused, never guessed: a cancelled or
//! missing run between the red head and the last green commit, a culprit
//! that is not a PR merge, or no green commit within reach. Every outcome
//! is journaled once per red head as `main_revert_check`; the journal is
//! the arm's memory, so a restart never opens a second revert.

use std::collections::HashSet;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{json, Map, Value};

use crate::paths::AgentsHome;

const INTERVAL: Duration = Duration::from_secs(60);
const WINDOW_MS: i64 = 7 * 24 * 60 * 60 * 1000;
const MAX_EVENTS: u32 = 10_000;
const DEFAULT_DELAY_MIN: i64 = 10;
/// How far back the first-parent walk looks for a green commit.
const WALK_DEPTH: usize = 10;
const GIT_TIMEOUT: Duration = Duration::from_secs(120);
pub(crate) const CHECK_EVENT: &str = "main_revert_check";
/// Outcomes after which this red head needs nothing more.
const FINAL: &[&str] = &["opened", "already_opened", "ambiguous", "flaky", "failed"];

#[derive(Default)]
pub struct Arm {
    last_tick: Mutex<Option<Instant>>,
    in_flight: Arc<AtomicBool>,
}

/// One commit on main's first-parent chain, with the red workflow's newest
/// push-run bucket at that sha (`pass`, `fail`, `cancel`, `pending`, or
/// `none` when the workflow never ran there).
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Commit {
    pub sha: String,
    pub parents: usize,
    pub subject: String,
    pub bucket: String,
}

#[derive(Debug, PartialEq)]
pub(crate) enum Decision {
    Wait,
    Ambiguous(String),
    Flaky,
    AlreadyOpened,
    Revert { sha: String, pr: u64 },
}

/// One journaled read, folded per repo slug and red head.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Row {
    pub ts_ms: i64,
    pub slug: String,
    pub head: String,
    pub outcome: String,
    pub culprit: String,
}

fn short(sha: &str) -> String {
    sha.chars().take(8).collect()
}

/// The oldest red commit after the newest green one. `walk[0]` is the red
/// head. A commit with no settled verdict before a green one is refused: a
/// cancelled run can hide the commit that broke main.
pub(crate) fn culprit(walk: &[Commit]) -> Result<&Commit, String> {
    let mut oldest_red: Option<&Commit> = None;
    for commit in walk {
        match commit.bucket.as_str() {
            "fail" => oldest_red = Some(commit),
            "pass" => {
                return oldest_red
                    .ok_or_else(|| format!("{} is green, not red", short(&commit.sha)));
            }
            other => {
                return Err(format!(
                    "no settled run at {} ({other}), so the first red commit is unknown",
                    short(&commit.sha)
                ));
            }
        }
    }
    Err(format!(
        "no green commit in the last {} on main",
        walk.len()
    ))
}

/// `Merge pull request #3958 from owner/branch` -> 3958.
pub(crate) fn merged_pr(subject: &str) -> Option<u64> {
    let rest = subject.strip_prefix("Merge pull request #")?;
    let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
    digits.parse().ok()
}

/// The pure decision for one red head that has stayed red since
/// `first_seen_ms`. `failing` is the culprit's failing check names and
/// `flakes` the names heal's ledger saw pass on a rerun.
#[allow(clippy::too_many_arguments)]
pub(crate) fn decide(
    first_seen_ms: i64,
    now_ms: i64,
    delay_ms: i64,
    walk: &[Commit],
    opened: &HashSet<String>,
    failing: &[String],
    flakes: &HashSet<String>,
) -> Decision {
    if now_ms.saturating_sub(first_seen_ms) < delay_ms {
        return Decision::Wait;
    }
    let commit = match culprit(walk) {
        Ok(commit) => commit,
        Err(reason) => return Decision::Ambiguous(reason),
    };
    if opened.contains(&commit.sha) {
        return Decision::AlreadyOpened;
    }
    let pr = match merged_pr(&commit.subject) {
        Some(pr) if commit.parents >= 2 => pr,
        _ => {
            return Decision::Ambiguous(format!(
                "first red commit {} is not a PR merge",
                short(&commit.sha)
            ))
        }
    };
    if !failing.is_empty() && failing.iter().all(|name| flakes.contains(name)) {
        return Decision::Flaky;
    }
    Decision::Revert {
        sha: commit.sha.clone(),
        pr,
    }
}

/// When the arm first saw this head red, and whether a final outcome already
/// closed it. `None` first-seen means this tick is the first sighting.
pub(crate) fn head_state(rows: &[Row], slug: &str, head: &str) -> (Option<i64>, bool) {
    let mine = rows.iter().filter(|r| r.slug == slug && r.head == head);
    let mut first: Option<i64> = None;
    let mut closed = false;
    for row in mine {
        first = Some(first.map_or(row.ts_ms, |f| f.min(row.ts_ms)));
        closed |= FINAL.contains(&row.outcome.as_str());
    }
    (first, closed)
}

/// Culprit shas this repo already opened a revert for, under any head.
pub(crate) fn opened_culprits(rows: &[Row], slug: &str) -> HashSet<String> {
    rows.iter()
        .filter(|r| r.slug == slug && r.outcome == "opened" && !r.culprit.is_empty())
        .map(|r| r.culprit.clone())
        .collect()
}

fn millis_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

fn read_rows(path: &Path, now_ms: i64) -> Result<Vec<Row>, String> {
    let query = crate::event_store::EventQuery {
        since_ms: Some(now_ms.saturating_sub(WINDOW_MS)),
        limit: Some(MAX_EVENTS),
        ..crate::event_store::EventQuery::of_types(&[CHECK_EVENT])
    };
    let text = crate::event_store::journal_text_checked(path, &query)?;
    let mut rows = Vec::new();
    for line in text.lines().filter(|l| !l.trim().is_empty()) {
        let Ok(row) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let ts = row
            .get("ts")
            .and_then(Value::as_str)
            .and_then(|stamp| chrono::DateTime::parse_from_rfc3339(stamp).ok())
            .map(|stamp| stamp.timestamp_millis());
        let (Some(ts_ms), Some(data)) = (ts, row.get("data")) else {
            continue;
        };
        let text = |key: &str| {
            data.get(key)
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string()
        };
        rows.push(Row {
            ts_ms,
            slug: text("slug"),
            head: text("head"),
            outcome: text("outcome"),
            culprit: text("culprit"),
        });
    }
    Ok(rows)
}

fn enabled(root: &Path) -> bool {
    crate::agents_config::config_value_deep(root, &["main_repair", "auto_revert"])
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
}

fn delay_ms(root: &Path) -> i64 {
    crate::agents_config::config_value_deep(root, &["main_repair", "revert_after_minutes"])
        .and_then(|v| v.as_integer())
        .filter(|m| *m >= 0)
        .unwrap_or(DEFAULT_DELAY_MIN)
        * 60_000
}

/// The red workflow's newest push-run bucket at one sha.
fn run_bucket(root: &Path, workflow: &str, sha: &str) -> Result<String, String> {
    let raw = crate::pr_push::gh_api(
        "gh",
        root,
        &format!("repos/{{owner}}/{{repo}}/actions/runs?head_sha={sha}&event=push&per_page=50"),
        &[],
    )?;
    let page: Value = serde_json::from_str(&raw).map_err(|e| format!("unparseable runs: {e}"))?;
    let newest = page
        .get("workflow_runs")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|run| run.get("name").and_then(Value::as_str) == Some(workflow))
        .max_by_key(|run| {
            run.get("created_at")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string()
        });
    Ok(newest
        .map_or("none", crate::pr_push::rest_bucket)
        .to_string())
}

/// main's first-parent chain from `head`, each commit carrying the red
/// workflow's bucket. Stops at the first green commit: nothing older matters.
fn walk_main(root: &Path, workflow: &str, head: &str) -> Result<Vec<Commit>, String> {
    let raw = crate::pr_push::gh_api(
        "gh",
        root,
        &format!("repos/{{owner}}/{{repo}}/commits?sha={head}&per_page=100"),
        &[],
    )?;
    let page: Value =
        serde_json::from_str(&raw).map_err(|e| format!("unparseable commits: {e}"))?;
    let rows = page.as_array().cloned().unwrap_or_default();
    let mut walk = Vec::new();
    let mut next = Some(head.to_string());
    while let Some(sha) = next.take() {
        if walk.len() >= WALK_DEPTH {
            break;
        }
        let Some(row) = rows
            .iter()
            .find(|r| r.get("sha").and_then(Value::as_str) == Some(sha.as_str()))
        else {
            break;
        };
        let parents: Vec<String> = row
            .get("parents")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|p| p.get("sha").and_then(Value::as_str).map(str::to_string))
            .collect();
        let subject = row
            .pointer("/commit/message")
            .and_then(Value::as_str)
            .unwrap_or("")
            .lines()
            .next()
            .unwrap_or("")
            .to_string();
        let bucket = run_bucket(root, workflow, &sha)?;
        let green = bucket == "pass";
        next = parents.first().cloned();
        walk.push(Commit {
            sha,
            parents: parents.len(),
            subject,
            bucket,
        });
        if green {
            break;
        }
    }
    Ok(walk)
}

fn failing_checks(root: &Path, sha: &str) -> Vec<String> {
    let Ok(rows) = crate::pr_push::read_checks_rows("gh", root, sha) else {
        return Vec::new();
    };
    let latest = crate::check_supersession::latest_per_name(&Value::Array(rows));
    latest
        .as_array()
        .into_iter()
        .flatten()
        .filter(|row| row.get("bucket").and_then(Value::as_str) == Some("fail"))
        .filter_map(|row| row.get("name").and_then(Value::as_str).map(str::to_string))
        .collect()
}

fn run(root: &Path, bin: &str, args: &[&str]) -> Result<String, String> {
    match crate::pr_push::run_labeled("main-revert", bin, args, root, GIT_TIMEOUT)? {
        (true, out, _) => Ok(out),
        (false, _, err) => Err(format!("{bin} {}: {}", args.join(" "), err.trim())),
    }
}

/// File the repair node, open the revert PR bound to it, and tell the
/// culprit. Returns the repair node and the revert PR URL.
fn open_revert(
    root: &Path,
    sha: &str,
    pr: u64,
    workflow: &str,
) -> Result<(String, String), String> {
    let title = format!("Revert PR {pr}: main red at {}", short(sha));
    let details = format!(
        "main went red in {workflow} at {}, the merge of PR {pr}. The main_revert arm opened this revert so main turns green in one CI run. The author of PR {pr} re-lands it with the fix.",
        short(sha)
    );
    let receipt = run(
        root,
        "fno",
        &["backlog", "idea", &title, "-J", "--details", &details],
    )?;
    let node = serde_json::from_str::<Value>(receipt.trim())
        .ok()
        .and_then(|v| v.get("id").and_then(Value::as_str).map(str::to_string))
        .ok_or_else(|| format!("backlog idea printed no id: {}", receipt.trim()))?;
    let tag = format!("tags+={}", crate::authorized_merge::MAIN_REPAIR_TAG);
    run(root, "fno", &["backlog", "update", &node, &tag])?;

    let branch = format!("bugfix/{node}-revert-pr{pr}");
    let tree = std::env::temp_dir().join(format!("fno-main-revert-{node}"));
    let tree_s = tree.to_string_lossy().into_owned();
    run(root, "git", &["fetch", "origin", "main"])?;
    run(
        root,
        "git",
        &["worktree", "add", "-b", &branch, &tree_s, "origin/main"],
    )?;
    let opened = (|| {
        run(&tree, "git", &["revert", "-m", "1", "--no-edit", sha])?;
        run(&tree, "git", &["push", "-u", "origin", &branch])?;
        let body = format!(
            "main is red: {workflow} failed at {}, the merge of #{pr}. This reverts that merge so main turns green in one CI run. The author of #{pr} re-lands it with the fix.\n\nFixes {node}",
            short(sha)
        );
        let url = run(
            &tree,
            "gh",
            &[
                "pr", "create", "--base", "main", "--head", &branch, "--title", &title, "--body",
                &body,
            ],
        )?;
        Ok::<String, String>(url.trim().to_string())
    })();
    let _ = run(root, "git", &["worktree", "remove", "--force", &tree_s]);
    let url = opened?;

    let ask = format!(
        "main went red at {} in {workflow}. The fno daemon opened a revert: {url}. Please re-land this change with the fix in a new PR.",
        short(sha)
    );
    let _ = run(
        root,
        "gh",
        &[
            "api",
            &format!("repos/{{owner}}/{{repo}}/issues/{pr}/comments"),
            "-f",
            &format!("body={ask}"),
        ],
    );
    if let Some(culprit_node) = crate::authorized_merge::pr_bound_entry(root, pr)
        .and_then(|e| crate::graph_store::entry_id(&e).map(str::to_string))
    {
        let _ = run(root, "fno", &["backlog", "note", &culprit_node, &ask]);
    }
    Ok((node, url))
}

/// One repo: read main, decide, act. Returns the outcome word and the
/// fields its journal row carries, or `None` when nothing is worth a row.
fn check_repo(
    root: &Path,
    slug: &str,
    rows: &[Row],
    flakes: &HashSet<String>,
    now_ms: i64,
) -> Option<(&'static str, Map<String, Value>)> {
    let token = crate::main_ci::main_ci_reading_cached(root).ok()?;
    let (workflow, head) = crate::main_ci::main_ci_red_run(&token)?;
    let (first_seen, closed) = head_state(rows, slug, &head);
    if closed {
        return None;
    }
    let mut fields = Map::new();
    fields.insert("head".into(), json!(head));
    fields.insert("workflow".into(), json!(workflow));
    let Some(first_seen) = first_seen else {
        return Some(("seen", fields));
    };
    let delay = delay_ms(root);
    if now_ms.saturating_sub(first_seen) < delay {
        return None;
    }
    let walk = match walk_main(root, &workflow, &head) {
        Ok(walk) => walk,
        Err(reason) => {
            fields.insert("reason".into(), json!(reason));
            return Some(("unreadable", fields));
        }
    };
    let failing = culprit(&walk)
        .map(|c| failing_checks(root, &c.sha))
        .unwrap_or_default();
    let opened = opened_culprits(rows, slug);
    match decide(first_seen, now_ms, delay, &walk, &opened, &failing, flakes) {
        Decision::Wait => None,
        Decision::Ambiguous(reason) => {
            fields.insert("reason".into(), json!(reason));
            Some(("ambiguous", fields))
        }
        Decision::Flaky => {
            fields.insert("failing".into(), json!(failing));
            Some(("flaky", fields))
        }
        Decision::AlreadyOpened => Some(("already_opened", fields)),
        Decision::Revert { sha, pr } => {
            fields.insert("culprit".into(), json!(sha));
            fields.insert("pr".into(), json!(pr));
            match open_revert(root, &sha, pr, &workflow) {
                Ok((node, url)) => {
                    fields.insert("node".into(), json!(node));
                    fields.insert("url".into(), json!(url));
                    Some(("opened", fields))
                }
                Err(reason) => {
                    fields.insert("reason".into(), json!(reason));
                    Some(("failed", fields))
                }
            }
        }
    }
}

pub(crate) fn run_pass(home: &AgentsHome) -> Result<(), String> {
    let roots: Vec<String> = crate::daemon::worktree_sweep::registry_repo_roots(home)
        .into_iter()
        .filter(|root| enabled(Path::new(root)))
        .collect();
    if roots.is_empty() {
        return Ok(());
    }
    let now_ms = millis_now();
    let path = crate::daemon::global_events_path(home);
    let rows = read_rows(&path, now_ms)?;
    let flakes = crate::ci_cadence::flake_checks(&path);
    let emitter = crate::events::EventEmitter::new(path, "daemon");
    let mut seen_slugs = HashSet::new();
    for root in roots {
        let root = Path::new(&root);
        // Two clones of one repo are one main.
        let Some(slug) = crate::pr_status::cache::git_slug(root) else {
            continue;
        };
        if !seen_slugs.insert(slug.clone()) {
            continue;
        }
        let Some((outcome, mut fields)) = check_repo(root, &slug, &rows, &flakes, now_ms) else {
            continue;
        };
        fields.insert("slug".into(), json!(slug));
        fields.insert("outcome".into(), json!(outcome));
        emitter
            .emit_fields(CHECK_EVENT, fields)
            .map_err(|e| format!("main_revert_check row did not land: {e}"))?;
    }
    Ok(())
}

pub fn maybe_tick(arm: &Arm, home: AgentsHome) {
    {
        let mut last = arm
            .last_tick
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if last.is_some_and(|tick| tick.elapsed() < INTERVAL)
            || arm.in_flight.swap(true, Ordering::SeqCst)
        {
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

    const MIN: i64 = 60_000;

    fn commit(sha: &str, bucket: &str, subject: &str, parents: usize) -> Commit {
        Commit {
            sha: sha.into(),
            parents,
            subject: subject.into(),
            bucket: bucket.into(),
        }
    }

    fn merge(sha: &str, bucket: &str, pr: u64) -> Commit {
        commit(
            sha,
            bucket,
            &format!("Merge pull request #{pr} from o/feature/x-1"),
            2,
        )
    }

    fn row(ts_ms: i64, head: &str, outcome: &str, culprit: &str) -> Row {
        Row {
            ts_ms,
            slug: "o/r".into(),
            head: head.into(),
            outcome: outcome.into(),
            culprit: culprit.into(),
        }
    }

    fn none() -> HashSet<String> {
        HashSet::new()
    }

    #[test]
    fn the_oldest_red_merge_after_the_last_green_is_reverted() {
        // AC1-HP: head and its parent are red, the grandparent green; the
        // parent broke main, not the head.
        let walk = [
            merge("c3", "fail", 12),
            merge("c2", "fail", 11),
            merge("c1", "pass", 10),
        ];
        let decision = decide(0, 11 * MIN, 10 * MIN, &walk, &none(), &[], &none());
        assert_eq!(
            decision,
            Decision::Revert {
                sha: "c2".into(),
                pr: 11
            }
        );
    }

    #[test]
    fn an_unsettled_run_before_the_last_green_refuses_to_guess() {
        // AC2-ERR: a cancelled run can hide the commit that broke main.
        let walk = [
            merge("c3", "fail", 12),
            merge("c2", "cancel", 11),
            merge("c1", "pass", 10),
        ];
        let decision = decide(0, 11 * MIN, 10 * MIN, &walk, &none(), &[], &none());
        assert!(matches!(decision, Decision::Ambiguous(r) if r.contains("c2")));
        // No green within reach is refused too.
        let walk = [merge("c3", "fail", 12)];
        assert!(matches!(
            decide(0, 11 * MIN, 10 * MIN, &walk, &none(), &[], &none()),
            Decision::Ambiguous(_)
        ));
    }

    #[test]
    fn a_culprit_that_is_not_a_pr_merge_is_refused() {
        let walk = [
            commit("c2", "fail", "fix: direct push", 1),
            merge("c1", "pass", 10),
        ];
        assert!(matches!(
            decide(0, 11 * MIN, 10 * MIN, &walk, &none(), &[], &none()),
            Decision::Ambiguous(r) if r.contains("not a PR merge")
        ));
    }

    #[test]
    fn a_red_made_only_of_known_flakes_is_not_reverted() {
        // AC3-ERR: every failing check sits on heal's flake ledger.
        let walk = [merge("c2", "fail", 11), merge("c1", "pass", 10)];
        let flakes: HashSet<String> = ["cli-ci / e2e".to_string()].into();
        let failing = ["cli-ci / e2e".to_string()];
        assert_eq!(
            decide(0, 11 * MIN, 10 * MIN, &walk, &none(), &failing, &flakes),
            Decision::Flaky
        );
        // One real failure beside the flake makes it a real red.
        let failing = ["cli-ci / e2e".to_string(), "cli-ci / unit".to_string()];
        assert!(matches!(
            decide(0, 11 * MIN, 10 * MIN, &walk, &none(), &failing, &flakes),
            Decision::Revert { .. }
        ));
    }

    #[test]
    fn a_culprit_already_reverted_under_any_head_is_not_reverted_again() {
        // AC4-EDGE: the journal is the memory across heads and restarts.
        let rows = [
            row(0, "c2", "seen", ""),
            row(MIN, "c2", "opened", "c2"),
            row(2 * MIN, "c3", "seen", ""),
        ];
        let opened = opened_culprits(&rows, "o/r");
        let walk = [
            merge("c3", "fail", 12),
            merge("c2", "fail", 11),
            merge("c1", "pass", 10),
        ];
        assert_eq!(
            decide(0, 13 * MIN, 10 * MIN, &walk, &opened, &[], &none()),
            Decision::AlreadyOpened
        );
        assert_eq!(head_state(&rows, "o/r", "c2"), (Some(0), true));
        assert_eq!(head_state(&rows, "o/r", "c3"), (Some(2 * MIN), false));
        assert_eq!(head_state(&rows, "other/repo", "c3"), (None, false));
    }

    #[test]
    fn a_red_younger_than_the_delay_waits() {
        // AC5-EDGE: a red that a rerun may clear gets its window first.
        let walk = [merge("c2", "fail", 11), merge("c1", "pass", 10)];
        assert_eq!(
            decide(0, 9 * MIN, 10 * MIN, &walk, &none(), &[], &none()),
            Decision::Wait
        );
    }

    #[test]
    fn the_pr_number_comes_from_the_merge_subject_only() {
        assert_eq!(
            merged_pr("Merge pull request #3958 from o/feature/x"),
            Some(3958)
        );
        assert_eq!(merged_pr("Revert \"Merge pull request #3958\""), None);
        assert_eq!(merged_pr("feat: thing (#3958)"), None);
    }
}
