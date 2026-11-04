//! The power block behind the eval reports: a paired per-task fold, a task
//! bootstrap, the smallest detectable change, and the measured A/A noise
//! floor. `report --compare`, `--climb` and `--predict` attach it to their
//! aggregate rates so a flat line says how large a change it could have
//! seen - the difference between "no change" and "this run could not see a
//! change this size". One module holds the statistics so three readers
//! quote one floor, never three.

use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};

use serde_json::{json, Value};

use crate::evals_trend::{is_scored, Row};

/// Below this many informative tasks a paired delta is a rumour: the block
/// reads `underpowered` whatever the numbers say.
const MIN_INFORMATIVE: usize = 5;

/// Two-sided alpha 0.05 (1.96) plus power 0.80 (0.84): the multiplier in
/// the smallest detectable change.
const MDD_Z: f64 = 2.80;

/// Resamples for the task bootstrap. 2,000 over a few dozen tasks keeps
/// the 2.5th and 97.5th percentiles stable at the printed precision and
/// costs microseconds.
pub(crate) const RESAMPLES: usize = 2_000;

/// An A/A cohort pairs fewer tasks than this only by accident, and a floor
/// measured on fewer is a claim about noise the fold never saw.
pub(crate) const AA_MIN_TASKS: usize = 5;

/// splitmix64: a seeded stream with no crate behind it, so the printed
/// seed reproduces every interval.
pub(crate) const SEED: u64 = 20_261_102;

pub(crate) struct Rng(pub(crate) u64);

impl Rng {
    pub(crate) fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
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

/// The smallest change this fold could have seen, in the units of `sd`:
/// 2.80 x sd / sqrt(repeats x tasks). `repeats = 1` reads the detectable
/// change of a comparison's own per-task deltas.
pub(crate) fn mdd(sd: f64, n: usize, repeats: usize) -> f64 {
    MDD_Z * sd / ((repeats.max(1) * n.max(1)) as f64).sqrt()
}

/// The paired per-task fold behind the power block.
pub(crate) struct Paired {
    /// (task, a rate, b rate, delta), the informative tasks only.
    pub(crate) deltas: Vec<(String, f64, f64, f64)>,
    pub(crate) mean: Option<f64>,
    /// Sample sd of the deltas: the raw material of the own-delta MDD.
    pub(crate) sd: Option<f64>,
    pub(crate) informative: usize,
    pub(crate) constant: usize,
    pub(crate) error: usize,
}

/// Per task graded on both sides, delta = b rate minus a rate. A task
/// whose every grade passes, or every grade fails, on both sides is
/// constant and stays out of the score; a task with no grade on a side
/// counts as an error.
pub(crate) fn paired(
    a: &BTreeMap<String, (usize, usize)>,
    b: &BTreeMap<String, (usize, usize)>,
) -> Paired {
    let mut deltas = Vec::new();
    let (mut constant, mut error) = (0usize, 0usize);
    for (task, (ga, pa)) in a {
        let Some((gb, pb)) = b.get(task) else {
            continue;
        };
        if *ga == 0 || *gb == 0 {
            error += 1;
            continue;
        }
        let (ra, rb) = (*pa as f64 / *ga as f64, *pb as f64 / *gb as f64);
        if (*pa == *ga && *pb == *gb) || (*pa == 0 && *pb == 0) {
            constant += 1;
            continue;
        }
        deltas.push((task.clone(), ra, rb, rb - ra));
    }
    let vals: Vec<f64> = deltas.iter().map(|d| d.3).collect();
    Paired {
        mean: mean(&vals),
        sd: sd(&vals),
        informative: deltas.len(),
        constant,
        error,
        deltas,
    }
}

/// Task bootstrap: resample the deltas with replacement, keep each
/// resample's mean, return the 2.5th and 97.5th percentiles. The same seed
/// reads the same interval, so a rerun never moves a printed bound.
pub(crate) fn bootstrap_ci(deltas: &[f64], rng: &mut Rng, resamples: usize) -> Option<(f64, f64)> {
    if deltas.len() < 2 {
        return None;
    }
    let mut means = Vec::with_capacity(resamples);
    for _ in 0..resamples {
        let mut acc = 0.0;
        for _ in 0..deltas.len() {
            acc += deltas[(rng.next() % deltas.len() as u64) as usize];
        }
        means.push(acc / deltas.len() as f64);
    }
    means.sort_by(|a, b| a.partial_cmp(b).unwrap_or(Ordering::Equal));
    let at = |q: f64| means[((means.len() - 1) as f64 * q).round() as usize];
    Some((at(0.025), at(0.975)))
}

/// Why no A/A floor was measured.
#[derive(Clone, Copy)]
pub(crate) enum AaSkip {
    /// Fewer than `AA_MIN_TASKS` tasks carry grades at repeat 0 and 1.
    TooFewTasks,
    /// Enough tasks paired but none flipped: a zero sd is an A/A that saw
    /// nothing, never a floor of zero (it would let any move read as real).
    NoFlips,
}

impl AaSkip {
    pub(crate) fn name(self) -> &'static str {
        match self {
            AaSkip::TooFewTasks => "too_few_tasks",
            AaSkip::NoFlips => "no_flips",
        }
    }
}

