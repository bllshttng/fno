//! The merge queue: green, reviewed PRs land on main in batches, each batch
//! tested once on main plus its PRs. Opt-in per repo with
//! `merge_queue.enabled`. The rules and their reasons live in
//! docs/architecture/authorized-merge.md, "The merge queue".
//!
//! `authorized_merge` writes one `merge_queue_entry` row in place of a merge.
//! The arm folds those rows and its own `merge_queue_step` rows into one
//! queue state, asks [`decide`] for one step, and applies it through the
//! REST refs and merges APIs. Nothing ever writes to a PR branch.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{json, Map, Value};

use crate::paths::AgentsHome;

const INTERVAL: Duration = Duration::from_secs(120);
const WINDOW_MS: i64 = 3 * 24 * 3_600_000;
/// A gate run not listed yet this soon after its dispatch is still starting.
const DISPATCH_GRACE_MS: i64 = 5 * 60_000;
/// Rebuilds in a row, each because main moved, before the queue holds.
const MAX_REBUILDS: u32 = 3;
const DEFAULT_MAX_BATCH: usize = 6;
pub(crate) const QUEUE_BASE: &str = "main";
pub(crate) const GATE_WORKFLOW: &str = "merge-queue-gate.yml";
pub(crate) const ENTRY_EVENT: &str = "merge_queue_entry";
pub(crate) const STEP_EVENT: &str = "merge_queue_step";

#[derive(Default)]
pub struct Arm {
    last_tick: Mutex<Option<Instant>>,
    in_flight: Arc<AtomicBool>,
}

/// One PR waiting to land, pinned to the head `authorized_merge` cleared.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Entry {
    pub pr: u64,
    pub head: String,
    pub head_ref: String,
    pub repair: bool,
    pub priority: u8,
    pub ts_ms: i64,
}

/// The batch on its queue ref, waiting on its gate run.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Batch {
    pub id: String,
    pub prs: Vec<u64>,
    /// The batch commit: main at `base` plus every PR in `prs`.
    pub sha: String,
    pub base: String,
    pub repair: bool,
    pub reran: bool,
    /// When the gate was last dispatched.
    pub since_ms: i64,
}

/// The newest gate run on the batch ref.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Gate {
    Missing,
    Pending,
    Green,
    Red { failing: Vec<String> },
}

/// What the journal says about the queue, before this tick's reads.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct State {
    pub active: Option<Batch>,
    /// The batch size cap, `merge_queue.max_batch`.
    pub cap: usize,
    /// The left half of the last split: it builds next, ahead of any entry
    /// queued since, until it lands or ejects.
    pub next: Vec<u64>,
    pub rebuilds: u32,
    /// Main's sha when the queue held because main kept moving.
    pub hold_main: Option<String>,
    ejected: HashSet<(u64, String)>,
    landed: HashSet<u64>,
}

pub(crate) struct View<'a> {
    pub entries: &'a [Entry],
    pub state: &'a State,
    pub main: &'a str,
    pub main_red: bool,
    pub gate: &'a Gate,
    pub flakes: &'a HashSet<String>,
    pub now_ms: i64,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Step {
    Idle,
    Wait,
    Build { prs: Vec<u64> },
    Land { sha: String },
    Split { left: Vec<u64>, right: Vec<u64> },
    Eject { pr: u64, failing: Vec<String> },
    Rerun,
    Rebuild { main_moved: bool },
    Hold { reason: String },
}

/// Queue order: a main-repair entry first, then priority, then age.
fn ordered(entries: &[Entry]) -> Vec<&Entry> {
    let mut out: Vec<&Entry> = entries.iter().collect();
    out.sort_by_key(|e| (!e.repair, e.priority, e.ts_ms, e.pr));
    out
}

