//! The look_hold arm: a paint PR that waits for the user's look runs only
//! its shots. The rules and their reasons live in
//! docs/architecture/pr-heal.md, "The look hold".

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::ci_tier::{word, Head};
use crate::paths::AgentsHome;

const INTERVAL: Duration = Duration::from_secs(120);
// A look can wait days for the user.
const WINDOW_MS: i64 = 7 * 24 * 3_600_000;
const PAINT_CACHE_CAP: usize = 2_000;
pub(crate) const CANCEL_EVENT: &str = "look_hold_cancel";
pub(crate) const RELEASE_EVENT: &str = "look_hold_release";
/// The workflow a held head keeps when `merge.visual_shot_workflows` is
/// unset: footnote's cheap build (fmt, check) and the shots the user looks at.
const SHOT_WORKFLOWS: &str = "rust-ci.yml";

type PaintCache = Arc<Mutex<HashMap<String, bool>>>;

#[derive(Default)]
pub struct Arm {
    last_tick: Mutex<Option<Instant>>,
    in_flight: Arc<AtomicBool>,
    // A head's changed files never change, so each head costs one files read.
    paint: PaintCache,
}

/// The workflow files a held head keeps running, by file name.
fn shot_workflows(root: &Path) -> Vec<String> {
    let keys = ["merge", "visual_shot_workflows"];
    match crate::agents_config::config_value_deep(root, &keys) {
        Some(toml::Value::Array(items)) => items
            .iter()
            .filter_map(|i| i.as_str().map(str::to_string))
            .collect(),
        _ => vec![SHOT_WORKFLOWS.to_string()],
    }
}

/// One head's runs, the shot workflows left out: the hold never touches them.
pub(crate) fn head_of(runs: &[Value], keep: &[String]) -> Head {
    let file = |r: &Value| {
        word(r, "/path")
            .rsplit('/')
            .next()
            .unwrap_or("")
            .to_string()
    };
    let held: Vec<Value> = runs
        .iter()
        .filter(|r| !keep.contains(&file(r)))
        .cloned()
        .collect();
    crate::ci_tier::head_of(&held)
}

/// The runs this arm cancelled at `sha` and has not rerun, from its own rows,
/// oldest first. A run cancelled by hand or by other automation is never rerun.
pub(crate) fn ours(rows: &[Value], slug: &str, sha: &str) -> HashSet<u64> {
    let mut ours = HashSet::new();
    let mine = rows
        .iter()
        .filter(|r| word(r, "/data/slug") == slug && word(r, "/data/sha") == sha);
    for row in mine {
        let landed = row
            .pointer("/data/runs")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter(|p| p["ok"] == json!(true))
            .filter_map(|p| p["run"].as_u64());
        match row.get("type").and_then(Value::as_str) {
            Some(CANCEL_EVENT) => ours.extend(landed),
            Some(RELEASE_EVENT) => landed.for_each(|run| {
                ours.remove(&run);
            }),
            _ => {}
        }
    }
    ours
}

/// The runs to cancel and the runs to rerun on one head. `held` is the
/// visual-approval gate at that head: None when the gate cannot answer,
/// which neither holds nor releases.
pub(crate) fn decide(head: &Head, ours: &HashSet<u64>, held: Option<bool>) -> (Vec<u64>, Vec<u64>) {
    match held {
        Some(true) => (head.in_flight.clone(), Vec::new()),
        Some(false) => {
            let rerun = head.cancelled.iter().filter(|r| ours.contains(r));
            (Vec::new(), rerun.copied().collect())
        }
        None => (Vec::new(), Vec::new()),
    }
}

/// The visual-approval gate at `sha`. A head that touches no paint path
/// answers from the cache, so only paint heads read the gate.
fn held(paint: &PaintCache, root: &Path, slug: &str, pr: u64, sha: &str) -> Option<bool> {
    let probes = crate::authorized_merge::RealProbes;
    let key = format!("{slug}#{pr}@{sha}");
    let cached = paint
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(&key)
        .copied();
    let touches = match cached {
        Some(touches) => touches,
        None => {
            let files = crate::merge_gates::pr_file_paths(&probes, root, pr)?;
            let paths = crate::agents_config::visual_paint_paths(root);
            let touches = files
                .iter()
                .any(|f| crate::merge_gates::path_matches_paint(f, &paths));
            let mut cache = paint.lock().unwrap_or_else(|e| e.into_inner());
            if cache.len() >= PAINT_CACHE_CAP {
                cache.clear();
            }
            cache.insert(key, touches);
            touches
        }
    };
    if !touches {
        return Some(false);
    }
    match crate::merge_gates::visual_approval_blocker(&probes, root, pr, sha) {
        None => Some(false),
        Some(b) if b.code == "visual_approval" => Some(true),
        Some(_) => None,
    }
}

