//! The weekly capability leg beside the regression arm.
//!
//! The arm's regression leg never launches the capability tier, and the
//! daily gate runs only golden-tagged tasks, so the other capability tasks
//! ran by hand and never accrued the passes graduation needs. This leg runs
//! them (goldens excluded) once per `evals.schedule_days` under a
//! `gate-capability-<date>` cohort, so the run counts against the same
//! `evals.gate.daily_usd` cap the gate reads. Capability failures are data:
//! no backlog nodes. A settled run opens one graduation PR for the tasks
//! whose last three full runs passed.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use serde_json::{json, Value};

use crate::claims;
use crate::events::EventEmitter;

pub(crate) const CLAIM_KEY: &str = "evals:capability-run";
pub(crate) const COHORT_PREFIX: &str = "gate-capability-";
const GOLDEN_TAG: &str = "golden";
const WORKERS: &str = "2";

pub(crate) struct Leg<'a> {
    pub history: &'a Path,
    pub repo: &'a Path,
    pub window_days: i64,
    pub run_timeout_s: u64,
    pub claims_root: Option<&'a Path>,
}

#[derive(Debug, PartialEq)]
pub(crate) enum Verdict {
    Fresh,
    Budget,
    Launch,
}

/// Capability tasks the leg runs: non-golden, not demoted.
fn leg_tasks(repo: &Path) -> Vec<crate::evals::bank::TaskSpec> {
    crate::evals::bank::discover_bank(&repo.join("evals").join("bank"))
        .unwrap_or_default()
        .into_iter()
        .filter(|t| t.tier == "capability" && !t.is_demoted())
        .filter(|t| !t.tags.iter().any(|g| g == GOLDEN_TAG))
        .collect()
}

fn row_ts(row: &Value) -> Option<DateTime<Utc>> {
    row.get("ts")
        .and_then(Value::as_str)
        .and_then(|ts| DateTime::parse_from_rfc3339(ts).ok())
        .map(|dt| dt.with_timezone(&Utc))
}

fn row_cohort(row: &Value) -> &str {
    row.get("experiment_id")
        .and_then(Value::as_str)
        .unwrap_or_default()
}

/// Newest capability-leg row. The cohort prefix is what separates it from the
/// gate's daily main run, whose golden tasks are capability tier too.
fn newest_leg_row(rows: &[Value]) -> Option<DateTime<Utc>> {
    rows.iter()
        .filter(|r| row_cohort(r).starts_with(COHORT_PREFIX))
        .filter_map(row_ts)
        .max()
}

/// Due when the newest leg row is older than the window and the budget holds
/// the whole run: tasks x repeats x one worker attempt, beside today's spend.
pub(crate) fn decide(
    rows: &[Value],
    now: DateTime<Utc>,
    window_days: i64,
    task_count: usize,
    daily_usd: f64,
) -> Verdict {
    if task_count == 0 {
        return Verdict::Fresh;
    }
    if newest_leg_row(rows).is_some_and(|ts| (now - ts).num_days() < window_days) {
        return Verdict::Fresh;
    }
    let estimate = task_count as f64
        * crate::evals_gate::REPEATS as f64
        * crate::evals_gate::WORKER_ATTEMPT_USD;
    if crate::evals_gate::spent_today(rows, now) + estimate > daily_usd {
        return Verdict::Budget;
    }
    Verdict::Launch
}

fn cohort_for(day: DateTime<Utc>) -> String {
    format!("{COHORT_PREFIX}{}", day.format("%Y-%m-%d"))
}

/// A launch is recorded so a run that fails before it writes a row waits a
/// day instead of relaunching on every tick.
fn attempt_path(history: &Path) -> PathBuf {
    history.with_file_name("evals-capability-attempt.txt")
}

fn tried_within_a_day(history: &Path, now: DateTime<Utc>) -> bool {
    std::fs::read_to_string(attempt_path(history))
        .ok()
        .and_then(|t| DateTime::parse_from_rfc3339(t.trim()).ok())
        .is_some_and(|ts| (now - ts.with_timezone(&Utc)).num_hours() < 24)
}

