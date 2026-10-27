//! The ci_tier arm: open PRs run CI in tiers of 6, opt-in per repo with
//! `ci_tier.enabled`. The rules and their reasons live in
//! docs/architecture/pr-heal.md, "The CI tier".

use std::collections::{BTreeMap, HashSet};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::paths::AgentsHome;

const INTERVAL: Duration = Duration::from_secs(180);
const SIZE: usize = 6;
const SPARE_PASS_SHARE: f64 = 0.8;
pub(crate) const PASS_EVENT: &str = "ci_tier_pass";

#[derive(Default)]
pub struct Arm {
    last_tick: Mutex<Option<Instant>>,
    in_flight: Arc<AtomicBool>,
}

/// Sorts a PR into line: main-repair, not draft, priority, then age.
pub(crate) type Rank = (bool, bool, u8, u64);

/// One head's runs, latest per workflow.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct Head {
    pub in_flight: Vec<u64>,
    pub cancelled: Vec<u64>,
    pub failed: bool,
    pub passed: usize,
    pub total: usize,
}

#[derive(Debug, Default, PartialEq)]
pub(crate) struct Plan {
    pub active: Vec<u64>,
    pub cancel: Vec<(u64, Vec<u64>)>,
    pub rerun: Vec<(u64, Vec<u64>)>,
}

fn word<'a>(v: &'a Value, pointer: &str) -> &'a str {
    v.pointer(pointer).and_then(Value::as_str).unwrap_or("")
}

fn id(run: &Value) -> u64 {
    run.get("id").and_then(Value::as_u64).unwrap_or(0)
}

fn is_gate(run: &Value) -> bool {
    word(run, "/name") == "merge-queue-gate" || word(run, "/path").ends_with("merge-queue-gate.yml")
}

/// Fold one head's `workflow_runs` to its latest run per workflow. A
/// merge-queue-gate run is left out: the tier never cancels or reruns one.
pub(crate) fn head_of(runs: &[Value]) -> Head {
    let mut latest: BTreeMap<&str, &Value> = BTreeMap::new();
    for run in runs.iter().filter(|r| !is_gate(r)) {
        let slot = latest.entry(word(run, "/name")).or_insert(run);
        if id(run) > id(slot) {
            *slot = run;
        }
    }
    let mut head = Head {
        total: latest.len(),
        ..Head::default()
    };
    for run in latest.into_values() {
        match (word(run, "/status"), word(run, "/conclusion")) {
            ("completed", "success" | "skipped" | "neutral") => head.passed += 1,
            ("completed", "cancelled") => head.cancelled.push(id(run)),
            ("completed", _) => head.failed = true,
            _ => head.in_flight.push(id(run)),
        }
    }
    head
}

/// The tier for one pass over ranked PRs and their heads.
pub(crate) fn plan(ranked: &[(Rank, Head)], slots: usize) -> Plan {
    let mut plan = Plan::default();
    for ((.., pr), head) in ranked {
        let idle = head.in_flight.is_empty();
        if idle && (head.cancelled.is_empty() || head.failed) {
            continue;
        }
        if plan.active.len() < slots {
            plan.active.push(*pr);
            if idle {
                plan.rerun.push((*pr, head.cancelled.clone()));
            }
        } else if !idle && (head.passed as f64) < SPARE_PASS_SHARE * head.total as f64 {
            // A head at 80 percent passed finishes instead.
            plan.cancel.push((*pr, head.in_flight.clone()));
        }
    }
    plan
}

fn pages(root: &Path, path: &str) -> Result<Vec<Value>, String> {
    crate::pr_push::gh_api_pages_conditional("gh", root, path)
}

fn runs_of(root: &Path, query: &str) -> Result<Vec<Value>, String> {
    let path = format!("repos/{{owner}}/{{repo}}/actions/runs?{query}&per_page=100");
    Ok(pages(root, &path)?
        .iter()
        .filter_map(|page| page.get("workflow_runs").and_then(Value::as_array))
        .flatten()
        .cloned()
        .collect())
}

