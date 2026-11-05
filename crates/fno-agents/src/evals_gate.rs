//! The evals gate: the daemon's check on skill, agent, hook, command and bank
//! changes. CI cannot run it: a bank run needs worker lanes.
//!
//! Each `evals-arm` tick runs one pass over two independent legs.
//!
//! **Main leg.** Once a day, and at once when the route config hash changes,
//! it runs one cohort per seat at `origin/main`. The worker seat runs the
//! golden tier on `evals.gate.lane` as cohort `gate-main-<date>-<hash>`. The
//! lead seat runs the `lead` goldens on the first lane of
//! `agents.profiles.role.lanes` as `gate-main-<date>-<hash>-lead`. One seat
//! runs at a time, and the worker goes first. The golden set is read at the
//! fetched sha through a detached worktree, never from this repo's working tree: the
//! checkout the daemon runs from can sit behind the ref, and a tag read
//! there misses a golden main added. Each finished cohort becomes the
//! baseline of its seat, and it is compared to that seat's one before. A
//! regression files a node, so a route change that breaks a golden reaches
//! the backlog.
//!
//! **Head leg.** An open same-repo PR whose head touches a gated path gets a
//! pending `evals/gate` status, then one run of the `gate` goldens at that
//! head, one PR at a time. A head whose gated hits are all bank files grades
//! only the goldens it changed, so a goldens-only PR fits the 20-minute wall.
//! A newer head kills the older run. The head runs on
//! the worker seat and pays one side only: it compares to the worker
//! baseline. Until a first baseline exists the
//! leg posts nothing, so a fresh install holds no PR on a check it cannot do.
//!
//! The runs are detached, and the tick never waits on one. A later tick reads
//! the history rows, so a daemon restart loses no run. Plumbing never reads as
//! a pass: no result by the limit, or a spent budget, is red and names the
//! cause and the clearing command.

use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::evals_trend::read_rows;
use crate::gate_probes::RunProbe;

const CONTEXT: &str = "evals/gate";
const GATED_PATHS: [&str; 5] = ["skills/", "agents/", "hooks/", "commands/", "evals/bank/"];
/// The gated prefixes minus the bank: these paths have no per-golden
/// `covers:` granularity, so a diff that hits one selects the whole tier.
const TIER_PATHS: [&str; 4] = ["skills/", "agents/", "hooks/", "commands/"];
/// The bank prefix. A diff whose gated hits are all bank files changed the
/// measurement, not the system under test, so it grades only the goldens it
/// touched instead of the whole tier.
const BANK_PATH: &str = "evals/bank/";
const GATE_TAG: &str = "gate";
const GOLDEN_TAG: &str = "golden";
const LEAD_TAG: &str = "lead";
pub(crate) const REPEATS: usize = 3;
const HEAD_WORKERS: u32 = 4;
const MAIN_WORKERS: u32 = 2;
const HEAD_LIMIT_S: i64 = 1200;
const MAIN_LIMIT_S: i64 = 8 * 3600;
/// A task slower than this cannot sit in a 20-minute gate at 3 repeats.
const MAX_TASK_WALL_S: f64 = 180.0;
const NEW_HEADS_PER_TICK: usize = 10;
const DEFAULT_DAILY_USD: f64 = 15.0;
const DEFAULT_LANE: &str = "zai-flash";
/// True when a routing row serves `vendor` by account or route prefix.
fn row_is_vendor(r: &Value, vendor: &str) -> bool {
    r["account"].as_str() == Some(vendor)
        || r["route"]
            .as_str()
            .is_some_and(|v| v.starts_with(&format!("{vendor}/")))
}

/// True when `lane` rides zai, read from the routing inventory. A lane the
/// inventory does not list is no zai lane.
fn lane_is_zai(repo: &Path, lane: &str) -> bool {
    crate::route_gather::inventory(repo)
        .get("rows")
        .and_then(Value::as_array)
        .and_then(|rows| rows.iter().find(|r| r["name"].as_str() == Some(lane)))
        .is_some_and(|r| row_is_vendor(r, "zai"))
}

/// True when every zai record's 5h window reads under `threshold`. Rows sit
/// in records order (`usage::collect` maps them so). A zai record with no
/// readable 5h window answers false: a probe that cannot see the window is
/// exactly the case that moves the run off zai. No zai record is the same
/// answer - the lane cannot be proven runnable.
fn zai_window_room(
    records: &[crate::accounts::records::Record],
    rows: &[crate::accounts::usage::Row],
    threshold: f64,
) -> bool {
    let mut saw_zai = false;
    for (record, row) in records.iter().zip(rows.iter()) {
        let provider = record
            .route
            .as_deref()
            .and_then(crate::spawn_route::parse_target)
            .map(|(p, _)| p);
        let key = provider.as_deref().unwrap_or(&record.harness);
        if key != "zai" {
            continue;
        }
        saw_zai = true;
        let room = row
            .entry
            .get("windows")
            .and_then(Value::as_array)
            .and_then(|ws| ws.iter().find(|w| w["label"].as_str() == Some("5h")))
            .and_then(|w| w["used_pct"].as_f64())
            .is_some_and(|pct| pct < threshold);
        if !room {
            return false;
        }
    }
    saw_zai
}

/// The live room read the lane rule rides: the same probe
/// `fno config accounts usage` runs, refreshed, folded into
/// `zai_window_room`. None when the probe cannot answer at all.
pub(crate) fn real_room(repo: &Path, now: DateTime<Utc>) -> Option<bool> {
    crate::accounts::usage::report(repo, true, now.timestamp() as f64)
        .ok()
        .map(|(records, rows, threshold)| zai_window_room(&records, &rows, threshold))
}

/// The lane this run rides. A zai lane runs while its 5h window has room
/// under the quota defer threshold, read through the same probe
/// `fno config accounts usage` runs; a window at the cap, an unreadable
/// reading, or a failed probe moves the run to the fixed sonnet fallback
/// lane (the user's ruling: z.ai usage matters, not the clock). The fallback
/// is fixed, never the first claude inventory row: that row can be opus, and
/// opus never runs target work.
pub(crate) fn lane_with_room(
    repo: &Path,
    lane: &str,
    now: DateTime<Utc>,
    room: &dyn Fn(&Path, DateTime<Utc>) -> Option<bool>,
) -> String {
    if !lane_is_zai(repo, lane) {
        return lane.to_string();
    }
    if room(repo, now) == Some(true) {
        return lane.to_string();
    }
    FALLBACK_LANE.to_string()
}

/// The lane a run moves to when it cannot ride its own: the main lane's
/// no-grade fallback, and a zai window with no room. A fixed sonnet lane,
/// never the first claude inventory row, which can be opus, and opus never
/// runs target work.
const FALLBACK_LANE: &str = "claude-sonnet-5";
const FALLBACK_SUFFIX: &str = "-fallback";

/// Who does the work a cohort grades. Spend is each gate row of the day at its
/// seat's attempt price, because a row's usage has no price until a manifest
/// names one. The cohort suffix names the seat, so an old row prices as worker.
struct Seat {
    name: &'static str,
    tag: &'static str,
    suffix: &'static str,
    attempt_usd: f64,
}

/// GLM's measured mean is 0.032 USD an attempt.
const WORKER: Seat = Seat {
    name: "worker",
    tag: GOLDEN_TAG,
    suffix: "",
    attempt_usd: 0.04,
};
/// What one worker attempt costs, for a run priced beside the gate.
pub(crate) const WORKER_ATTEMPT_USD: f64 = WORKER.attempt_usd;
/// The kept GLM gate transcripts cost a mean 0.67 USD at Opus prices.
const LEAD: Seat = Seat {
    name: "lead",
    tag: LEAD_TAG,
    suffix: "-lead",
    attempt_usd: 0.70,
};
const SEATS: [&Seat; 2] = [&WORKER, &LEAD];
/// GitHub refuses a status description above this length.
const DESCRIPTION_MAX: usize = 140;

const SKIP: &str = "skip";
const QUEUED: &str = "queued";
const DONE: &str = "done";
const BUDGET_PREFIX: &str = "budget:";
/// A main run that writes no grade may try again, so one bad launch does not
/// spend the day. The cap keeps a broken run from looping.
const MAIN_MAX_TRIES: u32 = 3;
const MAIN_RETRY_S: i64 = 3600;
const MAIN_LOG: &str = "main.log";
const HEAD_LOG: &str = "head.log";
const CAUSE_MAX: usize = 300;

#[derive(Serialize, Deserialize, Clone)]
struct HeadRun {
    pr: u64,
    sha: String,
    cohort: String,
    pid: u32,
    started: i64,
    tasks: Vec<String>,
    /// Bank task files the head-leg read could not parse (a check kind
    /// newer than this binary), named in the pending post and the verdict.
    #[serde(default)]
    skipped: Vec<String>,
    /// The run built the head first, so a slow build can read as the cause.
    #[serde(default)]
    built: bool,
}

#[derive(Serialize, Deserialize, Clone)]
struct MainRun {
    cohort: String,
    pid: u32,
    started: i64,
    /// Empty on a run an older binary launched: that run was the worker's.
    #[serde(default)]
    seat: String,
}

/// Why the cohort has no grade yet. `last_note` is rewritten every tick, so the
/// cause lives here.
#[derive(Serialize, Deserialize, Clone)]
struct MainFailure {
    cohort: String,
    tries: u32,
    next_try: i64,
    cause: String,
}

#[derive(Serialize, Deserialize, Default)]
struct SeatState {
    baseline: Option<String>,
    /// The cohort last launched. With no `main_failure` for it, the day is done.
    main_tried: Option<String>,
    #[serde(default)]
    main_failure: Option<MainFailure>,
    /// The cohort whose fallback run was launched. One fallback per cohort.
    #[serde(default)]
    fallback_tried: Option<String>,
}

#[derive(Serialize, Deserialize, Default)]
struct State {
    /// Head sha to its disposition. Only open PR heads stay.
    heads: BTreeMap<String, String>,
    head_run: Option<HeadRun>,
    main_run: Option<MainRun>,
    /// Flattened, so a `state.json` written before seats loads as the worker's.
    #[serde(flatten)]
    worker: SeatState,
    #[serde(default)]
    lead: SeatState,
    last_note: String,
}

impl State {
    fn seat(&mut self, seat: &Seat) -> &mut SeatState {
        if seat.name == LEAD.name {
            &mut self.lead
        } else {
            &mut self.worker
        }
    }
}

struct Pr {
    number: u64,
    sha: String,
    /// GitHub's own word that the PR sits in the auto-merge queue (`auto_merge`
    /// non-null on the REST pull row). The disarm sweep reads only armed rows.
    armed: bool,
}

struct Deps<'a> {
    run: RunProbe<'a>,
    /// The slot plumbing's own probe: the same argv seam with the 10-minute
    /// slot bound, so a slow worktree add outlives the 30-second probe kill.
    slot_run: RunProbe<'a>,
    spawn: &'a dyn Fn(&[String], &Path, &Path) -> Result<u32, String>,
    alive: &'a dyn Fn(u32) -> bool,
    kill: &'a dyn Fn(u32),
    /// The zai 5h window read behind `lane_with_room`; `real_room` in
    /// production, an injected answer in tests.
    room: &'a dyn Fn(&Path, DateTime<Utc>) -> Option<bool>,
    now: DateTime<Utc>,
}

struct Paths<'a> {
    history: &'a Path,
    fno_bin: &'a str,
    repo: &'a Path,
    state_dir: PathBuf,
    graph: Option<&'a Path>,
}

fn state_dir(history: &Path) -> PathBuf {
    history
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join("evals-gate")
}

