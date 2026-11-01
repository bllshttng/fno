//! `evals-trend`: the eval-history report fold and the time-window
//! trend, native under d-b6cc1a2a (all new code lands in crates; Python is a
//! thin caller). One transport-only action behind two Python leaves:
//! `fno doctor evals report` forwards `--mode report`, `fno doctor evals
//! trend` forwards `--mode trend`, and `evals_health_summary` reads the
//! windowed alarm and `regressed` through `--mode summary`.
//!
//! Semantics are the Python fold they replace (`cli/src/fno/evals/report.py`
//! at this PR's base): tier-segment stats, half-open windows, graduation
//! candidates. The per-task compare of two runs lives in `evals_compare`.
use std::collections::BTreeMap;
use std::path::PathBuf;

use chrono::DateTime;
use chrono::Utc;
use serde_json::json;
use serde_json::Map;
use serde_json::Value;

const EXIT_USAGE: i32 = 2;
const USAGE: &str = "usage: fno-agents evals-trend --history <jsonl> (--mode report [--since K] [--graduate N] [--json] [--compare A --to B [--repeats N] [--bank DIR]] [--by-failure-type [--repeats N] [--bank DIR]] [--check-metrics [--bank DIR]] [--planned <json>] [--qualification <manifest.json>] [--by-cohort [--prices <json>]] [--climb --baseline C --trial C [--cohorts-yaml P] [--stage V --trial-lane L]] | --mode trend | --mode summary) [--stale-days N] [--now <rfc3339>]";

/// One history row: the fields the folds read. Absent keys read as the
/// Python fold read them (missing `variant` is baseline, missing `ts` is
/// unparseable, missing `tier` is "unknown"). `raw` is kept so the attempt
/// verdict comes from the ONE native classifier, never a second fold.
#[derive(Clone)]
pub(crate) struct Row {
    pub(crate) task_id: String,
    tier: String,
    pub(crate) pass: bool,
    ts: Option<DateTime<Utc>>,
    pub(crate) variant: Option<String>,
    pub(crate) bank_rev: Option<String>,
    pub(crate) experiment_id: Option<String>,
    raw: Value,
}

impl Row {
    /// The content id of the tree this attempt left, when the row recorded it.
    pub(crate) fn tree_hash(&self) -> Option<&str> {
        self.raw.get("tree_hash").and_then(Value::as_str)
    }
}

pub(crate) fn read_rows(history: &str, variant: Option<&str>, since: Option<usize>) -> Vec<Row> {
    let text = std::fs::read_to_string(history).unwrap_or_default();
    let mut rows: Vec<Row> = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let row_variant = v
            .get("variant")
            .and_then(Value::as_str)
            .map(str::to_string)
            .unwrap_or_else(|| "baseline".into());
        if let Some(want) = variant {
            if row_variant != want {
                continue;
            }
        }
        rows.push(Row {
            task_id: v
                .get("task_id")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            tier: v
                .get("tier")
                .and_then(Value::as_str)
                .unwrap_or("unknown")
                .to_string(),
            pass: v.get("pass") == Some(&Value::Bool(true)),
            ts: v
                .get("ts")
                .and_then(Value::as_str)
                .and_then(|ts| DateTime::parse_from_rfc3339(ts).ok())
                .map(|dt| dt.with_timezone(&Utc)),
            variant: v.get("variant").and_then(Value::as_str).map(str::to_string),
            bank_rev: v
                .get("bank_rev")
                .and_then(Value::as_str)
                .map(str::to_string),
            experiment_id: v
                .get("experiment_id")
                .and_then(Value::as_str)
                .map(str::to_string),
            raw: v,
        });
    }
    if let Some(k) = since {
        let at = rows.len().saturating_sub(k);
        rows.drain(..at);
    }
    rows
}

/// Per-task stats over one row list: attempts/passes since the task's latest
/// tier change (the anti-false-alarm segment rule).
///
/// Two denominators: a task with ANY natively classified row reports
/// `grades` (valid task grades) and correctness over those grades only -
/// infrastructure/unavailable/ungraded attempts never drag the pass rate. A
/// task whose rows are ALL legacy (pre-attempt history) keeps the old
/// boolean fold: legacy evidence is never reinterpreted, and existing
/// history keeps its alarm semantics. `runs` is the attempt count either way.
struct TaskStat {
    task_id: String,
    tier: String,
    runs: usize,
    passes: usize,
    grades: usize,
    infrastructure: usize,
    unavailable: usize,
    ungraded: usize,
    legacy: usize,
    legacy_fold: bool,
}

impl TaskStat {
    /// Correctness denominator: valid grades for a modern task, attempts for
    /// an all-legacy task.
    fn grade_denominator(&self) -> usize {
        if self.legacy_fold {
            self.runs
        } else {
            self.grades
        }
    }
    fn pass_at_1(&self) -> f64 {
        let den = self.grade_denominator();
        if den == 0 {
            0.0
        } else {
            self.passes as f64 / den as f64
        }
    }
    fn pass_k(&self) -> bool {
        let den = self.grade_denominator();
        den > 0 && self.passes == den
    }
    /// A real grade below 100%. A window holding only plumbing attempts graded
    /// nothing, so it says nothing about the task.
    fn alarms(&self) -> bool {
        self.grade_denominator() > 0 && self.pass_at_1() < 1.0
    }
    fn flake(&self) -> bool {
        let den = self.grade_denominator();
        self.passes > 0 && self.passes < den
    }
}