/// The one step for this tick. Pure: every fact comes in through `view`.
pub(crate) fn decide(view: &View) -> Step {
    let state = view.state;
    if let Some(batch) = &state.active {
        if batch.base != view.main {
            if state.rebuilds >= MAX_REBUILDS {
                return Step::Hold {
                    reason: "main_moving".to_string(),
                };
            }
            return Step::Rebuild { main_moved: true };
        }
        let live: HashSet<u64> = view.entries.iter().map(|e| e.pr).collect();
        if batch.prs.iter().any(|pr| !live.contains(pr)) {
            return Step::Rebuild { main_moved: false };
        }
        // A red main lands only a repair, and a parked batch would keep the
        // repair entry from building.
        if view.main_red && !batch.repair {
            return Step::Rebuild { main_moved: false };
        }
        return match view.gate {
            Gate::Pending => Step::Wait,
            Gate::Missing if view.now_ms - batch.since_ms < DISPATCH_GRACE_MS => Step::Wait,
            Gate::Missing => Step::Rerun,
            Gate::Green => Step::Land {
                sha: batch.sha.clone(),
            },
            Gate::Red { failing } => {
                let flaky = !failing.is_empty() && failing.iter().all(|c| view.flakes.contains(c));
                if flaky && !batch.reran {
                    Step::Rerun
                } else if batch.prs.len() == 1 {
                    Step::Eject {
                        pr: batch.prs[0],
                        failing: failing.clone(),
                    }
                } else {
                    let (left, right) = batch.prs.split_at(batch.prs.len() / 2);
                    Step::Split {
                        left: left.to_vec(),
                        right: right.to_vec(),
                    }
                }
            }
        };
    }
    if state.hold_main.as_deref().is_some_and(|m| m != view.main) {
        return Step::Hold {
            reason: "main_moving".to_string(),
        };
    }
    let order = ordered(view.entries);
    let Some(first) = order.first() else {
        return Step::Idle;
    };
    if first.repair {
        return Step::Build {
            prs: vec![first.pr],
        };
    }
    if view.main_red {
        return Step::Idle;
    }
    let pinned: Vec<u64> = order
        .iter()
        .map(|e| e.pr)
        .filter(|pr| state.next.contains(pr))
        .collect();
    if !pinned.is_empty() {
        return Step::Build { prs: pinned };
    }
    let prs = order.iter().take(state.cap.max(1)).map(|e| e.pr).collect();
    Step::Build { prs }
}

fn text(row: &Value, pointer: &str) -> String {
    row.pointer(pointer)
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string()
}

fn numbers(row: &Value, pointer: &str) -> Vec<u64> {
    row.pointer(pointer)
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_u64)
        .collect()
}

/// Fold the step rows, oldest first, into the queue state.
pub(crate) fn fold(steps: &[Value], max_batch: usize) -> State {
    let mut state = State {
        active: None,
        cap: max_batch,
        next: Vec::new(),
        rebuilds: 0,
        hold_main: None,
        ejected: HashSet::new(),
        landed: HashSet::new(),
    };
    for row in steps {
        let ts = crate::ci_cadence::ts_ms(row).unwrap_or(0);
        let batch_id = text(row, "/data/batch");
        let ends_active = state.active.as_ref().is_some_and(|b| b.id == batch_id);
        match text(row, "/data/step").as_str() {
            "build" => {
                state.hold_main = None;
                state.active = Some(Batch {
                    id: batch_id,
                    prs: numbers(row, "/data/prs"),
                    sha: text(row, "/data/sha"),
                    base: text(row, "/data/base"),
                    repair: row.pointer("/data/repair").and_then(Value::as_bool) == Some(true),
                    reran: false,
                    since_ms: ts,
                });
            }
            "rerun" => {
                if let Some(batch) = state.active.as_mut().filter(|_| ends_active) {
                    batch.reran = true;
                    batch.since_ms = ts;
                }
            }
            "land" => {
                state.landed.extend(numbers(row, "/data/prs"));
                state.active = None;
                state.next.clear();
                state.rebuilds = 0;
            }
            "split" => {
                state.active = None;
                state.next = numbers(row, "/data/left");
                state.rebuilds = 0;
            }
            "eject" => {
                if let Some(pr) = row.pointer("/data/pr").and_then(Value::as_u64) {
                    state.ejected.insert((pr, text(row, "/data/head")));
                }
                if ends_active {
                    state.active = None;
                    state.next.clear();
                    state.rebuilds = 0;
                }
            }
            "rebuild" => {
                state.active = None;
                if row.pointer("/data/main_moved").and_then(Value::as_bool) == Some(true) {
                    state.rebuilds += 1;
                }
            }
            "hold" => {
                state.active = None;
                state.rebuilds = 0;
                state.hold_main = Some(text(row, "/data/main"));
            }
            _ => {}
        }
    }
    state
}

