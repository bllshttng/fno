//! The ci_cadence arm: after `fno do pr push` lands a commit, the daemon
//! reads that PR's CI at 1, 15, 30 and 55 minutes, so no worker has to
//! remember to look.
//!
//! The push writes one `pr_push_landed` row. Each tick folds the recent rows,
//! keeps the newest push per branch, and reads the cached PR status at the
//! largest offset not yet journaled. A red whose every failing check sits on
//! heal's flake ledger is rerun once (or waited on while its runs still
//! test). Any other red is real: the head's in-flight runs are cancelled to
//! free the shared runners, and the pushing session is woken with the
//! failing checks and the log tail. A green, a newer head, a closed PR or a
//! delivered wake ends the series. Every read, cancel and wake rides an
//! existing reader: the status cache, `wait::cancel_in_flight`,
//! `pr_status::logs`, and burn-watch's mail/resume lane.

use std::collections::{BTreeMap, HashSet};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{json, Map, Value};

use crate::paths::AgentsHome;
use crate::pr_status_facts::{GhProbe, RealGhProbe};

const INTERVAL: Duration = Duration::from_secs(60);
const WINDOW_MS: i64 = 2 * 60 * 60 * 1000;
const MAX_EVENTS: u32 = 10_000;
const RERUN_TIMEOUT: Duration = Duration::from_secs(60);
const LOG_LINES: u64 = 30;
pub(crate) const OFFSETS_S: [i64; 4] = [60, 900, 1800, 3300];
pub(crate) const PUSH_EVENT: &str = "pr_push_landed";
pub(crate) const CHECK_EVENT: &str = "ci_cadence_check";
/// Outcomes after which nothing on that push needs reading again.
const FINAL: &[&str] = &["green", "superseded", "woke", "red_no_session"];