fn load_state(dir: &Path) -> State {
    std::fs::read_to_string(dir.join("state.json"))
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

/// Temp plus rename: `gate-retry` can read this between two ticks.
fn save_state(dir: &Path, state: &State) {
    let Ok(text) = serde_json::to_string_pretty(state) else {
        return;
    };
    let _ = std::fs::create_dir_all(dir);
    let tmp = dir.join(format!("state.json.{}", std::process::id()));
    if std::fs::write(&tmp, text).is_ok() {
        let _ = std::fs::rename(&tmp, dir.join("state.json"));
    }
}

fn touches_gated_path(files: &[&str]) -> bool {
    files
        .iter()
        .any(|f| GATED_PATHS.iter().any(|p| f.get(..p.len()) == Some(*p)))
}

fn touches_tier_path(files: &[&str]) -> bool {
    files
        .iter()
        .any(|f| TIER_PATHS.iter().any(|p| f.get(..p.len()) == Some(*p)))
}

/// FNV-1a over the route config. One hash decides a change, and a new binary
/// must read the same value, which the std hasher does not promise.
fn route_hash(repo: &Path) -> String {
    let read = |keys: &[&str]| {
        crate::agents_config::config_value_deep(repo, keys)
            .map(|v| v.to_string())
            .unwrap_or_default()
    };
    let text = read(&["routing", "models"]) + &read(&["agents", "profiles"]);
    let hash = text.bytes().fold(0xcbf2_9ce4_8422_2325_u64, |h, b| {
        (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3)
    });
    format!("{:08x}", hash >> 32)
}

pub(crate) fn history_rows(history: &Path) -> Vec<Value> {
    std::fs::read_to_string(history)
        .unwrap_or_default()
        .lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect()
}

fn row_cohort(row: &Value) -> &str {
    row.get("experiment_id")
        .and_then(Value::as_str)
        .unwrap_or_default()
}

/// Estimated dollars the gate spent on this UTC day.
pub(crate) fn spent_today(rows: &[Value], now: DateTime<Utc>) -> f64 {
    let day = now.format("%Y-%m-%d").to_string();
    rows.iter()
        .filter(|r| row_cohort(r).get(..5) == Some("gate-"))
        // A grade-only attempt starts no worker and costs nothing. A row that
        // does not say, an old one, still counts.
        .filter(|r| r.pointer("/obs/worker_required") != Some(&Value::Bool(false)))
        .filter(|r| {
            r.get("ts")
                .and_then(Value::as_str)
                .and_then(|ts| ts.get(..10))
                == Some(day.as_str())
        })
        .map(|r| {
            let lead = row_cohort(r).ends_with(LEAD.suffix);
            if lead {
                LEAD.attempt_usd
            } else {
                WORKER.attempt_usd
            }
        })
        .sum()
}

/// Tasks whose median attempt in the baseline ran over the wall limit. A task
/// with no rows stays in: the first baseline has nothing to measure it by.
fn slow_tasks(rows: &[Value], baseline: &str) -> BTreeSet<String> {
    let mut walls: BTreeMap<String, Vec<f64>> = BTreeMap::new();
    for row in rows.iter().filter(|r| row_cohort(r) == baseline) {
        if let (Some(task), Some(wall)) = (
            row.get("task_id").and_then(Value::as_str),
            row.get("duration_s").and_then(Value::as_f64),
        ) {
            walls.entry(task.to_string()).or_default().push(wall);
        }
    }
    walls
        .into_iter()
        .filter(|(_, w)| {
            let mut w = w.clone();
            w.sort_by(f64::total_cmp);
            w[w.len() / 2] > MAX_TASK_WALL_S
        })
        .map(|(task, _)| task)
        .collect()
}

/// A task file's repo-relative path: `evals/bank/<id>.yaml`, or
/// `skills/<name>/evals/<id>.yaml` for a task a skill carries.
fn repo_relative_task(source: &std::path::Path) -> String {
    let tail = |n: usize| {
        let parts: Vec<_> = source.iter().rev().take(n).collect();
        parts
            .iter()
            .rev()
            .map(|c| c.to_string_lossy())
            .collect::<Vec<_>>()
            .join("/")
    };
    let skill_local = source
        .parent()
        .and_then(|d| d.file_name())
        .is_some_and(|n| n == "evals");
    tail(if skill_local { 4 } else { 3 })
}

/// The goldens a head grades. A skill, agent, hook or command change selects
/// the whole `gate` tier; a crate-only diff selects the tasks whose `covers:`
/// it touches; a bank edit changes the measurement, not the system under
/// test, so it grades only the goldens the head touched. A golden added to
/// main after the head branched has no fixture there, and would read as a
/// regression.
fn gate_tasks(
    tasks: &[crate::evals::bank::TaskSpec],
    files: &[String],
    sha: &str,
    slow: &BTreeSet<String>,
    d: &Deps,
    p: &Paths,
) -> Vec<crate::evals::bank::TaskSpec> {
    let gated = touches_gated_path(&files.iter().map(String::as_str).collect::<Vec<_>>());
    let tier = touches_tier_path(&files.iter().map(String::as_str).collect::<Vec<_>>());
    let changed: BTreeSet<String> = files
        .iter()
        .filter(|f| f.starts_with(BANK_PATH))
        .cloned()
        .collect();
    tasks
        .iter()
        .filter(|t| !t.is_demoted())
        .filter(|t| {
            (gated && tier && t.tags.iter().any(|tag| tag == GATE_TAG))
                || files.iter().any(|f| is_covered(f, &t.covers))
                || changed.contains(&repo_relative_task(&t.source_path))
        })
        .filter(|t| !slow.contains(&t.id))
        .filter(|t| {
            let spec = format!("{sha}:{}", repo_relative_task(&t.source_path));
            (d.run)(&["git", "cat-file", "-e", &spec], p.repo).is_some_and(|r| r.ok)
        })
        .cloned()
        .collect()
}

/// The status for one finished compare. Only always pass to always fail, or a
/// missing result, is red. Everything else lists and passes.
fn verdict(view: &Value, sha: &str) -> (&'static str, String) {
    let names = |key: &str, keep: &dyn Fn(&Value) -> bool| -> Vec<String> {
        view[key]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|r| keep(r))
            .filter_map(|r| r["task"].as_str().map(str::to_string))
            .collect()
    };
    let regressed = names("regressions", &|_| true);
    if !regressed.is_empty() {
        return (
            "failure",
            format!(
                "regressed over {REPEATS} runs: {}. Fix it or push a new head.",
                regressed.join(", ")
            ),
        );
    }
    let missing = names("errors", &|r| {
        r["b"] == "error" && r["a_grades"].as_u64().unwrap_or(0) > 0
    });
    if !missing.is_empty() {
        return ("failure", no_result(sha, &missing.join(", ")));
    }
    let flaky = names("now_flaky", &|_| true);
    let unmeasured = names("errors", &|_| true);
    let mut text = format!("0 regressed, {} now flaky", flaky.len());
    if !flaky.is_empty() {
        text += &format!(": {}", flaky.join(", "));
    }
    if !unmeasured.is_empty() {
        text += &format!("; {} with no baseline", unmeasured.len());
    }
    ("success", text)
}

/// The clearing command leads, so the 140-character cut drops only names.
fn no_result(sha: &str, cause: &str) -> String {
    format!(
        "no result. Clear: fno-agents evals gate-retry {}. Cause: {cause}",
        short(sha)
    )
}

fn short(sha: &str) -> &str {
    sha.get(..12).unwrap_or(sha)
}

/// The clause a status line gains when the head's bank carried files this
/// binary could not read, so a green verdict names what it did not grade.
fn skip_suffix(skipped: &[String]) -> String {
    if skipped.is_empty() {
        return String::new();
    }
    format!("; unread bank task(s) skipped: {}", skipped.join(", "))
}

fn post_status(p: &Paths, d: &Deps, slug: &str, sha: &str, state: &str, text: &str) -> bool {
    let endpoint = format!("repos/{slug}/statuses/{sha}");
    let cut: String = text.chars().take(DESCRIPTION_MAX).collect();
    let (state_arg, context, description) = (
        format!("state={state}"),
        format!("context={CONTEXT}"),
        format!("description={cut}"),
    );
    (d.run)(
        &[
            "gh",
            "api",
            "--method",
            "POST",
            &endpoint,
            "-f",
            &state_arg,
            "-f",
            &context,
            "-f",
            &description,
        ],
        p.repo,
    )
    .is_some_and(|r| r.ok)
}

fn open_prs(p: &Paths, d: &Deps, slug: &str) -> Option<Vec<Pr>> {
    let endpoint = format!("repos/{slug}/pulls?state=open&per_page=100");
    let out = (d.run)(&["gh", "api", &endpoint], p.repo).filter(|r| r.ok)?;
    let rows: Vec<Value> = serde_json::from_str(&out.stdout).ok()?;
    let mut prs: Vec<Pr> = rows
        .iter()
        // A fork's head would run its own hooks and skills on this machine.
        .filter(|r| r.pointer("/head/repo/full_name").and_then(Value::as_str) == Some(slug))
        .filter_map(|r| {
            Some(Pr {
                number: r.get("number")?.as_u64()?,
                sha: r.pointer("/head/sha")?.as_str()?.to_string(),
                armed: r.get("auto_merge").is_some_and(|v| !v.is_null()),
            })
        })
        .collect();
    prs.sort_by_key(|pr| pr.number);
    Some(prs)
}

/// Disarms every queue-armed PR whose head's gate reads red, pending, or
/// missing at a gated head. GitHub enforces only the ruleset's required
/// checks (main requires one, stacked-base-guard), so an armed PR merges
/// past a gate the queue never waits on: a green-checked PR landed 87
/// seconds after its gate went red. The tick posts the status, then this
/// sweep pulls the armed PR out of the queue; the worker's own merge door
/// re-lands it when the gate settles. One line per disarm, into the tick
/// note; a failed disarm retries next tick.
fn disarm_sweep(p: &Paths, d: &Deps, slug: &str, prs: &[Pr]) -> Vec<String> {
    let mut notes = Vec::new();
    for pr in prs.iter().filter(|pr| pr.armed) {
        let Some(word) = gate_word_at(p, d, slug, pr) else {
            continue;
        };
        let argv = [
            "gh",
            "pr",
            "merge",
            &pr.number.to_string(),
            "--disable-auto",
        ];
        let ok = (d.run)(&argv, p.repo).is_some_and(|r| r.ok);
        notes.push(format!(
            "disarmed pr {} at {} ({word}, {})",
            pr.number,
            short(&pr.sha),
            if ok {
                "queue removed"
            } else {
                "gh refused; retries next tick"
            }
        ));
    }
    notes
}

/// The gate's word at an armed PR's head, from the live combined status:
/// FAILURE/ERROR reads red, PENDING reads pending, and an absent context
/// reads missing only on a head that owes a status (the same fail-open
/// probe the merge door holds on). None leaves the arm alone.
fn gate_word_at(p: &Paths, d: &Deps, slug: &str, pr: &Pr) -> Option<&'static str> {
    let endpoint = format!("repos/{slug}/commits/{}/status", pr.sha);
    let out = (d.run)(&["gh", "api", &endpoint], p.repo).filter(|r| r.ok)?;
    let status: Value = serde_json::from_str(&out.stdout).ok()?;
    // The REST status states are GitHub's own lowercase words; the door
    // uppercases every state read, and the arms below speak the same
    // uppercase words.
    let state = status
        .get("statuses")?
        .as_array()?
        .iter()
        .find(|s| s.get("context").and_then(Value::as_str) == Some(CONTEXT))
        .and_then(|s| s.get("state"))
        .and_then(Value::as_str)
        .map(str::to_uppercase);
    match state.as_deref() {
        Some("FAILURE") | Some("ERROR") => return Some("red"),
        Some("PENDING") => return Some("pending"),
        // A settled-green gate keeps its arm; only a genuinely absent
        // context falls through to the missing check.
        Some(_) => return None,
        None => {}
    }
    // Absent context: hold only when the head actually owes a status, so an
    // ungated diff (the SKIP disposition) never pulls its PR out of the queue.
    let probe = DepsProbe { run: d.run };
    let files = crate::pr_status::lists::pr_file_paths(&probe, p.repo, slug, pr.number).ok()?;
    head_owes_status(p.repo, p.history, &files).then_some("missing")
}

/// Lifts the evals gate's RunProbe seam onto the shared GhProbe shape
/// pr_file_paths takes: cwd passes through, argv converts, None (a spawn
/// that could not run at all) reads as a failed call.
struct DepsProbe<'a> {
    run: RunProbe<'a>,
}

impl crate::pr_status_facts::GhProbe for DepsProbe<'_> {
    fn run_gh(&self, cwd: &Path, args: &[String]) -> Result<(bool, String, String), String> {
        // GhProbe args carry no program word (RealGhProbe adds gh); the
        // RunProbe seam sees the full argv, so restore the head it drops.
        let mut refs: Vec<&str> = Vec::with_capacity(args.len() + 1);
        refs.push("gh");
        refs.extend(args.iter().map(String::as_str));
        match (self.run)(&refs, cwd) {
            Some(r) => Ok((r.ok, r.stdout.clone(), r.stderr.clone())),
            None => Err("gh spawn failed".to_string()),
        }
    }
}

fn is_covered(file: &str, covers: &[String]) -> bool {
    covers.iter().any(|c| file.starts_with(c.as_str()))
}

/// The `covers:` prefixes of main's bank, read once per pass. The map is the
/// bank's contract at main, so a head that edits it also touches `evals/bank/`.
fn main_covers(p: &Paths) -> Vec<String> {
    crate::evals::bank::discover_bank(&ref_bank(p, "main"))
        .unwrap_or_default()
        .into_iter()
        .flat_map(|t| t.covers)
        .collect()
}

