//! The climb verdict: one preregistered comparison of two eval cohorts
//! (baseline vs trial) over the declared train/held-out split, reachable as
//! `fno doctor evals report --climb --baseline <cohort> --trial <cohort>`.
//! The method is the harness-fit study's: a change wins on held-out, never
//! on train; train rising while held-out stays flat is the revert shape.
//! The bar is the noise floor: measured from the baseline's A/A rows (two
//! repeats of the same tree) when they exist, else the preregistration's
//! named constant (evals/harness-fit/README.md, "Decision rules"), which
//! is also the scorecard router's bar. No threshold is tuned per run.
//!
//! The fold never prints a held-out prompt, trace, or per-attempt row - the
//! cells are cohort aggregates only, the same projection `qualify_aggregate`
//! keeps (a held-out claim is evidence, not a transcript).

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use serde_json::{json, Value};

use crate::agents_config::config_value_deep;
use crate::eval_attempt::{bank_same, load_cohorts_yaml, validate_cohorts};
use crate::evals_trend::{cohort_score, is_scored, read_rows, Row};

const EXIT_USAGE: i32 = 2;

/// The preregistered fallback bar (about 15 points at these n): the noise
/// floor when no A/A cohort measures one, and the scorecard router's bar,
/// because a node's merge rates have no repeat run to measure one.
pub(crate) const NOISE: f64 = 0.15;

/// A `propose` on a flat held-out needs the trial to cost at least this
/// much less per accepted change (the harness-fit tie rule: a tie counts
/// only with 30% lower cost per accepted change).
pub(crate) const COST_CUT: f64 = 0.30;

/// Scored held-out rows per cohort below which no verdict but
/// `underpowered` may read. The floor counts scored rows - the rows the
/// rates actually read - so substituted or otherwise excluded attempts
/// cannot satisfy it, and a gate-blocked run can never read past it. The
/// floor is the minimum evidence a comparison needs, not a noise cure: at
/// this n a 15-point difference still sits inside the Wilson interval,
/// which is why the verdict rules decide on interval separation, never on
/// the point move alone. The same floor applies to a train cell before the
/// overfit rule reads its rate.
const MIN_ROWS: usize = 20;

pub(crate) struct ClimbArgs<'a> {
    pub history: &'a str,
    pub baseline: &'a str,
    pub trial: &'a str,
    pub cohorts_yaml: Option<&'a str>,
    pub stage: Option<&'a str>,
    pub trial_lane: Option<&'a str>,
    pub prices: Option<&'a Value>,
    pub json_out: bool,
}

/// Run the climb fold and print its verdict. Refusals (missing or invalid
/// split, no git repo) exit 2 with the reason; every verdict itself is a
/// successful read and exits 0.
pub(crate) fn run_climb(args: &ClimbArgs) -> i32 {
    match climb_payload(args) {
        Ok(v) => {
            if args.json_out {
                println!("{}", serde_json::to_string_pretty(&v).unwrap_or_default());
            } else {
                print_verdict(&v);
            }
            0
        }
        Err(reason) => {
            eprintln!("evals-trend climb: {reason}");
            EXIT_USAGE
        }
    }
}

fn git_toplevel() -> Option<String> {
    let out = std::process::Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if s.is_empty() {
        None
    } else {
        Some(s)
    }
}

/// Which config scope declares a stage's `lanes`. The merged read lets a
/// project declaration outrank global (config_candidates order), and
/// `fno config set` defaults to global, so an accept line that copies
/// project-scoped lanes into global would change nothing and duplicate the
/// list. Both scopes declaring is ambiguous and prints no accept line.
enum LanesScope {
    None,
    Global,
    Project,
    Ambiguous,
}

fn lanes_scope(cwd: &std::path::Path, stage: &str) -> LanesScope {
    let global_declares =
        crate::agents_config::config_lookup_global(&["agents", "profiles", stage, "lanes"])
            .is_some();
    let global_path = crate::agents_config::global_config_path();
    let project_declares = crate::agents_config::config_candidates(cwd)
        .iter()
        .filter(|p| Some(p.as_path()) != global_path.as_deref())
        .any(|p| {
            std::fs::read_to_string(p)
                .ok()
                .and_then(|c| crate::agents_config::parse_config(&c))
                .is_some_and(|t| {
                    t.get("agents")
                        .and_then(|a| a.get("profiles"))
                        .and_then(|p| p.get(stage))
                        .and_then(|s| s.get("lanes"))
                        .is_some()
                })
        });
    match (project_declares, global_declares) {
        (false, false) => LanesScope::None,
        (false, true) => LanesScope::Global,
        (true, false) => LanesScope::Project,
        (true, true) => LanesScope::Ambiguous,
    }
}