fn pass_repo(root: &Path, slug: &str, path: &Path, paint: &PaintCache) -> Result<(), String> {
    let query = crate::event_store::EventQuery {
        since_ms: Some(crate::ci_cadence::millis_now() - WINDOW_MS),
        ..crate::event_store::EventQuery::of_types(&[CANCEL_EVENT, RELEASE_EVENT])
    };
    let rows: Vec<Value> = crate::event_store::journal_text_checked(path, &query)?
        .lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect();
    let emitter = crate::events::EventEmitter::new(path.to_path_buf(), "daemon");
    let keep = shot_workflows(root);
    let pulls = crate::pr_push::gh_api_pages_conditional(
        "gh",
        root,
        "repos/{owner}/{repo}/pulls?state=open&per_page=100",
    )?;
    for pull in pulls.iter().filter_map(Value::as_array).flatten() {
        let pr = pull.get("number").and_then(Value::as_u64).unwrap_or(0);
        let sha = word(pull, "/head/sha");
        if pr == 0 || sha.is_empty() {
            continue;
        }
        let runs = crate::ci_tier::runs_of(root, &format!("head_sha={sha}"))?;
        let head = head_of(&runs, &keep);
        let ours = ours(&rows, slug, sha);
        if head.in_flight.is_empty() && !head.cancelled.iter().any(|r| ours.contains(r)) {
            continue;
        }
        let (cancel, rerun) = decide(&head, &ours, held(paint, root, slug, pr, sha));
        for (kind, list, action) in [
            (CANCEL_EVENT, cancel, "cancel"),
            (RELEASE_EVENT, rerun, "rerun"),
        ] {
            if list.is_empty() {
                continue;
            }
            let runs: Vec<Value> = list
                .iter()
                .map(|run| crate::ci_tier::post(root, run, action))
                .collect();
            let data = json!({"slug": slug, "pr": pr, "sha": sha, "runs": runs});
            emitter
                .emit(kind, &data)
                .map_err(|e| format!("{kind} row did not land: {e}"))?;
        }
    }
    Ok(())
}

pub(crate) fn run_pass(home: &AgentsHome, paint: &PaintCache) -> Result<(), String> {
    let path = crate::daemon::global_events_path(home);
    let mut slugs = HashSet::new();
    for root in crate::daemon::worktree_sweep::registry_repo_roots(home) {
        let root = Path::new(&root);
        // A project with no paint surface has no look to wait for.
        if crate::agents_config::visual_paint_paths(root).is_empty() {
            continue;
        }
        let slug = crate::pr_status::cache::git_slug(root).filter(|s| slugs.insert(s.clone()));
        let Some(slug) = slug else { continue };
        if let Err(e) = pass_repo(root, &slug, &path, paint) {
            eprintln!("look-hold: {slug}: {e}");
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
    let paint = Arc::clone(&arm.paint);
    tokio::task::spawn_blocking(move || {
        let _gate = crate::daemon::SweepGate(flag);
        if let Err(error) = run_pass(&home, &paint) {
            eprintln!("look-hold: {error}");
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(id: u64, workflow: &str, status: &str, conclusion: &str) -> Value {
        let path = format!(".github/workflows/{workflow}.yml");
        json!({"id": id, "name": workflow, "path": path, "status": status, "conclusion": conclusion})
    }

    #[test]
    fn a_held_head_keeps_its_shots_and_cancels_the_rest() {
        let runs = [
            run(1, "cli-ci", "in_progress", ""),
            run(2, "rust-ci", "in_progress", ""),
            run(3, "guards", "queued", ""),
            run(4, "merge-queue-gate", "queued", ""),
        ];
        let head = head_of(&runs, &["rust-ci.yml".to_string()]);
        assert_eq!(
            decide(&head, &HashSet::new(), Some(true)),
            (vec![1, 3], vec![])
        );
        // An unreadable gate neither holds nor releases.
        assert_eq!(decide(&head, &HashSet::new(), None), (vec![], vec![]));
    }

    #[test]
    fn the_answer_reruns_only_the_runs_this_arm_cancelled_at_that_head() {
        let row = |kind: &str, sha: &str, run: u64, ok: bool| json!({"type": kind, "data": {"slug": "o/r", "sha": sha, "runs": [{"run": run, "ok": ok}]}});
        let rows = [
            row(CANCEL_EVENT, "aaa", 1, true),
            row(CANCEL_EVENT, "aaa", 2, false),
            row(CANCEL_EVENT, "aaa", 5, true),
            row(RELEASE_EVENT, "aaa", 5, true),
            row(CANCEL_EVENT, "bbb", 3, true),
        ];
        let ours = ours(&rows, "o/r", "aaa");
        assert_eq!(ours, HashSet::from([1]));
        let done = |id, workflow| run(id, workflow, "completed", "cancelled");
        let head = head_of(
            &[done(1, "cli-ci"), done(2, "guards"), done(5, "release")],
            &[],
        );
        // 2 was cancelled by hand, and 5 was already rerun once.
        assert_eq!(decide(&head, &ours, Some(false)), (vec![], vec![1]));
        assert_eq!(decide(&head, &ours, Some(true)), (vec![], vec![]));
    }
}