#[derive(Default)]
pub struct Arm {
    last_tick: Mutex<Option<Instant>>,
    in_flight: Arc<AtomicBool>,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Push {
    pub ts_ms: i64,
    pub branch: String,
    pub sha: String,
    pub cwd: String,
    pub session_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Check {
    pub branch: String,
    pub sha: String,
    pub offset_s: i64,
    pub outcome: String,
}

/// The checks due now: per branch, only the newest push, and for it the
/// largest elapsed offset not yet journaled. A daemon that was down reads
/// once at the latest offset instead of replaying the missed ones.
pub(crate) fn due(pushes: &[Push], checks: &[Check], now_ms: i64) -> Vec<(Push, i64)> {
    let mut newest: BTreeMap<(&str, &str), &Push> = BTreeMap::new();
    for push in pushes {
        let slot = newest
            .entry((push.cwd.as_str(), push.branch.as_str()))
            .or_insert(push);
        if push.ts_ms > slot.ts_ms {
            *slot = push;
        }
    }
    newest
        .into_values()
        .filter_map(|push| {
            let mine: Vec<&Check> = checks
                .iter()
                .filter(|c| c.sha == push.sha && c.branch == push.branch)
                .collect();
            if mine.iter().any(|c| FINAL.contains(&c.outcome.as_str())) {
                return None;
            }
            let elapsed_s = now_ms.saturating_sub(push.ts_ms) / 1000;
            let offset = OFFSETS_S.into_iter().filter(|o| *o <= elapsed_s).max()?;
            let done = mine.iter().map(|c| c.offset_s).max();
            done.map_or(true, |d| offset > d)
                .then(|| (push.clone(), offset))
        })
        .collect()
}

#[derive(Debug, PartialEq)]
pub(crate) enum Verdict {
    Pending,
    Green,
    Superseded,
    FlakeWait,
    FlakeRerun(Vec<u64>),
    Real,
}

/// One read's verdict. `failing` is (check name, run id) from the live rows
/// of the pushed head; `flakes` is the check names heal's ledger saw pass on
/// a rerun. A flake gets one rerun per head: a red after it is real.
pub(crate) fn decide(
    sha: &str,
    payload: &Value,
    failing: &[(String, Option<u64>)],
    in_flight: bool,
    flakes: &HashSet<String>,
    rerun_before: bool,
) -> Verdict {
    let head = payload.get("head").and_then(Value::as_str).unwrap_or("");
    let open = payload.get("pr_state").and_then(Value::as_str) == Some("OPEN");
    if !open || (!head.is_empty() && head != sha) {
        return Verdict::Superseded;
    }
    // A degraded serve cannot prove its verdict belongs to this head.
    if payload.get("stale_reason").is_some() {
        return Verdict::Pending;
    }
    if payload.get("green").and_then(Value::as_bool) == Some(true) {
        return Verdict::Green;
    }
    if payload.get("verdict").and_then(Value::as_str) != Some("red") || failing.is_empty() {
        return Verdict::Pending;
    }
    if rerun_before || !failing.iter().all(|(name, _)| flakes.contains(name)) {
        return Verdict::Real;
    }
    if in_flight {
        return Verdict::FlakeWait;
    }
    let mut runs: Vec<u64> = failing.iter().filter_map(|(_, run)| *run).collect();
    runs.sort_unstable();
    runs.dedup();
    if runs.is_empty() {
        Verdict::Real
    } else {
        Verdict::FlakeRerun(runs)
    }
}

pub(crate) fn wake_text(
    pr: u64,
    push: &Push,
    offset_s: i64,
    cancelled: &[String],
    logs: &str,
) -> String {
    let short: String = push.sha.chars().take(8).collect();
    format!(
        "Automatic CI notice from the fno daemon. PR {pr} ({}) is red at {short}, {} minutes after the push. {} Fix it, commit, and push with `fno do pr push --force-ci-cancel`.\n\n{}",
        push.branch,
        offset_s / 60,
        cancelled.join(" "),
        logs.trim_end()
    )
}

fn millis_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

fn ts_ms(row: &Value) -> Option<i64> {
    row.get("ts")
        .and_then(Value::as_str)
        .and_then(|stamp| chrono::DateTime::parse_from_rfc3339(stamp).ok())
        .map(|stamp| stamp.timestamp_millis())
}

fn read_rows(path: &Path, now_ms: i64) -> Result<(Vec<Push>, Vec<Check>), String> {
    let query = crate::event_store::EventQuery {
        since_ms: Some(now_ms.saturating_sub(WINDOW_MS)),
        limit: Some(MAX_EVENTS),
        ..crate::event_store::EventQuery::of_types(&[PUSH_EVENT, CHECK_EVENT])
    };
    let text = crate::event_store::journal_text_checked(path, &query)?;
    let (mut pushes, mut checks) = (Vec::new(), Vec::new());
    for line in text.lines().filter(|l| !l.trim().is_empty()) {
        let Ok(row) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let (Some(ts), Some(data)) = (ts_ms(&row), row.get("data")) else {
            continue;
        };
        let text = |key: &str| {
            data.get(key)
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string()
        };
        match row.get("type").and_then(Value::as_str) {
            Some(PUSH_EVENT) if ts >= now_ms.saturating_sub(WINDOW_MS) => pushes.push(Push {
                ts_ms: ts,
                branch: text("branch"),
                sha: text("sha"),
                cwd: text("cwd"),
                session_id: Some(text("session_id")).filter(|s| !s.is_empty()),
            }),
            Some(CHECK_EVENT) => checks.push(Check {
                branch: text("branch"),
                sha: text("sha"),
                offset_s: data.get("offset_s").and_then(Value::as_i64).unwrap_or(0),
                outcome: text("outcome"),
            }),
            _ => {}
        }
    }
    pushes.retain(|p| !p.sha.is_empty() && !p.branch.is_empty() && !p.cwd.is_empty());
    Ok((pushes, checks))
}

/// Check names heal's flake ledger saw fail and then pass on a rerun.
fn flake_checks(path: &Path) -> HashSet<String> {
    crate::event_store::journal_text(path, &["pr_heal_flake"])
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .filter_map(|row| {
            row.pointer("/data/check")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .collect()
}

/// The open PR whose head is `branch`; None when no open PR has it.
fn open_pr(cwd: &Path, slug: &str, branch: &str) -> Result<Option<u64>, String> {
    let owner = slug.split('/').next().unwrap_or(slug);
    let args = vec![
        "api".to_string(),
        format!(
            "repos/{slug}/pulls?state=open&head={owner}:{}&per_page=2",
            crate::claims::encode_key(branch)
        ),
    ];
    let (ok, stdout, _) = RealGhProbe.run_gh(cwd, &args)?;
    if !ok {
        return Err("pulls lookup failed".into());
    }
    let rows: Value =
        serde_json::from_str(&stdout).map_err(|e| format!("unparseable pulls list: {e}"))?;
    Ok(rows
        .as_array()
        .and_then(|rows| {
            rows.iter()
                .find(|pr| pr.pointer("/head/ref").and_then(Value::as_str) == Some(branch))
        })
        .and_then(|pr| pr.get("number"))
        .and_then(Value::as_u64))
}

/// Read one push at one offset and act. Returns the outcome word and the
/// fields its journal row carries.
fn check_one(
    push: &Push,
    offset_s: i64,
    checks: &[Check],
    flakes: &HashSet<String>,
    runner: crate::burn_watch::Runner<'_>,
) -> (&'static str, Map<String, Value>) {
    let mut fields = Map::new();
    let cwd = Path::new(&push.cwd);
    if !cwd.is_dir() {
        fields.insert("reason".into(), json!("checkout gone"));
        return ("superseded", fields);
    }
    let Some(slug) = crate::pr_status::cache::git_slug(cwd) else {
        fields.insert("reason".into(), json!("no repo slug"));
        return ("unreadable", fields);
    };
    let pr = match open_pr(cwd, &slug, &push.branch) {
        Ok(Some(pr)) => pr,
        Ok(None) => {
            fields.insert("reason".into(), json!("no open PR"));
            return ("superseded", fields);
        }
        Err(e) => {
            fields.insert("reason".into(), json!(e));
            return ("unreadable", fields);
        }
    };
    fields.insert("pr".into(), json!(pr));
    let (head, _) = crate::pr_status::cache::head_read(cwd, pr);
    let block = head.unwrap_or_else(|| json!({"head_sha": ""}));
    let (_, payload, _, _) =
        crate::pr_status::cache::cached_status(&push.cwd, pr, false, Some(&block));
    let (mut failing, mut in_flight) = (Vec::new(), false);
    if payload.get("verdict").and_then(Value::as_str) == Some("red") {
        match crate::pr_push::read_checks_rows("gh", cwd, &push.sha) {
            Ok(rows) => {
                let latest = crate::check_supersession::latest_per_name(&Value::Array(rows));
                for row in latest.as_array().into_iter().flatten() {
                    let link = row.get("link").and_then(Value::as_str).unwrap_or("");
                    match row.get("bucket").and_then(Value::as_str).unwrap_or("") {
                        "fail" => failing.push((
                            row.get("name")
                                .and_then(Value::as_str)
                                .unwrap_or("")
                                .to_string(),
                            crate::pr_push::run_id(link),
                        )),
                        "pass" | "skipping" | "cancel" => {}
                        _ => in_flight = true,
                    }
                }
            }
            Err(e) => {
                fields.insert("reason".into(), json!(e));
                return ("unreadable", fields);
            }
        }
    }
    let rerun_before = checks
        .iter()
        .any(|c| c.sha == push.sha && c.branch == push.branch && c.outcome == "flake_rerun");
    let names: Vec<&str> = failing.iter().map(|(n, _)| n.as_str()).collect();
    if !names.is_empty() {
        fields.insert("failing".into(), json!(names));
    }
    match decide(
        &push.sha,
        &payload,
        &failing,
        in_flight,
        flakes,
        rerun_before,
    ) {
        Verdict::Pending => ("pending", fields),
        Verdict::Green => ("green", fields),
        Verdict::Superseded => ("superseded", fields),
        Verdict::FlakeWait => ("flake_wait", fields),
        Verdict::FlakeRerun(runs) => {
            for run in &runs {
                let id = run.to_string();
                if let Err(e) = crate::pr_push::run_labeled(
                    "ci-cadence",
                    "gh",
                    &["run", "rerun", &id, "--failed"],
                    cwd,
                    RERUN_TIMEOUT,
                ) {
                    eprintln!("ci-cadence: rerun of run {id} failed: {e}");
                }
            }
            fields.insert("runs".into(), json!(runs));
            ("flake_rerun", fields)
        }
        Verdict::Real => {
            let Some(cancelled) = crate::pr_status::wait::cancel_in_flight("gh", cwd, &push.sha)
            else {
                return ("stale_red", fields);
            };
            fields.insert("cancelled".into(), json!(cancelled));
            let Some(sid) = push.session_id.as_deref() else {
                return ("red_no_session", fields);
            };
            let (_, logs, _) = crate::pr_status::logs::run_logs_door(
                &json!({"cwd": push.cwd, "pr": pr, "lines": LOG_LINES}),
            );
            let text = wake_text(pr, push, offset_s, &cancelled, &logs);
            let (delivered, via) =
                crate::burn_watch::wake_with_text(sid, false, &text, "ci-cadence", runner);
            fields.insert("session_id".into(), json!(sid));
            fields.insert("delivered".into(), json!(delivered));
            fields.insert("via".into(), json!(via));
            if delivered || via == "durable" {
                ("woke", fields)
            } else {
                ("wake_failed", fields)
            }
        }
    }
}

pub(crate) fn run_pass(home: &AgentsHome) -> Result<(), String> {
    let now_ms = millis_now();
    let path = crate::daemon::global_events_path(home);
    let (pushes, checks) = read_rows(&path, now_ms)?;
    let due = due(&pushes, &checks, now_ms);
    if due.is_empty() {
        return Ok(());
    }
    let flakes = flake_checks(&path);
    let emitter = crate::events::EventEmitter::new(path, "daemon");
    let mut runner = crate::burn_watch::run_command;
    for (push, offset_s) in due {
        let (outcome, mut fields) = check_one(&push, offset_s, &checks, &flakes, &mut runner);
        fields.insert("branch".into(), json!(push.branch));
        fields.insert("sha".into(), json!(push.sha));
        fields.insert("offset_s".into(), json!(offset_s));
        fields.insert("outcome".into(), json!(outcome));
        emitter
            .emit_fields(CHECK_EVENT, fields)
            .map_err(|e| format!("ci_cadence_check row did not land: {e}"))?;
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
            eprintln!("ci-cadence: {error}");
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    const SHA: &str = "a1b2c3d4e5f60718293a4b5c6d7e8f9012345678";

    fn push(ts_ms: i64, sha: &str) -> Push {
        Push {
            ts_ms,
            branch: "feature/x".into(),
            sha: sha.into(),
            cwd: "/w".into(),
            session_id: Some("s1".into()),
        }
    }

    fn check(sha: &str, offset_s: i64, outcome: &str) -> Check {
        Check {
            branch: "feature/x".into(),
            sha: sha.into(),
            offset_s,
            outcome: outcome.into(),
        }
    }

    #[test]
    fn a_push_reads_once_at_the_largest_elapsed_offset_and_a_final_outcome_ends_it() {
        let now = 16 * 60 * 1000;
        let p = push(0, SHA);
        assert_eq!(due(&[p.clone()], &[], now), vec![(p.clone(), 900)]);
        // Read at 15m: nothing more until 30m.
        let read = [check(SHA, 900, "pending")];
        assert!(due(&[p.clone()], &read, now).is_empty());
        assert_eq!(
            due(&[p.clone()], &read, 31 * 60 * 1000),
            vec![(p.clone(), 1800)]
        );
        // A wake, a green or a newer head ends the series.
        for outcome in ["woke", "green", "superseded", "red_no_session"] {
            assert!(due(&[p.clone()], &[check(SHA, 60, outcome)], 31 * 60 * 1000).is_empty());
        }
        // Under a minute nothing is due.
        assert!(due(&[p], &[], 59_000).is_empty());
    }

    #[test]
    fn only_the_newest_push_on_a_branch_is_read() {
        let old = push(0, "0000000000000000000000000000000000000000");
        let new = push(5 * 60 * 1000, SHA);
        let got = due(&[old, new.clone()], &[], 6 * 60 * 1000);
        assert_eq!(got, vec![(new, 60)]);
    }

    fn red() -> Value {
        json!({"head": SHA, "pr_state": "OPEN", "verdict": "red", "green": false})
    }

    #[test]
    fn a_red_on_known_flakes_waits_or_reruns_once_and_any_other_red_is_real() {
        let flakes: HashSet<String> = ["stress".to_string()].into();
        let flaky = vec![("stress".to_string(), Some(7))];
        assert_eq!(
            decide(SHA, &red(), &flaky, true, &flakes, false),
            Verdict::FlakeWait
        );
        assert_eq!(
            decide(SHA, &red(), &flaky, false, &flakes, false),
            Verdict::FlakeRerun(vec![7])
        );
        // The rerun was spent on this head: the same red is now real.
        assert_eq!(
            decide(SHA, &red(), &flaky, false, &flakes, true),
            Verdict::Real
        );
        // One failing check off the ledger makes the whole red real, in flight or not.
        let mixed = vec![flaky[0].clone(), ("cargo-test".to_string(), Some(8))];
        assert_eq!(
            decide(SHA, &red(), &mixed, true, &flakes, false),
            Verdict::Real
        );
    }

    #[test]
    fn a_new_head_a_closed_pr_or_a_degraded_serve_never_cancels() {
        let failing = vec![("cargo-test".to_string(), Some(8))];
        let none = HashSet::new();
        let moved = json!({"head": "ffff", "pr_state": "OPEN", "verdict": "red"});
        assert_eq!(
            decide(SHA, &moved, &failing, true, &none, false),
            Verdict::Superseded
        );
        let merged = json!({"head": SHA, "pr_state": "MERGED", "verdict": "green"});
        assert_eq!(
            decide(SHA, &merged, &[], false, &none, false),
            Verdict::Superseded
        );
        let mut stale = red();
        stale["stale_reason"] = json!("backoff");
        assert_eq!(
            decide(SHA, &stale, &failing, true, &none, false),
            Verdict::Pending
        );
        // A red the live rows no longer show is not acted on.
        assert_eq!(
            decide(SHA, &red(), &[], true, &none, false),
            Verdict::Pending
        );
        let green = json!({"head": SHA, "pr_state": "OPEN", "verdict": "green", "green": true});
        assert_eq!(
            decide(SHA, &green, &[], false, &none, false),
            Verdict::Green
        );
    }
}