fn stats(rows: &[Row]) -> Vec<TaskStat> {
    let mut by_task: BTreeMap<String, Vec<&Row>> = BTreeMap::new();
    for r in rows {
        by_task.entry(r.task_id.clone()).or_default().push(r);
    }
    let mut out = Vec::new();
    for (tid, task_rows) in by_task {
        let current_tier = task_rows
            .last()
            .map(|r| r.tier.clone())
            .unwrap_or_else(|| "unknown".into());
        let mut segment_rows: Vec<&Row> = Vec::new();
        for r in task_rows.iter().rev() {
            if r.tier != current_tier {
                break;
            }
            segment_rows.push(r);
        }
        let mut stat = TaskStat {
            task_id: tid,
            tier: current_tier,
            runs: segment_rows.len(),
            passes: 0,
            grades: 0,
            infrastructure: 0,
            unavailable: 0,
            ungraded: 0,
            legacy: 0,
            legacy_fold: false,
        };
        for r in &segment_rows {
            match crate::eval_attempt::classify(&r.raw, None).status {
                "graded" => {
                    stat.grades += 1;
                    if r.pass {
                        stat.passes += 1;
                    }
                }
                "infrastructure" => stat.infrastructure += 1,
                "unavailable" => stat.unavailable += 1,
                "ungraded" => stat.ungraded += 1,
                _ => stat.legacy += 1,
            }
        }
        // All-legacy segment: keep the pre-attempt boolean fold verbatim.
        stat.legacy_fold =
            stat.grades + stat.infrastructure + stat.unavailable + stat.ungraded == 0;
        if stat.legacy_fold {
            stat.passes = segment_rows.iter().filter(|r| r.pass).count();
        }
        out.push(stat);
    }
    out
}

/// `stats` over the rows whose ts lands in `(start, end]`.
fn stats_window(rows: &[Row], start: DateTime<Utc>, end: DateTime<Utc>) -> Vec<TaskStat> {
    let kept: Vec<Row> = rows
        .iter()
        .filter(|r| match r.ts {
            Some(dt) => start < dt && dt <= end,
            None => false,
        })
        .cloned()
        .collect();
    stats(&kept)
}

/// One rounding rule for every fold that emits ratios (evals trend and
/// corrections verify), so two reports never disagree by display precision.
pub(crate) fn round4(x: f64) -> f64 {
    (x * 10000.0).round() / 10000.0
}

fn window_rows<'a>(rows: &'a [Row], start: DateTime<Utc>, end: DateTime<Utc>) -> Vec<&'a Row> {
    rows.iter()
        .filter(|r| match r.ts {
            Some(dt) => start < dt && dt <= end,
            None => false,
        })
        .collect()
}

/// Per-task verdict of one row list against another: runs, pass@1 per side,
/// delta (b - a) and the improved/regressed/unchanged verdict.
fn pair_verdict(a: &[&Row], b: &[&Row]) -> (usize, f64, usize, f64, f64, &'static str) {
    let a_p1 = a.iter().filter(|r| r.pass).count() as f64 / a.len() as f64;
    let b_p1 = b.iter().filter(|r| r.pass).count() as f64 / b.len() as f64;
    let delta = b_p1 - a_p1;
    let verdict = if delta > 0.0 {
        "improved"
    } else if delta < 0.0 {
        "regressed"
    } else {
        "unchanged"
    };
    (a.len(), a_p1, b.len(), b_p1, delta, verdict)
}

/// The trend fold: recent `(now - W, now]` against prior `(now - 2W, now - W]`,
/// plus `regressed` (regression-tier tasks whose verdict is regressed) and the
/// recent-window regression alarm.
fn trend_fold(rows: &[Row], window_days: i64, now: DateTime<Utc>) -> (Value, Vec<String>) {
    let w = chrono::Duration::days(window_days);
    // A plumbing attempt carries pass=false with no grade behind it: left in,
    // an outage reads as a regression against the prior window.
    let graded: Vec<Row> = rows
        .iter()
        .filter(|r| {
            !matches!(
                crate::eval_attempt::classify(&r.raw, None).status,
                "infrastructure" | "unavailable" | "ungraded"
            )
        })
        .cloned()
        .collect();
    let rows = graded.as_slice();
    let prior: BTreeMap<String, Vec<&Row>> = group_by_task(window_rows(rows, now - w - w, now - w));
    let recent: BTreeMap<String, Vec<&Row>> = group_by_task(window_rows(rows, now - w, now));
    let mut tasks = Map::new();
    let mut missing_in_prior = Vec::new();
    let mut missing_in_recent = Vec::new();
    let mut ids: Vec<&String> = prior.keys().chain(recent.keys()).collect();
    ids.sort();
    ids.dedup();
    for tid in ids {
        let p = prior.get(tid).cloned().unwrap_or_default();
        let r = recent.get(tid).cloned().unwrap_or_default();
        if p.is_empty() {
            missing_in_prior.push(tid.clone());
        }
        if r.is_empty() {
            missing_in_recent.push(tid.clone());
        }
        if p.is_empty() || r.is_empty() {
            continue;
        }
        let (pr, pp1, rr, rp1, delta, verdict) = pair_verdict(&p, &r);
        tasks.insert(
            tid.clone(),
            json!({
                "prior": {"runs": pr, "pass_at_1": round4(pp1)},
                "recent": {"runs": rr, "pass_at_1": round4(rp1)},
                "delta": round4(delta),
                "verdict": verdict,
            }),
        );
    }
    let newest_tier: BTreeMap<String, String> = group_by_task(rows.iter().collect())
        .into_iter()
        .map(|(tid, trs)| {
            let tier = trs
                .last()
                .map(|r| r.tier.clone())
                .unwrap_or_else(|| "unknown".into());
            (tid, tier)
        })
        .collect();
    let regressed: Vec<String> = tasks
        .iter()
        .filter(|(tid, t)| {
            t["verdict"] == "regressed"
                && newest_tier.get(*tid).map(String::as_str) == Some("regression")
        })
        .map(|(tid, _)| tid.clone())
        .collect();
    let recent_alarm: Vec<String> = stats_window(rows, now - w, now)
        .into_iter()
        .filter(|s| s.tier == "regression" && s.alarms())
        .map(|s| s.task_id)
        .collect();
    let view = json!({
        "window_days": window_days,
        "prior_start": (now - w - w).to_rfc3339(),
        "recent_start": (now - w).to_rfc3339(),
        "tasks": tasks,
        "missing_in_prior": missing_in_prior,
        "missing_in_recent": missing_in_recent,
        "regressed": regressed,
        "regression_alarm": recent_alarm,
    });
    (view, regressed)
}