/// The live entries: the oldest row per pinned head, minus a head the queue
/// ejected, a PR it landed, and a PR whose open head is not the pin.
pub(crate) fn live_entries(
    rows: &[Value],
    state: &State,
    open: &HashMap<u64, String>,
) -> Vec<Entry> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for row in rows {
        let Some(pr) = row.pointer("/data/pr").and_then(Value::as_u64) else {
            continue;
        };
        let head = text(row, "/data/head");
        if open.get(&pr) != Some(&head)
            || state.landed.contains(&pr)
            || state.ejected.contains(&(pr, head.clone()))
            || !seen.insert((pr, head.clone()))
        {
            continue;
        }
        out.push(Entry {
            pr,
            head,
            head_ref: text(row, "/data/head_ref"),
            repair: row.pointer("/data/repair").and_then(Value::as_bool) == Some(true),
            priority: row
                .pointer("/data/priority")
                .and_then(Value::as_u64)
                .unwrap_or(2) as u8,
            ts_ms: crate::ci_cadence::ts_ms(row).unwrap_or(0),
        });
    }
    out
}

pub(crate) fn enabled(cwd: &Path) -> bool {
    crate::agents_config::config_value_deep(cwd, &["merge_queue", "enabled"])
        .and_then(|v| v.as_bool())
        == Some(true)
}

fn max_batch(cwd: &Path) -> usize {
    crate::agents_config::config_value_deep(cwd, &["merge_queue", "max_batch"])
        .and_then(|v| v.as_integer())
        .filter(|n| *n >= 1)
        .map_or(DEFAULT_MAX_BATCH, |n| n as usize)
}

fn events_path() -> Option<std::path::PathBuf> {
    AgentsHome::from_env_opt().map(|home| crate::daemon::global_events_path(&home))
}

