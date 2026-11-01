//! The evals gate: the daemon's check on skill, agent, hook, command and bank
//! changes. CI cannot run it: a bank run needs worker lanes.
//!
//! Each `evals-arm` tick runs one pass over two independent legs.
//!
//! **Main leg.** Once a day, and at once when the route config hash changes,
//! it runs the golden tier at `origin/main` as cohort
//! `gate-main-<date>-<hash>`. The golden set is read at the fetched sha
//! through a detached worktree, never from this repo's working tree: the
//! checkout the daemon runs from can sit behind the ref, and a tag read
//! there misses a golden main added. Each finished cohort becomes the
//! baseline, and it is compared to the one before. A regression files a
//! node, so a route change that breaks a golden reaches the backlog.
//!
//! **Head leg.** An open same-repo PR whose head touches a gated path gets a
//! pending `evals/gate` status, then one run of the `gate` goldens at that
//! head, one PR at a time. A newer head kills the older run. The head pays one
//! side only: it compares to the baseline. Until a first baseline exists the
//! leg posts nothing, so a fresh install holds no PR on a check it cannot do.
//!
//! The runs are detached, and the tick never waits on one. A later tick reads
//! the history rows, so a daemon restart loses no run. Plumbing never reads as
//! a pass: no result by the limit, or a spent budget, is red and names the
//! cause and the clearing command.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::evals_trend::read_rows;
use crate::gate_probes::RunProbe;

const CONTEXT: &str = "evals/gate";
const GATED_PATHS: [&str; 5] = ["skills/", "agents/", "hooks/", "commands/", "evals/bank/"];
const GATE_TAG: &str = "gate";
const GOLDEN_TAG: &str = "golden";
const REPEATS: usize = 3;
const HEAD_WORKERS: u32 = 4;
const MAIN_WORKERS: u32 = 2;
const HEAD_LIMIT_S: i64 = 1200;
const MAIN_LIMIT_S: i64 = 8 * 3600;
/// A task slower than this cannot sit in a 20-minute gate at 3 repeats.
const MAX_TASK_WALL_S: f64 = 180.0;
const NEW_HEADS_PER_TICK: usize = 10;
const DEFAULT_DAILY_USD: f64 = 15.0;
const DEFAULT_LANE: &str = "zai-flash";
/// The plan's measured GLM cost per attempt. Spend is this times the gate rows
/// of the day, because a row's usage has no price until a manifest names one.
const ATTEMPT_USD: f64 = 0.04;
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
}

#[derive(Serialize, Deserialize, Clone)]
struct MainRun {
    cohort: String,
    pid: u32,
    started: i64,
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
struct State {
    /// Head sha to its disposition. Only open PR heads stay.
    heads: BTreeMap<String, String>,
    head_run: Option<HeadRun>,
    main_run: Option<MainRun>,
    baseline: Option<String>,
    /// The cohort last launched. With no `main_failure` for it, the day is done.
    main_tried: Option<String>,
    #[serde(default)]
    main_failure: Option<MainFailure>,
    last_note: String,
}

struct Pr {
    number: u64,
    sha: String,
}

struct Deps<'a> {
    run: RunProbe<'a>,
    spawn: &'a dyn Fn(&[String], &Path, &Path) -> Result<u32, String>,
    alive: &'a dyn Fn(u32) -> bool,
    kill: &'a dyn Fn(u32),
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

fn history_rows(history: &Path) -> Vec<Value> {
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
fn spent_today(rows: &[Value], now: DateTime<Utc>) -> f64 {
    let day = now.format("%Y-%m-%d").to_string();
    let attempts = rows
        .iter()
        .filter(|r| row_cohort(r).get(..5) == Some("gate-"))
        .filter(|r| {
            r.get("ts")
                .and_then(Value::as_str)
                .and_then(|ts| ts.get(..10))
                == Some(day.as_str())
        })
        .count();
    attempts as f64 * ATTEMPT_USD
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

/// The `gate` goldens this head can run, read from `bank_dir`: the tested
/// ref's bank when one is materialized, else the checkout's. A golden added
/// to main after the head branched has no fixture there, and would read as a
/// regression.
fn gate_tasks(
    p: &Paths,
    d: &Deps,
    bank_dir: &Path,
    sha: &str,
    slow: &BTreeSet<String>,
) -> Vec<String> {
    let tasks = crate::evals::bank::discover_bank(bank_dir).unwrap_or_default();
    tasks
        .iter()
        .filter(|t| t.tags.iter().any(|tag| tag == GATE_TAG))
        .filter(|t| !slow.contains(&t.id))
        .filter(|t| {
            let spec = format!("{sha}:evals/bank/{}.yaml", t.id);
            (d.run)(&["git", "cat-file", "-e", &spec], p.repo).is_some_and(|r| r.ok)
        })
        .map(|t| t.id.clone())
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
            })
        })
        .collect();
    prs.sort_by_key(|pr| pr.number);
    Some(prs)
}