fn group_by_task<'a>(rows: Vec<&'a Row>) -> BTreeMap<String, Vec<&'a Row>> {
    let mut out: BTreeMap<String, Vec<&Row>> = BTreeMap::new();
    for r in rows {
        out.entry(r.task_id.clone()).or_default().push(r);
    }
    out
}

/// The report fold: tiers, tasks, flakes, the windowed alarm, graduation.
/// *planned* (task_id -> expected attempts) projects the planned denominator:
/// missing attempts stay visible and completion is reported beside correctness.
fn report_fold(
    rows: &[Row],
    window_days: i64,
    now: DateTime<Utc>,
    graduate_n: Option<usize>,
    planned: Option<&BTreeMap<String, usize>>,
) -> Value {
    let all = stats(rows);
    let mut tiers: BTreeMap<String, (usize, usize)> = BTreeMap::new();
    for s in &all {
        let e = tiers.entry(s.tier.clone()).or_insert((0, 0));
        e.0 += s.runs;
        e.1 += s.passes;
        // The tier rate sums each task's OWN denominator (grades for a modern
        // task, attempts for an all-legacy task) so the aggregate never mixes
        // infrastructure evidence into correctness.
    }
    let tier_json = Map::from_iter(tiers.iter().map(|(tier, (runs, passes))| {
        let den: usize = all
            .iter()
            .filter(|s| s.tier == *tier)
            .map(|s| s.grade_denominator())
            .sum();
        let rate = if den == 0 {
            0.0
        } else {
            *passes as f64 / den as f64
        };
        let plumbing: usize = all
            .iter()
            .filter(|s| s.tier == *tier)
            .map(|s| s.infrastructure + s.unavailable + s.ungraded)
            .sum();
        (
            tier.clone(),
            json!({
                "runs": runs, "passes": passes, "grade_den": den,
                "plumbing": plumbing, "pass_rate": round4(rate),
            }),
        )
    }));
    let tasks: Vec<Value> = all
        .iter()
        .map(|s| {
            let mut task = json!({
                "task_id": s.task_id, "tier": s.tier, "runs": s.runs,
                "passes": s.passes, "pass_at_1": round4(s.pass_at_1()),
                "pass_k": s.pass_k(), "flake": s.flake(),
                "grades": s.grades,
                "grade_den": s.grade_denominator(),
                "attempts": {
                    "infrastructure": s.infrastructure,
                    "unavailable": s.unavailable,
                    "ungraded": s.ungraded,
                    "legacy": s.legacy,
                },
                "legacy_fold": s.legacy_fold,
            });
            if let Some(plan) = planned {
                let expected = plan.get(&s.task_id).copied().unwrap_or(0);
                let missing = expected.saturating_sub(s.runs);
                let completion = if expected == 0 {
                    0.0
                } else {
                    s.runs as f64 / expected as f64
                };
                task["expected_attempts"] = json!(expected);
                task["missing_attempts"] = json!(missing);
                task["completion"] = json!(round4(completion));
            }
            task
        })
        .collect();
    let flakes: Vec<&str> = all
        .iter()
        .filter(|s| s.flake())
        .map(|s| s.task_id.as_str())
        .collect();
    let w = chrono::Duration::days(window_days);
    let alarm: Vec<String> = stats_window(rows, now - w, now)
        .into_iter()
        .filter(|s| s.tier == "regression" && s.alarms())
        .map(|s| s.task_id)
        .collect();
    let mut report = json!({
        "no_data": all.is_empty(),
        "tiers": tier_json,
        "tasks": tasks,
        "flakes": flakes,
        "regression_alarm": alarm,
    });
    if let Some(n) = graduate_n {
        let candidates: Vec<String> = group_by_task(rows.iter().collect())
            .into_iter()
            .filter(|(_, trs)| {
                trs.last().map(|r| r.tier.as_str()) == Some("capability")
                    && trs.len() >= n
                    && trs[trs.len() - n..].iter().all(|r| r.pass)
            })
            .map(|(tid, _)| tid)
            .collect();
        report["graduation_eligible"] =
            Value::Array(candidates.into_iter().map(Value::String).collect());
    }
    report
}

/// A valid grade the lane evidence does not exclude. Everything else is
/// counted by reason and never scored.
pub(crate) fn is_scored(r: &Row) -> bool {
    r.raw.get("status").and_then(Value::as_str) == Some("graded")
        && r.raw.get("lane_status").and_then(Value::as_str) != Some("substituted")
        && r.raw
            .get("excluded_reason")
            .and_then(Value::as_str)
            .is_none()
}

/// The per-cohort fold behind `--mode report --by-cohort`: one row list per
/// `experiment_id`, each folded into the harness-fit comparison projection.
/// Scored rows are natively `graded` rows that the lane evidence does not
/// exclude (not `substituted`, no `excluded_reason`); everything else is
/// counted by reason and never scored. Cost is priced only when every scored
/// row carries usage and a complete price line (input and output rates, plus
/// a cache rate for any cache tokens the row carries): `unmeasured`
/// otherwise, never a partial sum (AC3-EDGE).
fn cohort_fold(rows: &[Row], prices: Option<&Value>) -> Value {
    let mut cohorts: BTreeMap<String, Vec<&Row>> = BTreeMap::new();
    for r in rows {
        let id = r
            .experiment_id
            .clone()
            .unwrap_or_else(|| "(no cohort)".into());
        cohorts.entry(id).or_default().push(r);
    }
    let mut out = Map::new();
    for (id, cohort_rows) in &cohorts {
        out.insert(id.clone(), cohort_score(cohort_rows, prices));
    }
    json!({ "cohorts": Value::Object(out) })
}