/// The merge door's expectation answer: does this head owe an evals/gate
/// status? The same map the tick reads decides it - the five gated prefixes,
/// or a `covers:` prefix of main's bank - and only once the worker seat has
/// a baseline. A first install holds nobody on a check it cannot run yet.
/// A machine with no gate state at all reads as first install, so a merge
/// from a non-daemon machine keeps today's pass. Fail-open by construction:
/// an unread bank answers an empty map, a missing state answers no baseline.
pub(crate) fn head_owes_status(repo: &Path, history: &Path, files: &[String]) -> bool {
    let dir = state_dir(history);
    let Some(state) = std::fs::read_to_string(dir.join("state.json"))
        .ok()
        .and_then(|t| serde_json::from_str::<State>(&t).ok())
    else {
        return false;
    };
    if state.worker.baseline.is_none() {
        return false;
    }
    let ref_bank = dir.join("ref-main").join("evals").join("bank");
    let bank = if ref_bank.is_dir() {
        ref_bank
    } else {
        repo.join("evals").join("bank")
    };
    let covers = crate::evals::bank::discover_bank(&bank)
        .unwrap_or_default()
        .into_iter()
        .flat_map(|t| t.covers)
        .collect::<Vec<_>>();
    touches_gated_path(&files.iter().map(String::as_str).collect::<Vec<_>>())
        || files.iter().any(|f| is_covered(f, &covers))
}

/// A head is gated by a skill, agent, hook, command or bank change, or by a
/// change some task's `covers:` names. None when the file read failed.
fn head_is_gated(p: &Paths, d: &Deps, slug: &str, pr: &Pr, covers: &[String]) -> Option<bool> {
    let probe = DepsProbe { run: d.run };
    let files = crate::pr_status::lists::pr_file_paths(&probe, &p.repo, slug, pr.number).ok()?;
    let gated = touches_gated_path(&files.iter().map(String::as_str).collect::<Vec<_>>());
    Some(gated || files.iter().any(|f| is_covered(f, covers)))
}

fn fetch(p: &Paths, d: &Deps, refspec: &str) -> bool {
    (d.slot_run)(&["git", "fetch", "--quiet", "origin", refspec], p.repo).is_some_and(|r| r.ok)
}

const REF_LOG: &str = "ref-checkout.log";

/// Slot plumbing fails silently into `None` at the caller; a line in this log
/// keeps the next wedge diagnosable from the state dir alone.
fn log_ref_failure(p: &Paths, d: &Deps, line: &str) {
    let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(p.state_dir.join(REF_LOG))
    else {
        return;
    };
    let _ = writeln!(f, "[{}] {}", d.now.to_rfc3339(), line);
}

/// True when the repo can resolve `sha` as a commit.
fn commit_present(p: &Paths, d: &Deps, sha: &str) -> bool {
    let spec = format!("{sha}^{{commit}}");
    (d.slot_run)(&["git", "cat-file", "-e", &spec], p.repo).is_some_and(|r| r.ok)
}

/// The checkout of `sha` the gate reads the bank at: a detached worktree
/// under the state dir, one fixed slot per leg, reused when the slot already
/// sits at that sha. The repo the daemon runs from can sit behind the ref,
/// and a tag read from its working tree misses a golden the ref added. A sha
/// the repo has never fetched (a head pushed from another machine) is fetched
/// from `refspec` before the add. None on any plumbing failure; callers then
/// read the checkout's own bank, which is the behavior this replaces.
fn ref_checkout(
    p: &Paths,
    d: &Deps,
    slot: &str,
    sha: &str,
    refspec: Option<&str>,
) -> Option<PathBuf> {
    let dir = p.state_dir.join(format!("ref-{slot}"));
    let bank = dir.join("evals").join("bank");
    let dir_str = dir.to_string_lossy().into_owned();
    if bank.is_dir()
        && (d.run)(&["git", "-C", &dir_str, "rev-parse", "HEAD"], p.repo)
            .is_some_and(|r| r.ok && r.stdout.trim() == sha)
    {
        return Some(dir);
    }
    // Double force: git holds an `initializing` lock on the worktree it
    // builds and drops it when the add lands; an add killed midway leaves
    // the lock, and a locked slot refuses a single-force remove, so every
    // later rebuild failed with "no checkout". The gate owns its slots. The
    // slot calls ride the 10-minute bound: the 30-second probe kill was what
    // murdered the adds mid-flight and leaked the lock.
    let _ = (d.slot_run)(
        &["git", "worktree", "remove", "--force", "--force", &dir_str],
        p.repo,
    );
    (d.slot_run)(&["git", "worktree", "prune"], p.repo);
    // Both legs fetch the ref they grade before this point; the guard keeps a
    // sha only GitHub knows from failing the add anyway.
    if !commit_present(p, d, sha) {
        if let Some(refspec) = refspec {
            if !fetch(p, d, refspec) {
                log_ref_failure(p, d, &format!("fetch {refspec} failed for {sha}"));
            }
        }
    }
    let added = (d.slot_run)(
        &["git", "worktree", "add", "--detach", &dir_str, sha],
        p.repo,
    );
    match &added {
        Some(r) if !r.ok => log_ref_failure(
            p,
            d,
            &format!("worktree add {slot} at {}: {}", sha, r.stderr.trim()),
        ),
        None => log_ref_failure(p, d, &format!("worktree add {slot} at {sha}: spawn failed")),
        _ => {}
    }
    added.filter(|r| r.ok).map(|_| dir)
}

fn run_argv(
    p: &Paths,
    selector: (&str, &str),
    workers: u32,
    lane: &str,
    cohort: &str,
    sha: &str,
    bank: Option<&PathBuf>,
) -> Vec<String> {
    let mut argv: Vec<String> = vec![
        p.fno_bin.into(),
        "doctor".into(),
        "evals".into(),
        "run".into(),
    ];
    for (flag, value) in [
        (selector.0, selector.1),
        ("--repeat", &REPEATS.to_string()),
        ("--workers", &workers.to_string()),
        ("--lane", lane),
        ("--cohort", cohort),
        ("--variant", "v1"),
        ("--ref", sha),
    ] {
        argv.push(flag.into());
        argv.push(value.into());
    }
    if let Some(bank) = bank {
        argv.push("--bank".into());
        argv.push(bank.to_string_lossy().into_owned());
    }
    argv.push("-y".into());
    argv
}

/// Builds the ref's `fno-agents` pair into a target dir that outlives the ref
/// slot, so a repeat build is incremental, then runs the rest of argv with the
/// grader pointed at it. `set -e`: a failed build runs nothing and grades nothing.
const BUILD_SCRIPT: &str = r#"set -e
root=$1; shift
cargo build --quiet --manifest-path "$root/crates/fno-agents/Cargo.toml" --bins
FNO_EVALS_HEAD_BIN="$CARGO_TARGET_DIR/debug" exec "$@""#;

/// `argv` behind a build of the checkout `root`. The build runs inside the
/// detached run, so the tick still never waits on it.
fn with_build(p: &Paths, slot: &str, root: &Path, argv: Vec<String>) -> Vec<String> {
    let target = p.state_dir.join(format!("target-{slot}"));
    let mut wrapped: Vec<String> = vec![
        "env".into(),
        format!("CARGO_TARGET_DIR={}", target.display()),
        "sh".into(),
        "-c".into(),
        BUILD_SCRIPT.into(),
        "gate-build".into(),
        root.to_string_lossy().into_owned(),
    ];
    wrapped.extend(argv);
    wrapped
}

/// The checkout a bank dir sits in: `<root>/evals/bank`.
fn checkout_of(bank: &Path) -> Option<&Path> {
    bank.parent()?.parent()
}

/// TOML writes the budget as a float or an integer; a string read alone missed both.
pub(crate) fn config_daily_usd(repo: &Path) -> f64 {
    match crate::agents_config::config_value_deep(repo, &["evals", "gate", "daily_usd"]) {
        Some(toml::Value::Float(f)) => f,
        Some(toml::Value::Integer(i)) => i as f64,
        Some(toml::Value::String(v)) => v.trim().parse().unwrap_or(DEFAULT_DAILY_USD),
        _ => DEFAULT_DAILY_USD,
    }
}

pub(crate) fn config_lane(repo: &Path) -> String {
    crate::paths_cli::cfg_str(repo, &["evals", "gate", "lane"])
        .filter(|v| !v.trim().is_empty())
        .unwrap_or_else(|| DEFAULT_LANE.to_string())
}

/// The worker seat keeps `evals.gate.lane`. The lead seat runs the first lane
/// leads are routed to, so the gate grades the seat the user corrects.
fn seat_lane(repo: &Path, seat: &Seat) -> Result<String, String> {
    if seat.name != LEAD.name {
        return Ok(config_lane(repo));
    }
    crate::agents_config::config_value_deep(repo, &["agents", "profiles", "role", "lanes"])
        .as_ref()
        .and_then(toml::Value::as_array)
        .and_then(|lanes| lanes.first())
        .and_then(toml::Value::as_str)
        .map(str::trim)
        .filter(|lane| !lane.is_empty())
        .map(str::to_string)
        .ok_or_else(|| "no lead lane: agents.profiles.role.lanes has no first lane".into())
}

/// Compare a run to the baseline, over `tasks` when named. A head run covers
/// only the gate goldens, and a golden it did not select is no missing result.
fn compare(
    bank: &Path,
    history: &Path,
    a: &str,
    b: &str,
    tasks: Option<&[String]>,
) -> Result<Value, String> {
    let mut rows = read_rows(&history.to_string_lossy(), None, None);
    if let Some(tasks) = tasks {
        rows.retain(|r| tasks.contains(&r.task_id));
        // A stale baseline holds no rows for the kept tasks (its own main run
        // can fail on a wedged slot). fold errors on an empty side, so the
        // unmeasured view verdict() passes is folded here instead. A task
        // whose head rows exist but score nothing stays red: a lane that
        // scores nothing is no result.
        if !rows.iter().any(|r| r.experiment_id.as_deref() == Some(a)) {
            return Ok(serde_json::json!({
                "regressions": [],
                "now_flaky": [],
                "errors": tasks
                    .iter()
                    .map(|t| serde_json::json!({"task": t, "b": "graded", "a_grades": 0}))
                    .collect::<Vec<_>>(),
            }));
        }
    }
    // An unreadable catalog proves nothing unproved, so a regression stays red.
    let unproved = crate::evals::failure_types::unproved_tasks(bank).unwrap_or_default();
    crate::evals_compare::fold(&rows, a, b, REPEATS, &unproved)
}

/// The bank the compares read: the ref slot's copy when one exists, else the checkout's.
fn ref_bank(p: &Paths, slot: &str) -> PathBuf {
    let bank = p
        .state_dir
        .join(format!("ref-{slot}"))
        .join("evals")
        .join("bank");
    if bank.is_dir() {
        bank
    } else {
        p.repo.join("evals").join("bank")
    }
}

fn main_leg(p: &Paths, d: &Deps, state: &mut State, can_launch: bool, daily_usd: f64) -> String {
    if let Some(run) = state.main_run.clone() {
        let over = d.now.timestamp() - run.started > MAIN_LIMIT_S;
        let alive = (d.alive)(run.pid);
        if alive && !over {
            return format!("main {} running", run.cohort);
        }
        if alive {
            (d.kill)(run.pid);
        }
        state.main_run = None;
        let seat = if run.seat == LEAD.name {
            &LEAD
        } else {
            &WORKER
        };
        return finish_main(p, d, state, seat, &run.cohort);
    }
    let day = format!(
        "gate-main-{}-{}",
        d.now.format("%Y-%m-%d"),
        route_hash(p.repo)
    );
    let mut notes = Vec::new();
    for seat in SEATS {
        let (note, settled) = main_seat(p, d, state, seat, &day, can_launch, daily_usd);
        notes.push(note);
        // The next seat waits until this one is settled for now.
        if !settled {
            break;
        }
    }
    notes.join(", ")
}