/// The bound node's main-repair tag and priority; untagged p2 when none.
/// The binding keys matter: the daemon's own revert PR names its repair
/// node only by branch and `Fixes` trailer, never by `pr_number`.
fn node_rank(pull: &Value, entries: &[Value]) -> (bool, u8) {
    let number = pull.get("number").and_then(Value::as_i64).unwrap_or(0);
    let body = pull.get("body").and_then(Value::as_str);
    let url = pull.get("html_url").and_then(Value::as_str);
    let keys =
        crate::org_board::prs::pr_binding_keys(number, word(pull, "/head/ref"), url, body, entries);
    let mut ids = keys
        .branch
        .iter()
        .chain(&keys.backrefs)
        .chain(&keys.trailer);
    let bound = |nid: &String| {
        let id = Some(nid.as_str());
        entries
            .iter()
            .find(|e| crate::graph_store::entry_id(e) == id)
    };
    let Some(node) = ids.find_map(bound) else {
        return (false, 2);
    };
    let priority = word(node, "/priority").strip_prefix('p');
    let tag = crate::authorized_merge::MAIN_REPAIR_TAG;
    (
        crate::authorized_merge::node_carries_tag(node, tag),
        priority.and_then(|p| p.parse().ok()).unwrap_or(2),
    )
}

/// The open PRs, ranked, each with its head's runs, and the gate runs in
/// flight.
fn read_repo(root: &Path) -> Result<(Vec<(Rank, Head)>, usize), String> {
    let store = crate::backlog::api::Store::new(&crate::org_board::scope::graph_json_path(root));
    let entries = crate::backlog::api::rows(&store).map_err(|e| e.0)?;
    let mut prs = Vec::new();
    for page in pages(root, "repos/{owner}/{repo}/pulls?state=open&per_page=100")? {
        for pull in page.as_array().into_iter().flatten() {
            let number = pull.get("number").and_then(Value::as_u64).unwrap_or(0);
            let (repair, priority) = node_rank(pull, &entries);
            let draft = pull.get("draft").and_then(Value::as_bool) == Some(true);
            let sha = word(pull, "/head/sha").to_string();
            prs.push(((!repair, draft, priority, number), sha));
        }
    }
    prs.sort();
    let mut ranked = Vec::new();
    for (pr, sha) in prs {
        ranked.push((pr, head_of(&runs_of(root, &format!("head_sha={sha}"))?)));
    }
    let mut gates = HashSet::new();
    for status in ["queued", "in_progress"] {
        let runs = runs_of(root, &format!("status={status}"))?;
        gates.extend(runs.iter().filter(|r| is_gate(r)).map(id));
    }
    Ok((ranked, gates.len()))
}

/// POST `action` on each run; the journal records what gh refused.
fn act(root: &Path, list: &[(u64, Vec<u64>)], action: &str) -> Vec<Value> {
    let post = |run: &u64| {
        let path = format!("repos/{{owner}}/{{repo}}/actions/runs/{run}/{action}");
        match crate::pr_push::gh_api("gh", root, &path, &["-X", "POST"]) {
            Ok(_) => json!({"run": run, "ok": true}),
            Err(e) => json!({"run": run, "ok": false, "error": e}),
        }
    };
    list.iter()
        .map(|(pr, runs)| json!({"pr": pr, "runs": runs.iter().map(post).collect::<Vec<_>>()}))
        .collect()
}

/// The runs this tier cancelled in the last day, from its own rows: a run
/// cancelled by hand or by other automation is never rerun.
fn ours(path: &Path, slug: &str) -> Result<HashSet<u64>, String> {
    let since = crate::ci_cadence::millis_now() - 24 * 3_600_000;
    let query = crate::event_store::EventQuery {
        since_ms: Some(since),
        ..crate::event_store::EventQuery::of_types(&[PASS_EVENT])
    };
    let text = crate::event_store::journal_text_checked(path, &query)?;
    let rows = text
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok());
    let mine = rows.filter(|r| r.pointer("/data/slug").and_then(Value::as_str) == Some(slug));
    let cancels = mine.flat_map(|r| r.pointer("/data/cancelled").cloned());
    let runs = cancels.flat_map(|c| c.as_array().cloned().unwrap_or_default());
    let posts = runs.flat_map(|pr| pr["runs"].as_array().cloned().unwrap_or_default());
    let landed = posts.filter(|p| p["ok"] == json!(true));
    Ok(landed.filter_map(|p| p["run"].as_u64()).collect())
}