pub(crate) fn cohort_score(rows: &[&Row], prices: Option<&Value>) -> Value {
    let mut statuses: BTreeMap<String, usize> = BTreeMap::new();
    let mut lane_statuses: BTreeMap<String, usize> = BTreeMap::new();
    let mut by_reason: BTreeMap<String, usize> = BTreeMap::new();
    let mut scored: Vec<&Row> = Vec::new();
    let mut excluded = 0usize;
    let mut usage_missing = 0usize;
    for r in rows {
        let status = r
            .raw
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or("legacy");
        *statuses.entry(status.to_string()).or_default() += 1;
        let lane_status = r
            .raw
            .get("lane_status")
            .and_then(Value::as_str)
            .unwrap_or("unverified");
        *lane_statuses.entry(lane_status.to_string()).or_default() += 1;
        let reason = r.raw.get("excluded_reason").and_then(Value::as_str);
        if is_scored(r) {
            scored.push(r);
        } else {
            excluded += 1;
            let why = match reason {
                Some(reason) => reason,
                None if lane_status == "substituted" => "substituted",
                None => status,
            };
            *by_reason.entry(why.to_string()).or_default() += 1;
        }
    }
    let accepted = scored.iter().filter(|r| r.pass).count();
    let (rate, lo, hi) = wilson_rate(scored.len(), accepted);
    let mut durations: Vec<f64> = scored
        .iter()
        .filter_map(|r| r.raw.get("duration_s").and_then(Value::as_f64))
        .collect();
    durations.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let median_wall_s = match durations.len() {
        0 => 0.0,
        odd if odd % 2 == 1 => durations[odd / 2],
        even => (durations[even / 2 - 1] + durations[even / 2]) / 2.0,
    };
    let mut tokens = (0u64, 0u64, 0u64, 0u64);
    let mut cost_total = 0.0f64;
    let mut measured = !scored.is_empty();
    if scored.is_empty() {
        measured = false;
    }
    for r in &scored {
        let usage = r.raw.get("usage");
        let Some(u) = usage.and_then(Value::as_object) else {
            usage_missing += 1;
            continue;
        };
        let num = |k: &str| u.get(k).and_then(Value::as_u64);
        let (Some(input), Some(output)) = (num("input"), num("output")) else {
            usage_missing += 1;
            continue;
        };
        let cache_read = num("cache_read").unwrap_or(0);
        let cache_write = num("cache_write").unwrap_or(0);
        tokens = (
            tokens.0 + input,
            tokens.1 + output,
            tokens.2 + cache_read,
            tokens.3 + cache_write,
        );
        let observed = r.raw.get("observed_model").and_then(Value::as_str);
        let requested = r.raw.get("requested_model").and_then(Value::as_str);
        // opencode observes `provider/model`; a price table keyed by the bare
        // model still prices it.
        let bare = observed.and_then(|m| m.rsplit_once('/')).map(|(_, m)| m);
        let price = [observed, bare, requested]
            .into_iter()
            .flatten()
            .find_map(|key| prices.and_then(|p| p.get(key)).and_then(Value::as_object));
        let Some(price) = price else {
            measured = false;
            continue;
        };
        // A price line missing a rate the row's usage needs prices that token
        // class at $0 silently. Treat it like no price line at all: a cell
        // reads unmeasured rather than measured-cheap (a partial line once
        // let a typo'd key read `propose` on near-zero cost).
        let rate_of = |k: &str| price.get(k).and_then(Value::as_f64);
        let (Some(in_rate), Some(out_rate)) = (rate_of("input_per_m"), rate_of("output_per_m"))
        else {
            measured = false;
            continue;
        };
        let cache_rate = |k: &str, tokens_used: u64| -> Option<f64> {
            if tokens_used == 0 {
                return Some(0.0);
            }
            rate_of(k)
        };
        let (Some(read_rate), Some(write_rate)) = (
            cache_rate("cache_read_per_m", cache_read),
            cache_rate("cache_write_per_m", cache_write),
        ) else {
            measured = false;
            continue;
        };
        cost_total += (input as f64 * in_rate
            + output as f64 * out_rate
            + cache_read as f64 * read_rate
            + cache_write as f64 * write_rate)
            / 1_000_000.0;
    }
    if usage_missing > 0 {
        measured = false;
        tokens = (0, 0, 0, 0);
        cost_total = 0.0;
    }
    let dollars_per_accepted = if measured && accepted > 0 {
        Value::from(round4(cost_total / accepted as f64))
    } else {
        Value::Null
    };
    json!({
        "attempts": rows.len(),
        "graded": statuses.get("graded").copied().unwrap_or(0),
        "scored": scored.len(),
        "accepted": accepted,
        // A zero-scored cell has no measured rate: wilson(0, 0) emits a
        // bracketing [0, 0] that reads as a measured 0 percent to a human,
        // so the cell nulls the rate and the interval instead.
        "pass_rate": if scored.is_empty() {
            Value::Null
        } else {
            json!(round4(rate))
        },
        "pass_rate_ci95": if scored.is_empty() {
            Value::Null
        } else {
            json!([round4(lo), round4(hi)])
        },
        "median_wall_s": round4(median_wall_s),
        "lane_status_counts": lane_statuses,
        "status_counts": statuses,
        "excluded": {
            "total": excluded,
            "by_reason": by_reason,
            "usage_missing": usage_missing,
        },
        "tokens": {"input": tokens.0, "output": tokens.1,
                    "cache_read": tokens.2, "cache_write": tokens.3},
        "cost": {
            "measured": measured,
            "dollars_total": if measured { json!(round4(cost_total)) } else { Value::Null },
            "dollars_per_accepted": dollars_per_accepted,
        },
    })
}

/// The 95% Wilson score interval over the pass indicator (z = 1.96), the
/// closed form the harness-fit study scored with
/// (evals/harness-fit/run/score_run1.py). Same numbers on every run of the
/// same rows: a report is reproducible evidence, not a die roll. Closed form
/// behaves at small n, where lane cohorts sit; the bootstrap percentile it
/// replaces bracketed about [0.01, 0.99] at every n and could never
/// separate two cohorts.
pub(crate) fn wilson_rate(n: usize, passes: usize) -> (f64, f64, f64) {
    if n == 0 {
        return (0.0, 0.0, 0.0);
    }
    let z = 1.96_f64;
    let rate = passes as f64 / n as f64;
    let denom = 1.0 + z * z / n as f64;
    let center = (rate + z * z / (2.0 * n as f64)) / denom;
    let half =
        z * (rate * (1.0 - rate) / n as f64 + z * z / (4.0 * n as f64 * n as f64)).sqrt() / denom;
    (rate, (center - half).max(0.0), (center + half).min(1.0))
}