/// One seat's main run. The flag reads true when the seat is settled for now,
/// so the next seat may go.
fn main_seat(
    p: &Paths,
    d: &Deps,
    state: &mut State,
    seat: &Seat,
    day: &str,
    can_launch: bool,
    daily_usd: f64,
) -> (String, bool) {
    let cohort = format!("{day}{}", seat.suffix);
    let s = state.seat(seat);
    let mut fallback = false;
    if s.main_tried.as_deref() == Some(cohort.as_str()) {
        match s.main_failure.as_ref().filter(|f| f.cohort == cohort) {
            None => return (format!("main {cohort} tried"), true),
            Some(f) if f.tries >= MAIN_MAX_TRIES => {
                if s.fallback_tried.as_deref() == Some(cohort.as_str()) {
                    let note =
                        format!("main {cohort} gave up after {} tries: {}", f.tries, f.cause);
                    return (note, true);
                }
                // A config cause (no lead lane, bank unread) is no lane outage.
                if !f.cause.starts_with("wrote no grade") {
                    let note =
                        format!("main {cohort} gave up after {} tries: {}", f.tries, f.cause);
                    return (note, true);
                }
                fallback = true;
            }
            Some(f) if d.now.timestamp() < f.next_try => {
                return (format!("main {cohort} waits to retry: {}", f.cause), true);
            }
            Some(_) => {}
        }
    }
    if !can_launch {
        return ("main held by the spawn gate".into(), false);
    }
    let lane = if fallback {
        FALLBACK_LANE.to_string()
    } else {
        match seat_lane(p.repo, seat) {
            Ok(lane) => lane,
            Err(cause) => return (main_failed(d, state, seat, &cohort, cause), false),
        }
    };
    let lane = lane_with_room(p.repo, &lane, d.now, d.room);
    let run_cohort = if fallback {
        format!("{cohort}{FALLBACK_SUFFIX}")
    } else {
        cohort.clone()
    };
    if !fetch(p, d, "main") {
        return ("main fetch failed".into(), false);
    }
    let Some(sha) = (d.run)(&["git", "rev-parse", "FETCH_HEAD"], p.repo)
        .filter(|r| r.ok)
        .map(|r| r.stdout.trim().to_string())
    else {
        return ("main rev unread".into(), false);
    };
    // The golden set is read at the sha the run grades, not at the checkout.
    let arg_bank =
        ref_checkout(p, d, "main", &sha, Some("main")).map(|root| root.join("evals").join("bank"));
    let bank_dir = arg_bank
        .clone()
        .unwrap_or_else(|| p.repo.join("evals").join("bank"));
    let selected = match crate::evals::bank::discover_bank(&bank_dir) {
        Ok(tasks) => tasks
            .into_iter()
            .filter(|t| t.tags.iter().any(|tag| tag == seat.tag) && !t.is_demoted())
            .collect::<Vec<_>>(),
        // No launch on an unread bank: the retry loop carries the cause.
        Err(e) => {
            let cause = format!("bank unread at the ref: {}", e.0);
            return (main_failed(d, state, seat, &cohort, cause), false);
        }
    };
    if selected.is_empty() {
        state.seat(seat).main_tried = Some(cohort.clone());
        return (format!("main {cohort} has no {} task", seat.tag), true);
    }
    // A grade-only task starts no worker and costs nothing.
    let workers = selected.iter().filter(|t| t.prompt.is_some()).count();
    let cost = (workers * REPEATS) as f64 * seat.attempt_usd;
    if spent_today(&history_rows(p.history), d.now) + cost > daily_usd {
        let note = format!("main {cohort} held: {cost:.2} USD over the daily budget");
        return (note, false);
    }
    let mut argv = run_argv(
        p,
        ("--tag", seat.tag),
        MAIN_WORKERS,
        &lane,
        &run_cohort,
        &sha,
        arg_bank.as_ref(),
    );
    // The baseline of a CLI golden must grade main's own build, never the deployed one.
    if selected.iter().any(|t| !t.covers.is_empty()) {
        let Some(root) = arg_bank.as_deref().and_then(checkout_of) else {
            let cause = "no checkout of the ref to build".to_string();
            return (main_failed(d, state, seat, &cohort, cause), false);
        };
        argv = with_build(p, "main", root, argv);
    }
    if fallback {
        state.seat(seat).fallback_tried = Some(cohort.clone());
    } else {
        state.seat(seat).main_tried = Some(cohort.clone());
    }
    match (d.spawn)(&argv, p.repo, &p.state_dir.join(MAIN_LOG)) {
        Ok(pid) => {
            state.main_run = Some(MainRun {
                cohort: run_cohort.clone(),
                pid,
                started: d.now.timestamp(),
                seat: seat.name.into(),
            });
            // Saved at once: a tick killed here must not launch a second run.
            save_state(&p.state_dir, state);
            (format!("main {run_cohort} launched on {lane}"), false)
        }
        Err(e) => {
            let cause = format!("launch failed: {e}");
            (main_failed(d, state, seat, &cohort, cause), false)
        }
    }
}

/// Marks the cohort tried too, so the retry wait and the cap also hold for a
/// failure that came before the launch.
fn main_failed(d: &Deps, state: &mut State, seat: &Seat, cohort: &str, cause: String) -> String {
    let s = state.seat(seat);
    let earlier = s
        .main_failure
        .as_ref()
        .filter(|f| f.cohort == cohort)
        .map_or(0, |f| f.tries);
    let note = format!("main {cohort} {cause}");
    s.main_tried = Some(cohort.to_string());
    s.main_failure = Some(MainFailure {
        cohort: cohort.to_string(),
        tries: earlier + 1,
        next_try: d.now.timestamp() + MAIN_RETRY_S,
        cause,
    });
    note
}

/// The last line a run printed, which names why a run that wrote no grade quit.
fn log_tail(log: &Path) -> String {
    let text = std::fs::read_to_string(log).unwrap_or_default();
    let last = text.lines().rev().find(|l| !l.trim().is_empty());
    last.unwrap_or("the run printed nothing")
        .chars()
        .take(CAUSE_MAX)
        .collect()
}

/// True when the evals tree (bank, fixtures, runner) differs between any bank
/// rev the older cohort ran at and any the newer one did. A compare across such
/// a change measures the harder bank, not the pipeline. Only a git diff that
/// exits 1 counts: a rev git cannot read leaves the compare as it was.
fn bank_moved(p: &Paths, d: &Deps, rows: &[Value], older: &str, newer: &str) -> bool {
    let revs = |cohort: &str| -> BTreeSet<String> {
        rows.iter()
            .filter(|r| row_cohort(r) == cohort)
            .filter_map(|r| r["bank_rev"].as_str())
            .map(str::to_string)
            .collect()
    };
    let new_revs = revs(newer);
    revs(older).iter().any(|old| {
        new_revs.iter().any(|new| {
            old != new
                && (d.run)(
                    &[
                        "git",
                        "diff",
                        "--quiet",
                        old,
                        new,
                        "--",
                        "evals",
                        "crates/fno-agents/src/evals",
                    ],
                    p.repo,
                )
                .is_some_and(|r| r.code == Some(1))
        })
    })
}

/// A cohort with no grade is no baseline. A real one is compared to the one
/// before, and that compare is the number the bank exists to print. When the
/// bank changed between the two, the new cohort starts a fresh baseline and no
/// compare runs: every golden the harder bank fails would file a regression.
fn finish_main(p: &Paths, d: &Deps, state: &mut State, seat: &Seat, cohort: &str) -> String {
    let graded = read_rows(&p.history.to_string_lossy(), None, None)
        .iter()
        .any(|r| r.experiment_id.as_deref() == Some(cohort) && crate::evals_trend::is_scored(r));
    if cohort.ends_with(FALLBACK_SUFFIX) {
        // A fallback lane measures a different seat: it is never a baseline.
        let tail = log_tail(&p.state_dir.join(MAIN_LOG));
        return if graded {
            format!("main {cohort} graded on the fallback lane; no baseline, no compare")
        } else {
            format!("main {cohort} wrote no grade: {tail}")
        };
    }
    if !graded {
        let tail = log_tail(&p.state_dir.join(MAIN_LOG));
        return main_failed(d, state, seat, cohort, format!("wrote no grade: {tail}"));
    }
    let s = state.seat(seat);
    s.main_failure = None;
    let previous = s.baseline.replace(cohort.to_string());
    let Some(previous) = previous else {
        return format!("main {cohort} is the first baseline");
    };
    if bank_moved(p, d, &history_rows(p.history), &previous, cohort) {
        return format!(
            "main {cohort}: the bank changed since {previous}; new baseline, no compare"
        );
    }
    let Ok(view) = compare(&ref_bank(p, "main"), p.history, &previous, cohort, None) else {
        return format!("main {cohort} compare failed");
    };
    let _ = std::fs::create_dir_all(&p.state_dir);
    let _ = std::fs::write(
        p.state_dir.join(format!("compare-{cohort}.json")),
        serde_json::to_string_pretty(&view).unwrap_or_default(),
    );
    let failures: BTreeMap<String, String> = view["regressions"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|r| r["task"].as_str())
        .map(|t| {
            (
                t.to_string(),
                format!("always pass to always fail, {cohort}"),
            )
        })
        .collect();
    let count = failures.len();
    crate::evals_arm::file_failure_nodes(
        p.fno_bin,
        Some(p.repo),
        p.graph,
        failures,
        "The gate's main run",
    );
    format!("main {cohort}: {count} regressed against {previous}")
}

fn head_leg(
    p: &Paths,
    d: &Deps,
    state: &mut State,
    slug: &str,
    prs: &[Pr],
    can_launch: bool,
    daily_usd: f64,
) -> String {
    settle_head_run(p, d, state, slug, prs);
    let open: BTreeSet<&str> = prs.iter().map(|pr| pr.sha.as_str()).collect();
    state.heads.retain(|sha, _| open.contains(sha.as_str()));
    let Some(baseline) = state.worker.baseline.clone() else {
        return "head leg waits for a first baseline".into();
    };
    let covers = main_covers(p);
    let mut new_heads = 0;
    for pr in prs {
        if state.heads.contains_key(&pr.sha) {
            continue;
        }
        if new_heads == NEW_HEADS_PER_TICK {
            break;
        }
        new_heads += 1;
        match head_is_gated(p, d, slug, pr, &covers) {
            Some(true) => {
                post_status(
                    p,
                    d,
                    slug,
                    &pr.sha,
                    "pending",
                    "queued for the gate goldens",
                );
                state.heads.insert(pr.sha.clone(), QUEUED.into());
            }
            Some(false) => {
                state.heads.insert(pr.sha.clone(), SKIP.into());
            }
            None => {}
        }
    }
    if state.head_run.is_some() {
        return "head run in flight".into();
    }
    if !can_launch {
        return "head held by the spawn gate".into();
    }
    let today = d.now.format("%Y-%m-%d").to_string();
    let due = |disposition: &String| {
        disposition == QUEUED
            || disposition
                .get(..BUDGET_PREFIX.len())
                .is_some_and(|prefix| prefix == BUDGET_PREFIX)
                && disposition.get(BUDGET_PREFIX.len()..) != Some(today.as_str())
    };
    let Some(pr) = prs
        .iter()
        .find(|pr| state.heads.get(&pr.sha).is_some_and(|h| due(h)))
    else {
        return "no head queued".into();
    };
    launch_head(p, d, state, slug, pr, &baseline, daily_usd, &today)
}

fn wrote_rows(p: &Paths, cohort: &str) -> bool {
    history_rows(p.history)
        .iter()
        .any(|r| row_cohort(r) == cohort)
}

/// The in-flight head run: drop it when its head moved, read it when done.
fn settle_head_run(p: &Paths, d: &Deps, state: &mut State, slug: &str, prs: &[Pr]) {
    let Some(run) = state.head_run.clone() else {
        return;
    };
    let current = prs
        .iter()
        .any(|pr| pr.number == run.pr && pr.sha == run.sha);
    let alive = (d.alive)(run.pid);
    if !current {
        if alive {
            (d.kill)(run.pid);
        }
        state.head_run = None;
        save_state(&p.state_dir, state);
        return;
    }
    let over = d.now.timestamp() - run.started > HEAD_LIMIT_S;
    if alive && !over {
        return;
    }
    if alive {
        (d.kill)(run.pid);
    }
    let (status, text) = match (&state.worker.baseline, over) {
        (_, true) => (
            "failure",
            no_result(
                &run.sha,
                &format!(
                    "{}over the {}-minute limit",
                    if run.built { "build and run " } else { "" },
                    HEAD_LIMIT_S / 60
                ),
            ),
        ),
        // A run that wrote no row quit early: a failed build names itself in the log.
        (Some(_), false) if run.built && !wrote_rows(p, &run.cohort) => (
            "failure",
            no_result(
                &run.sha,
                &format!("no grade: {}", log_tail(&p.state_dir.join(HEAD_LOG))),
            ),
        ),
        (Some(baseline), false) => {
            match compare(
                &ref_bank(p, "head"),
                p.history,
                baseline,
                &run.cohort,
                Some(&run.tasks),
            ) {
                Ok(view) => verdict(&view, &run.sha),
                Err(e) => ("failure", no_result(&run.sha, &e)),
            }
        }
        (None, false) => ("failure", no_result(&run.sha, "no baseline")),
    };
    // A dropped POST keeps the run, so the next tick posts it. Pending
    // forever is how a green PR never merges.
    let skip_note = skip_suffix(&run.skipped);
    let text = format!("{text}{skip_note}");
    if post_status(p, d, slug, &run.sha, status, &text) {
        state.heads.insert(run.sha.clone(), DONE.into());
        state.head_run = None;
    }
}

