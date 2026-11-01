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
const HEARTBEAT_MS: i64 = 10 * 60_000;
const FRESH_MS: i64 = 20 * 60_000;
const DAY_MS: i64 = 24 * 3_600_000;
pub(crate) const PASS_EVENT: &str = "ci_tier_pass";
const HELD_EVENT: &str = "ci_tier_held";
const WAKE_EVENT: &str = "ci_tier_wake";

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

pub(crate) fn word<'a>(v: &'a Value, pointer: &str) -> &'a str {
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

pub(crate) fn runs_of(root: &Path, query: &str) -> Result<Vec<Value>, String> {
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

type Repo = (Vec<(Rank, Head)>, Vec<Value>, usize);

/// The open PRs, ranked, each with its head's runs; the same order as
/// `{pr, branch}` rows; and the gate runs in flight.
fn read_repo(root: &Path) -> Result<Repo, String> {
    let store = crate::backlog::api::Store::new(&crate::org_board::scope::graph_json_path(root));
    let entries = crate::backlog::api::rows(&store).map_err(|e| e.0)?;
    let mut prs = Vec::new();
    for page in pages(root, "repos/{owner}/{repo}/pulls?state=open&per_page=100")? {
        for pull in page.as_array().into_iter().flatten() {
            let number = pull.get("number").and_then(Value::as_u64).unwrap_or(0);
            let (repair, priority) = node_rank(pull, &entries);
            let draft = pull.get("draft").and_then(Value::as_bool) == Some(true);
            let sha = word(pull, "/head/sha").to_string();
            let branch = word(pull, "/head/ref").to_string();
            prs.push(((!repair, draft, priority, number), sha, branch));
        }
    }
    prs.sort();
    let (mut ranked, mut order) = (Vec::new(), Vec::new());
    for (pr, sha, branch) in prs {
        order.push(json!({"pr": pr.3, "branch": branch}));
        ranked.push((pr, head_of(&runs_of(root, &format!("head_sha={sha}"))?)));
    }
    let mut gates = HashSet::new();
    for status in ["queued", "in_progress"] {
        let runs = runs_of(root, &format!("status={status}"))?;
        gates.extend(runs.iter().filter(|r| is_gate(r)).map(id));
    }
    Ok((ranked, order, gates.len()))
}

/// POST `action` on each run; the journal records what gh refused.
pub(crate) fn act(root: &Path, list: &[(u64, Vec<u64>)], action: &str) -> Vec<Value> {
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

/// This repo's rows of one kind since `since_ms`, oldest first.
fn rows_of(path: &Path, kind: &str, slug: &str, since_ms: i64) -> Result<Vec<Value>, String> {
    let query = crate::event_store::EventQuery {
        since_ms: Some(since_ms),
        ..crate::event_store::EventQuery::of_types(&[kind])
    };
    let text = crate::event_store::journal_text_checked(path, &query)?;
    let rows = text
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok());
    Ok(rows
        .filter(|r| r.pointer("/data/slug").and_then(Value::as_str) == Some(slug))
        .collect())
}

/// The runs this tier cancelled, from its own rows.
fn ours(passes: &[Value]) -> HashSet<u64> {
    let cancels = passes.iter().flat_map(|r| r.pointer("/data/cancelled"));
    let runs = cancels.flat_map(|c| c.as_array().cloned().unwrap_or_default());
    let posts = runs.flat_map(|pr| pr["runs"].as_array().cloned().unwrap_or_default());
    let landed = posts.filter(|p| p["ok"] == json!(true));
    landed.filter_map(|p| p["run"].as_u64()).collect()
}

/// The runs a look hold cancelled at this slug: that gate owns their rerun
/// when the user's look clears, so the tier never fights it.
fn look_held(path: &Path, slug: &str, since_ms: i64) -> Result<HashSet<u64>, String> {
    let rows = rows_of(path, "look_hold_cancel", slug, since_ms)?;
    Ok(rows
        .iter()
        .flat_map(|r| {
            r.pointer("/data/runs")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default()
        })
        .filter(|p| p.get("ok").and_then(Value::as_bool) == Some(true))
        .filter_map(|p| p.get("run").and_then(Value::as_u64))
        .collect())
}

/// Whether a cancelled run stays on the rerun ledger: the tier cancelled it,
/// or no row names its cancel. A foreign cancel with no verdict strands the
/// head otherwise: the tier skips it, heal's digest calls it unchanged, and
/// the push's cadence series has ended. Only a look hold's cancel drops off
/// here; that gate reruns its own when the look clears.
fn rerunnable(run: u64, ours: &HashSet<u64>, held: &HashSet<u64>) -> bool {
    ours.contains(&run) || !held.contains(&run)
}

/// The PR a push of `branch` must wait for, and the refusal sentence. A
/// push waits only when the pass row is under 20 minutes old, its tier is
/// full, and the branch's PR ranks below every PR in the tier.
pub(crate) fn hold_reason(pass_row: &Value, branch: &str, now_ms: i64) -> Option<(u64, String)> {
    let ts = crate::ci_cadence::ts_ms(pass_row)?;
    if now_ms - ts >= FRESH_MS {
        return None;
    }
    let data = pass_row.get("data")?;
    let slots = data.get("slots").and_then(Value::as_u64)? as usize;
    let active: Vec<u64> = data["active"]
        .as_array()?
        .iter()
        .filter_map(Value::as_u64)
        .collect();
    let ranked: Vec<(u64, &str)> = data["ranked"]
        .as_array()?
        .iter()
        .map(|r| (r["pr"].as_u64().unwrap_or(0), word(r, "/branch")))
        .collect();
    let at = ranked.iter().position(|(_, b)| *b == branch)?;
    let pr = ranked[at].0;
    let last_member = ranked.iter().rposition(|(n, _)| active.contains(n));
    if active.len() < slots || active.contains(&pr) || last_member.is_some_and(|m| at < m) {
        return None;
    }
    let tier: Vec<String> = active.iter().map(|n| format!("#{n}")).collect();
    Some((
        pr,
        format!(
            "refusing: the CI tier is full ({}) and PR #{pr} ranks below every PR in it. \
             Nothing pushed. The daemon wakes this session when a slot frees; then run \
             fno do pr push again.",
            tier.join(", ")
        ),
    ))
}

/// The refusal for a push of `branch` from `cwd`, or None to push. A held
/// push writes one `ci_tier_held` row, which the arm wakes from.
pub(crate) fn push_hold(cwd: &Path, branch: &str) -> Option<String> {
    let home = AgentsHome::from_env_opt()?;
    let slug = crate::pr_status::cache::git_slug(cwd)?;
    let path = crate::daemon::global_events_path(&home);
    let now = crate::ci_cadence::millis_now();
    let passes = rows_of(&path, PASS_EVENT, &slug, now - FRESH_MS).ok()?;
    let (pr, sentence) = hold_reason(passes.last()?, branch, now)?;
    let (session_id, _) = crate::claims::resolve_identity();
    let row = json!({"slug": slug, "branch": branch, "pr": pr,
        "cwd": cwd.to_string_lossy(), "session_id": session_id});
    let emitter = crate::events::EventEmitter::new(path, "rust");
    if let Err(e) = emitter.emit(HELD_EVENT, &row) {
        eprintln!("pr-push: the {HELD_EVENT} row did not land ({e}); no wake for this hold");
    }
    Some(sentence)
}

/// Wake each held push once, when its hold clears against `pass_row`. A
/// wake covers every older hold of the same branch.
fn wake_held(path: &Path, slug: &str, pass_row: &Value, now_ms: i64) -> Result<(), String> {
    // A hold can outlast a day below a full tier.
    let since = now_ms - 7 * DAY_MS;
    let key = |r: &Value| {
        (
            word(r, "/data/branch").to_string(),
            crate::ci_cadence::ts_ms(r),
        )
    };
    let mut woken: BTreeMap<String, i64> = BTreeMap::new();
    for row in rows_of(path, WAKE_EVENT, slug, since)? {
        let held_at = row
            .pointer("/data/held_ms")
            .and_then(Value::as_i64)
            .unwrap_or(0);
        let slot = woken
            .entry(word(&row, "/data/branch").to_string())
            .or_default();
        *slot = (*slot).max(held_at);
    }
    let mut due: BTreeMap<String, (i64, Value)> = BTreeMap::new();
    for row in rows_of(path, HELD_EVENT, slug, since)? {
        let (branch, Some(ts)) = key(&row) else {
            continue;
        };
        if woken.get(&branch).is_some_and(|w| *w >= ts) {
            continue;
        }
        due.insert(branch, (ts, row));
    }
    let data = &pass_row["data"];
    let ranked: Vec<&str> = data["ranked"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|p| word(p, "/branch"))
        .collect();
    let active = data["active"].as_array().map_or(0, Vec::len);
    let slots = data["slots"].as_u64().unwrap_or(0) as usize;
    // Every woken session pushes before the next pass, so wake in rank
    // order and only as many as the free slots, at least one.
    let mut budget = slots.saturating_sub(active).max(1);
    let mut order: Vec<_> = due.into_iter().collect();
    order.sort_by_key(|(b, _)| ranked.iter().position(|r| r == b));
    let emitter = crate::events::EventEmitter::new(path.to_path_buf(), "daemon");
    for (branch, (held_ms, held)) in order {
        if hold_reason(pass_row, &branch, now_ms).is_some() {
            continue;
        }
        let open = ranked.contains(&branch.as_str());
        if open && budget == 0 {
            continue;
        }
        let pr = held.pointer("/data/pr").cloned().unwrap_or(Value::Null);
        // A closed PR clears its hold too; its session has nothing to push.
        let mut row = json!({"slug": slug, "branch": branch, "pr": pr, "held_ms": held_ms,
            "closed": !open});
        let sid = held.pointer("/data/session_id").and_then(Value::as_str);
        if let Some(sid) = sid.filter(|_| open) {
            budget -= 1;
            let text = format!(
                "The CI tier has a free slot for PR #{pr} ({branch}). Run fno do pr push again \
                 to push the commits it held."
            );
            let mut runner = crate::burn_watch::run_command;
            let (delivered, via) =
                crate::burn_watch::wake_with_text(sid, false, &text, "ci-tier", &mut runner);
            if !delivered && via != "durable" {
                // No wake row, so the next pass retries this hold.
                eprintln!("ci-tier: {slug}: wake for {branch} failed via {via}");
                continue;
            }
            row["session_id"] = json!(sid);
            row["via"] = json!(via);
        }
        emitter
            .emit(WAKE_EVENT, &row)
            .map_err(|e| format!("{WAKE_EVENT} row did not land: {e}"))?;
    }
    Ok(())
}

fn pass_repo(root: &Path, slug: &str, path: &Path) -> Result<(), String> {
    let (mut ranked, order, gates) = read_repo(root)?;
    let now = crate::ci_cadence::millis_now();
    let passes = rows_of(path, PASS_EVENT, slug, now - DAY_MS)?;
    let ours = ours(&passes);
    let held = look_held(path, slug, now - DAY_MS)?;
    for (_, head) in &mut ranked {
        head.cancelled.retain(|run| rerunnable(*run, &ours, &held));
    }
    // Each merge-queue-gate run in flight holds one slot.
    let slots = SIZE.saturating_sub(gates);
    let plan = plan(&ranked, slots);
    let cancelled = act(root, &plan.cancel, "cancel");
    // A full rerun: a cancelled run has no failed job for rerun-failed-jobs.
    let rerun = act(root, &plan.rerun, "rerun");
    let data = json!({"slug": slug, "slots": slots, "gates": gates, "active": plan.active,
        "ranked": order, "cancelled": cancelled, "rerun": rerun});
    let pass_row = json!({"ts": chrono::Utc::now().to_rfc3339(), "data": data});
    // pr push reads the newest row, so a changed tier and a 10-minute
    // heartbeat both write one.
    let last = passes.last();
    let changed = last.is_none_or(|r| {
        r.pointer("/data/active") != Some(&data["active"])
            || r.pointer("/data/ranked") != Some(&data["ranked"])
            || r.pointer("/data/slots") != Some(&data["slots"])
    });
    let stale = last
        .and_then(crate::ci_cadence::ts_ms)
        .is_none_or(|ts| now - ts >= HEARTBEAT_MS);
    if changed || stale || !cancelled.is_empty() || !rerun.is_empty() {
        crate::events::EventEmitter::new(path.to_path_buf(), "daemon")
            .emit(PASS_EVENT, &data)
            .map_err(|e| format!("{PASS_EVENT} row did not land: {e}"))?;
    }
    wake_held(path, slug, &pass_row, now)
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
    fn a_cancelled_run_reruns_unless_a_look_hold_named_it() {
        let own: HashSet<u64> = [10].into();
        let held: HashSet<u64> = [30].into();
        // The tier's own cancel reruns; an unnamed foreign cancel reruns; a
        // look hold's cancel drops off, that gate owns it.
        assert!(rerunnable(10, &own, &held));
        assert!(rerunnable(20, &own, &held));
        assert!(!rerunnable(30, &own, &held));
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

    #[test]
    fn a_push_waits_only_below_a_full_fresh_tier() {
        let now = chrono::DateTime::parse_from_rfc3339("2026-10-27T12:00:00Z")
            .unwrap()
            .timestamp_millis();
        let row = |age_min: i64, slots: u64, active: &[u64]| {
            let ts = chrono::DateTime::from_timestamp_millis(now - age_min * 60_000).unwrap();
            let ranked: Vec<Value> = (1..=4)
                .map(|n| json!({"pr": n, "branch": format!("b{n}")}))
                .collect();
            json!({"ts": ts.to_rfc3339(), "data": {"slots": slots, "active": active, "ranked": ranked}})
        };
        let full = row(5, 2, &[1, 3]);
        assert_eq!(hold_reason(&full, "b4", now).map(|(pr, _)| pr), Some(4));
        // 2 outranks member 3; 3 is a member; b9 has no open PR.
        for branch in ["b2", "b3", "b9"] {
            assert_eq!(hold_reason(&full, branch, now), None, "{branch}");
        }
        assert_eq!(hold_reason(&row(5, 3, &[1, 3]), "b4", now), None);
        assert_eq!(hold_reason(&row(20, 2, &[1, 3]), "b4", now), None);
    }
}