/// The summary document `evals_health_summary` reads: alarm, regressed set,
/// tier pass rate, flake count, and the staleness fold (newest
/// regression-tier timestamp vs the window). Staleness lives here so Python
/// carries the fields instead of re-folding them (d-b6cc1a2a).
pub(crate) fn summary_payload(history: &str, stale_days: i64, now: DateTime<Utc>) -> Value {
    let rows = read_rows(history, Some("baseline"), None);
    let (_, regressed) = trend_fold(&rows, stale_days, now);
    let w = chrono::Duration::days(stale_days);
    let recent_alarm: Vec<String> = stats_window(&rows, now - w, now)
        .into_iter()
        .filter(|s| s.tier == "regression" && s.alarms())
        .map(|s| s.task_id)
        .collect();
    let fold = report_fold(&rows, stale_days, now, None, None);
    let reg = fold.get("tiers").and_then(|t| t.get("regression"));
    let pass_rate = reg
        .and_then(|t| t.get("pass_rate"))
        .cloned()
        .unwrap_or(json!(null));
    let flake_count = fold
        .get("flakes")
        .and_then(Value::as_array)
        .map(|a| a.len())
        .unwrap_or(0);
    let reg_rows: Vec<&Row> = rows.iter().filter(|r| r.tier == "regression").collect();
    let never_ran = reg_rows.is_empty();
    let age_days =
        reg_rows.iter().filter_map(|r| r.ts).max().map(|newest| {
            ((now - newest).num_seconds() as f64 / 86_400.0 * 1000.0).round() / 1000.0
        });
    let stale = age_days.map_or(false, |age| age > stale_days as f64);
    json!({
        "regression_alarm": recent_alarm,
        "regressed": regressed,
        "regression_pass_rate": pass_rate,
        "flake_count": flake_count,
        "row_count": rows.len(),
        "never_ran": never_ran,
        "age_days": age_days,
        "stale": stale,
    })
}

fn print_summary(history: &str, stale_days: i64, now: DateTime<Utc>) -> i32 {
    println!(
        "{}",
        serde_json::to_string(&summary_payload(history, stale_days, now)).unwrap_or_default()
    );
    0
}

/// The health projection (the deleted Python leg's evals_health_summary
/// shape): the summary's fields under the health keys, or the literal null
/// when no history or no baseline rows exist. One denominator authority:
/// the callers never re-fold.
fn print_health(history: &str, stale_days: i64, now: DateTime<Utc>) -> i32 {
    let payload = summary_payload(history, stale_days, now);
    let row_count = payload
        .get("row_count")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    if row_count == 0 {
        println!("null");
        return 0;
    }
    let health = json!({
        "regression_pass_rate": payload.get("regression_pass_rate").cloned().unwrap_or(Value::Null),
        "flake_count": payload.get("flake_count").and_then(Value::as_i64).unwrap_or(0),
        "regression_alarm": payload.get("regression_alarm").cloned().unwrap_or(json!([])),
        "regressed": payload.get("regressed").cloned().unwrap_or(json!([])),
        "window_days": stale_days,
        "age_days": payload.get("age_days").cloned().unwrap_or(Value::Null),
        "stale": payload.get("stale").and_then(Value::as_bool).unwrap_or(false),
        "never_ran": payload.get("never_ran").and_then(Value::as_bool).unwrap_or(false),
    });
    println!("{}", serde_json::to_string(&health).unwrap_or_default());
    0
}

/// The qualification projection surface, shared by the `--qualification`
/// flag and the stdin `{"op": "qualification"}` payload. A missing history
/// file folds over zero rows: every scenario reports missing, which is the
/// honest answer, never an error.
fn qualification_report(
    history: &str,
    manifest_path: &str,
    json_out: bool,
    since: Option<usize>,
) -> i32 {
    let text = match std::fs::read_to_string(manifest_path) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("evals-trend: cannot read qualification manifest {manifest_path}: {e}");
            return EXIT_USAGE;
        }
    };
    let manifest: Value = match serde_json::from_str(&text) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("evals-trend: qualification manifest {manifest_path} is not valid JSON: {e}");
            return EXIT_USAGE;
        }
    };
    let raw_rows: Vec<Value> = if PathBuf::from(history).exists() {
        read_rows(history, Some("baseline"), since)
            .into_iter()
            .map(|r| r.raw)
            .collect()
    } else {
        Vec::new()
    };
    let projection = crate::evals_qualification::projection(&raw_rows, &manifest);
    if json_out {
        println!(
            "{}",
            serde_json::to_string_pretty(&projection).unwrap_or_default()
        );
    } else {
        print!("{}", crate::evals_qualification::render_text(&projection));
    }
    crate::evals_qualification::exit_code(&projection)
}