/// Tick mode: launch the detached run when due. The outcome word is for the
/// caller's log and the tests; a skip is silent.
pub(crate) fn tick(
    leg: &Leg,
    now: DateTime<Utc>,
    can_launch: bool,
    argv: &[String],
    spawner: &dyn Fn(&[String]) -> Result<u32, String>,
) -> &'static str {
    let root = leg
        .claims_root
        .map(Path::to_path_buf)
        .or_else(claims::global_claims_root);
    match claims::status(CLAIM_KEY, root.as_deref()) {
        (claims::ClaimState::Live, _) => return "in_flight",
        (claims::ClaimState::Corrupted, _) => return "claim_corrupted",
        (claims::ClaimState::Suspect | claims::ClaimState::Stale, Some(rec)) => {
            let _ = claims::release(CLAIM_KEY, &rec.holder, root.as_deref(), None);
        }
        _ => {}
    }
    let rows = crate::evals_gate::history_rows(leg.history);
    let daily = crate::evals_gate::config_daily_usd(leg.repo);
    match decide(
        &rows,
        now,
        leg.window_days,
        leg_tasks(leg.repo).len(),
        daily,
    ) {
        Verdict::Fresh => return "fresh",
        Verdict::Budget => return "budget",
        Verdict::Launch => {}
    }
    if !can_launch {
        return "gated";
    }
    if tried_within_a_day(leg.history, now) {
        return "retry_wait";
    }
    let _ = std::fs::write(attempt_path(leg.history), now.to_rfc3339());
    let Ok(pid) = spawner(argv) else {
        return "spawn_failed";
    };
    let opts = claims::AcquireOpts {
        host: None,
        pid: Some(pid),
        pid_unavailable: false,
        ttl_ms: Some((leg.run_timeout_s as i64 + 600) * 1000),
        reason: Some("evals weekly capability run".into()),
        metadata: None,
        pid_provenance: None,
        identity: None,
        root: root.clone(),
        events_dir: None,
    };
    let _ = claims::acquire(CLAIM_KEY, &pid.to_string(), opts);
    "launched"
}

pub(crate) struct Run<'a> {
    pub leg: Leg<'a>,
    pub events: &'a Path,
    pub fno_bin: &'a str,
    pub started: DateTime<Utc>,
}

/// Run mode: the detached child. Runs the leg bounded, journals the outcome,
/// opens the graduation PR, and always releases the claim.
pub(crate) fn run(r: &Run) -> i32 {
    let t0 = Instant::now();
    let cohort = cohort_for(r.started);
    let lane = crate::evals_gate::config_lane(r.leg.repo);
    let repeat = crate::evals_gate::REPEATS.to_string();
    let mut cmd = Command::new(r.fno_bin);
    cmd.current_dir(r.leg.repo)
        .args(["doctor", "evals", "run", "--tier", "capability"])
        .args(["--exclude-tag", GOLDEN_TAG, "--repeat", &repeat])
        .args([
            "--cohort",
            &cohort,
            "--lane",
            &lane,
            "--workers",
            WORKERS,
            "-y",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    crate::evals_arm::setsid_hook(&mut cmd);
    let release = || {
        let _ = claims::release(
            CLAIM_KEY,
            &std::process::id().to_string(),
            r.leg.claims_root,
            None,
        );
    };
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            release();
            eprintln!("evals capability leg: could not spawn {}: {e}", r.fno_bin);
            return 0;
        }
    };
    let drain = crate::evals_arm::spawn_stderr_drain(&mut child);
    let deadline = t0 + Duration::from_secs(r.leg.run_timeout_s);
    let code = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status.code().unwrap_or(-1)),
            Ok(None) if Instant::now() >= deadline => break None,
            Ok(None) => std::thread::sleep(Duration::from_millis(200)),
            Err(_) => break Some(-1),
        }
    };
    if code.is_none() {
        // SAFETY: the child leads its own session, so -pid is only its group.
        unsafe {
            libc::kill(-(child.id() as i32), libc::SIGKILL);
        }
        let _ = child.wait();
    }
    let stderr = drain.and_then(|h| h.join().ok()).unwrap_or_default();
    release();
    if code != Some(0) {
        let why = crate::evals_arm::stderr_tail(&stderr);
        eprintln!("evals capability leg failed ({code:?}): {why}");
        return 0;
    }
    let rows: Vec<Value> = crate::evals_gate::history_rows(r.leg.history)
        .into_iter()
        .filter(|row| row_cohort(row) == cohort)
        .filter(|row| row_ts(row).is_some_and(|ts| ts >= r.started))
        .collect();
    let task_count = rows
        .iter()
        .filter_map(|row| row.get("task_id").and_then(Value::as_str))
        .collect::<std::collections::BTreeSet<_>>()
        .len();
    let passes = rows
        .iter()
        .filter(|row| row.get("pass") == Some(&Value::Bool(true)))
        .count();
    let _ = EventEmitter::new(r.events, "daemon").emit(
        "evals_scheduled_run",
        &json!({
            "tier": "capability",
            "cohort": cohort,
            "task_count": task_count,
            "passes": passes,
            "duration_s": (t0.elapsed().as_secs_f64() * 1000.0).round() / 1000.0,
            "window_days": r.leg.window_days,
        }),
    );
    match graduation_pr(r.leg.history, r.leg.repo, r.started) {
        Ok(Some(url)) => eprintln!("evals capability leg: graduation PR {url}"),
        Ok(None) => {}
        Err(e) => eprintln!("evals capability leg: graduation PR failed: {e}"),
    }
    0
}

