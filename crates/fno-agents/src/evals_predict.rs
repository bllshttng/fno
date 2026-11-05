//! `evals report --predict`: does a golden's score say anything about the
//! real work that follows it? Read-only: it writes no file and calls no model.
//!
//! The join is by tree. An outcome counts against the newest scored tree
//! whose commit time is before the outcome's time, so an outcome needs no
//! `head_sha` of its own. Only sealed, graded rows score, one series per lane
//! and seat, and lanes never mix. Each outcome feed stays apart: an
//! unreadable feed reads `unmeasured`, never zero.
//!
//! Same-tree noise prints first. When the spread between trees is no larger
//! than the spread of one tree run again, a tree score cannot separate them.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use serde_json::{json, Map, Value};

pub(crate) use crate::evals_power::{Rng, SEED};
use crate::evals_trend::{is_scored, Row};

pub(crate) const SHUFFLES: usize = 5_000;
const Z95: f64 = 1.959_964;
const Q_MAX: f64 = 0.10;
const NO_SIGNAL_BOUND: f64 = 0.50;
const MIN_TREES: usize = 14;
const MIN_MINORITY: usize = 5;
/// A tree in force for less time than this joins no outcome: a rate over
/// minutes is noise. Its rows still feed the noise block.
const MIN_WINDOW_S: i64 = 6 * 3_600;
const DAY_S: i64 = 86_400;

/// One outcome event: when it happened, by the join rule's clock.
pub(crate) type Times = Vec<i64>;

/// One outcome feed, read or not, with the facts its coverage line prints.
pub(crate) struct Feed {
    pub(crate) times: Result<Times, String>,
    pub(crate) coverage: Map<String, Value>,
}

/// The facts the fold reads that live outside the history file.
pub(crate) struct Inputs {
    /// tree sha -> commit epoch seconds; absent when git cannot read it.
    pub(crate) tree_time: BTreeMap<String, i64>,
    /// trees that are ancestors of main; any other tree feeds noise only.
    pub(crate) on_main: BTreeSet<String>,
    pub(crate) lead_tasks: BTreeSet<String>,
    pub(crate) now: i64,
    /// UTC day -> typed user turns, the corrections denominator.
    pub(crate) typed_turns: Option<BTreeMap<String, u64>>,
    /// Feeds by name, in print order.
    pub(crate) feeds: Vec<(String, Feed)>,
}

fn sealed(r: &Row) -> bool {
    r.raw_obs_bool("worker_sealed") == Some(true)
}

fn series_key(r: &Row, lead: &BTreeSet<String>) -> String {
    let lane = r.raw_str("requested_lane").unwrap_or("default").to_string();
    let seat = r.raw_str("seat").map(str::to_string).unwrap_or_else(|| {
        if lead.contains(&r.task_id) {
            "lead".into()
        } else {
            "worker".into()
        }
    });
    format!("{lane}/{seat}")
}

fn mean(xs: &[f64]) -> Option<f64> {
    (!xs.is_empty()).then(|| xs.iter().sum::<f64>() / xs.len() as f64)
}

/// Sample standard deviation; None under two values.
fn sd(xs: &[f64]) -> Option<f64> {
    if xs.len() < 2 {
        return None;
    }
    let m = mean(xs)?;
    let var = xs.iter().map(|x| (x - m).powi(2)).sum::<f64>() / (xs.len() - 1) as f64;
    Some(var.sqrt())
}

fn r2(x: f64) -> f64 {
    (x * 100.0).round() / 100.0
}

fn r4(x: f64) -> f64 {
    crate::evals_trend::round4(x)
}