#[allow(clippy::too_many_arguments)]
fn launch_head(
    p: &Paths,
    d: &Deps,
    state: &mut State,
    slug: &str,
    pr: &Pr,
    baseline: &str,
    daily_usd: f64,
    today: &str,
) -> String {
    let refspec = format!("pull/{}/head", pr.number);
    if !fetch(p, d, &refspec) {
        return format!("head {} fetch failed", short(&pr.sha));
    }
    // The gate-tagged set is read at the head sha: the checkout can sit behind it.
    let arg_bank = ref_checkout(p, d, "head", &pr.sha, Some(&refspec))
        .map(|root| root.join("evals").join("bank"));
    let bank_dir = arg_bank
        .clone()
        .unwrap_or_else(|| p.repo.join("evals").join("bank"));
    let rows = history_rows(p.history);
    let slow = slow_tasks(&rows, baseline);
    // An unreadable bank at the head is red, never green: a PR that broke
    // its bank file must not pass for want of tasks. A check kind newer
    // than this binary is the one exception: the readable tasks still run,
    // and the unread files are named in the pending post and the verdict.
    let (tasks, skipped) = match crate::evals::bank::discover_bank_tolerant(&bank_dir) {
        Ok((tasks, skipped)) => (tasks, skipped),
        Err(e) => {
            let text = no_result(&pr.sha, &format!("bank unread at the head: {}", e.0));
            if post_status(p, d, slug, &pr.sha, "failure", &text) {
                state.heads.insert(pr.sha.clone(), DONE.into());
            }
            return format!("head {} bank unread", short(&pr.sha));
        }
    };
    let mut finish = |status: &str, text: &str, disposition: String| {
        let text = format!("{text}{}", skip_suffix(&skipped));
        if post_status(p, d, slug, &pr.sha, status, &text) {
            state.heads.insert(pr.sha.clone(), disposition);
        }
    };
    let probe = DepsProbe { run: d.run };
    let Some(files) = crate::pr_status::lists::pr_file_paths(&probe, &p.repo, slug, pr.number).ok()
    else {
        return format!("head {} files unread", short(&pr.sha));
    };
    let tasks = gate_tasks(&tasks, &files, &pr.sha, &slow, d, p);
    if tasks.is_empty() {
        finish(
            "success",
            "no gate golden applies at this head",
            DONE.into(),
        );
        return format!("head {} has no gate golden", short(&pr.sha));
    }
    let workers = tasks.iter().filter(|t| t.prompt.is_some()).count();
    let cost = (workers * REPEATS) as f64 * WORKER.attempt_usd;
    // A covering task grades the head's own build. A `cli/`-only head needs no
    // build: its covering tasks call `uv run` from the tree.
    let builds = tasks.iter().any(|t| !t.covers.is_empty())
        && files.iter().any(|f| f.starts_with("crates/"));
    let tasks: Vec<String> = tasks.into_iter().map(|t| t.id).collect();
    if spent_today(&rows, d.now) + cost > daily_usd {
        finish(
            "failure",
            &format!(
                "budget spent: {daily_usd:.2} USD a day. Clear: raise evals.gate.daily_usd, or wait for 00:00 UTC."
            ),
            format!("{BUDGET_PREFIX}{today}"),
        );
        return format!("head {} over budget", short(&pr.sha));
    }
    let cohort = format!("gate-head-{}", short(&pr.sha));
    let lane = config_lane(p.repo);
    let lane = lane_with_room(p.repo, &lane, d.now, d.room);
    // Posted here, not earlier: only a launch attempt that reaches the
    // spawn names the skips, so an early return neither repeats the post
    // on retry nor supersedes it within the same attempt.
    let skip_note = skip_suffix(&skipped);
    if !skipped.is_empty() {
        post_status(
            p,
            d,
            slug,
            &pr.sha,
            "pending",
            &format!("run in flight{skip_note}"),
        );
    }
    let mut argv = run_argv(
        p,
        ("--task", &tasks.join(",")),
        HEAD_WORKERS,
        &lane,
        &cohort,
        &pr.sha,
        arg_bank.as_ref(),
    );
    if builds {
        let Some(root) = arg_bank.as_deref().and_then(checkout_of) else {
            // The deployed binary must never grade this head.
            let cause = no_result(&pr.sha, "no checkout of the head to build");
            finish("failure", &cause, DONE.into());
            return format!("head {} has no checkout to build", short(&pr.sha));
        };
        argv = with_build(p, "head", root, argv);
    }
    match (d.spawn)(&argv, p.repo, &p.state_dir.join(HEAD_LOG)) {
        Ok(pid) => {
            state.head_run = Some(HeadRun {
                pr: pr.number,
                sha: pr.sha.clone(),
                cohort,
                pid,
                started: d.now.timestamp(),
                tasks,
                skipped: skipped.clone(),
                built: builds,
            });
            save_state(&p.state_dir, state);
            format!("head {} launched", short(&pr.sha))
        }
        Err(e) => {
            finish("failure", &no_result(&pr.sha, &e), DONE.into());
            format!("head {} launch failed", short(&pr.sha))
        }
    }
}

fn tick_with(p: &Paths, d: &Deps, can_launch: bool) -> String {
    let daily_usd = config_daily_usd(p.repo);
    if daily_usd <= 0.0 {
        return "gate off: evals.gate.daily_usd is 0".into();
    }
    let mut state = load_state(&p.state_dir);
    let main_note = main_leg(p, d, &mut state, can_launch, daily_usd);
    let (head_note, disarm_notes) = match crate::stacked_base_stamp::origin_slug(p.repo, d.run) {
        Some(slug) => match open_prs(p, d, &slug) {
            Some(prs) => {
                let note = head_leg(p, d, &mut state, &slug, &prs, can_launch, daily_usd);
                let disarms = disarm_sweep(p, d, &slug, &prs);
                (note, disarms)
            }
            None => ("open PR read failed".into(), Vec::new()),
        },
        None => ("origin slug unread".into(), Vec::new()),
    };
    let mut note = format!("{main_note}; {head_note}");
    if !disarm_notes.is_empty() {
        note.push_str("; ");
        note.push_str(&disarm_notes.join("; "));
    }
    state.last_note = note.clone();
    save_state(&p.state_dir, &state);
    note
}

/// The log is replaced at each launch, so one file per leg bounds its size.
fn spawn_detached(argv: &[String], cwd: &Path, log: &Path) -> Result<u32, String> {
    if let Some(dir) = log.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    let out = std::fs::File::create(log).map_err(|e| e.to_string())?;
    let err = out.try_clone().map_err(|e| e.to_string())?;
    let mut cmd = Command::new(&argv[0]);
    cmd.args(&argv[1..])
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(out)
        .stderr(err);
    crate::evals_arm::setsid_hook(&mut cmd);
    cmd.spawn()
        .map(|c| {
            let pid = c.id();
            crate::orphan_reap::release_child(pid);
            pid
        })
        .map_err(|e| e.to_string())
}

/// One pass of both legs. The daemon's tick calls this first, and it never
/// fails the tick: its note lands in `state.json`.
pub(crate) fn tick(
    history: &Path,
    fno_bin: &str,
    repo: &Path,
    graph: Option<&Path>,
    can_launch: bool,
) {
    let run = crate::stacked_base_stamp::runner("git", "gh");
    let paths = Paths {
        history,
        fno_bin,
        repo,
        state_dir: state_dir(history),
        graph,
    };
    let kill = |pid: u32| {
        // SAFETY: the run leads its own session, so -pid is only its group.
        unsafe {
            libc::kill(-(pid as i32), libc::SIGKILL);
        }
    };
    let slot_runner =
        crate::stacked_base_stamp::runner_with("git", "gh", &crate::gate_probes::run_slot_probe);
    let deps = Deps {
        run: &run,
        slot_run: &slot_runner,
        spawn: &spawn_detached,
        alive: &crate::evals_arm::pid_alive,
        kill: &kill,
        room: &real_room,
        now: Utc::now(),
    };
    tick_with(&paths, &deps, can_launch);
}