/// Capability tasks of the leg whose last three full runs passed, per the
/// trend report's own rule. Golden tasks are the gate's, so they stay out.
fn eligible(history: &Path, repo: &Path) -> Vec<String> {
    let out = Command::new(crate::evals_arm::self_exe())
        .args([
            "evals-trend",
            "--mode",
            "report",
            "--graduate",
            "3",
            "--json",
        ])
        .arg("--history")
        .arg(history)
        .stdin(Stdio::null())
        .output();
    let Ok(out) = out else { return Vec::new() };
    let report: Value = serde_json::from_slice(&out.stdout).unwrap_or(Value::Null);
    let ids: Vec<String> = report["graduation_eligible"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|v| v.as_str().map(str::to_string))
        .collect();
    let mine: Vec<String> = leg_tasks(repo).into_iter().map(|t| t.id).collect();
    ids.into_iter().filter(|id| mine.contains(id)).collect()
}

/// The last three cohorts a task passed in, for the PR body.
fn last_cohorts(rows: &[Value], task: &str) -> Vec<String> {
    let mut seen: Vec<String> = rows
        .iter()
        .filter(|r| r.get("task_id").and_then(Value::as_str) == Some(task))
        .map(|r| {
            let c = row_cohort(r);
            if c.is_empty() {
                row_ts(r)
                    .map(|t| t.format("%Y-%m-%d").to_string())
                    .unwrap_or_default()
            } else {
                c.to_string()
            }
        })
        .collect();
    let keep = seen.len().saturating_sub(3);
    seen.drain(..keep);
    seen
}

/// One PR retags every eligible task. None when nothing is eligible.
pub(crate) fn graduation_pr(
    history: &Path,
    repo: &Path,
    day: DateTime<Utc>,
) -> Result<Option<String>, String> {
    let ids = eligible(history, repo);
    open_graduation(history, repo, day, &ids)
}