/// None on a failed read, so a blip never records a head as ungated.
fn head_is_gated(p: &Paths, d: &Deps, slug: &str, number: u64) -> Option<bool> {
    let endpoint = format!("repos/{slug}/pulls/{number}/files?per_page=100");
    let out = (d.run)(
        &["gh", "api", "--paginate", &endpoint, "--jq", ".[].filename"],
        p.repo,
    )
    .filter(|r| r.ok)?;
    let files: Vec<&str> = out.stdout.lines().collect();
    Some(touches_gated_path(&files))
}

fn fetch(p: &Paths, d: &Deps, refspec: &str) -> bool {
    (d.run)(&["git", "fetch", "--quiet", "origin", refspec], p.repo).is_some_and(|r| r.ok)
}

/// The checkout of `sha` the gate reads the bank at: a detached worktree
/// under the state dir, one fixed slot per leg, reused when the slot already
/// sits at that sha. The repo the daemon runs from can sit behind the ref,
/// and a tag read from its working tree misses a golden the ref added. None
/// on any plumbing failure; callers then read the checkout's own bank, which
/// is the behavior this replaces.
fn ref_checkout(p: &Paths, d: &Deps, slot: &str, sha: &str) -> Option<PathBuf> {
    let dir = p.state_dir.join(format!("ref-{slot}"));
    let bank = dir.join("evals").join("bank");
    let dir_str = dir.to_string_lossy().into_owned();
    if bank.is_dir()
        && (d.run)(&["git", "-C", &dir_str, "rev-parse", "HEAD"], p.repo)
            .is_some_and(|r| r.ok && r.stdout.trim() == sha)
    {
        return Some(dir);
    }
    let _ = (d.run)(&["git", "worktree", "remove", "--force", &dir_str], p.repo);
    (d.run)(&["git", "worktree", "prune"], p.repo);
    (d.run)(
        &["git", "worktree", "add", "--detach", &dir_str, sha],
        p.repo,
    )
    .filter(|r| r.ok)
    .map(|_| dir)
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

fn config_daily_usd(repo: &Path) -> f64 {
    crate::paths_cli::cfg_str(repo, &["evals", "gate", "daily_usd"])
        .and_then(|v| v.trim().parse().ok())
        .unwrap_or(DEFAULT_DAILY_USD)
}

fn config_lane(repo: &Path) -> String {
    crate::paths_cli::cfg_str(repo, &["evals", "gate", "lane"])
        .filter(|v| !v.trim().is_empty())
        .unwrap_or_else(|| DEFAULT_LANE.to_string())
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
    }
    // An unreadable catalog proves nothing unproved, so a regression stays red.
    let unproved = crate::evals::failure_types::unproved_tasks(bank).unwrap_or_default();
    crate::evals_compare::fold(&rows, a, b, REPEATS, &unproved)
}

/// The bank the compares read: the ref slot's copy when the launch
/// materialized one, else the checkout's.
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
        return finish_main(p, d, state, &run.cohort);
    }
    let cohort = format!(
        "gate-main-{}-{}",
        d.now.format("%Y-%m-%d"),
        route_hash(p.repo)
    );
    if state.main_tried.as_deref() == Some(cohort.as_str()) {
        match state.main_failure.as_ref().filter(|f| f.cohort == cohort) {
            None => return format!("main {cohort} tried"),
            Some(f) if f.tries >= MAIN_MAX_TRIES => {
                return format!("main {cohort} gave up after {} tries: {}", f.tries, f.cause);
            }
            Some(f) if d.now.timestamp() < f.next_try => {
                return format!("main {cohort} waits to retry: {}", f.cause);
            }
            Some(_) => {}
        }
    }
    if !can_launch {
        return "main held by the spawn gate".into();
    }
    if !fetch(p, d, "main") {
        return "main fetch failed".into();
    }
    let Some(sha) = (d.run)(&["git", "rev-parse", "FETCH_HEAD"], p.repo)
        .filter(|r| r.ok)
        .map(|r| r.stdout.trim().to_string())
    else {
        return "main rev unread".into();
    };
    // The golden set is read at the sha the run grades, not at the checkout.
    let arg_bank = ref_checkout(p, d, "main", &sha).map(|root| root.join("evals").join("bank"));
    let bank_dir = arg_bank
        .clone()
        .unwrap_or_else(|| p.repo.join("evals").join("bank"));
    let goldens = crate::evals::bank::discover_bank(&bank_dir)
        .unwrap_or_default()
        .iter()
        .filter(|t| t.tags.iter().any(|tag| tag == GOLDEN_TAG))
        .count();
    let cost = (goldens * REPEATS) as f64 * ATTEMPT_USD;
    if spent_today(&history_rows(p.history), d.now) + cost > daily_usd {
        return format!("main held: {cost:.2} USD over the daily budget");
    }
    let lane = config_lane(p.repo);
    let argv = run_argv(
        p,
        ("--tag", GOLDEN_TAG),
        MAIN_WORKERS,
        &lane,
        &cohort,
        &sha,
        arg_bank.as_ref(),
    );
    state.main_tried = Some(cohort.clone());
    match (d.spawn)(&argv, p.repo, &p.state_dir.join(MAIN_LOG)) {
        Ok(pid) => {
            state.main_run = Some(MainRun {
                cohort: cohort.clone(),
                pid,
                started: d.now.timestamp(),
            });
            // Saved at once: a tick killed here must not launch a second run.
            save_state(&p.state_dir, state);
            format!("main {cohort} launched")
        }
        Err(e) => main_failed(d, state, &cohort, format!("launch failed: {e}")),
    }
}