/// `fno-agents evals gate-retry <sha>`: forget a head, so the next tick runs it
/// again and posts a fresh status.
pub(crate) fn retry(rest: &[String]) -> i32 {
    let Some(prefix) = rest.first().filter(|s| s.len() >= 7) else {
        eprintln!("usage: fno-agents evals gate-retry <sha, 7 characters or more>");
        return 2;
    };
    let dir = state_dir(&crate::evals::history_path());
    let mut state = load_state(&dir);
    let found: Vec<String> = state
        .heads
        .keys()
        .filter(|sha| sha.get(..prefix.len()) == Some(prefix.as_str()))
        .cloned()
        .collect();
    let [sha] = found.as_slice() else {
        eprintln!("gate-retry: {} heads match {prefix}", found.len());
        return 1;
    };
    state.heads.insert(sha.clone(), QUEUED.into());
    save_state(&dir, &state);
    println!("{sha} queued for the next tick");
    0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gate_probes::Run;
    use serde_json::json;
    use std::cell::RefCell;
    use std::io::Write as _;

    const SHA: &str = "abcdef1234567890";

    fn ok(stdout: &str) -> Option<Run> {
        Some(Run {
            ok: true,
            code: Some(0),
            stdout: stdout.into(),
            stderr: String::new(),
        })
    }

    struct World {
        dir: tempfile::TempDir,
        posts: RefCell<Vec<String>>,
        spawns: RefCell<Vec<Vec<String>>>,
        kills: RefCell<Vec<u32>>,
        adds: RefCell<Vec<String>>,
        real_git: bool,
        files: String,
        /// PR 7 sits in GitHub's auto-merge queue (`auto_merge` non-null).
        armed: bool,
        /// The state the combined-status read answers for the evals/gate
        /// context; empty means the context is absent.
        gate_status: String,
        /// The zai 5h window answer `real_room` would give; None is a failed
        /// probe.
        room: Option<bool>,
        disarms: RefCell<Vec<String>>,
    }

    impl World {
        fn new(files: &str) -> World {
            let dir = tempfile::tempdir().unwrap();
            let bank = dir.path().join("evals/bank");
            std::fs::create_dir_all(&bank).unwrap();
            // g4 is a demoted gate golden with baseline grades: every head
            // run must leave it out, and its baseline must not turn a head red.
            for (id, tag) in [
                ("g1", GATE_TAG),
                ("g2", GOLDEN_TAG),
                ("g4", "gate, demoted"),
            ] {
                std::fs::write(
                    bank.join(format!("{id}.yaml")),
                    format!("id: {id}\ntier: capability\nprompt: p\ngrade:\n  - kind: file-exists\n    path: x\ntags: [{tag}]\n"),
                )
                .unwrap();
            }
            World::config(dir.path(), "[agents.profiles.role]\nlanes = []\n");
            World {
                dir,
                posts: RefCell::default(),
                spawns: RefCell::default(),
                kills: RefCell::default(),
                adds: RefCell::default(),
                real_git: false,
                files: files.into(),
                armed: false,
                gate_status: String::new(),
                room: None,
                disarms: RefCell::default(),
            }
        }

        /// A world with real git: the checkout detached at X while main sits
        /// ahead at Y, the stale-checkout shape this fix serves.
        fn new_git(files: &str) -> World {
            let mut w = World::new(files);
            w.real_git = true;
            let git = |args: &[&str]| -> std::process::Output {
                let out = std::process::Command::new("git")
                    .args(args)
                    .current_dir(w.dir.path())
                    .output()
                    .expect("git runs in the test repo");
                assert!(
                    out.status.success(),
                    "git {args:?} failed: {} {}",
                    String::from_utf8_lossy(&out.stdout),
                    String::from_utf8_lossy(&out.stderr)
                );
                out
            };
            git(&["init", "-b", "main"]);
            git(&["config", "user.email", "t@t"]);
            git(&["config", "user.name", "t"]);
            // g1 is in both commits, so the checkout's bank and the head's bank
            // both carry its covers line.
            std::fs::write(
                w.dir.path().join("evals/bank/g1.yaml"),
                format!("id: g1\ntier: capability\nprompt: p\ngrade:\n  - kind: file-exists\n    path: x\ntags: [{GATE_TAG}]\ncovers: [\"crates/x/\"]\n"),
            )
            .unwrap();
            git(&["add", "evals"]);
            git(&["commit", "-m", "x"]);
            let x = String::from_utf8_lossy(&git(&["rev-parse", "HEAD"]).stdout)
                .trim()
                .to_string();
            std::fs::write(
                w.dir.path().join("evals/bank/g3.yaml"),
                "id: g3\ntier: capability\nprompt: p\ngrade:\n  - kind: file-exists\n    path: x\ntags: [gate]\n",
            )
            .unwrap();
            git(&["add", "evals"]);
            git(&["commit", "-m", "y"]);
            git(&["checkout", "--detach", &x]);
            w
        }

        fn config(repo: &Path, text: &str) {
            std::fs::create_dir_all(repo.join(".fno")).unwrap();
            std::fs::write(repo.join(".fno/config.toml"), text).unwrap();
        }

        fn history(&self) -> PathBuf {
            self.dir.path().join("h/history.jsonl")
        }

        fn state(&self, f: impl FnOnce(&mut State)) {
            let dir = state_dir(&self.history());
            let mut state = load_state(&dir);
            f(&mut state);
            save_state(&dir, &state);
        }

        fn rows(&self, cohort: &str, task: &str, passes: [bool; 3]) {
            std::fs::create_dir_all(self.history().parent().unwrap()).unwrap();
            let mut f = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(self.history())
                .unwrap();
            for pass in passes {
                let row = json!({"task_id": task, "tier": "capability", "pass": pass,
                    "status": "graded", "variant": "v1", "experiment_id": cohort});
                writeln!(f, "{row}").unwrap();
            }
        }

        fn tick(&self, alive: bool, now: DateTime<Utc>) -> String {
            let run = |args: &[&str], _: &Path| -> Option<Run> {
                match args {
                    ["git", "remote", ..] => ok("git@github.com:o/r.git\n"),
                    ["git", "worktree", ..] if self.real_git => {
                        let out = std::process::Command::new("git")
                            .args(&args[1..])
                            .current_dir(self.dir.path())
                            .output()
                            .expect("git runs in the test repo");
                        if args.get(2) == Some(&"add") {
                            self.adds
                                .borrow_mut()
                                .push(args.get(4).map(|s| s.to_string()).unwrap_or_default());
                        }
                        Some(Run {
                            ok: out.status.success(),
                            code: out.status.code(),
                            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
                            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
                        })
                    }
                    ["git", "rev-parse", "FETCH_HEAD"] if self.real_git => {
                        let out = std::process::Command::new("git")
                            .args(["rev-parse", "main"])
                            .current_dir(self.dir.path())
                            .output()
                            .expect("git runs in the test repo");
                        Some(Run {
                            ok: out.status.success(),
                            code: out.status.code(),
                            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
                            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
                        })
                    }
                    ["git", "diff", "--quiet", ..] => Some(Run {
                        ok: false,
                        code: Some(1),
                        stdout: String::new(),
                        stderr: String::new(),
                    }),
                    ["git", "worktree", ..] => {
                        // Fallback mode: the gate reads the checkout bank.
                        Some(Run {
                            ok: false,
                            code: Some(1),
                            stdout: String::new(),
                            stderr: String::new(),
                        })
                    }
                    ["gh", "api", "--method", "POST", _, "-f", state, _, _, _, desc] => {
                        self.posts.borrow_mut().push(format!("{state} {desc}"));
                        ok("{}")
                    }
                    // The paginated pr_file_paths read asks one plain page
                    // at a time; the fixture keeps the --jq line shape and
                    // the arm wraps it into the rows the read parses.
                    ["gh", "api", e] if e.contains("/files?") => {
                        let rows: Vec<Value> = self
                            .files
                            .lines()
                            .filter(|l| !l.is_empty())
                            .map(|f| serde_json::json!({"filename": f}))
                            .collect();
                        ok(&Value::Array(rows).to_string())
                    }
                    ["gh", "api", e] if e.contains("/status") => {
                        // GitHub's REST commit-status states are lowercase.
                        let statuses = if self.gate_status.is_empty() {
                            Vec::new()
                        } else {
                            vec![
                                json!({"context": CONTEXT, "state": self.gate_status, "description": "fixture"}),
                            ]
                        };
                        ok(&json!({"statuses": statuses}).to_string())
                    }
                    ["gh", "api", _] => {
                        // The real-git world heads the PR at main, so the
                        // worktree add has a revision it can check out.
                        let head = if self.real_git {
                            String::from_utf8_lossy(
                                &std::process::Command::new("git")
                                    .args(["rev-parse", "main"])
                                    .current_dir(self.dir.path())
                                    .output()
                                    .expect("rev-parse main")
                                    .stdout,
                            )
                            .trim()
                            .to_string()
                        } else {
                            SHA.to_string()
                        };
                        let auto_merge = if self.armed {
                            json!({"merge_method": "squash"})
                        } else {
                            Value::Null
                        };
                        ok(&json!([{"number": 7,
                            "auto_merge": auto_merge,
                            "head": {"sha": head, "repo": {"full_name": "o/r"}}}])
                        .to_string())
                    }
                    ["gh", "pr", "merge", n, "--disable-auto"] => {
                        self.disarms.borrow_mut().push(n.to_string());
                        ok("{}")
                    }
                    _ => ok("mainsha\n"),
                }
            };
            let spawn = |argv: &[String], _: &Path, _: &Path| -> Result<u32, String> {
                self.spawns.borrow_mut().push(argv.to_vec());
                Ok(4242)
            };
            let is_alive = move |_: u32| alive;
            let kill = |pid: u32| self.kills.borrow_mut().push(pid);
            let paths = Paths {
                history: &self.history(),
                fno_bin: "fno",
                repo: self.dir.path(),
                state_dir: state_dir(&self.history()),
                graph: None,
            };
            let room_v = self.room;
            let room_fn = move |_: &Path, _: DateTime<Utc>| room_v;
            let deps = Deps {
                run: &run,
                slot_run: &run,
                spawn: &spawn,
                alive: &is_alive,
                kill: &kill,
                room: &room_fn,
                now,
            };
            tick_with(&paths, &deps, true)
        }

        /// A finished main run, so the head leg has its baseline.
        fn with_baseline(&self) {
            self.rows("base", "g1", [true, true, true]);
            self.rows("base", "g4", [true, true, true]);
            let tried = format!(
                "gate-main-{}-{}",
                Utc::now().format("%Y-%m-%d"),
                route_hash(self.dir.path())
            );
            self.state(|s| {
                s.worker.baseline = Some("base".into());
                s.worker.main_tried = Some(tried);
            });
        }
    }

    #[test]
    fn a_crate_head_runs_only_the_tasks_covering_it_and_an_uncovered_one_posts_nothing() {
        // (changed files, tasks the one launch must hold or None, statuses posted)
        let cases = [
            ("cli/x/a.rs\n", Some("--task c1 "), 1),
            ("skills/s\ncli/x/a.rs\n", Some("--task c1,g1 "), 1),
            ("cli/y/a.rs\n", None, 0),
            // A crate head needs a build, and with no checkout to build the
            // deployed binary must not grade it: red, no launch.
            ("crates/x/a.rs\n", None, 2),
        ];
        for (files, task_arg, posted) in cases {
            let w = World::new(files);
            std::fs::write(
                w.dir.path().join("evals/bank/c1.yaml"),
                "id: c1\ntier: regression\ngrade:\n  - kind: file-exists\n    path: x\ncovers: [\"cli/x/\", \"crates/x/\"]\n",
            )
            .unwrap();
            w.with_baseline();
            w.tick(true, Utc::now());
            let spawns = w.spawns.borrow();
            let argv = spawns.first().map(|a| a.join(" ")).unwrap_or_default();
            match task_arg {
                Some(arg) => assert!(argv.contains(arg), "{files}: {argv}"),
                None => assert!(spawns.is_empty(), "{files}: {argv}"),
            }
            let posts = w.posts.borrow();
            assert_eq!(posts.len(), posted, "{files}: {posts:?}");
            if posted == 2 {
                assert!(posts[1].starts_with("state=failure"), "{files}: {posts:?}");
            }
        }
    }

    #[test]
    fn a_grade_only_row_costs_nothing_and_a_row_that_does_not_say_still_counts() {
        let now = Utc::now();
        let ts = now.to_rfc3339();
        let row = |obs: Value| json!({"experiment_id": "gate-head-abc", "ts": ts, "obs": obs});
        let rows = [
            row(json!({"worker_required": false})),
            row(json!({"worker_required": true})),
            row(json!({})),
        ];
        assert!((spent_today(&rows, now) - 2.0 * WORKER.attempt_usd).abs() < 1e-9);
    }

    #[test]
    fn the_head_leg_reads_the_bank_at_the_head_sha_not_the_checkout() {
        let w = World::new_git("skills/x/SKILL.md\n");
        let y = String::from_utf8_lossy(
            &std::process::Command::new("git")
                .args(["rev-parse", "main"])
                .current_dir(w.dir.path())
                .output()
                .unwrap()
                .stdout,
        )
        .trim()
        .to_string();
        w.with_baseline();
        w.tick(true, Utc::now());
        let spawns = w.spawns.borrow();
        assert_eq!(spawns.len(), 1, "one head launch");
        let argv = spawns[0].join(" ");
        assert!(argv.contains("--task g1,g3"), "{argv}");
        assert!(argv.contains("--bank"), "{argv}");
        assert!(argv.contains(&format!("--ref {y}")), "{argv}");
        assert_eq!(w.adds.borrow().len(), 1, "one worktree add");
    }

    #[test]
    fn a_covered_crate_head_builds_the_checkout_it_read_the_bank_from() {
        let w = World::new_git("crates/x/a.rs\n");
        w.with_baseline();
        w.tick(true, Utc::now());
        let spawns = w.spawns.borrow();
        assert_eq!(spawns.len(), 1, "one head launch");
        let slot = state_dir(&w.history()).join("ref-head");
        let argv = spawns[0].join(" ");
        assert!(
            argv.contains(&format!("gate-build {} ", slot.display())),
            "{argv}"
        );
    }

    #[test]
    fn with_no_baseline_the_main_leg_runs_goldens_and_no_status_posts() {
        let w = World::new("skills/x/SKILL.md\n");
        w.tick(true, Utc::now());
        let spawns = w.spawns.borrow();
        assert_eq!(spawns.len(), 1);
        let argv = spawns[0].join(" ");
        assert!(argv.contains("--tag golden --repeat 3"), "{argv}");
        assert!(argv.contains("--ref mainsha"), "{argv}");
        assert!(w.posts.borrow().is_empty());
    }

    /// The merge door's expectation probe: the tick's own precondition,
    /// answered from the same state and the same covers map. A head the
    /// daemon cannot judge (no baseline yet, no gate state at all) owes no
    /// status, so the door never holds it.
    #[test]
    fn the_merge_door_expects_a_status_only_for_a_gated_head_with_a_seated_baseline() {
        let w = World::new("skills/x/SKILL.md\n");
        let gated = vec!["skills/x/SKILL.md".to_string()];
        let ungated = vec!["cli/src/fno/whatever.py".to_string()];
        // First install: no gate state at all reads no baseline -> no debt.
        assert!(!head_owes_status(w.dir.path(), &w.history(), &gated));

        // A seated baseline + a gated diff owes a status.
        w.state(|s| s.worker.baseline = Some("base".into()));
        assert!(head_owes_status(w.dir.path(), &w.history(), &gated));

        // The same baseline + an ungated diff owes none.
        assert!(!head_owes_status(w.dir.path(), &w.history(), &ungated));

        // A covered prefix from the checkout's own bank owes one too (the
        // ref slot is absent here, so the checkout bank is the map).
        std::fs::write(
            w.dir.path().join("evals/bank/c1.yaml"),
            "id: c1\ntier: capability\nprompt: p\ngrade:\n  - kind: file-exists\n    path: x\ntags: [gate]\ncovers: [\"crates/fno-agents/src/pr_status/\"]\n",
        )
        .unwrap();
        assert!(head_owes_status(
            w.dir.path(),
            &w.history(),
            &["crates/fno-agents/src/pr_status/compose.rs".to_string()]
        ));
    }

    #[test]
    fn a_main_run_with_no_grade_keeps_its_cause_and_retries_up_to_the_cap() {
        let w = World::new("crates/x.rs\n");
        let dir = state_dir(&w.history());
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(MAIN_LOG), "Error: unknown lane\n\n").unwrap();
        // Early in the day: the cohort names the date, so a clock past 21:00 UTC
        // crosses midnight here and the tries count starts over.
        let now = Utc::now()
            .date_naive()
            .and_hms_opt(1, 0, 0)
            .unwrap()
            .and_utc();
        let hour = chrono::Duration::seconds(MAIN_RETRY_S + 1);
        let at = |tries: i32| now + hour * tries;

        w.tick(true, now);
        w.tick(false, now);
        w.tick(false, now);
        let state = load_state(&dir);
        assert_eq!(w.spawns.borrow().len(), 1, "no retry before the wait ends");
        let failure = state.worker.main_failure.as_ref().unwrap();
        assert_eq!(failure.cause, "wrote no grade: Error: unknown lane");
        assert!(
            state.last_note.contains("unknown lane"),
            "{}",
            state.last_note
        );

        for tries in 1..=2 {
            w.tick(true, at(tries));
            w.tick(false, at(tries));
        }
        w.tick(true, at(3));
        assert_eq!(w.spawns.borrow().len(), MAIN_MAX_TRIES as usize + 1);
        let fallback = w.spawns.borrow()[MAIN_MAX_TRIES as usize].join(" ");
        assert!(fallback.contains("--lane claude-sonnet-5"), "{fallback}");
        assert!(fallback.contains("-fallback"), "{fallback}");
        w.tick(false, at(3));
        w.tick(true, at(4));
        assert_eq!(
            w.spawns.borrow().len(),
            MAIN_MAX_TRIES as usize + 1,
            "one fallback per cohort"
        );
        let state = load_state(&dir);
        assert!(
            state.last_note.contains("gave up after 3 tries"),
            "{}",
            state.last_note
        );
        assert!(state.worker.baseline.is_none());
    }

    #[test]
    fn zai_peak_is_monday_to_friday_0600_to_1000_utc() {
        let at = |s: &str| DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc);
        assert!(zai_peak(at("2026-11-02T06:00:00Z")), "Monday 06:00 opens");
        assert!(zai_peak(at("2026-11-06T09:59:00Z")), "Friday 09:59 holds");
        assert!(!zai_peak(at("2026-11-02T10:00:00Z")), "10:00 closes");
        assert!(!zai_peak(at("2026-11-02T05:59:00Z")), "before 06:00");
        assert!(
            !zai_peak(at("2026-11-07T07:00:00Z")),
            "Saturday is off peak"
        );
    }

    #[test]
    fn a_main_run_with_a_grade_is_the_baseline_and_ends_the_day() {
        let w = World::new("crates/x.rs\n");
        let now = Utc::now()
            .date_naive()
            .and_hms_opt(1, 0, 0)
            .unwrap()
            .and_utc();
        w.tick(true, now);
        let cohort = load_state(&state_dir(&w.history()))
            .worker
            .main_tried
            .unwrap();
        w.rows(&cohort, "g2", [true, true, true]);
        w.tick(false, now);
        w.tick(false, now + chrono::Duration::seconds(MAIN_RETRY_S + 1));
        let state = load_state(&state_dir(&w.history()));
        assert_eq!(state.worker.baseline.as_deref(), Some(cohort.as_str()));
        assert!(state.worker.main_failure.is_none());
        assert_eq!(w.spawns.borrow().len(), 1);
    }

    #[test]
    fn a_main_run_after_a_bank_change_resets_the_baseline_and_compares_nothing() {
        let w = World::new("crates/x.rs\n");
        let now = Utc::now()
            .date_naive()
            .and_hms_opt(1, 0, 0)
            .unwrap()
            .and_utc();
        w.state(|s| s.worker.baseline = Some("old".into()));
        w.tick(true, now);
        let cohort = load_state(&state_dir(&w.history()))
            .worker
            .main_tried
            .unwrap();
        std::fs::create_dir_all(w.history().parent().unwrap()).unwrap();
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(w.history())
            .unwrap();
        for (cohort, rev, pass) in [("old", "aaa", true), (cohort.as_str(), "bbb", false)] {
            let row = json!({"task_id": "g2", "tier": "capability", "pass": pass,
                "status": "graded", "variant": "v1", "experiment_id": cohort, "bank_rev": rev});
            writeln!(f, "{row}").unwrap();
        }
        drop(f);
        w.tick(false, now);
        let state = load_state(&state_dir(&w.history()));
        assert_eq!(state.worker.baseline.as_deref(), Some(cohort.as_str()));
        assert!(!state_dir(&w.history())
            .join(format!("compare-{cohort}.json"))
            .exists());
    }

    #[test]
    fn a_detached_run_writes_stdout_and_stderr_to_its_log() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("logs/main.log");
        let argv: Vec<String> = ["sh", "-c", "echo out; echo err >&2"]
            .map(String::from)
            .to_vec();
        let pid = spawn_detached(&argv, dir.path(), &log).unwrap();
        for _ in 0..200 {
            if std::fs::read_to_string(&log).is_ok_and(|t| t.contains("err")) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let text = std::fs::read_to_string(&log).unwrap();
        assert!(
            text.contains("out") && text.contains("err"),
            "{pid}: {text}"
        );
    }

    #[test]
    fn a_gated_head_gets_pending_then_three_runs_and_an_untouched_one_is_skipped() {
        let w = World::new("skills/x/SKILL.md\n");
        w.with_baseline();
        w.tick(true, Utc::now());
        assert_eq!(w.posts.borrow().len(), 1);
        assert!(w.posts.borrow()[0].contains("state=pending"));
        let argv = w.spawns.borrow()[0].join(" ");
        assert!(argv.contains("--task g1 --repeat 3 --workers 4"), "{argv}");
        assert!(argv.contains(&format!("--ref {SHA}")), "{argv}");

        let other = World::new("crates/x.rs\n");
        other.with_baseline();
        other.tick(true, Utc::now());
        assert!(other.posts.borrow().is_empty());
        assert!(other.spawns.borrow().is_empty());
    }

    #[test]
    fn a_head_with_a_newer_check_kind_runs_the_rest_and_names_the_skip() {
        let w = World::new("skills/x/SKILL.md\n");
        w.with_baseline();
        std::fs::write(
            w.dir.path().join("evals/bank/r1.yaml"),
            "id: r1\ntier: capability\nprompt: p\ngrade:\n  - kind: rubric\n    rubric: grade me\ntags: [gate]\n",
        )
        .unwrap();
        w.tick(true, Utc::now());
        let argv = w.spawns.borrow()[0].join(" ");
        assert!(argv.contains("--task g1"), "{argv}");
        assert!(!argv.contains("g1,r1"), "{argv}");
        assert!(
            w.posts
                .borrow()
                .iter()
                .any(|p| p.contains("state=pending") && p.contains("skipped: r1.yaml")),
            "{0:?}",
            w.posts.borrow()
        );
        let cohort = format!("gate-head-{}", short(SHA));
        w.rows(&cohort, "g1", [true, true, true]);
        w.tick(false, Utc::now());
        let post = w.posts.borrow().last().cloned().unwrap();
        assert!(post.contains("state=success"), "{post}");
        assert!(post.contains("0 regressed"), "{post}");
        assert!(post.contains("skipped: r1.yaml"), "{post}");
    }

    #[test]
    fn a_new_flake_passes_and_lists_a_new_always_fail_is_red_and_names_it() {
        let w = World::new("skills/x/SKILL.md\n");
        w.with_baseline();
        w.tick(true, Utc::now());
        let cohort = format!("gate-head-{}", short(SHA));
        w.rows(&cohort, "g1", [true, false, true]);
        w.tick(false, Utc::now());
        let post = w.posts.borrow().last().cloned().unwrap();
        assert!(post.contains("state=success"), "{post}");
        assert!(post.contains("1 now flaky: g1"), "{post}");

        let w = World::new("skills/x/SKILL.md\n");
        w.with_baseline();
        w.tick(true, Utc::now());
        w.rows(&cohort, "g1", [false, false, false]);
        w.tick(false, Utc::now());
        let post = w.posts.borrow().last().cloned().unwrap();
        assert!(post.contains("state=failure"), "{post}");
        assert!(post.contains("regressed over 3 runs: g1"), "{post}");
    }

    #[test]
    fn no_result_by_the_limit_kills_the_run_and_red_names_the_clearing_command() {
        let w = World::new("skills/x/SKILL.md\n");
        w.with_baseline();
        let now = Utc::now();
        w.tick(true, now);
        w.tick(true, now + chrono::Duration::seconds(HEAD_LIMIT_S + 1));
        assert_eq!(*w.kills.borrow(), [4242]);
        let post = w.posts.borrow().last().cloned().unwrap();
        assert!(post.contains("state=failure"), "{post}");
        assert!(post.contains("gate-retry abcdef123456"), "{post}");
        assert!(post.contains("20-minute limit"), "{post}");
    }

    #[test]
    fn a_spent_budget_launches_nothing_and_red_names_the_clearing_key() {
        let w = World::new("skills/x/SKILL.md\n");
        w.with_baseline();
        let now = Utc::now();
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(w.history())
            .unwrap();
        for _ in 0..400 {
            let row = json!({"task_id": "z", "tier": "capability", "pass": true, "status": "graded",
                "experiment_id": "gate-main-old", "ts": now.to_rfc3339()});
            writeln!(f, "{row}").unwrap();
        }
        w.tick(true, now);
        assert!(w.spawns.borrow().is_empty());
        let post = w.posts.borrow().last().cloned().unwrap();
        assert!(post.contains("state=failure"), "{post}");
        assert!(post.contains("evals.gate.daily_usd"), "{post}");
    }

    #[test]
    fn the_budget_key_reads_as_a_number_and_zero_turns_the_gate_off() {
        let w = World::new("skills/x/SKILL.md\n");
        World::config(w.dir.path(), "[evals.gate]\ndaily_usd = 2.5\n");
        assert_eq!(config_daily_usd(w.dir.path()), 2.5);
        World::config(w.dir.path(), "[evals.gate]\ndaily_usd = 0\n");
        let note = w.tick(true, Utc::now());
        assert!(w.spawns.borrow().is_empty());
        assert_eq!(note, "gate off: evals.gate.daily_usd is 0");
    }

    #[test]
    fn a_lead_row_costs_the_lead_price_and_an_old_row_the_worker_price() {
        let now = Utc::now();
        let row = |cohort: &str| json!({"experiment_id": cohort, "ts": now.to_rfc3339()});
        let rows = [
            row("gate-main-d-h-lead"),
            row("gate-main-d-h"),
            row("gate-head-abc"),
        ];
        let spent = spent_today(&rows, now);
        assert!(
            (spent - (LEAD.attempt_usd + 2.0 * WORKER.attempt_usd)).abs() < 1e-9,
            "{spent}"
        );
    }

    #[test]
    fn an_old_state_keeps_its_baseline_on_the_worker_seat_and_the_lead_seat_names_its_holds() {
        let w = World::new("crates/x.rs\n");
        let now = Utc::now()
            .date_naive()
            .and_hms_opt(1, 0, 0)
            .unwrap()
            .and_utc();
        let day = format!(
            "gate-main-{}-{}",
            now.format("%Y-%m-%d"),
            route_hash(w.dir.path())
        );
        let dir = state_dir(&w.history());
        std::fs::create_dir_all(&dir).unwrap();
        // The shape a binary before seats wrote.
        let old = json!({"heads": {}, "head_run": null, "main_run": null, "baseline": "base",
            "main_tried": day, "last_note": ""});
        std::fs::write(dir.join("state.json"), old.to_string()).unwrap();
        w.tick(true, now);
        let state = load_state(&dir);
        assert_eq!(state.worker.baseline.as_deref(), Some("base"));
        assert!(state.lead.baseline.is_none());
        assert!(
            w.spawns.borrow().is_empty(),
            "the worker seat already ran today"
        );
        let cause = &state.lead.main_failure.as_ref().unwrap().cause;
        assert!(cause.contains("agents.profiles.role.lanes"), "{cause}");

        let w = World::new("crates/x.rs\n");
        World::config(
            w.dir.path(),
            "[evals.gate]\ndaily_usd = 1\n[agents.profiles.role]\nlanes = [\"opus\"]\n",
        );
        std::fs::write(
            w.dir.path().join("evals/bank/l1.yaml"),
            "id: l1\ntier: capability\nprompt: p\ngrade:\n  - kind: file-exists\n    path: x\ntags: [golden, lead]\n",
        )
        .unwrap();
        w.tick(true, now);
        let cohort = load_state(&dir_of(&w)).worker.main_tried.unwrap();
        w.rows(&cohort, "g2", [true, true, true]);
        w.tick(false, now);
        w.tick(false, now);
        assert_eq!(w.spawns.borrow().len(), 1, "only the worker seat launched");
        let note = load_state(&dir_of(&w)).last_note;
        assert!(
            note.contains("-lead held: 2.10 USD over the daily budget"),
            "{note}"
        );
    }

    fn dir_of(w: &World) -> PathBuf {
        state_dir(&w.history())
    }

    // ── the disarm sweep ─────────────────────────────────────────────────

    /// An armed PR whose gate reads ungreen comes out of the queue: the
    /// ruleset requires only stacked-base-guard, so the queue would merge
    /// past the gate (FAILURE and PENDING alike).
    #[test]
    fn an_armed_pr_with_an_ungreen_gate_disarms() {
        for (state, word) in [("failure", "red"), ("pending", "pending")] {
            let mut w = World::new("docs/x.md");
            w.with_baseline();
            w.armed = true;
            w.gate_status = state.into();
            let note = w.tick(true, Utc::now());
            assert_eq!(
                w.disarms.borrow().as_slice(),
                &["7".to_string()],
                "{state}: {note}"
            );
            assert!(note.contains("disarmed pr 7"), "{state}: {note}");
            assert!(note.contains(&format!("({word}")), "{state}: {note}");
        }
    }

    /// A settled-green gate keeps its arm, and so does an ungated head
    /// with no status at all: nothing to protect the queue from, and a
    /// disarm would strand a mergeable PR for nothing.
    #[test]
    fn an_armed_pr_with_a_green_gate_or_an_ungated_head_stays_armed() {
        let mut w = World::new("docs/x.md");
        w.with_baseline();
        w.armed = true;
        w.gate_status = "success".into();
        let note = w.tick(true, Utc::now());
        assert!(w.disarms.borrow().is_empty(), "{note}");

        // Absent context on the same ungated head: the missing gate is not
        // this PR's fact.
        let mut w = World::new("docs/x.md");
        w.with_baseline();
        w.armed = true;
        let note = w.tick(true, Utc::now());
        assert!(w.disarms.borrow().is_empty(), "{note}");
    }

    /// A gated head with no status pulls its armed PR out of the queue:
    /// the daemon never reached the head, and the queue would not wait for
    /// a status the ruleset does not require. An unarmed PR never enters
    /// the sweep - the disarm only answers an arm the queue could act on.
    #[test]
    fn a_missing_gate_status_at_a_gated_head_disarms_and_an_unarmed_pr_is_never_touched() {
        let mut w = World::new("evals/bank/g3.yaml");
        w.with_baseline();
        w.armed = true;
        let note = w.tick(true, Utc::now());
        assert_eq!(w.disarms.borrow().as_slice(), &["7".to_string()], "{note}");
        assert!(note.contains("(missing"), "{note}");

        let mut w = World::new("docs/x.md");
        w.with_baseline();
        w.gate_status = "failure".into();
        let note = w.tick(true, Utc::now());
        assert!(w.disarms.borrow().is_empty(), "{note}");
    }

    #[test]
    fn a_bank_only_head_grades_its_changed_goldens_and_a_skills_head_keeps_the_tier() {
        // The split the table below rides: the tier prefixes plus the bank
        // make up the gated set, with the bank the only non-tier entry.
        for p in TIER_PATHS {
            assert!(GATED_PATHS.contains(&p), "{p} must stay gated");
        }
        assert_eq!(
            GATED_PATHS
                .iter()
                .filter(|p| !TIER_PATHS.contains(p))
                .count(),
            1
        );
        // (changed files, the task list the launch must carry)
        for (files, tasks) in [
            (
                "evals/bank/g1.yaml\nevals/fixtures/metrics/g1.yaml\n",
                "--task g1 ",
            ),
            ("skills/x/SKILL.md\n", "--task g1,g5 "),
        ] {
            let w = World::new(files);
            std::fs::write(
                w.dir.path().join("evals/bank/g5.yaml"),
                "id: g5\ntier: capability\nprompt: p\ngrade:\n  - kind: file-exists\n    path: x\ntags: [gate]\n",
            )
            .unwrap();
            w.with_baseline();
            w.tick(true, Utc::now());
            let spawns = w.spawns.borrow();
            assert_eq!(spawns.len(), 1, "{files}: one head launch");
            let argv = spawns[0].join(" ");
            assert!(argv.contains(tasks), "{files}: {argv}");
            if tasks == "--task g1 " {
                assert!(!argv.contains("g5"), "{files}: {argv}");
            }
        }
    }

    #[test]
    fn ref_checkout_fetches_a_sha_the_repo_lacks_and_skips_it_when_present() {
        let dir = tempfile::tempdir().unwrap();
        let state = dir.path().join("state");
        std::fs::create_dir_all(&state).unwrap();
        let fetches: RefCell<Vec<String>> = RefCell::default();
        let present = RefCell::new(true);
        let add_ok = RefCell::new(true);
        let run = |args: &[&str], _: &Path| -> Option<Run> {
            match args {
                ["git", "cat-file", "-e", ..] => Some(Run {
                    ok: *present.borrow(),
                    code: None,
                    stdout: String::new(),
                    stderr: String::new(),
                }),
                ["git", "fetch", "--quiet", "origin", rs] => {
                    fetches.borrow_mut().push(rs.to_string());
                    ok("")
                }
                ["git", "worktree", "add", ..] => Some(Run {
                    ok: *add_ok.borrow(),
                    code: None,
                    stdout: String::new(),
                    stderr: "fatal: no".into(),
                }),
                _ => ok(""),
            }
        };
        let spawn = |_: &[String], _: &Path, _: &Path| -> Result<u32, String> { Ok(1) };
        let alive = |_: u32| true;
        let kill = |_: u32| {};
        let paths = Paths {
            history: &dir.path().join("h.jsonl"),
            fno_bin: "fno",
            repo: dir.path(),
            state_dir: state.clone(),
            graph: None,
        };
        let d = Deps {
            run: &run,
            slot_run: &run,
            spawn: &spawn,
            alive: &alive,
            kill: &kill,
            room: &|_: &Path, _: DateTime<Utc>| None,
            now: Utc::now(),
        };
        let slot_sha = "0123456789abcdef0123456789abcdef01234567";

        // The repo already has the sha: no fetch runs.
        let got = ref_checkout(&paths, &d, "head", slot_sha, Some("pull/7/head"));
        assert!(got.is_some(), "present sha checks out");
        assert!(fetches.borrow().is_empty(), "no fetch when present");

        // The repo lacks the sha: the refspec fetch runs, then the add lands.
        *present.borrow_mut() = false;
        fetches.borrow_mut().clear();
        let got = ref_checkout(&paths, &d, "head", slot_sha, Some("pull/7/head"));
        assert!(got.is_some(), "fetched sha checks out");
        assert_eq!(fetches.borrow().as_slice(), ["pull/7/head"]);

        // No refspec: no fetch is attempted, and the add still runs.
        fetches.borrow_mut().clear();
        let got = ref_checkout(&paths, &d, "head", slot_sha, None);
        assert!(got.is_some());
        assert!(fetches.borrow().is_empty());

        // A failed add leaves the cause in the ref-checkout log.
        *add_ok.borrow_mut() = false;
        let got = ref_checkout(&paths, &d, "head", slot_sha, Some("pull/7/head"));
        assert!(got.is_none(), "a failed add reads as no checkout");
        let log = std::fs::read_to_string(state.join(REF_LOG)).unwrap();
        assert!(log.contains("worktree add head"), "{log}");
        assert!(log.contains("fatal: no"), "{log}");
    }

    #[test]
    fn a_leaked_add_lock_no_longer_wedges_the_slot_rebuild() {
        // The wedge this heals: git holds an `initializing` lock on the
        // worktree it builds and drops it when the add lands; an add killed
        // midway leaves it, and a locked slot refuses `worktree remove
        // --force`, so every later rebuild failed with "no checkout".
        let w = World::new_git("skills/x/SKILL.md\ncrates/x/a.rs\n");
        w.with_baseline();
        w.tick(true, Utc::now());
        let common = String::from_utf8_lossy(
            &std::process::Command::new("git")
                .args(["rev-parse", "--path-format=absolute", "--git-common-dir"])
                .current_dir(w.dir.path())
                .output()
                .unwrap()
                .stdout,
        )
        .trim()
        .to_string();
        let admin = Path::new(&common).join("worktrees").join("ref-head");
        assert!(admin.join("gitdir").exists(), "slot admin entry exists");
        std::fs::write(admin.join("locked"), "initializing").unwrap();

        // Advance main: the next head sha forces a rebuild of the slot.
        let git = |args: &[&str]| {
            let out = std::process::Command::new("git")
                .args(args)
                .current_dir(w.dir.path())
                .output()
                .unwrap();
            assert!(
                out.status.success(),
                "git {args:?} failed: {} {}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            );
        };
        git(&["checkout", "-q", "main"]);
        std::fs::write(
            w.dir.path().join("evals/bank/g6.yaml"),
            "id: g6\ntier: capability\nprompt: p\ngrade:\n  - kind: file-exists\n    path: x\ntags: [gate]\n",
        )
        .unwrap();
        git(&["add", "evals"]);
        git(&["commit", "-q", "-m", "z"]);

        w.tick(true, Utc::now());
        assert_eq!(
            w.adds.borrow().len(),
            2,
            "the rebuild ran: {:?}",
            w.adds.borrow()
        );
        let spawns = w.spawns.borrow();
        assert_eq!(spawns.len(), 2, "the head launched");
        let posts = w.posts.borrow();
        assert!(
            posts.iter().all(|p| p.starts_with("state=pending")),
            "no failure posted: {posts:?}"
        );
        assert!(!admin.join("locked").exists(), "the leaked lock is gone");
        assert!(
            !Path::new(&common)
                .join("worktrees/ref-head/locked")
                .exists(),
            "the leaked lock did not come back"
        );
    }

    fn usage_record(route: Option<&str>) -> crate::accounts::records::Record {
        crate::accounts::records::Record {
            id: "z1".into(),
            name: "z".into(),
            harness: "claude".into(),
            auth: "managed".into(),
            priority: 0,
            route_provider_id: None,
            model_name: None,
            credentials_source: None,
            env: None,
            route: route.map(str::to_string),
            reset_timezone: None,
            window_minutes: None,
            account_id: String::new(),
            tags: vec![],
            description: None,
            config_dir: None,
            pricing: None,
            cost_cap_usd_per_session: None,
            extra: Default::default(),
        }
    }

    fn usage_row(windows: Value) -> crate::accounts::usage::Row {
        crate::accounts::usage::Row {
            entry: windows,
            identity: None,
        }
    }

    #[test]
    fn zai_window_room_reads_every_zai_records_5h_window() {
        let windows = |pct: Value| {
            json!({"windows": [
                {"label": "weekly", "used_pct": 10.0},
                {"label": "5h", "used_pct": pct},
            ]})
        };
        let zai = usage_record(Some("zai/glm-5.3-flash"));
        let other = usage_record(Some("claude/sonnet-5"));
        let threshold = 90.0;

        // Room: the 5h window under the threshold. The weekly window and the
        // non-zai record are ignored.
        assert!(zai_window_room(
            &[other.clone(), zai.clone()],
            &[
                usage_row(windows(json!(40.0))),
                usage_row(windows(json!(40.0)))
            ],
            threshold
        ));

        // At the cap: no room.
        assert!(!zai_window_room(
            &[zai.clone()],
            &[usage_row(windows(json!(90.0)))],
            threshold
        ));

        // No 5h window at all: the probe cannot prove the lane runnable.
        assert!(!zai_window_room(
            &[zai.clone()],
            &[usage_row(
                json!({"windows": [{"label": "weekly", "used_pct": 10.0}]})
            )],
            threshold
        ));

        // An unreadable row is no room.
        assert!(!zai_window_room(
            &[zai.clone()],
            &[usage_row(
                json!({"state": "unknown", "reason": "probe-failed"})
            )],
            threshold
        ));

        // No zai record in the store: no proven room.
        assert!(!zai_window_room(
            &[other],
            &[usage_row(windows(json!(10.0)))],
            threshold
        ));
    }

    #[test]
    fn a_capped_zai_lane_falls_back_to_the_fixed_sonnet_lane() {
        // The probe reads no room in both worlds (no account store), so the
        // lane moves. The fallback is the fixed sonnet lane: never the first
        // claude inventory row, which can be opus.
        for (config, banned) in [
            (
                "[[routing.models]]\nname = \"zai-flash\"\naccount = \"zai\"\nroute = \"zai/glm-5.3-flash\"\n\n[[routing.models]]\nname = \"claude-opus\"\naccount = \"claude\"\nroute = \"claude/opus-5\"\n\n[[routing.models]]\nname = \"claude-sonnet\"\naccount = \"claude\"\nroute = \"claude/sonnet-5\"\n\n[agents.profiles.role]\nlanes = []\n",
                "claude-opus",
            ),
            (
                "[[routing.models]]\nname = \"zai-flash\"\naccount = \"zai\"\nroute = \"zai/glm-5.3-flash\"\n\n[agents.profiles.role]\nlanes = []\n",
                "",
            ),
        ] {
            let w = World::new("skills/x/SKILL.md\n");
            World::config(w.dir.path(), config);
            w.with_baseline();
            w.tick(true, Utc::now());
            let spawns = w.spawns.borrow();
            assert_eq!(spawns.len(), 1, "the head launched on the fallback");
            let argv = spawns[0].join(" ");
            assert!(argv.contains("--lane claude-sonnet-5 "), "{argv}");
            if !banned.is_empty() {
                assert!(!argv.contains(banned), "{argv}");
            }
        }
    }

    #[test]
    fn a_head_task_with_no_baseline_rows_grades_green_and_no_scored_rows_stays_red() {
        // Two shapes, two verdicts. 4494: the baseline cohort held no rows
        // for the kept tasks (its own main run failed on the wedged slot),
        // which folds as the unmeasured case verdict() passes. 4497: every
        // head row rode a substituted lane, so lane evidence excluded all of
        // them and nothing scored. A lane that scores nothing is no result:
        // it stays red.
        let g5 = "id: g5\ntier: capability\nprompt: p\ngrade:\n  - kind: file-exists\n    path: x\ntags: [gate]\n";

        let w = World::new("evals/bank/g5.yaml\n");
        std::fs::write(w.dir.path().join("evals/bank/g5.yaml"), g5).unwrap();
        w.with_baseline();
        w.tick(true, Utc::now());
        assert_eq!(w.spawns.borrow().len(), 1, "one head launch");
        w.rows("gate-head-abcdef123456", "g5", [true, true, true]);
        w.tick(false, Utc::now());
        let posts = w.posts.borrow();
        let success = posts.iter().find(|p| p.starts_with("state=success"));
        assert!(success.is_some(), "no baseline reads green: {posts:?}");
        assert!(success.unwrap().contains("with no baseline"), "{posts:?}");
        drop(posts);

        // 4497's shape: the head rows exist but no row scores, while the
        // baseline holds grades for the same task. Red, no result.
        let w = World::new("evals/bank/g5.yaml\n");
        std::fs::write(w.dir.path().join("evals/bank/g5.yaml"), g5).unwrap();
        w.with_baseline();
        w.rows("base", "g5", [true, true, true]);
        w.tick(true, Utc::now());
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(w.history())
            .unwrap();
        for _ in 0..3 {
            let row = json!({"task_id": "g5", "tier": "capability", "pass": true,
                "status": "graded", "variant": "v1", "lane_status": "substituted",
                "experiment_id": "gate-head-abcdef123456"});
            writeln!(f, "{row}").unwrap();
        }
        drop(f);
        w.tick(false, Utc::now());
        let posts = w.posts.borrow();
        let failure = posts.iter().find(|p| p.starts_with("state=failure"));
        assert!(
            failure.is_some(),
            "a scoring-less lane stays red: {posts:?}"
        );
        assert!(failure.unwrap().contains("no result"), "{posts:?}");
        assert!(failure.unwrap().contains("g5"), "{posts:?}");
    }
}