/// This repo's rows of one kind in the queue window, oldest first.
fn rows_of(path: &Path, kind: &str, slug: &str, now_ms: i64) -> Result<Vec<Value>, String> {
    let query = crate::event_store::EventQuery {
        since_ms: Some(now_ms.saturating_sub(WINDOW_MS)),
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

/// Queue this PR head. `Ok(None)` queued (or already queued); `Ok(Some)` the
/// queue already ejected this head, with the reason. A new push re-queues.
pub(crate) fn enqueue(
    cwd: &Path,
    pr: u64,
    head: &str,
    head_ref: &str,
    repair: bool,
    priority: u8,
) -> Result<Option<String>, String> {
    let slug = crate::pr_status::cache::git_slug(cwd)
        .ok_or_else(|| "merge queue: no origin slug for this checkout".to_string())?;
    let path = events_path().ok_or_else(|| "merge queue: no agents home".to_string())?;
    let now = crate::ci_cadence::millis_now();
    let same = |r: &Value| {
        r.pointer("/data/pr").and_then(Value::as_u64) == Some(pr) && text(r, "/data/head") == head
    };
    if let Some(eject) = rows_of(&path, STEP_EVENT, &slug, now)?
        .into_iter()
        .filter(|r| text(r, "/data/step") == "eject")
        .rfind(same)
    {
        return Ok(Some(format!(
            "the merge queue ejected head {head}: {}; push a fix to queue again",
            text(&eject, "/data/reason")
        )));
    }
    if rows_of(&path, ENTRY_EVENT, &slug, now)?.iter().any(same) {
        return Ok(None);
    }
    let row = json!({"slug": slug, "pr": pr, "head": head, "head_ref": head_ref,
        "repair": repair, "priority": priority, "cwd": cwd.to_string_lossy()});
    crate::events::EventEmitter::new(path, "rust")
        .emit(ENTRY_EVENT, &row)
        .map_err(|e| format!("merge queue: the entry row did not land: {e}"))?;
    Ok(None)
}

fn gh(root: &Path, path: &str, extra: &[&str]) -> Result<String, String> {
    crate::pr_push::gh_api("gh", root, path, extra)
}

fn queue_ref(batch: &str) -> String {
    format!("fno-merge-queue/{QUEUE_BASE}/{batch}")
}

fn main_sha(root: &Path) -> Result<String, String> {
    let raw = gh(
        root,
        &format!("repos/{{owner}}/{{repo}}/git/ref/heads/{QUEUE_BASE}"),
        &[],
    )?;
    let v: Value = serde_json::from_str(&raw).map_err(|e| e.to_string())?;
    Some(text(&v, "/object/sha"))
        .filter(|s| !s.is_empty())
        .ok_or_else(|| "main ref read carried no sha".to_string())
}

/// Open PR heads. Read only when the queue has rows to judge.
fn open_heads(root: &Path) -> Result<HashMap<u64, String>, String> {
    let path = "repos/{owner}/{repo}/pulls?state=open&per_page=100";
    let pages = crate::pr_push::gh_api_pages_conditional("gh", root, path)?;
    let pulls = pages.iter().filter_map(Value::as_array).flatten();
    Ok(pulls
        .filter_map(|p| Some((p.get("number")?.as_u64()?, text(p, "/head/sha"))))
        .collect())
}

/// Check names without the caller-job prefix a reusable workflow adds, so
/// they match the flake ledger's PR-run names.
fn check_name(job: &str) -> String {
    job.rsplit(" / ").next().unwrap_or(job).to_string()
}

fn gate_of(root: &Path, batch: &Batch) -> Result<Gate, String> {
    let path = format!(
        "repos/{{owner}}/{{repo}}/actions/workflows/{GATE_WORKFLOW}/runs?branch={}&per_page=20",
        queue_ref(&batch.id)
    );
    let v: Value = serde_json::from_str(&gh(root, &path, &[])?).map_err(|e| e.to_string())?;
    let runs = v.get("workflow_runs").and_then(Value::as_array);
    let newest = runs
        .into_iter()
        .flatten()
        .filter(|r| text(r, "/head_sha") == batch.sha)
        .max_by_key(|r| r.get("id").and_then(Value::as_u64).unwrap_or(0));
    let Some(run) = newest else {
        return Ok(Gate::Missing);
    };
    if text(run, "/status") != "completed" {
        return Ok(Gate::Pending);
    }
    if text(run, "/conclusion") == "success" {
        return Ok(Gate::Green);
    }
    let id = run.get("id").and_then(Value::as_u64).unwrap_or(0);
    let jobs = gh(
        root,
        &format!("repos/{{owner}}/{{repo}}/actions/runs/{id}/jobs"),
        &[],
    )?;
    let jobs: Value = serde_json::from_str(&jobs).map_err(|e| e.to_string())?;
    let failing = jobs
        .get("jobs")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|j| matches!(text(j, "/conclusion").as_str(), "failure" | "timed_out"))
        .map(|j| check_name(&text(j, "/name")))
        .collect();
    Ok(Gate::Red { failing })
}

fn dispatch_gate(root: &Path, batch: &str) -> Result<String, String> {
    let path = format!("repos/{{owner}}/{{repo}}/actions/workflows/{GATE_WORKFLOW}/dispatches");
    let field = format!("ref={}", queue_ref(batch));
    gh(root, &path, &["-X", "POST", "-f", &field])
}

fn delete_ref(root: &Path, batch: &str) {
    let path = format!(
        "repos/{{owner}}/{{repo}}/git/refs/heads/{}",
        queue_ref(batch)
    );
    if let Err(e) = gh(root, &path, &["-X", "DELETE"]) {
        eprintln!("merge-queue: batch ref {batch} not deleted: {e}");
    }
}

fn comment(root: &Path, pr: u64, body: &str) {
    let path = format!("repos/{{owner}}/{{repo}}/issues/{pr}/comments");
    let field = format!("body={body}");
    if let Err(e) = gh(root, &path, &["-X", "POST", "-f", &field]) {
        eprintln!("merge-queue: PR {pr} comment not posted: {e}");
    }
}

/// Build the batch on its own ref: main's tip, then one merge commit per PR.
/// A conflicting PR is ejected; the rest build without it.
fn build(
    root: &Path,
    emit: &dyn Fn(Map<String, Value>),
    prs: &[u64],
    entries: &[Entry],
    main: &str,
    now_ms: i64,
) -> Result<(), String> {
    let batch = now_ms.to_string();
    let branch = queue_ref(&batch);
    gh(
        root,
        "repos/{owner}/{repo}/git/refs",
        &[
            "-X",
            "POST",
            "-f",
            &format!("ref=refs/heads/{branch}"),
            "-f",
            &format!("sha={main}"),
        ],
    )?;
    let (mut sha, mut merged) = (main.to_string(), Vec::new());
    for entry in prs
        .iter()
        .filter_map(|pr| entries.iter().find(|e| e.pr == *pr))
    {
        let message = format!("Merge pull request #{} from {}", entry.pr, entry.head_ref);
        let fields = [
            format!("base={branch}"),
            format!("head={}", entry.head),
            format!("commit_message={message}"),
        ];
        let args = [
            "-X", "POST", "-f", &fields[0], "-f", &fields[1], "-f", &fields[2],
        ];
        match gh(root, "repos/{owner}/{repo}/merges", &args) {
            Ok(out) => {
                let v: Value = serde_json::from_str(&out).unwrap_or(Value::Null);
                if let Some(next) = v.get("sha").and_then(Value::as_str) {
                    sha = next.to_string();
                }
                merged.push(entry.pr);
            }
            Err(e) if e.contains("409") => {
                let reason = "conflicts with the batch; merge main into the branch";
                emit(step_row(
                    "eject",
                    &batch,
                    json!({"pr": entry.pr, "head": entry.head,
                    "reason": reason, "ends_batch": false}),
                ));
                comment(root, entry.pr, &format!("Merge queue: this PR {reason}."));
            }
            Err(e) => {
                delete_ref(root, &batch);
                return Err(format!(
                    "batch {batch}: merging PR {} failed: {e}",
                    entry.pr
                ));
            }
        }
    }
    if merged.is_empty() {
        delete_ref(root, &batch);
        return Ok(());
    }
    let repair = entries.iter().any(|e| e.repair && merged.contains(&e.pr));
    let dispatched = dispatch_gate(root, &batch);
    emit(step_row(
        "build",
        &batch,
        json!({"prs": merged, "sha": sha, "base": main,
        "repair": repair, "dispatch_error": dispatched.err()}),
    ));
    Ok(())
}

fn step_row(step: &str, batch: &str, extra: Value) -> Map<String, Value> {
    let mut row = Map::new();
    row.insert("step".into(), json!(step));
    row.insert("batch".into(), json!(batch));
    if let Value::Object(more) = extra {
        row.extend(more);
    }
    row
}

fn apply(
    root: &Path,
    emit: &dyn Fn(Map<String, Value>),
    step: &Step,
    view: &View,
) -> Result<(), String> {
    let active = view.state.active.as_ref();
    let id = active.map(|b| b.id.clone()).unwrap_or_default();
    match step {
        Step::Idle | Step::Wait => {}
        Step::Build { prs } => build(root, emit, prs, view.entries, view.main, view.now_ms)?,
        Step::Rerun => {
            let dispatched = dispatch_gate(root, &id);
            emit(step_row(
                "rerun",
                &id,
                json!({"dispatch_error": dispatched.err()}),
            ));
        }
        Step::Land { sha } => {
            let path = format!("repos/{{owner}}/{{repo}}/git/refs/heads/{QUEUE_BASE}");
            let field = format!("sha={sha}");
            match gh(
                root,
                &path,
                &["-X", "PATCH", "-f", &field, "-F", "force=false"],
            ) {
                Ok(_) => {
                    let prs = active.map(|b| b.prs.clone()).unwrap_or_default();
                    emit(step_row("land", &id, json!({"prs": prs, "sha": sha})));
                }
                // A refused fast-forward over a moved main rebuilds. Any other
                // refusal keeps the green batch, and the next tick lands again.
                Err(e) if main_sha(root).is_ok_and(|now| now != view.main) => emit(step_row(
                    "rebuild",
                    &id,
                    json!({"main_moved": true, "error": e}),
                )),
                Err(e) => return Err(format!("batch {id}: landing refused: {e}")),
            }
            delete_ref(root, &id);
        }
        Step::Split { left, right } => {
            emit(step_row(
                "split",
                &id,
                json!({"left": left, "right": right}),
            ));
            delete_ref(root, &id);
        }
        Step::Eject { pr, failing } => {
            let head = view
                .entries
                .iter()
                .find(|e| e.pr == *pr)
                .map(|e| e.head.clone());
            let reason = format!(
                "the gate failed on main plus this PR: {}",
                failing.join(", ")
            );
            emit(step_row(
                "eject",
                &id,
                json!({"pr": pr, "head": head, "failing": failing,
                "reason": reason, "ends_batch": true}),
            ));
            comment(
                root,
                *pr,
                &format!("Merge queue: {reason}. Push a fix to queue again."),
            );
            delete_ref(root, &id);
        }
        Step::Rebuild { main_moved } => {
            emit(step_row("rebuild", &id, json!({"main_moved": main_moved})));
            delete_ref(root, &id);
        }
        Step::Hold { reason } => {
            emit(step_row(
                "hold",
                &id,
                json!({"reason": reason, "main": view.main}),
            ));
            if active.is_some() {
                delete_ref(root, &id);
            }
        }
    }
    Ok(())
}

fn pass_repo(root: &Path, slug: &str, path: &Path) -> Result<(), String> {
    let now_ms = crate::ci_cadence::millis_now();
    let entry_rows = rows_of(path, ENTRY_EVENT, slug, now_ms)?;
    if entry_rows.is_empty() {
        return Ok(());
    }
    let state = fold(&rows_of(path, STEP_EVENT, slug, now_ms)?, max_batch(root));
    let entries = live_entries(&entry_rows, &state, &open_heads(root)?);
    if entries.is_empty() && state.active.is_none() {
        return Ok(());
    }
    let main = main_sha(root)?;
    let main_red = crate::main_ci::main_ci_reading_cached(root)
        .ok()
        .is_some_and(|token| crate::main_ci::main_ci_red_run(&token).is_some());
    let gate = match &state.active {
        Some(batch) => gate_of(root, batch)?,
        None => Gate::Pending,
    };
    let flakes = crate::ci_cadence::flake_checks(path, root);
    let view = View {
        entries: &entries,
        state: &state,
        main: &main,
        main_red,
        gate: &gate,
        flakes: &flakes,
        now_ms,
    };
    let step = decide(&view);
    let emitter = crate::events::EventEmitter::new(path.to_path_buf(), "daemon");
    let emit = |mut fields: Map<String, Value>| {
        fields.insert("slug".into(), json!(slug));
        if let Err(e) = emitter.emit_fields(STEP_EVENT, fields) {
            eprintln!("merge-queue: {slug}: step row did not land: {e}");
        }
    };
    apply(root, &emit, &step, &view)
}

pub(crate) fn run_pass(home: &AgentsHome) -> Result<(), String> {
    let path = crate::daemon::global_events_path(home);
    let mut slugs = HashSet::new();
    for root in crate::daemon::worktree_sweep::registry_repo_roots(home) {
        let root = Path::new(&root);
        if !enabled(root) {
            continue;
        }
        // Two clones of one repo are one queue.
        let slug = crate::pr_status::cache::git_slug(root).filter(|s| slugs.insert(s.clone()));
        let Some(slug) = slug else { continue };
        if let Err(e) = pass_repo(root, &slug, &path) {
            eprintln!("merge-queue: {slug}: {e}");
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
            eprintln!("merge-queue: {error}");
        }
    });
}

#[cfg(test)]
mod tests;