pub(crate) fn open_graduation(
    history: &Path,
    repo: &Path,
    day: DateTime<Utc>,
    ids: &[String],
) -> Result<Option<String>, String> {
    if ids.is_empty() {
        return Ok(None);
    }
    let tasks = leg_tasks(repo);
    let rows = crate::evals_gate::history_rows(history);
    let mut edits: Vec<(PathBuf, String)> = Vec::new();
    let mut body = String::from("These capability tasks passed their last three full runs. This PR retags each to `tier: regression`.\n\n");
    for id in ids {
        let Some(task) = tasks.iter().find(|t| &t.id == id) else {
            continue;
        };
        let rel = task
            .source_path
            .strip_prefix(repo)
            .map_err(|_| format!("{id}: bank file is outside {}", repo.display()))?
            .to_path_buf();
        let text = std::fs::read_to_string(&task.source_path).map_err(|e| format!("{id}: {e}"))?;
        edits.push((rel, crate::evals::graduate::graduate_text(&text)?));
        body.push_str(&format!(
            "- `{id}`: {}\n",
            last_cohorts(&rows, id).join(", ")
        ));
    }
    if edits.is_empty() {
        return Ok(None);
    }
    let bytes: Vec<(PathBuf, Vec<u8>)> = edits
        .into_iter()
        .map(|(rel, text)| (rel, text.into_bytes()))
        .collect();
    let branch = format!("evals/graduate-{}", day.format("%Y-%m-%d"));
    let title = format!("Graduate {} capability eval(s) to regression", bytes.len());
    crate::bank_pr::open(repo, &branch, &title, &body, &bytes).map(Some)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(cohort: &str, ts: &str) -> Value {
        json!({"experiment_id": cohort, "ts": ts, "task_id": "t"})
    }

    fn at(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    #[test]
    fn due_when_no_leg_row_in_the_window_and_budget_left() {
        let now = at("2026-11-10T12:00:00Z");
        let rows = vec![row("gate-capability-2026-10-30", "2026-10-30T12:00:00Z")];
        assert_eq!(decide(&rows, now, 7, 21, 15.0), Verdict::Launch);
    }

    #[test]
    fn a_golden_main_row_does_not_count_as_the_leg() {
        let now = at("2026-11-10T12:00:00Z");
        let rows = vec![row("gate-main-abc", "2026-11-10T01:00:00Z")];
        assert_eq!(decide(&rows, now, 7, 21, 15.0), Verdict::Launch);
    }

    #[test]
    fn a_recent_leg_row_is_fresh() {
        let now = at("2026-11-10T12:00:00Z");
        let rows = vec![row("gate-capability-2026-11-07", "2026-11-07T12:00:00Z")];
        assert_eq!(decide(&rows, now, 7, 21, 15.0), Verdict::Fresh);
    }

    #[test]
    fn spend_plus_estimate_over_the_cap_reads_budget() {
        let now = at("2026-11-10T12:00:00Z");
        // 21 tasks x 3 x 0.04 = 2.52 USD; 14 gate rows today = 0.56 USD.
        let rows: Vec<Value> = (0..14)
            .map(|_| row("gate-main-x", "2026-11-10T01:00:00Z"))
            .collect();
        assert_eq!(decide(&rows, now, 7, 21, 3.0), Verdict::Budget);
        assert_eq!(decide(&rows, now, 7, 21, 15.0), Verdict::Launch);
    }

    #[test]
    fn last_cohorts_keeps_the_final_three() {
        let rows: Vec<Value> = ["a", "b", "c", "d"]
            .iter()
            .map(|c| row(c, "2026-11-01T00:00:00Z"))
            .collect();
        assert_eq!(last_cohorts(&rows, "t"), vec!["b", "c", "d"]);
    }

    #[test]
    fn no_eligible_task_opens_no_pr() {
        let dir = tempfile::tempdir().unwrap();
        let out = open_graduation(&dir.path().join("h.jsonl"), dir.path(), Utc::now(), &[]);
        assert_eq!(out, Ok(None));
    }

    #[test]
    fn tick_skips_while_fresh_and_never_spawns() {
        let dir = tempfile::tempdir().unwrap();
        let history = dir.path().join("h.jsonl");
        let now = Utc::now();
        let line = row(&cohort_for(now), &now.to_rfc3339()).to_string();
        std::fs::write(&history, format!("{line}\n")).unwrap();
        let bank = dir.path().join("evals").join("bank");
        std::fs::create_dir_all(&bank).unwrap();
        std::fs::write(
            bank.join("c.yaml"),
            "id: c\ntier: capability\nprompt: p\ngrade:\n  - kind: file-exists\n    path: x\n",
        )
        .unwrap();
        let claims = dir.path().join("claims");
        let leg = Leg {
            history: &history,
            repo: dir.path(),
            window_days: 7,
            run_timeout_s: 60,
            claims_root: Some(&claims),
        };
        let spawn = |_: &[String]| -> Result<u32, String> { panic!("must not spawn") };
        assert_eq!(tick(&leg, now, true, &[], &spawn), "fresh");
    }
}