fn pass_repo(root: &Path, slug: &str, path: &Path) -> Result<(), String> {
    let (mut ranked, gates) = read_repo(root)?;
    let ours = ours(path, slug)?;
    for (_, head) in &mut ranked {
        head.cancelled.retain(|run| ours.contains(run));
    }
    // Each merge-queue-gate run in flight holds one slot.
    let slots = SIZE.saturating_sub(gates);
    let plan = plan(&ranked, slots);
    let cancelled = act(root, &plan.cancel, "cancel");
    // A full rerun: a cancelled run has no failed job for rerun-failed-jobs.
    let rerun = act(root, &plan.rerun, "rerun");
    if cancelled.is_empty() && rerun.is_empty() {
        return Ok(());
    }
    let row = json!({"slug": slug, "slots": slots, "gates": gates, "active": plan.active,
        "cancelled": cancelled, "rerun": rerun});
    crate::events::EventEmitter::new(path.to_path_buf(), "daemon")
        .emit(PASS_EVENT, &row)
        .map_err(|e| format!("{PASS_EVENT} row did not land: {e}"))
}

pub(crate) fn run_pass(home: &AgentsHome) -> Result<(), String> {
    let path = crate::daemon::global_events_path(home);
    let mut slugs = HashSet::new();
    for root in crate::daemon::worktree_sweep::registry_repo_roots(home) {
        let root = Path::new(&root);
        let on = crate::agents_config::config_value_deep(root, &["ci_tier", "enabled"]);
        if on.and_then(|v| v.as_bool()) != Some(true) {
            continue;
        }
        // Two clones of one repo are one tier.
        let slug = crate::pr_status::cache::git_slug(root).filter(|s| slugs.insert(s.clone()));
        let Some(slug) = slug else { continue };
        if let Err(e) = pass_repo(root, &slug, &path) {
            eprintln!("ci-tier: {slug}: {e}");
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
            eprintln!("ci-tier: {error}");
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pr(number: u64, priority: u8) -> Rank {
        (true, false, priority, number)
    }

    fn head(f: &[u64], c: &[u64], failed: bool, passed: usize, total: usize) -> Head {
        let (in_flight, cancelled) = (f.to_vec(), c.to_vec());
        Head {
            in_flight,
            cancelled,
            failed,
            passed,
            total,
        }
    }

    #[test]
    fn a_head_folds_to_its_latest_run_per_workflow_and_skips_the_queue_gate() {
        let run = |id: u64, wf: &str, status: &str, conclusion: &str| json!({"id": id, "name": wf, "status": status, "conclusion": conclusion});
        let runs = vec![
            run(10, "a", "completed", "cancelled"),
            run(11, "a", "in_progress", ""),
            run(12, "b", "completed", "success"),
            run(13, "c", "completed", "cancelled"),
            json!({"id": 14, "name": "merge-queue-gate", "status": "queued"}),
        ];
        assert_eq!(head_of(&runs), head(&[11], &[13], false, 1, 3));
    }

    #[test]
    fn the_tier_fills_by_rank_cancels_the_rest_spares_near_green_and_reruns_on_entry() {
        let ranked = vec![
            (pr(1, 0), head(&[], &[], false, 4, 4)),
            (pr(2, 0), head(&[20], &[], false, 0, 3)),
            (pr(3, 1), head(&[], &[70], false, 1, 2)),
            (pr(4, 1), head(&[], &[80], true, 0, 2)),
            (pr(5, 2), head(&[50], &[], false, 4, 5)),
            (pr(6, 2), head(&[60], &[], false, 1, 5)),
            (pr(7, 3), head(&[], &[90], false, 0, 1)),
        ];
        let plan = plan(&ranked, 2);
        // Green 1 and idle red 4 hold no slot.
        assert_eq!(plan.active, vec![2, 3]);
        assert_eq!(plan.rerun, vec![(3, vec![70])]);
        // 5 has 80 percent passed; 7 waits with nothing to cancel.
        assert_eq!(plan.cancel, vec![(6, vec![60])]);
    }
}