pub fn run_evals_trend(args: &[String]) -> i32 {
    // `--mode stability` rides this command in any argument position rather
    // than growing the over-cap fno-agents root; stability_report owns the
    // remaining flags.
    let mut mode_index: Option<usize> = None;
    let mut i = 0usize;
    while i < args.len() {
        if args[i] == "--mode" || args[i].starts_with("--mode=") {
            mode_index = Some(i);
            break;
        }
        i += 1;
    }
    if let Some(index) = mode_index {
        let inline = args[index].split_once('=').map(|(_, v)| v.to_string());
        let skip = if inline.is_some() { 1 } else { 2 };
        let value = match &inline {
            Some(v) => Some(v.as_str()),
            None => args.get(index + 1).map(String::as_str),
        };
        if value == Some("stability") {
            let mut rest: Vec<String> = Vec::new();
            rest.extend_from_slice(&args[..index]);
            rest.extend_from_slice(&args[index + skip..]);
            return crate::stability_report::run(&rest);
        }
    }
    if args.is_empty() {
        // stdin summary: a JSON payload on stdin (the command_call shape),
        // `{"op": "summary", "history": "...", "stale_days": N}`; the flags
        // below remain the human surface.
        use std::io::Read;

        let mut buf = String::new();
        if std::io::stdin().read_to_string(&mut buf).is_ok() {
            if let Ok(v) = serde_json::from_str::<Value>(&buf) {
                if v.get("op").and_then(Value::as_str) == Some("summary") {
                    let history = v
                        .get("history")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string();
                    let stale_days = v.get("stale_days").and_then(Value::as_i64).unwrap_or(7);
                    return print_summary(&history, stale_days, Utc::now());
                }
                // The health projection: the deleted Python leg's
                // evals_health_summary (report.py) shape, now the door's own
                // answer. None rules (no history, no rows) read as the
                // literal null so the callers keep their None-on-error
                // posture.
                if v.get("op").and_then(Value::as_str) == Some("health") {
                    let history = v
                        .get("history")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string();
                    let stale_days = v.get("stale_days").and_then(Value::as_i64).unwrap_or(7);
                    return print_health(&history, stale_days, Utc::now());
                }
                if v.get("op").and_then(Value::as_str) == Some("qualification") {
                    let history = v
                        .get("history")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string();
                    let manifest = v
                        .get("qualification")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string();
                    if manifest.is_empty() {
                        eprintln!("evals-trend: qualification op needs a manifest path");
                        return EXIT_USAGE;
                    }
                    return qualification_report(&history, &manifest, true, None);
                }
            }
        }
        eprintln!("{USAGE}");
        return EXIT_USAGE;
    }
    let mut history = String::new();
    let mut mode = String::from("report");
    let mut stale_days: i64 = 7;
    let mut since: Option<usize> = None;
    let mut graduate = false;
    let mut consecutive_n: usize = 3;
    let mut json_out = false;
    let mut compare: Option<String> = None;
    let mut compare_to: Option<String> = None;
    let mut repeats: usize = 3;
    let mut planned: Option<BTreeMap<String, usize>> = None;
    let mut qualification: Option<String> = None;
    let mut by_cohort = false;
    let mut by_failure_type = false;
    let mut check_metrics = false;
    let mut bank: Option<String> = None;
    let mut prices: Option<Value> = None;
    let mut climb = false;
    let mut climb_baseline: Option<String> = None;
    let mut climb_trial: Option<String> = None;
    let mut cohorts_yaml: Option<String> = None;
    let mut stage: Option<String> = None;
    let mut trial_lane: Option<String> = None;
    let mut now: Option<DateTime<Utc>> = None;
    let mut i = 0usize;
    while i < args.len() {
        let arg = args[i].clone();
        i += 1;
        let (name, inline) = match arg.split_once('=') {
            Some((n, v)) => (n.to_string(), Some(v.to_string())),
            None => (arg.clone(), None),
        };
        // `--graduate` is bare in the documented surface; an integer argument
        // is the optional count. `--consecutive` sizes the count without
        // enabling the graduation view (the old CLI's shapes, both kept).
        if name == "--graduate" || name == "--consecutive" {
            if name == "--graduate" {
                graduate = true;
            }
            let had_inline = inline.is_some();
            let raw = match inline {
                Some(v) => Some(v),
                None => args.get(i).filter(|t| !t.starts_with('-')).cloned(),
            };
            if let Some(raw) = raw {
                match raw.parse::<usize>() {
                    Ok(v) => consecutive_n = v,
                    Err(_) => {
                        eprintln!("evals-trend: --graduate needs an integer");
                        return EXIT_USAGE;
                    }
                }
                if !had_inline {
                    i += 1;
                }
            }
            continue;
        }
        let mut value = |what: &str| -> Result<String, ()> {
            match inline.clone() {
                Some(v) => Ok(v),
                None => match args.get(i).cloned() {
                    Some(v) => {
                        i += 1;
                        Ok(v)
                    }
                    None => {
                        eprintln!("evals-trend: {what} needs a value");
                        Err(())
                    }
                },
            }
        };
        match name.as_str() {
            "--history" => match value("--history") {
                Ok(v) => history = v,
                Err(()) => return EXIT_USAGE,
            },
            "--mode" => match value("--mode") {
                Ok(v) => mode = v,
                Err(()) => return EXIT_USAGE,
            },
            "--stale-days" => match value("--stale-days").map(|v| v.parse::<i64>()) {
                Ok(Ok(v)) => stale_days = v,
                _ => {
                    eprintln!("evals-trend: --stale-days needs an integer");
                    return EXIT_USAGE;
                }
            },
            "--since" => match value("--since").map(|v| v.parse::<usize>()) {
                Ok(Ok(v)) => since = Some(v),
                _ => {
                    eprintln!("evals-trend: --since needs an integer");
                    return EXIT_USAGE;
                }
            },
            "--json" | "-J" => json_out = true,
            "--compare" => match value("--compare") {
                Ok(v) => compare = Some(v),
                Err(()) => return EXIT_USAGE,
            },
            "--to" => match value("--to") {
                Ok(v) => compare_to = Some(v),
                Err(()) => return EXIT_USAGE,
            },
            "--repeats" => match value("--repeats").map(|v| v.parse::<usize>()) {
                Ok(Ok(v)) => repeats = v,
                _ => {
                    eprintln!("evals-trend: --repeats needs an integer");
                    return EXIT_USAGE;
                }
            },
            "--planned" => match value("--planned")
                .and_then(|v| serde_json::from_str::<BTreeMap<String, usize>>(&v).map_err(|_| ()))
            {
                Ok(v) => planned = Some(v),
                Err(()) => {
                    eprintln!("evals-trend: --planned must be a JSON object of task_id -> expected attempts");
                    return EXIT_USAGE;
                }
            },
            "--qualification" => match value("--qualification") {
                Ok(v) => qualification = Some(v),
                Err(()) => return EXIT_USAGE,
            },
            "--by-cohort" => by_cohort = true,
            "--by-failure-type" => by_failure_type = true,
            "--check-metrics" => check_metrics = true,
            "--bank" => match value("--bank") {
                Ok(v) => bank = Some(v),
                Err(()) => return EXIT_USAGE,
            },
            "--climb" => climb = true,
            "--baseline" => match value("--baseline") {
                Ok(v) => climb_baseline = Some(v),
                Err(()) => return EXIT_USAGE,
            },
            "--trial" => match value("--trial") {
                Ok(v) => climb_trial = Some(v),
                Err(()) => return EXIT_USAGE,
            },
            "--cohorts-yaml" => match value("--cohorts-yaml") {
                Ok(v) => cohorts_yaml = Some(v),
                Err(()) => return EXIT_USAGE,
            },
            "--stage" => match value("--stage") {
                Ok(v) => stage = Some(v),
                Err(()) => return EXIT_USAGE,
            },
            "--trial-lane" => match value("--trial-lane") {
                Ok(v) => trial_lane = Some(v),
                Err(()) => return EXIT_USAGE,
            },
            "--prices" => {
                match value("--prices")
                    .and_then(|v| serde_json::from_str::<Value>(&v).map_err(|_| ()))
                {
                    Ok(v) => prices = Some(v),
                    Err(()) => {
                        eprintln!("evals-trend: --prices must be a JSON object of model -> per-million rates");
                        return EXIT_USAGE;
                    }
                }
            }
            "--now" => match value("--now").map(|v| DateTime::parse_from_rfc3339(&v)) {
                Ok(Ok(dt)) => now = Some(dt.with_timezone(&Utc)),
                _ => {
                    eprintln!("evals-trend: --now must be RFC3339");
                    return EXIT_USAGE;
                }
            },
            _ => {
                eprintln!("evals-trend: unknown flag {name}");
                eprintln!("{USAGE}");
                return EXIT_USAGE;
            }
        }
    }
    if history.is_empty() {
        eprintln!("evals-trend: --history is required");
        eprintln!("{USAGE}");
        return EXIT_USAGE;
    }
    let now = now.unwrap_or_else(Utc::now);

    if qualification.is_some() && mode != "report" {
        eprintln!("evals-trend: --qualification requires --mode report (got '{mode}')");
        return EXIT_USAGE;
    }

    if mode == "summary" {
        return print_summary(&history, stale_days, now);
    }
    if climb && (mode != "report" || qualification.is_some() || by_cohort || compare.is_some()) {
        eprintln!("evals-trend: --climb is a report-mode fold and refuses other views (got mode '{mode}')");
        return EXIT_USAGE;
    }

    if mode == "trend" {
        if !PathBuf::from(&history).exists() {
            println!("evals trend: no_data (no history yet)");
            return 0;
        }
        let rows = read_rows(&history, Some("baseline"), None);
        let (view, regressed) = trend_fold(&rows, stale_days, now);
        if json_out {
            println!(
                "{}",
                serde_json::to_string_pretty(&view).unwrap_or_default()
            );
        } else {
            println!(
                "Eval trend: prior since {}, recent since {}",
                view["prior_start"].as_str().unwrap_or_default(),
                view["recent_start"].as_str().unwrap_or_default()
            );
            if let Some(tasks) = view["tasks"].as_object() {
                for (tid, t) in tasks {
                    let prior = t["prior"]["pass_at_1"].as_f64().unwrap_or(0.0) * 100.0;
                    let prior_runs = t["prior"]["runs"].to_string();
                    let recent = t["recent"]["pass_at_1"].as_f64().unwrap_or(0.0) * 100.0;
                    let recent_runs = t["recent"]["runs"].to_string();
                    let delta = t["delta"].as_f64().unwrap_or(0.0);
                    let verdict = t["verdict"].as_str().unwrap_or_default();
                    println!(
                        "  {tid}  prior {prior:.0}% ({prior_runs})  recent {recent:.0}% ({recent_runs})  delta={delta:+.2}  {verdict}"
                    );
                }
            }
            for (label, miss) in [
                ("prior", view["missing_in_prior"].clone()),
                ("recent", view["missing_in_recent"].clone()),
            ] {
                let ids: Vec<&str> = miss
                    .as_array()
                    .map(|a| a.iter().filter_map(Value::as_str).collect())
                    .unwrap_or_default();
                if !ids.is_empty() {
                    let id_list = ids.join(", ");
                    println!("  missing in {label}: {id_list}");
                }
            }
            let regressed_ids: Vec<&str> = view["regressed"]
                .as_array()
                .map(|a| a.iter().filter_map(Value::as_str).collect())
                .unwrap_or_default();
            if !regressed_ids.is_empty() {
                println!("  REGRESSED: {}", regressed_ids.join(", "));
            }
        }
        return if regressed.is_empty() { 0 } else { 4 };
    }

    // --mode report
    if climb {
        let Some(baseline) = &climb_baseline else {
            eprintln!("evals-trend: --climb needs --baseline <cohort>");
            return EXIT_USAGE;
        };
        let Some(trial) = &climb_trial else {
            eprintln!("evals-trend: --climb needs --trial <cohort>");
            return EXIT_USAGE;
        };
        return crate::evals_climb::run_climb(&crate::evals_climb::ClimbArgs {
            history: &history,
            baseline,
            trial,
            cohorts_yaml: cohorts_yaml.as_deref(),
            stage: stage.as_deref(),
            trial_lane: trial_lane.as_deref(),
            prices: prices.as_ref(),
            json_out,
        });
    }
    if let Some(qpath) = &qualification {
        return qualification_report(&history, qpath, json_out, since);
    }
    if by_cohort {
        let rows = read_rows(&history, None, since);
        let fold = cohort_fold(&rows, prices.as_ref());
        if json_out {
            println!(
                "{}",
                serde_json::to_string_pretty(&fold).unwrap_or_default()
            );
        } else {
            if let Some(cohorts) = fold["cohorts"].as_object() {
                for (id, c) in cohorts {
                    let rate = c["pass_rate"].as_f64().unwrap_or(0.0);
                    let ci = c["pass_rate_ci95"].as_array();
                    let (lo, hi) = match ci.map(|a| {
                        (
                            a.first().and_then(Value::as_f64),
                            a.get(1).and_then(Value::as_f64),
                        )
                    }) {
                        Some((Some(lo), Some(hi))) => (lo, hi),
                        _ => (0.0, 0.0),
                    };
                    let cost = if c["cost"]["measured"].as_bool().unwrap_or(false) {
                        match c["cost"]["dollars_per_accepted"].as_f64() {
                            Some(per) => format!("${:.4}/accepted", per),
                            // Measured total but zero accepted changes: there is
                            // no per-accepted number, and 0.0000 would read as a
                            // price.
                            None => format!(
                                "${:.4} total (0 accepted)",
                                c["cost"]["dollars_total"].as_f64().unwrap_or(0.0)
                            ),
                        }
                    } else {
                        "cost unmeasured".to_string()
                    };
                    let attempts = c["attempts"].to_string();
                    let graded = c["graded"].to_string();
                    let accepted = c["accepted"].to_string();
                    let rate_pct = rate * 100.0;
                    let lo_pct = lo * 100.0;
                    let hi_pct = hi * 100.0;
                    let median = c["median_wall_s"].to_string();
                    let tok_in = c["tokens"]["input"].to_string();
                    let tok_out = c["tokens"]["output"].to_string();
                    let cache_r = c["tokens"]["cache_read"].to_string();
                    let cache_w = c["tokens"]["cache_write"].to_string();
                    println!(
                        "  {id}: attempts={attempts} graded={graded} accepted={accepted} rate={rate_pct:.1}% [{lo_pct:.1},{hi_pct:.1}] median={median}s in={tok_in} out={tok_out} cache_r={cache_r} cache_w={cache_w} {cost}"
                    );
                }
            }
        }
        return 0;
    }
    let bank_dir = crate::evals::resolve_bank_dir(bank);
    if check_metrics {
        return crate::evals_compare::run_check_metrics(&bank_dir, json_out);
    }
    if by_failure_type {
        return crate::evals_compare::run_failure_report(
            &history, &bank_dir, repeats, since, json_out,
        );
    }
    if let Some(a) = &compare {
        let Some(b) = &compare_to else {
            eprintln!("evals-trend: --compare needs --to <cohort or variant>");
            return EXIT_USAGE;
        };
        return crate::evals_compare::run(&crate::evals_compare::CompareArgs {
            history: &history,
            a,
            b,
            repeats,
            since,
            json_out,
            bank: &bank_dir,
        });
    }

    if !PathBuf::from(&history).exists() {
        println!("evals report: no_data (no history yet)");
        return 0;
    }
    let rows = read_rows(&history, Some("baseline"), since);
    let report = report_fold(
        &rows,
        stale_days,
        now,
        graduate.then_some(consecutive_n),
        planned.as_ref(),
    );
    if json_out {
        println!(
            "{}",
            serde_json::to_string_pretty(&report).unwrap_or_default()
        );
        return if report["regression_alarm"]
            .as_array()
            .map_or(false, |a| !a.is_empty())
        {
            4
        } else {
            0
        };
    }
    if report["no_data"].as_bool().unwrap_or(false) {
        println!("evals report: no_data (no history yet)");
        return 0;
    }
    println!("Evals report:");
    if let Some(tiers) = report["tiers"].as_object() {
        for (tier, agg) in tiers {
            let passes = agg["passes"].to_string();
            let den = agg["grade_den"].as_u64().unwrap_or(0);
            let runs = agg["runs"].as_u64().unwrap_or(0);
            let plumbing = agg["plumbing"].as_u64().unwrap_or(0);
            let pct = agg["pass_rate"].as_f64().unwrap_or(0.0) * 100.0;
            // The rate divides by grades, so the printed fraction does too;
            // attempts with no grade are named beside it, never folded in.
            let mut line = format!("  {tier}: {passes}/{den} pass ({pct:.0}%)");
            if runs > den {
                let legacy = runs - den - plumbing;
                line += &format!(
                    "; {} of {runs} attempts not graded: {plumbing} plumbing, {legacy} legacy",
                    runs - den
                );
            }
            println!("{line}");
        }
    }
    if let Some(tasks) = report["tasks"].as_array() {
        for t in tasks {
            let no_grade = t["grade_den"].as_u64() == Some(0);
            let mark = if no_grade {
                "NO GRADE"
            } else if t["flake"].as_bool().unwrap_or(false) {
                "FLAKE"
            } else if t["pass_k"].as_bool().unwrap_or(false) {
                "PASS"
            } else {
                "FAIL"
            };
            println!(
                "    {:11} {}: pass@1={} pass^{}={} [{}]",
                t["tier"].as_str().unwrap_or_default(),
                t["task_id"].as_str().unwrap_or_default(),
                if no_grade {
                    "n/a".to_string()
                } else {
                    format!("{:.0}%", t["pass_at_1"].as_f64().unwrap_or(0.0) * 100.0)
                },
                t["grade_den"],
                t["pass_k"],
                mark,
            );
        }
    }
    let flake_ids: Vec<&str> = report["flakes"]
        .as_array()
        .map(|a| a.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    if !flake_ids.is_empty() {
        println!("  flakes: {}", flake_ids.join(", "));
    }
    let alarm_ids: Vec<&str> = report["regression_alarm"]
        .as_array()
        .map(|a| a.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    if !alarm_ids.is_empty() {
        println!("  REGRESSION ALARM: {} below 100%", alarm_ids.join(", "));
    }
    if let Some(elig) = report.get("graduation_eligible").and_then(Value::as_array) {
        let ids: Vec<&str> = elig.iter().filter_map(Value::as_str).collect();
        if ids.is_empty() {
            println!("  graduation-eligible: none");
        } else {
            println!("  graduation-eligible: {}", ids.join(", "));
        }
    }
    if alarm_ids.is_empty() {
        0
    } else {
        4
    }
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod tests_cohort;