/// Average ranks, ties shared.
fn ranks(xs: &[f64]) -> Vec<f64> {
    let mut idx: Vec<usize> = (0..xs.len()).collect();
    idx.sort_by(|&a, &b| {
        xs[a]
            .partial_cmp(&xs[b])
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    let mut out = vec![0.0; xs.len()];
    let mut i = 0;
    while i < idx.len() {
        let mut j = i;
        while j + 1 < idx.len() && xs[idx[j + 1]] == xs[idx[i]] {
            j += 1;
        }
        let avg = (i + j) as f64 / 2.0 + 1.0;
        for k in &idx[i..=j] {
            out[*k] = avg;
        }
        i = j + 1;
    }
    out
}

fn pearson(a: &[f64], b: &[f64]) -> Option<f64> {
    let (ma, mb) = (mean(a)?, mean(b)?);
    let (mut num, mut da, mut db) = (0.0, 0.0, 0.0);
    for (x, y) in a.iter().zip(b) {
        num += (x - ma) * (y - mb);
        da += (x - ma).powi(2);
        db += (y - mb).powi(2);
    }
    (da > 0.0 && db > 0.0).then(|| num / (da * db).sqrt())
}

/// Spearman's rho; None when either side is constant.
pub(crate) fn spearman(a: &[f64], b: &[f64]) -> Option<f64> {
    pearson(&ranks(a), &ranks(b))
}

/// Two-sided permutation p for Spearman's rho over ranks.
fn permutation_p(a: &[f64], b: &[f64], rho: f64, rng: &mut Rng) -> f64 {
    let ra = ranks(a);
    let mut rb = ranks(b);
    let mut hits = 0usize;
    for _ in 0..SHUFFLES {
        for i in (1..rb.len()).rev() {
            let j = (rng.next() % (i as u64 + 1)) as usize;
            rb.swap(i, j);
        }
        if pearson(&ra, &rb).is_some_and(|r| r.abs() >= rho.abs() - 1e-12) {
            hits += 1;
        }
    }
    (hits + 1) as f64 / (SHUFFLES + 1) as f64
}

/// The Fisher 95% interval for rho at n.
fn fisher(rho: f64, n: usize) -> Option<(f64, f64)> {
    if n <= 3 {
        return None;
    }
    let z = rho.clamp(-0.999_999, 0.999_999).atanh();
    let half = Z95 / ((n - 3) as f64).sqrt();
    Some(((z - half).tanh(), (z + half).tanh()))
}

/// The n at which an interval around |rho| would close under the no-signal
/// bound; None when |rho| already reaches it.
fn n_to_settle(rho_abs: f64) -> Option<usize> {
    let gap = NO_SIGNAL_BOUND.atanh() - rho_abs.min(0.999_999).atanh();
    (gap > 0.0).then(|| (3.0 + (Z95 / gap).powi(2)).ceil() as usize)
}

/// Benjamini-Hochberg q per p, in input order.
pub(crate) fn bh(ps: &[f64]) -> Vec<f64> {
    let m = ps.len();
    let mut idx: Vec<usize> = (0..m).collect();
    idx.sort_by(|&a, &b| {
        ps[a]
            .partial_cmp(&ps[b])
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    let mut q = vec![1.0; m];
    let mut running = 1.0f64;
    for (rank, &i) in idx.iter().enumerate().rev() {
        running = running.min(ps[i] * m as f64 / (rank + 1) as f64);
        q[i] = running.min(1.0);
    }
    q
}

/// One scored tree on the join: its window and per-task grades.
struct Tree {
    sha: String,
    time: i64,
    end: i64,
    tasks: BTreeMap<String, (usize, usize)>,
}

impl Tree {
    fn score(&self) -> Option<f64> {
        let (g, p) = self
            .tasks
            .values()
            .fold((0, 0), |(g, p), (tg, tp)| (g + tg, p + tp));
        (g > 0).then(|| p as f64 / g as f64)
    }
}

/// The noise block. Same-tree noise reads every cohort run three or more
/// times. The between-tree spread and the week means read main trees only:
/// an experiment tree off main is not a day the join can tell apart.
fn noise(rows: &[&Row], tree_time: &BTreeMap<String, i64>, on_main: &BTreeSet<String>) -> Value {
    // Same tree: a cohort run three or more times, one score per repeat.
    let mut by_cohort: BTreeMap<String, BTreeMap<i64, (usize, usize)>> = BTreeMap::new();
    for r in rows {
        let Some(cohort) = &r.experiment_id else {
            continue;
        };
        let rep = r.raw_i64("repeat_index").unwrap_or(0);
        let e = by_cohort
            .entry(cohort.clone())
            .or_default()
            .entry(rep)
            .or_default();
        e.0 += 1;
        e.1 += r.pass as usize;
    }
    let mut variances = Vec::new();
    let mut repeated = Vec::new();
    for (cohort, reps) in &by_cohort {
        if reps.len() < 3 {
            continue;
        }
        let scores: Vec<f64> = reps
            .values()
            .map(|(g, p)| 100.0 * *p as f64 / *g as f64)
            .collect();
        if let Some(s) = sd(&scores) {
            variances.push(s * s);
            repeated.push(json!({"cohort": cohort, "scores": scores.iter().map(|x| r2(*x)).collect::<Vec<_>>(), "sd_points": r2(s)}));
        }
    }
    let same = mean(&variances).map(f64::sqrt);
    // Between trees: one mean score per tree.
    let mut by_tree: BTreeMap<String, (usize, usize)> = BTreeMap::new();
    for r in rows {
        if let Some(sha) = r.bank_rev.as_ref().filter(|s| on_main.contains(*s)) {
            let e = by_tree.entry(sha.clone()).or_default();
            e.0 += 1;
            e.1 += r.pass as usize;
        }
    }
    let mut timed: Vec<(i64, f64)> = by_tree
        .iter()
        .filter_map(|(sha, (g, p))| {
            tree_time
                .get(sha)
                .map(|t| (*t, 100.0 * *p as f64 / *g as f64))
        })
        .collect();
    timed.sort_by_key(|(t, _)| *t);
    let tree_scores: Vec<f64> = timed.iter().map(|(_, s)| *s).collect();
    let between = sd(&tree_scores);
    let mut weeks: Vec<Vec<f64>> = Vec::new();
    if let Some((first, _)) = timed.first() {
        let day0 = first - first.rem_euclid(DAY_S);
        for (t, s) in &timed {
            let w = ((t - day0) / (7 * DAY_S)) as usize;
            if weeks.len() <= w {
                weeks.resize(w + 1, Vec::new());
            }
            weeks[w].push(*s);
        }
    }
    let week_means: Vec<Value> = weeks
        .iter()
        .map(|w| mean(w).map(|m| json!(r2(m))).unwrap_or(Value::Null))
        .collect();
    let k = weeks.iter().map(Vec::len).min().unwrap_or(0);
    let mdd_week = match (same, k) {
        (Some(s), k) if k > 0 => Some(crate::evals_power::mdd(s * std::f64::consts::SQRT_2, k, 1)),
        _ => None,
    };
    let separable = match (same, between) {
        (Some(s), Some(b)) => json!(b > s),
        _ => Value::Null,
    };
    json!({
        "same_tree_sd_points": same.map(r2),
        "same_tree_cohorts": repeated,
        "between_tree_sd_points": between.map(r2),
        "trees": json!(tree_scores.len()),
        "week_means": week_means,
        "separable": separable,
        "mdd_week_points": mdd_week.map(r2),
        "halves": halves_power(rows, &by_tree, tree_time),
    })
}

/// Per task, the earlier half against the later half of the main trees
/// (time order), paired and powered. Null under two main trees.
fn halves_power(
    rows: &[&Row],
    by_tree: &BTreeMap<String, (usize, usize)>,
    tree_time: &BTreeMap<String, i64>,
) -> Value {
    let mut timed_all: Vec<(String, i64)> = by_tree
        .iter()
        .filter_map(|(sha, _)| tree_time.get(sha).map(|t| (sha.clone(), *t)))
        .collect();
    timed_all.sort_by_key(|(_, t)| *t);
    if timed_all.len() < 2 {
        return Value::Null;
    }
    let mid = timed_all.len() / 2;
    let early: BTreeSet<&String> = timed_all[..mid].iter().map(|(s, _)| s).collect();
    let late: BTreeSet<&String> = timed_all[mid..].iter().map(|(s, _)| s).collect();
    let mut map_e: BTreeMap<String, (usize, usize)> = BTreeMap::new();
    let mut map_l: BTreeMap<String, (usize, usize)> = BTreeMap::new();
    for r in rows {
        let Some(sha) = r.bank_rev.as_ref() else {
            continue;
        };
        if early.contains(sha) || late.contains(sha) {
            let map = if early.contains(sha) {
                &mut map_e
            } else {
                &mut map_l
            };
            let e = map.entry(r.task_id.clone()).or_default();
            e.0 += 1;
            e.1 += r.pass as usize;
        }
    }
    let all_tasks: BTreeSet<String> = map_e.keys().chain(map_l.keys()).cloned().collect();
    for t in all_tasks {
        map_l.entry(t.clone()).or_insert((0, 0));
        map_e.entry(t).or_insert((0, 0));
    }
    let p = crate::evals_power::paired(&map_e, &map_l);
    let mut rng = Rng(SEED);
    crate::evals_power::power_block(&p, &mut rng, None, None)
}
fn outcome_series(
    name: &str,
    times: &Times,
    trees: &[Tree],
    turns: Option<&BTreeMap<String, u64>>,
    short: &[(i64, i64)],
) -> (Vec<Option<f64>>, Map<String, Value>) {
    let mut counts = vec![0u64; trees.len()];
    let (mut before, mut in_short) = (0u64, 0u64);
    for t in times {
        // A short window can sit before the first joined tree, so it reads first.
        if short.iter().any(|(s, e)| t >= s && t < e) {
            in_short += 1;
            continue;
        }
        if trees.first().is_none_or(|f| *t < f.time) {
            before += 1;
            continue;
        }
        if let Some(i) = trees.iter().rposition(|tr| tr.time <= *t) {
            if *t < trees[i].end {
                counts[i] += 1;
            }
        }
    }
    let per_turn = name == "corrections";
    let values: Vec<Option<f64>> = trees
        .iter()
        .zip(&counts)
        .map(|(tr, c)| {
            if per_turn {
                let turns = turns?;
                // A day's turns belong to the tree in force at its noon.
                let n: u64 = turns
                    .iter()
                    .filter_map(|(day, n)| {
                        let noon = chrono::NaiveDate::parse_from_str(day, "%Y-%m-%d")
                            .ok()?
                            .and_hms_opt(12, 0, 0)?
                            .and_utc()
                            .timestamp();
                        (noon >= tr.time && noon < tr.end).then_some(*n)
                    })
                    .sum();
                (n > 0).then(|| *c as f64 / n as f64)
            } else {
                let days = (tr.end - tr.time) as f64 / DAY_S as f64;
                Some(*c as f64 / days)
            }
        })
        .collect();
    let mut cov = Map::new();
    cov.insert("joined".into(), json!(counts.iter().sum::<u64>()));
    cov.insert("before_first_tree".into(), json!(before));
    cov.insert("in_short_window".into(), json!(in_short));
    cov.insert(
        "trees_with_denominator".into(),
        json!(values.iter().filter(|v| v.is_some()).count()),
    );
    (values, cov)
}

struct Pair {
    outcome: String,
    n: usize,
    rho: Option<f64>,
    p: Option<f64>,
    q: Option<f64>,
    ci: Option<(f64, f64)>,
}

impl Pair {
    fn to_json(&self) -> Value {
        json!({
            "outcome": self.outcome,
            "n": self.n,
            "rho": self.rho.map(r4),
            "p": self.p.map(r4),
            "q": self.q.map(r4),
            "ci95": self.ci.map(|(lo, hi)| json!([r4(lo), r4(hi)])),
        })
    }
}

fn pair(name: &str, scores: &[Option<f64>], outcome: &[Option<f64>], rng: &mut Rng) -> Pair {
    let (a, b): (Vec<f64>, Vec<f64>) = scores
        .iter()
        .zip(outcome)
        .filter_map(|(s, o)| Some(((*s)?, (*o)?)))
        .unzip();
    let rho = spearman(&a, &b);
    let p = rho.map(|r| permutation_p(&a, &b, r, rng));
    Pair {
        outcome: name.to_string(),
        n: a.len(),
        rho,
        p,
        q: None,
        ci: rho.and_then(|r| fisher(r, a.len())),
    }
}

/// The verdict for one task, rules in the documented order.
fn verdict(trees: usize, grades: usize, passes: usize, pairs: &[&Pair]) -> &'static str {
    if passes == 0 && trees >= MIN_TREES {
        return "constant_fail";
    }
    if grades > 0 && passes == grades {
        return "constant_pass";
    }
    if passes.min(grades - passes) < MIN_MINORITY || trees < MIN_TREES {
        return "underpowered";
    }
    let read: Vec<&&Pair> = pairs.iter().filter(|p| p.rho.is_some()).collect();
    let sig = |sign: f64| {
        read.iter()
            .any(|p| p.q.is_some_and(|q| q < Q_MAX) && p.rho.is_some_and(|r| r * sign > 0.0))
    };
    if sig(-1.0) {
        return "predicts";
    }
    if sig(1.0) {
        return "inverted";
    }
    if !read.is_empty()
        && read.iter().all(|p| {
            p.ci.is_some_and(|(lo, hi)| lo.abs().max(hi.abs()) < NO_SIGNAL_BOUND)
        })
    {
        return "no_signal";
    }
    "underpowered"
}

/// The whole report over one history.
pub(crate) fn fold(history_rows: &[Row], inputs: &Inputs) -> Value {
    let mut excluded: BTreeMap<&str, usize> = BTreeMap::new();
    let mut series: BTreeMap<String, Vec<&Row>> = BTreeMap::new();
    for r in history_rows {
        if !is_scored(r) {
            *excluded.entry("not_scored").or_default() += 1;
        } else if !sealed(r) {
            *excluded.entry("unsealed").or_default() += 1;
        } else {
            series
                .entry(series_key(r, &inputs.lead_tasks))
                .or_default()
                .push(r);
        }
    }
    let mut index = Map::new();
    for (k, v) in &series {
        index.insert(k.clone(), json!(v.len()));
    }
    if !inputs.lead_tasks.is_empty() && !series.keys().any(|k| k.ends_with("/lead")) {
        index.insert("lead".into(), json!("no_cohort"));
    }
    // The series the report reads: the one with the most rows. The others
    // are listed with their counts and never mixed in.
    let Some((key, rows)) = series.iter().max_by_key(|(_, v)| v.len()) else {
        return json!({"series": Value::Null, "series_index": index, "excluded": excluded,
            "noise": {"same_tree_sd_points": Value::Null}, "tasks": []});
    };
    let noise_block = noise(rows, &inputs.tree_time, &inputs.on_main);

    // The join's trees: timed, on main, in time order.
    let mut grades: BTreeMap<String, BTreeMap<String, (usize, usize)>> = BTreeMap::new();
    let mut tree_excluded: BTreeMap<&str, usize> = BTreeMap::new();
    for r in rows {
        let Some(sha) = &r.bank_rev else {
            *tree_excluded.entry("no_bank_rev").or_default() += 1;
            continue;
        };
        if !inputs.tree_time.contains_key(sha) {
            *tree_excluded.entry("tree_time_unread").or_default() += 1;
            continue;
        }
        if !inputs.on_main.contains(sha) {
            *tree_excluded.entry("off_main").or_default() += 1;
            continue;
        }
        let e = grades
            .entry(sha.clone())
            .or_default()
            .entry(r.task_id.clone())
            .or_default();
        e.0 += 1;
        e.1 += r.pass as usize;
    }
    let mut all: Vec<Tree> = grades
        .into_iter()
        .map(|(sha, tasks)| Tree {
            time: inputs.tree_time[&sha],
            sha,
            end: 0,
            tasks,
        })
        .collect();
    all.sort_by_key(|t| t.time);
    let ends: Vec<i64> = all
        .iter()
        .skip(1)
        .map(|t| t.time)
        .chain(std::iter::once(inputs.now))
        .collect();
    for (t, e) in all.iter_mut().zip(ends) {
        t.end = e;
    }
    let (trees, short): (Vec<Tree>, Vec<Tree>) = all
        .into_iter()
        .partition(|t| t.end - t.time >= MIN_WINDOW_S);
    let short_windows: Vec<(i64, i64)> = short.iter().map(|t| (t.time, t.end)).collect();
    for t in &short {
        *tree_excluded.entry("short_window").or_default() +=
            t.tasks.values().map(|(g, _)| g).sum::<usize>();
    }

    // Outcomes per tree.
    let mut outcomes = Map::new();
    let mut readable: Vec<(String, Vec<Option<f64>>)> = Vec::new();
    for (name, feed) in &inputs.feeds {
        let mut cov = feed.coverage.clone();
        match &feed.times {
            Err(reason) => {
                cov.insert("status".into(), json!("unmeasured"));
                cov.insert("reason".into(), json!(reason));
            }
            Ok(times) => {
                let (values, join) = outcome_series(
                    name,
                    times,
                    &trees,
                    inputs.typed_turns.as_ref(),
                    &short_windows,
                );
                cov.insert("status".into(), json!("measured"));
                cov.insert("count".into(), json!(times.len()));
                cov.extend(join);
                readable.push((name.clone(), values));
            }
        }
        outcomes.insert(name.clone(), Value::Object(cov));
    }

    let mut rng = Rng(SEED);
    let tree_scores: Vec<Option<f64>> = trees.iter().map(Tree::score).collect();
    let mut mean_pairs: Vec<Pair> = readable
        .iter()
        .map(|(n, v)| pair(n, &tree_scores, v, &mut rng))
        .collect();
    let task_ids: BTreeSet<String> = trees.iter().flat_map(|t| t.tasks.keys().cloned()).collect();
    let mut task_pairs: Vec<(String, Vec<Pair>)> = task_ids
        .iter()
        .map(|id| {
            let scores: Vec<Option<f64>> = trees
                .iter()
                .map(|t| t.tasks.get(id).map(|(g, p)| *p as f64 / *g as f64))
                .collect();
            let pairs = readable
                .iter()
                .map(|(n, v)| pair(n, &scores, v, &mut rng))
                .collect();
            (id.clone(), pairs)
        })
        .collect();
    // One BH step over every printed pair with a p.
    let mut ps = Vec::new();
    for p in mean_pairs
        .iter()
        .chain(task_pairs.iter().flat_map(|(_, v)| v.iter()))
    {
        if let Some(x) = p.p {
            ps.push(x);
        }
    }
    let qs = bh(&ps);
    let mut qi = qs.into_iter();
    for p in mean_pairs
        .iter_mut()
        .chain(task_pairs.iter_mut().flat_map(|(_, v)| v.iter_mut()))
    {
        if p.p.is_some() {
            p.q = qi.next();
        }
    }

    let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
    let tasks: Vec<Value> = task_pairs
        .iter()
        .map(|(id, pairs)| {
            let (n, g, p) = trees
                .iter()
                .fold((0, 0, 0), |(n, g, p), t| match t.tasks.get(id) {
                    Some((tg, tp)) => (n + 1, g + tg, p + tp),
                    None => (n, g, p),
                });
            let refs: Vec<&Pair> = pairs.iter().collect();
            let v = verdict(n, g, p, &refs);
            *counts.entry(v).or_default() += 1;
            let max_rho = pairs
                .iter()
                .filter_map(|p| p.rho.map(f64::abs))
                .fold(0.0, f64::max);
            json!({
                "task": id,
                "trees": n,
                "grades": g,
                "passes": p,
                "verdict": v,
                "n_to_settle": (v == "underpowered").then(|| n_to_settle(max_rho)).flatten(),
                "pairs": pairs.iter().map(Pair::to_json).collect::<Vec<_>>(),
            })
        })
        .collect();
    json!({
        "series": key,
        "series_index": index,
        "excluded": excluded,
        "join_excluded": tree_excluded,
        "noise": noise_block,
        "trees": trees.iter().map(|t| json!({"sha": t.sha, "time": t.time, "score": t.score().map(r4)})).collect::<Vec<_>>(),
        "outcomes": outcomes,
        "mean_score": mean_pairs.iter().map(Pair::to_json).collect::<Vec<_>>(),
        "verdict_counts": counts,
        "tasks": tasks,
        "seed": SEED,
        "shuffles": SHUFFLES,
    })
}

// ---- the readers: everything below touches git, the event store or a file.

fn git(repo: &Path, args: &[&str]) -> Option<String> {
    let mut cmd = std::process::Command::new("git");
    cmd.arg("-C").arg(repo).args(args);
    let out = crate::bounded_cmd::output_with_timeout_result(cmd, 30).ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

fn commit_time(repo: &Path, sha: &str) -> Option<i64> {
    git(repo, &["show", "-s", "--format=%ct", sha])?
        .trim()
        .parse()
        .ok()
}

fn main_ref(repo: &Path) -> &'static str {
    if git(repo, &["rev-parse", "--verify", "--quiet", "origin/main"]).is_some() {
        "origin/main"
    } else {
        "HEAD"
    }
}

/// Reverts on main since `since`, each at the time of the commit it reverts,
/// else at its own time.
fn read_reverts(repo: &Path, main: &str, since: i64) -> Feed {
    let since_arg = format!(
        "--since={}",
        chrono::DateTime::from_timestamp(since, 0)
            .unwrap_or_default()
            .to_rfc3339()
    );
    let Some(text) = git(
        repo,
        &[
            "log",
            main,
            "--no-merges",
            "-i",
            "--grep=^Revert",
            &since_arg,
            "--format=%ct%x1f%B%x1e",
        ],
    ) else {
        return Feed {
            times: Err("git log over main failed".into()),
            coverage: Map::new(),
        };
    };
    let re = regex::Regex::new(r"This reverts commit ([0-9a-f]{7,40})").expect("static regex");
    let (mut times, mut named, mut fallback) = (Vec::new(), 0u64, 0u64);
    for rec in text.split('\x1e').map(str::trim).filter(|r| !r.is_empty()) {
        let Some((own, body)) = rec.split_once('\x1f') else {
            continue;
        };
        let Ok(own) = own.trim().parse::<i64>() else {
            continue;
        };
        match re.captures(body).and_then(|c| commit_time(repo, &c[1])) {
            Some(t) => {
                named += 1;
                times.push(t);
            }
            None => {
                fallback += 1;
                times.push(own);
            }
        }
    }
    let mut coverage = Map::new();
    coverage.insert("reverted_commit_time".into(), json!(named));
    coverage.insert("revert_time_fallback".into(), json!(fallback));
    coverage.insert("seat".into(), json!("unattributed"));
    Feed {
        times: Ok(times),
        coverage,
    }
}

fn event_secs(line: &Value, fallback_ms: i64) -> i64 {
    line["data"]["ts_turn"]
        .as_str()
        .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
        .map(|d| d.timestamp())
        .unwrap_or(fallback_ms / 1_000)
}

/// Typed user corrections: `correct` and `reject`, never `interrupt`, never
/// a machine's.
fn read_corrections(journal: &Path, since: i64) -> Feed {
    let mut q = crate::event_store::EventQuery::of_types(&["agent_corrected"]);
    q.since_ms = Some(since * 1_000);
    let rows = match crate::event_store::query_events(journal, &q) {
        Ok(rows) => rows,
        Err(e) => {
            return Feed {
                times: Err(format!("event store unread: {e}")),
                coverage: Map::new(),
            }
        }
    };
    let (mut times, mut seated) = (Vec::new(), 0u64);
    for row in rows {
        let Ok(line) = serde_json::from_str::<Value>(&row.line) else {
            continue;
        };
        let d = &line["data"];
        if d["corrected_by"].as_str() != Some("user")
            || !matches!(d["kind"].as_str(), Some("correct" | "reject"))
        {
            continue;
        }
        if d["seat"]["value"].as_str().is_some() {
            seated += 1;
        }
        times.push(event_secs(&line, row.ts_ms));
    }
    let mut coverage = Map::new();
    coverage.insert("seat_stamped".into(), json!(seated));
    coverage.insert("seat".into(), json!("unattributed"));
    Feed {
        times: Ok(times),
        coverage,
    }
}

/// Fail review verdicts: each (reviewer, head) pair whose latest verdict is
/// a fail, at that verdict's time. The same latest-verdict rule as
/// `review_fails_by_pr`, kept with its clock.
fn read_review_fails(journal: &Path, since: i64) -> Feed {
    let mut q = crate::event_store::EventQuery::of_types(&["review_attestation"]);
    q.since_ms = Some(since * 1_000);
    let rows = match crate::event_store::query_events(journal, &q) {
        Ok(rows) => rows,
        Err(e) => {
            return Feed {
                times: Err(format!("event store unread: {e}")),
                coverage: Map::new(),
            }
        }
    };
    let mut latest: BTreeMap<(String, String), (i64, bool)> = BTreeMap::new();
    for row in rows {
        let Ok(line) = serde_json::from_str::<Value>(&row.line) else {
            continue;
        };
        let d = &line["data"];
        let (Some(reviewer), Some(head), Some(v)) = (
            d["reviewer"].as_str(),
            d["head_sha"].as_str(),
            d["verdict"].as_str(),
        ) else {
            continue;
        };
        let key = (
            reviewer.trim_start_matches('/').to_string(),
            head.to_string(),
        );
        let t = row.ts_ms / 1_000;
        if latest.get(&key).is_none_or(|(old, _)| t >= *old) {
            latest.insert(key, (t, v == "fail"));
        }
    }
    let times: Vec<i64> = latest
        .values()
        .filter(|(_, f)| *f)
        .map(|(t, _)| *t)
        .collect();
    let mut coverage = Map::new();
    coverage.insert("verdict_pairs".into(), json!(latest.len()));
    coverage.insert("seat".into(), json!("unattributed"));
    Feed {
        times: Ok(times),
        coverage,
    }
}

/// UTC day -> typed user turns, from the correction fold's own cache.
fn read_typed_turns(state: &Path) -> Option<BTreeMap<String, u64>> {
    let text = std::fs::read_to_string(crate::correction_fold::cache_path(state)).ok()?;
    let parsed: Value = serde_json::from_str(&text).ok()?;
    let mut out: BTreeMap<String, u64> = BTreeMap::new();
    for (key, n) in parsed.get("typed_turns")?.as_object()? {
        if let Some(day) = key.split('|').next() {
            *out.entry(day.to_string()).or_default() += n.as_u64().unwrap_or(0);
        }
    }
    Some(out)
}

pub(crate) fn run(history: &str, bank: Option<String>, json_out: bool) -> i32 {
    let rows = crate::evals_trend::read_rows(history, None, None);
    let repo = crate::evals::canonical_repo_root();
    let bank_dir = crate::evals::resolve_bank_dir(bank);
    let lead_tasks: BTreeSet<String> = crate::evals::bank::discover_bank(&bank_dir)
        .map(|ts| {
            ts.into_iter()
                .filter(|t| t.tags.iter().any(|g| g == "lead"))
                .map(|t| t.id)
                .collect()
        })
        .unwrap_or_default();
    let shas: BTreeSet<String> = rows
        .iter()
        .filter(|r| is_scored(r) && sealed(r))
        .filter_map(|r| r.bank_rev.clone())
        .collect();
    let main = main_ref(&repo);
    let mut tree_time = BTreeMap::new();
    let mut on_main = BTreeSet::new();
    for sha in &shas {
        if let Some(t) = commit_time(&repo, sha) {
            tree_time.insert(sha.clone(), t);
        }
        if git(&repo, &["merge-base", "--is-ancestor", sha, main]).is_some() {
            on_main.insert(sha.clone());
        }
    }
    let first = tree_time
        .iter()
        .filter(|(s, _)| on_main.contains(*s))
        .map(|(_, t)| *t)
        .min()
        .unwrap_or(0);
    // The first tree's whole day, so an outcome that day before the tree
    // lands under before_first_tree.
    let since = first - first.rem_euclid(DAY_S);
    let state = crate::evals_candidates::state_root();
    let journal = state.join("events.jsonl");
    let mut returned = Map::new();
    returned.insert("seat".into(), json!("unattributed"));
    let inputs = Inputs {
        tree_time,
        on_main,
        lead_tasks,
        now: chrono::Utc::now().timestamp(),
        typed_turns: read_typed_turns(&state),
        feeds: vec![
            ("corrections".into(), read_corrections(&journal, since)),
            ("reverts".into(), read_reverts(&repo, main, since)),
            ("review_fails".into(), read_review_fails(&journal, since)),
            (
                "returned_complaints".into(),
                Feed {
                    // The intel scorecard reads one session per lead, and a
                    // whole-transcript scan finds returns it misses.
                    times: Err(
                        "the intel scorecard reads lead sessions only, so its zero is blind".into(),
                    ),
                    coverage: returned,
                },
            ),
        ],
    };
    let view = fold(&rows, &inputs);
    if json_out {
        println!(
            "{}",
            serde_json::to_string_pretty(&view).unwrap_or_default()
        );
    } else {
        print_text(&view);
    }
    0
}

fn print_text(v: &Value) {
    let n = &v["noise"];
    println!(
        "series {}: same-tree sd {} points, between-tree sd {} points, separable {}",
        v["series"], n["same_tree_sd_points"], n["between_tree_sd_points"], n["separable"]
    );
    println!("  week means: {}", n["week_means"]);
    let week_txt = n["mdd_week_points"]
        .as_f64()
        .map(|m| format!("{m:.1} points"))
        .unwrap_or_else(|| "unmeasured".to_string());
    let halves = &n["halves"];
    let halves_line = if halves.is_null() {
        "halves: unmeasured".to_string()
    } else if halves["paired"]["tasks_informative"].as_u64().unwrap_or(0) == 0 {
        "halves: underpowered, 0 informative tasks".to_string()
    } else {
        let mean = halves["paired"]["mean_delta_points"]
            .as_f64()
            .unwrap_or(0.0);
        let ci = halves["paired"]["ci95_points"].as_array();
        let (lo, hi) = match ci {
            Some(a) => (
                a.first().and_then(Value::as_f64).unwrap_or(0.0),
                a.get(1).and_then(Value::as_f64).unwrap_or(0.0),
            ),
            None => (0.0, 0.0),
        };
        let mdd = halves["mdd_points"].as_f64().unwrap_or(0.0);
        format!(
            "halves: paired delta {mean:+.1} points over {} informative tasks, 95% CI [{lo:+.1}, {hi:+.1}], smallest detectable {mdd:.1} points",
            halves["paired"]["tasks_informative"]
        )
    };
    println!("  smallest detectable week change {week_txt}; {halves_line}");
    for p in v["mean_score"].as_array().into_iter().flatten() {
        println!(
            "  mean score vs {}: n={} rho={} p={} q={} ci95={}",
            p["outcome"].as_str().unwrap_or_default(),
            p["n"],
            p["rho"],
            p["p"],
            p["q"],
            p["ci95"]
        );
    }
    if let Some(o) = v["outcomes"].as_object() {
        for (name, c) in o {
            println!("  outcome {name}: {c}");
        }
    }
    println!("  verdicts: {}", v["verdict_counts"]);
    for t in v["tasks"].as_array().into_iter().flatten() {
        let best = t["pairs"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|p| p["rho"].is_number())
            .map(|p| {
                format!(
                    "{} rho={} q={}",
                    p["outcome"].as_str().unwrap_or_default(),
                    p["rho"],
                    p["q"]
                )
            })
            .collect::<Vec<_>>()
            .join("; ");
        println!(
            "  {} {} ({} of {} over {} trees) {best}",
            t["task"].as_str().unwrap_or_default(),
            t["verdict"].as_str().unwrap_or_default(),
            t["passes"],
            t["grades"],
            t["trees"]
        );
    }
    println!("  seed {} over {} shuffles", v["seed"], v["shuffles"]);
}

#[cfg(test)]
#[path = "evals_predict_tests.rs"]
mod tests;