/// The declared lanes of one stage profile, verbatim (a string stays a
/// string; a table converts), across global and project config. No lanes is
/// an empty list: the accept line then sets the list fresh, which the route
/// walk reads as "this profile's lanes".
fn current_lanes(stage: &str) -> Vec<Value> {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    config_value_deep(&cwd, &["agents", "profiles", stage])
        .as_ref()
        .and_then(|t| t.get("lanes"))
        .and_then(|l| l.as_array())
        .map(|items| {
            items
                .iter()
                .map(crate::route_gather::lanes_raw_entry)
                .collect()
        })
        .unwrap_or_default()
}

/// The one-step accept line for a `propose`: the trial lane first in the
/// stage profile, then whatever lanes it already had. It also sets
/// `on_exhausted = degrade`: an exhausted lane defaults to refusing spawns,
/// which would turn the promoted lane into a wall the next time the account
/// runs dry. Both writes ride `fno config set`, schema-validated and
/// atomic, into the scope that declares the lanes today (`--local` for a
/// project declaration, global otherwise). The user runs the line; the fold
/// never edits config itself. `None` is the ambiguous case (both scopes
/// declare): no line prints rather than a line that cannot win.
pub(crate) fn accept_line(stage: &str, trial_lane: &str) -> Option<String> {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let local_flag = match lanes_scope(&cwd, stage) {
        LanesScope::Ambiguous => return None,
        LanesScope::Project => " --local",
        LanesScope::None | LanesScope::Global => "",
    };
    let mut lanes = vec![json!(trial_lane)];
    lanes.extend(
        current_lanes(stage)
            .into_iter()
            .filter(|l| l.as_str() != Some(trial_lane)),
    );
    let arr = serde_json::to_string(&Value::Array(lanes)).unwrap_or_default();
    // The array travels single-quoted; JSON's only single-quote is inside a
    // lane name, so the standard escape keeps the line paste-safe.
    let quoted = arr.replace('\'', "'\\''");
    Some(format!(
        "fno config set{local_flag} agents.profiles.{stage}.lanes '{quoted}' && \
         fno config set{local_flag} agents.profiles.{stage}.on_exhausted degrade"
    ))
}

fn cell_rate(cell: &Value) -> f64 {
    cell.get("pass_rate").and_then(Value::as_f64).unwrap_or(0.0)
}

/// The underpowered reason names the exclusion, not just the shortfall:
/// "trial 0" hides WHY rows dropped, and a lane that substituted every
/// attempt needs more rows the way a hole needs more water. Sourced from
/// the same cell the power gate reads.
fn subst_note(cell: &Value) -> String {
    let substituted = cell
        .get("lane_status_counts")
        .and_then(|c| c.get("substituted"))
        .and_then(Value::as_u64)
        .unwrap_or(0);
    if substituted == 0 {
        return String::new();
    }
    let attempts = cell.get("attempts").and_then(Value::as_u64).unwrap_or(0);
    let scored = cell.get("scored").and_then(Value::as_u64).unwrap_or(0);
    format!("; {substituted} of {attempts} substituted, scored {scored}")
}

fn cell_scored(cell: &Value) -> usize {
    cell.get("scored").and_then(Value::as_u64).unwrap_or(0) as usize
}

/// The cell's Wilson bounds as (lo, hi); `None` below the scored floor,
/// where an interval comparison would decide on noise.
fn cell_ci(cell: &Value) -> Option<(f64, f64)> {
    if cell_scored(cell) < MIN_ROWS {
        return None;
    }
    let ci = cell.get("pass_rate_ci95")?.as_array()?;
    Some((ci.first()?.as_f64()?, ci.get(1)?.as_f64()?))
}

/// Cost per accepted change of a cell; `None` is unmeasured or zero
/// accepted, and either way the cost rule cannot read it.
fn cell_cost(cell: &Value) -> Option<f64> {
    cell.get("cost")
        .and_then(|c| c.get("dollars_per_accepted"))
        .and_then(Value::as_f64)
}