pub(crate) struct AaFloor {
    pub(crate) cohort: String,
    pub(crate) tasks: usize,
    pub(crate) floor_points: f64,
}

/// The measured noise floor from an A/A cohort: per task, the repeat-0
/// rate minus the repeat-1 rate, both single runs. The cohort with the
/// most paired tasks wins (a tie keeps the first in id order). The floor
/// is the detectable change at `repeats` grades per task; everything is
/// returned in points.
pub(crate) fn aa_floor(
    rows: &[&Row],
    tasks: &BTreeSet<String>,
    repeats: usize,
) -> Result<AaFloor, AaSkip> {
    let mut rep0: BTreeMap<&str, BTreeMap<&str, (usize, usize)>> = BTreeMap::new();
    let mut rep1: BTreeMap<&str, BTreeMap<&str, (usize, usize)>> = BTreeMap::new();
    for r in rows {
        if !is_scored(r) {
            continue;
        }
        let Some(cohort) = r.experiment_id.as_deref() else {
            continue;
        };
        let bucket = match r.raw_i64("repeat_index").unwrap_or(0) {
            0 => &mut rep0,
            1 => &mut rep1,
            _ => continue,
        };
        let e = bucket
            .entry(cohort)
            .or_default()
            .entry(r.task_id.as_str())
            .or_default();
        e.0 += 1;
        e.1 += r.pass as usize;
    }
    let cohorts: BTreeSet<&str> = rep0.keys().chain(rep1.keys()).copied().collect();
    let mut best: Option<(usize, &str, Vec<f64>)> = None;
    for cohort in cohorts {
        let mut deltas = Vec::new();
        for task in tasks {
            if let (Some((ga, pa)), Some((gb, pb))) = (
                rep0.get(cohort).and_then(|t| t.get(task.as_str())),
                rep1.get(cohort).and_then(|t| t.get(task.as_str())),
            ) {
                if *ga > 0 && *gb > 0 {
                    deltas.push(*pa as f64 / *ga as f64 - *pb as f64 / *gb as f64);
                }
            }
        }
        let better = match &best {
            None => true,
            Some((n, _, _)) => deltas.len() > *n,
        };
        if better {
            best = Some((deltas.len(), cohort, deltas));
        }
    }
    let Some((n, cohort, deltas)) = best else {
        return Err(AaSkip::TooFewTasks);
    };
    if n < AA_MIN_TASKS {
        return Err(AaSkip::TooFewTasks);
    }
    if !deltas.iter().any(|d| *d != 0.0) {
        return Err(AaSkip::NoFlips);
    }
    let Some(spread) = sd(&deltas) else {
        return Err(AaSkip::NoFlips);
    };
    Ok(AaFloor {
        cohort: cohort.to_string(),
        tasks: n,
        floor_points: mdd(spread, n, repeats) * 100.0,
    })
}