fn main_failed(d: &Deps, state: &mut State, cohort: &str, cause: String) -> String {
    let earlier = state
        .main_failure
        .as_ref()
        .filter(|f| f.cohort == cohort)
        .map_or(0, |f| f.tries);
    let note = format!("main {cohort} {cause}");
    state.main_failure = Some(MainFailure {
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

/// A cohort with no grade is no baseline. A real one is compared to the one
/// before, and that compare is the number the bank exists to print.
fn finish_main(p: &Paths, d: &Deps, state: &mut State, cohort: &str) -> String {
    let graded = read_rows(&p.history.to_string_lossy(), None, None)
        .iter()
        .any(|r| r.experiment_id.as_deref() == Some(cohort) && crate::evals_trend::is_scored(r));
    if !graded {
        let tail = log_tail(&p.state_dir.join(MAIN_LOG));
        return main_failed(d, state, cohort, format!("wrote no grade: {tail}"));
    }
    state.main_failure = None;
    let previous = state.baseline.replace(cohort.to_string());
    let Some(previous) = previous else {
        return format!("main {cohort} is the first baseline");
    };
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
    let Some(baseline) = state.baseline.clone() else {
        return "head leg waits for a first baseline".into();
    };
    let mut new_heads = 0;
    for pr in prs {
        if state.heads.contains_key(&pr.sha) {
            continue;
        }
        if new_heads == NEW_HEADS_PER_TICK {
            break;
        }
        new_heads += 1;
        match head_is_gated(p, d, slug, pr.number) {
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
    let (status, text) = match (&state.baseline, over) {
        (_, true) => (
            "failure",
            no_result(
                &run.sha,
                &format!("over the {}-minute limit", HEAD_LIMIT_S / 60),
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
    let arg_bank = ref_checkout(p, d, "head", &pr.sha).map(|root| root.join("evals").join("bank"));
    let bank_dir = arg_bank
        .clone()
        .unwrap_or_else(|| p.repo.join("evals").join("bank"));
    let rows = history_rows(p.history);
    let tasks = gate_tasks(p, d, &bank_dir, &pr.sha, &slow_tasks(&rows, baseline));
    let mut finish = |status: &str, text: &str, disposition: String| {
        if post_status(p, d, slug, &pr.sha, status, text) {
            state.heads.insert(pr.sha.clone(), disposition);
        }
    };
    if tasks.is_empty() {
        finish(
            "success",
            "no gate golden applies at this head",
            DONE.into(),
        );
        return format!("head {} has no gate golden", short(&pr.sha));
    }
    let cost = (tasks.len() * REPEATS) as f64 * ATTEMPT_USD;
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
    let argv = run_argv(
        p,
        ("--task", &tasks.join(",")),
        HEAD_WORKERS,
        &lane,
        &cohort,
        &pr.sha,
        arg_bank.as_ref(),
    );
    match (d.spawn)(&argv, p.repo, &p.state_dir.join(HEAD_LOG)) {
        Ok(pid) => {
            state.head_run = Some(HeadRun {
                pr: pr.number,
                sha: pr.sha.clone(),
                cohort,
                pid,
                started: d.now.timestamp(),
                tasks,
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
    let head_note = match crate::stacked_base_stamp::origin_slug(p.repo, d.run) {
        Some(slug) => match open_prs(p, d, &slug) {
            Some(prs) => head_leg(p, d, &mut state, &slug, &prs, can_launch, daily_usd),
            None => "open PR read failed".into(),
        },
        None => "origin slug unread".into(),
    };
    let note = format!("{main_note}; {head_note}");
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
    cmd.spawn().map(|c| c.id()).map_err(|e| e.to_string())
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
    let deps = Deps {
        run: &run,
        spawn: &spawn_detached,
        alive: &crate::evals_arm::pid_alive,
        kill: &kill,
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
    }

    impl World {
        fn new(files: &str) -> World {
            let dir = tempfile::tempdir().unwrap();
            let bank = dir.path().join("evals/bank");
            std::fs::create_dir_all(&bank).unwrap();
            for (id, tag) in [("g1", GATE_TAG), ("g2", GOLDEN_TAG)] {
                std::fs::write(
                    bank.join(format!("{id}.yaml")),
                    format!("id: {id}\ntier: capability\nprompt: p\ngrade:\n  - kind: file-exists\n    path: x\ntags: [{tag}]\n"),
                )
                .unwrap();
            }
            World {
                dir,
                posts: RefCell::default(),
                spawns: RefCell::default(),
                kills: RefCell::default(),
                adds: RefCell::default(),
                real_git: false,
                files: files.into(),
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

        fn tick(&self, alive: bool, now: DateTime<Utc>) {
            let run = |args: &[&str], _: &Path| -> Option<Run> {
                match args {
                    ["git", "remote", ..] => ok("git@github.com:o/r.git\n"),
                    ["git", "worktree", ..] if self.real_git => {
                        let out = std::process::Command::new("git")
                            .args(&args[1..])
                            .current_dir(self.dir.path())
                            .output()
                            .expect("git runs in the test repo");
                        if args.get(1) == Some(&"add") {
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
                    ["git", "worktree", ..] => {
                        // Fallback mode: materialization fails, so the gate
                        // reads the checkout bank, the behavior before the
                        // ref-faithful read existed.
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
                    ["gh", "api", "--paginate", ..] => ok(&self.files),
                    ["gh", "api", _] => ok(&json!([{"number": 7,
                        "head": {"sha": SHA, "repo": {"full_name": "o/r"}}}])
                    .to_string()),
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
            let deps = Deps {
                run: &run,
                spawn: &spawn,
                alive: &is_alive,
                kill: &kill,
                now,
            };
            tick_with(&paths, &deps, true);
        }

        /// A finished main run, so the head leg has its baseline.
        fn with_baseline(&self) {
            self.rows("base", "g1", [true, true, true]);
            let tried = format!(
                "gate-main-{}-{}",
                Utc::now().format("%Y-%m-%d"),
                route_hash(self.dir.path())
            );
            self.state(|s| {
                s.baseline = Some("base".into());
                s.main_tried = Some(tried);
            });
        }
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
        // A checkout behind main misses g3 in its bank; the ref-faithful
        // read runs it. The fallback path lists only g1.
        assert!(argv.contains("--task g1,g3"), "{argv}");
        assert!(argv.contains("--bank"), "{argv}");
        assert!(argv.contains(&format!("--ref {y}")), "{argv}");
        assert_eq!(w.adds.borrow().len(), 1, "one worktree add");
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

    #[test]
    fn a_main_run_with_no_grade_keeps_its_cause_and_retries_up_to_the_cap() {
        let w = World::new("crates/x.rs\n");
        let dir = state_dir(&w.history());
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(MAIN_LOG), "Error: unknown lane\n\n").unwrap();
        let now = Utc::now();
        let hour = chrono::Duration::seconds(MAIN_RETRY_S + 1);
        let at = |tries: i32| now + hour * tries;

        w.tick(true, now);
        w.tick(false, now);
        w.tick(false, now);
        let state = load_state(&dir);
        assert_eq!(w.spawns.borrow().len(), 1, "no retry before the wait ends");
        let failure = state.main_failure.as_ref().unwrap();
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
        assert_eq!(w.spawns.borrow().len(), MAIN_MAX_TRIES as usize);
        let state = load_state(&dir);
        assert!(
            state.last_note.contains("gave up after 3 tries"),
            "{}",
            state.last_note
        );
        assert!(state.baseline.is_none());
    }

    #[test]
    fn a_main_run_with_a_grade_is_the_baseline_and_ends_the_day() {
        let w = World::new("crates/x.rs\n");
        let now = Utc::now();
        w.tick(true, now);
        let cohort = load_state(&state_dir(&w.history())).main_tried.unwrap();
        w.rows(&cohort, "g2", [true, true, true]);
        w.tick(false, now);
        w.tick(false, now + chrono::Duration::seconds(MAIN_RETRY_S + 1));
        let state = load_state(&state_dir(&w.history()));
        assert_eq!(state.baseline.as_deref(), Some(cohort.as_str()));
        assert!(state.main_failure.is_none());
        assert_eq!(w.spawns.borrow().len(), 1);
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
}