fn climb_payload(args: &ClimbArgs) -> Result<Value, String> {
    // The split. Its pin needs a repo to diff against, so a climb outside a
    // git checkout refuses before reading anything.
    let repo = git_toplevel()
        .ok_or_else(|| "no git repository at cwd; the split pin needs one".to_string())?;
    let decl_path = match args.cohorts_yaml {
        Some(p) => p.to_string(),
        None => PathBuf::from(&repo)
            .join("evals/bank/cohorts.yaml")
            .to_string_lossy()
            .into_owned(),
    };
    let decl = load_cohorts_yaml(&decl_path)?;
    let (ok, errors, _roles) = validate_cohorts(&decl, None, None);
    if !ok {
        return Err(errors.join("; "));
    }
    let pin = decl
        .get("bank_rev")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();

    // Role of each task id: train tunes, everything held back from tuning
    // (validation, qualification) is held-out evidence.
    let mut role_of: BTreeMap<&str, &str> = BTreeMap::new();
    for (role, target) in [
        ("train", "train"),
        ("validation", "heldout"),
        ("qualification", "heldout"),
    ] {
        if let Some(list) = decl.get(role).and_then(Value::as_array) {
            for id in list.iter().filter_map(Value::as_str) {
                role_of.insert(id, target);
            }
        }
    }

    // Keep the two cohorts' rows. Task content must match the pin: rows
    // carry the run-time HEAD, so exact rev equality would drop almost
    // every row - the diff is over bank content, cached per distinct rev.
    let rows = read_rows(args.history, None, None);
    let mut cells: BTreeMap<(&str, &str), Vec<&Row>> = BTreeMap::new();
    let mut wrong_rev = 0usize;
    let mut unsplit = 0usize;
    let mut rev_cache: BTreeMap<String, bool> = BTreeMap::new();
    for r in &rows {
        let Some(cohort) = r.experiment_id.as_deref() else {
            continue;
        };
        if cohort != args.baseline && cohort != args.trial {
            continue;
        }
        let same = match &r.bank_rev {
            Some(rev) => *rev_cache
                .entry(rev.clone())
                .or_insert_with(|| bank_same(&repo, rev, &pin)),
            None => false,
        };
        if !same {
            wrong_rev += 1;
            continue;
        }
        match role_of.get(r.task_id.as_str()) {
            Some(&role) => cells.entry((cohort, role)).or_default().push(r),
            None => unsplit += 1,
        }
    }

    let empty: Vec<&Row> = Vec::new();
    let score = |cohort: &str, role: &str| {
        cohort_score(
            cells
                .get(&(cohort, role))
                .map(Vec::as_slice)
                .unwrap_or(&empty),
            args.prices,
        )
    };
    let base_train = score(args.baseline, "train");
    let base_heldout = score(args.baseline, "heldout");
    let trial_train = score(args.trial, "train");
    let trial_heldout = score(args.trial, "heldout");

    // The measured bar: an A/A read over the baseline's held-out rows at
    // the smaller repeat count of the two cohorts. Every held-out task is
    // in the pairing, constants at delta 0, so the floor stays in the
    // pooled-rate unit `cell_rate` reads. No A/A reads the preregistered
    // constant, and today's verdicts.
    let heldout_tasks: BTreeSet<String> = cells
        .iter()
        .filter(|((_, role), _)| *role == "heldout")
        .flat_map(|(_, v)| v.iter().map(|r| r.task_id.clone()))
        .collect();
    let rep_count = |cohort: &str| -> usize {
        cells.get(&(cohort, "heldout")).map_or(1, |rows| {
            rows.iter()
                .filter(|r| is_scored(r))
                .map(|r| r.raw_i64("repeat_index").unwrap_or(0))
                .collect::<BTreeSet<i64>>()
                .len()
                .max(1)
        })
    };
    let r = rep_count(args.baseline).min(rep_count(args.trial));
    let base_cell_rows = cells
        .get(&(args.baseline, "heldout"))
        .cloned()
        .unwrap_or_default();
    let aa = crate::evals_power::aa_floor(&base_cell_rows, &heldout_tasks, r);
    let floor = match &aa {
        Ok(f) => f.floor_points / 100.0,
        Err(_) => NOISE,
    };
    let source = match &aa {
        Ok(_) => "aa_measured",
        Err(_) => "preregistered",
    };

    // The power object rides under heldout, the same shape compare emits:
    // paired per-task deltas over held-out tasks, with the measured floor.
    let cell_map = |cohort: &str| -> BTreeMap<String, (usize, usize)> {
        let mut m: BTreeMap<String, (usize, usize)> = BTreeMap::new();
        if let Some(rows) = cells.get(&(cohort, "heldout")) {
            for t in &heldout_tasks {
                let mut acc = (0usize, 0usize);
                for r in rows.iter().filter(|r| is_scored(r) && r.task_id == *t) {
                    acc.0 += 1;
                    acc.1 += r.pass as usize;
                }
                m.insert(t.clone(), acc);
            }
        }
        m
    };
    let base_map = cell_map(args.baseline);
    let trial_map = cell_map(args.trial);
    let p = crate::evals_power::paired(&base_map, &trial_map);
    let mut rng = crate::evals_power::Rng(crate::evals_power::SEED);
    let power = match &aa {
        Ok(f) => crate::evals_power::power_block(&p, &mut rng, Some(f), None),
        Err(skip) => crate::evals_power::power_block(&p, &mut rng, None, Some(*skip)),
    };

    // The verdict chain. The power gate reads first: every later label is a
    // claim about a difference, and below the floor a difference is not
    // measurable (the regression and propose shapes below can only fire on
    // 20+ scored rows per held-out cell). A difference also reads only when
    // the cells' 95 percent intervals separate: at this n the 15-point move
    // alone still sits inside the intervals, so the point move never
    // decides by itself.
    let train_readable =
        cell_scored(&base_train) >= MIN_ROWS && cell_scored(&trial_train) >= MIN_ROWS;
    let base_ci = cell_ci(&base_heldout);
    let trial_ci = cell_ci(&trial_heldout);
    // True when the first cell's lower bound sits above the second cell's
    // upper bound: the two intervals do not overlap and the move is real.
    let separated = |higher: Option<(f64, f64)>, lower: Option<(f64, f64)>| {
        matches!(
            (higher, lower),
            (Some((hi_lo, _)), Some((_, lo_hi))) if hi_lo > lo_hi
        )
    };
    let (verdict, reason): (&str, String) = if cell_scored(&base_heldout) < MIN_ROWS
        || cell_scored(&trial_heldout) < MIN_ROWS
    {
        (
            "underpowered",
            format!(
                "held-out needs {MIN_ROWS} scored rows per cohort (baseline {}, trial {}){}{}",
                cell_scored(&base_heldout),
                cell_scored(&trial_heldout),
                subst_note(&base_heldout),
                subst_note(&trial_heldout),
            ),
        )
    } else if cell_rate(&base_heldout) - cell_rate(&trial_heldout) > floor
        && separated(base_ci, trial_ci)
    {
        (
            "regression",
            format!(
                "trial held-out fell {:.0} points below baseline and the intervals separate",
                (cell_rate(&base_heldout) - cell_rate(&trial_heldout)) * 100.0,
            ),
        )
    } else if train_readable
        && cell_rate(&trial_train) - cell_rate(&base_train) > floor
        && cell_rate(&trial_heldout) - cell_rate(&base_heldout) <= floor
    {
        (
            "overfit",
            format!(
                "train rose {:.0} points while held-out stayed flat; revert per the preregistration",
                (cell_rate(&trial_train) - cell_rate(&base_train)) * 100.0,
            ),
        )
    } else if cell_cost(&base_heldout).is_none() || cell_cost(&trial_heldout).is_none() {
        (
            "unmeasured_cost",
            "cost per accepted is unmeasured for a held-out cell; every scored row needs usage and a --prices line".into(),
        )
    } else {
        let heldout_move = cell_rate(&trial_heldout) - cell_rate(&base_heldout);
        let (base_cost, trial_cost) = (
            cell_cost(&base_heldout).unwrap_or_default(),
            cell_cost(&trial_heldout).unwrap_or_default(),
        );
        // The tie arm accepts a trial up to the noise floor BELOW the
        // baseline rate only when even its interval's lower end stays
        // within that floor: a trial whose lower bound sits far under the
        // baseline rate cannot read as a win.
        let trial_lo = trial_ci.map(|(lo, _)| lo).unwrap_or_default();
        let flat_and_cheaper = heldout_move >= -floor
            && trial_lo >= cell_rate(&base_heldout) - floor
            && trial_cost <= base_cost * (1.0 - COST_CUT);
        let better_not_pricier =
            heldout_move > floor && separated(trial_ci, base_ci) && trial_cost <= base_cost;
        if flat_and_cheaper || better_not_pricier {
            // A zero baseline cost has no percent cut to name; the reason
            // then carries the absolute dollars per accepted instead.
            let cost_note = if base_cost > 0.0 {
                format!(
                    "cost per accepted {:.0}% lower",
                    (1.0 - trial_cost / base_cost) * 100.0
                )
            } else {
                format!("cost per accepted ${trial_cost:.4} at no higher cost")
            };
            (
                "propose",
                format!(
                    "held-out moved {:+.0} points, {cost_note}",
                    heldout_move * 100.0
                ),
            )
        } else {
            let mut why = "held-out flat and the cost per accepted is not lower enough to count a tie as a win".to_string();
            if !train_readable {
                why.push_str(&format!(
                    "; train cells under {MIN_ROWS} scored rows, so overfit is not judged"
                ));
            }
            ("hold", why)
        }
    };

    let mut payload = json!({
        "verdict": verdict,
        "reason": reason,
        "baseline": args.baseline,
        "trial": args.trial,
        "train": {"baseline": base_train, "trial": trial_train},
        "heldout": {
            "baseline": base_heldout,
            "trial": trial_heldout,
            "power": power,
        },
        "noise_floor": floor * 100.0,
        "noise_source": source,
        // Lane verification at a glance per cell: a baseline scored on
        // unverified rows while the trial is excluded on verified mismatch
        // is an asymmetric comparison, and the payload must say so where a
        // reader of one cell will not look for it.
        "lane_status_by_cell": {
            "train": {
                "baseline": base_train.get("lane_status_counts").cloned().unwrap_or(json!({})),
                "trial": trial_train.get("lane_status_counts").cloned().unwrap_or(json!({})),
            },
            "heldout": {
                "baseline": base_heldout.get("lane_status_counts").cloned().unwrap_or(json!({})),
                "trial": trial_heldout.get("lane_status_counts").cloned().unwrap_or(json!({})),
            },
        },
        "wrong_rev": wrong_rev,
        "unsplit": unsplit,
    });
    if verdict == "propose" {
        if let (Some(stage), Some(lane)) = (args.stage, args.trial_lane) {
            if let Some(line) = accept_line(stage, lane) {
                payload["accept"] = json!(line);
            }
        }
    }
    Ok(payload)
}