/// The `power` object the reports attach beside every aggregate rate. The
/// verdict compares the paired delta with the noise floor: `underpowered`
/// under five informative tasks, `beats_noise` when the delta clears the
/// floor and the interval excludes zero, else `inside_noise`. With no
/// measured A/A the floor is the preregistered constant and the MDD is the
/// one the comparison's own deltas imply (one grade per side).
pub(crate) fn power_block(
    paired: &Paired,
    rng: &mut Rng,
    aa: Option<&AaFloor>,
    aa_skipped: Option<AaSkip>,
) -> Value {
    let (floor_points, source) = match aa {
        Some(f) => (f.floor_points, "aa_measured"),
        None => (crate::evals_climb::NOISE * 100.0, "preregistered"),
    };
    let mdd_points = match aa {
        Some(f) => Some(f.floor_points),
        None => paired.sd.map(|s| mdd(s, paired.informative, 1) * 100.0),
    };
    let vals: Vec<f64> = paired.deltas.iter().map(|d| d.3).collect();
    let ci = bootstrap_ci(&vals, rng, RESAMPLES).map(|(lo, hi)| json!([lo * 100.0, hi * 100.0]));
    let ci_excludes_zero = matches!(&ci, Some(arr) if {
        let lo = arr.get(0).and_then(Value::as_f64);
        let hi = arr.get(1).and_then(Value::as_f64);
        lo.is_some_and(|v| v > 0.0) || hi.is_some_and(|v| v < 0.0)
    });
    let beats = paired.mean.is_some_and(|m| m.abs() * 100.0 > floor_points) && ci_excludes_zero;
    let verdict = if paired.informative < MIN_INFORMATIVE {
        "underpowered"
    } else if beats {
        "beats_noise"
    } else {
        "inside_noise"
    };
    let deltas: Vec<Value> = paired
        .deltas
        .iter()
        .map(|(task, ra, rb, d)| json!({"task": task, "a_rate": ra, "b_rate": rb, "delta": d}))
        .collect();
    let mut block = json!({
        "verdict": verdict,
        "paired": {
            "mean_delta_points": paired.mean.map(|m| m * 100.0),
            "ci95_points": ci.unwrap_or(Value::Null),
            "tasks_informative": paired.informative,
            "tasks_constant": paired.constant,
            "tasks_error": paired.error,
            "deltas": deltas,
        },
        "mdd_points": mdd_points,
        "noise_floor_points": floor_points,
        "noise_source": source,
        "aa": aa.map(|f| json!({"cohort": f.cohort, "repeats": [0, 1], "tasks": f.tasks}))
            .unwrap_or(Value::Null),
        "seed": SEED,
        "resamples": RESAMPLES,
    });
    if let Some(skip) = aa_skipped {
        block["aa_skipped"] = json!(skip.name());
    }
    block
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rows_from_jsonl(lines: &[Value]) -> Vec<Row> {
        let mut f = tempfile::NamedTempFile::new().unwrap();
        for v in lines {
            use std::io::Write as _;
            writeln!(f, "{v}").unwrap();
        }
        crate::evals_trend::read_rows(f.path().to_str().unwrap(), None, None)
    }

    #[test]
    fn bootstrap_ci_is_seeded_and_holds_the_mean() {
        let deltas = [0.0, 1.0, 0.0, 1.0, 1.0];
        let first = bootstrap_ci(&deltas, &mut Rng(SEED), RESAMPLES).unwrap();
        let second = bootstrap_ci(&deltas, &mut Rng(SEED), RESAMPLES).unwrap();
        assert_eq!(first, second, "the same seed must read the same interval");
        let m = mean(&deltas).unwrap();
        assert!(first.0 <= m && m <= first.1, "mean {m} outside {first:?}");
    }

    #[test]
    fn an_aa_with_under_five_paired_tasks_reads_preregistered() {
        // Four tasks, each graded once at repeats 0 and 1: one pairing
        // short of the floor, so the block must fall back and say so.
        let mut lines = Vec::new();
        for i in 0..4 {
            for rep in 0..2 {
                lines.push(json!({
                    "task_id": format!("t{i}"), "tier": "capability", "pass": i % 2 == 0,
                    "status": "graded", "experiment_id": "aa", "repeat_index": rep,
                }));
            }
        }
        let rows = rows_from_jsonl(&lines);
        let refs: Vec<&Row> = rows.iter().collect();
        let tasks: BTreeSet<String> = (0..4).map(|i| format!("t{i}")).collect();
        assert!(matches!(
            aa_floor(&refs, &tasks, 3),
            Err(AaSkip::TooFewTasks)
        ));
        let mut a = BTreeMap::new();
        let mut b = BTreeMap::new();
        for i in 0..4 {
            let (g, p) = if i % 2 == 0 { (3, 3) } else { (3, 1) };
            a.insert(format!("t{i}"), (g, p));
            b.insert(format!("t{i}"), (g, if i % 2 == 0 { 3 } else { 2 }));
        }
        let p = paired(&a, &b);
        let block = power_block(&p, &mut Rng(SEED), None, Some(AaSkip::TooFewTasks));
        assert_eq!(block["noise_source"], "preregistered", "{block}");
        assert_eq!(block["aa"], Value::Null, "{block}");
        assert_eq!(block["aa_skipped"], "too_few_tasks", "{block}");
    }
}