fn print_verdict(v: &Value) {
    println!(
        "Climb verdict: {}",
        v["verdict"].as_str().unwrap_or_default()
    );
    println!("  reason: {}", v["reason"].as_str().unwrap_or_default());
    let floor = v["noise_floor"].as_f64().unwrap_or(15.0);
    let source = v["noise_source"].as_str().unwrap_or("preregistered");
    let cohort = v["heldout"]["power"]["aa"]["cohort"].as_str();
    let cohort_note = cohort.map(|c| format!(", {c}")).unwrap_or_default();
    let mdd = v["heldout"]["power"]["mdd_points"].as_f64();
    let mdd_txt = mdd
        .map(|m| format!("{m:.1} points"))
        .unwrap_or_else(|| "unmeasured".to_string());
    println!(
        "  noise floor: {floor:.1} points ({source}{cohort_note}), smallest detectable {mdd_txt}"
    );
    for (role, label) in [("train", "train"), ("heldout", "held-out")] {
        for side in ["baseline", "trial"] {
            let cell = &v[role][side];
            let ci = cell["pass_rate_ci95"].as_array();
            let (lo, hi) = match ci {
                Some(a) => (
                    a.first().and_then(Value::as_f64).unwrap_or(0.0),
                    a.get(1).and_then(Value::as_f64).unwrap_or(0.0),
                ),
                None => (0.0, 0.0),
            };
            let cost = match cell_cost(cell) {
                Some(per) => format!("${per:.4}/accepted"),
                None => "cost unmeasured".to_string(),
            };
            println!(
                "  {label:8} {side:8} graded={} scored={} rate={:.1}% [{:.1},{:.1}] {cost}",
                cell["graded"],
                cell["scored"],
                cell_rate(cell) * 100.0,
                lo * 100.0,
                hi * 100.0,
            );
        }
    }
    println!(
        "  excluded: wrong_rev={} unsplit={}",
        v["wrong_rev"], v["unsplit"],
    );
    if let Some(accept) = v.get("accept").and_then(Value::as_str) {
        println!("  accept: {accept}");
    }
}

#[cfg(test)]
mod tests;
