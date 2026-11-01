//! `fno doctor evals report --compare A --to B`: one status per task on each
//! side, then what changed. The per-task lists print before the pass rate
//! because a rate hides which behaviour moved.
//!
//! A side names a cohort, else a variant. Only a scored row is a grade, so a
//! plumbing row can never read as a pass. A task needs `repeats` grades for a
//! status; with fewer it reads `error`. A flaky status names its noise from
//! the tree hashes. A golden whose failure type is not proved lists as
//! unproved, and it cannot fail the compare. Each side also names the rows
//! its lane evidence excludes and prints tokens and dollars per accepted
//! result, or `unmeasured`.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use serde_json::{json, Value};

use crate::evals::bank::TaskSpec;
use crate::evals::failure_types::{prove, Catalog, Proof};
use crate::evals_trend::{cohort_score, is_scored, lane_excludes, read_rows, Row};

const EXIT_REGRESSED: i32 = 1;
const EXIT_USAGE: i32 = 2;

pub(crate) struct CompareArgs<'a> {
    pub history: &'a str,
    pub a: &'a str,
    pub b: &'a str,
    pub repeats: usize,
    pub since: Option<usize>,
    pub json_out: bool,
    pub bank: &'a Path,
    pub prices: Option<&'a Value>,
}

#[derive(Clone, Copy, PartialEq)]
enum Status {
    AlwaysPass,
    AlwaysFail,
    Flaky,
    Error,
}

impl Status {
    fn name(self) -> &'static str {
        match self {
            Status::AlwaysPass => "always_pass",
            Status::AlwaysFail => "always_fail",
            Status::Flaky => "flaky",
            Status::Error => "error",
        }
    }
}

struct Side {
    status: Status,
    grades: usize,
    passes: usize,
    noise: Option<&'static str>,
}

/// Why a flaky task flaked. One tree with both grades proves grader noise,
/// whatever the other rows say. Otherwise a row with no hash leaves it unknown,
/// and differing trees mean the agent did different work.
fn flake_noise(newest: &[&&Row]) -> &'static str {
    let mut grades_by_tree: BTreeMap<&str, BTreeSet<bool>> = BTreeMap::new();
    let mut unhashed = false;
    for row in newest {
        match row.tree_hash() {
            Some(tree) => {
                grades_by_tree.entry(tree).or_default().insert(row.pass);
            }
            None => unhashed = true,
        }
    }
    if grades_by_tree.values().any(|grades| grades.len() > 1) {
        "grader_noise"
    } else if unhashed {
        "unknown"
    } else {
        "agent_noise"
    }
}

/// The rows a side names: its cohort when one matches, else its variant.
fn side_rows<'a>(rows: &'a [Row], name: &str) -> Vec<&'a Row> {
    let cohort: Vec<&Row> = rows
        .iter()
        .filter(|r| r.experiment_id.as_deref() == Some(name))
        .collect();
    if !cohort.is_empty() {
        return cohort;
    }
    rows.iter()
        .filter(|r| r.variant.as_deref().unwrap_or("baseline") == name)
        .collect()
}

/// The newest `repeats` grades decide, so an old run of a reused name cannot
/// outvote the run being compared.
fn fold_side(rows: &[&Row], task: &str, repeats: usize) -> Side {
    let grades: Vec<&&Row> = rows
        .iter()
        .filter(|r| r.task_id == task && is_scored(r))
        .collect();
    let newest = &grades[grades.len().saturating_sub(repeats)..];
    let passes = newest.iter().filter(|r| r.pass).count();
    let status = if grades.len() < repeats {
        Status::Error
    } else if passes == repeats {
        Status::AlwaysPass
    } else if passes == 0 {
        Status::AlwaysFail
    } else {
        Status::Flaky
    };
    Side {
        status,
        grades: grades.len(),
        passes,
        noise: (status == Status::Flaky).then(|| flake_noise(newest)),
    }
}

const FIXED: &str = "Fixed";
const REGRESSIONS: &str = "Regressions";
const NOW_FLAKY: &str = "Now flaky";
const STILL_FAILING: &str = "Still failing";
const ERRORS: &str = "Errors";
const UNPROVED: &str = "Unproved";
const LISTS: [&str; 6] = [
    FIXED,
    REGRESSIONS,
    NOW_FLAKY,
    STILL_FAILING,
    ERRORS,
    UNPROVED,
];

/// The list a task lands on, or None when it passes on both sides.
fn bucket(a: Status, b: Status) -> Option<&'static str> {
    use Status::{AlwaysFail, AlwaysPass, Error, Flaky};
    match (a, b) {
        (Error, _) | (_, Error) => Some(ERRORS),
        (AlwaysPass, AlwaysPass) => None,
        (_, AlwaysPass) => Some(FIXED),
        (AlwaysPass, AlwaysFail) => Some(REGRESSIONS),
        (AlwaysPass, Flaky) => Some(NOW_FLAKY),
        _ => Some(STILL_FAILING),
    }
}

fn revs(rows: &[&Row]) -> Vec<String> {
    let revs: BTreeSet<&str> = rows.iter().filter_map(|r| r.bank_rev.as_deref()).collect();
    revs.into_iter().map(str::to_string).collect()
}

/// The fold behind the report. The Err names a side with no rows, so a typo
/// never prints as every task in error.
pub(crate) fn fold(
    rows: &[Row],
    a: &str,
    b: &str,
    repeats: usize,
    unproved: &BTreeSet<String>,
) -> Result<Value, String> {
    let (rows_a, rows_b) = (side_rows(rows, a), side_rows(rows, b));
    for (name, side) in [(a, &rows_a), (b, &rows_b)] {
        if side.is_empty() {
            return Err(format!(
                "evals compare: no history rows name cohort or variant '{name}'"
            ));
        }
    }
    let tasks: BTreeSet<&str> = rows_a
        .iter()
        .chain(rows_b.iter())
        .map(|r| r.task_id.as_str())
        .collect();
    let mut lists: BTreeMap<&str, Vec<Value>> = LISTS.iter().map(|l| (*l, Vec::new())).collect();
    let mut counts_a: BTreeMap<&str, usize> = BTreeMap::new();
    let mut counts_b: BTreeMap<&str, usize> = BTreeMap::new();
    let (mut pass_a, mut den_a, mut pass_b, mut den_b) = (0, 0, 0, 0);
    let mut breaking = false;
    for task in tasks {
        let (sa, sb) = (
            fold_side(&rows_a, task, repeats),
            fold_side(&rows_b, task, repeats),
        );
        *counts_a.entry(sa.status.name()).or_default() += 1;
        *counts_b.entry(sb.status.name()).or_default() += 1;
        pass_a += sa.passes;
        den_a += sa.grades.min(repeats);
        pass_b += sb.passes;
        den_b += sb.grades.min(repeats);
        let is_unproved = unproved.contains(task);
        breaking |= !is_unproved
            && ((sa.status == Status::AlwaysPass && sb.status == Status::AlwaysFail)
                || (sa.grades > 0 && sb.status == Status::Error));
        if let Some(list) = bucket(sa.status, sb.status) {
            let list = if is_unproved { UNPROVED } else { list };
            let entry = json!({
                "task": task,
                "a": sa.status.name(),
                "b": sb.status.name(),
                "a_noise": sa.noise,
                "b_noise": sb.noise,
                "a_grades": sa.grades,
                "b_grades": sb.grades,
            });
            lists.entry(list).or_default().push(entry);
        }
    }
    let rate = |p: usize, d: usize| json!({"passes": p, "grades": d});
    Ok(json!({
        "a": {"name": a, "revs": revs(&rows_a), "counts": counts_a, "pass_rate": rate(pass_a, den_a)},
        "b": {"name": b, "revs": revs(&rows_b), "counts": counts_b, "pass_rate": rate(pass_b, den_b)},
        "repeats": repeats,
        "fixed": lists[FIXED],
        "regressions": lists[REGRESSIONS],
        "now_flaky": lists[NOW_FLAKY],
        "still_failing": lists[STILL_FAILING],
        "errors": lists[ERRORS],
        "unproved": lists[UNPROVED],
        "breaking": breaking,
    }))
}

/// One side's excluded rows by task, and its cost from the cohort fold's rule:
/// every scored row has usage and a complete price line, or `unmeasured`.
fn lane_fold(rows: &[&Row], prices: Option<&Value>) -> Value {
    let mut excluded: BTreeMap<(&str, &str), u64> = BTreeMap::new();
    for row in rows {
        if let Some(lane) = row.lane_status().filter(|l| lane_excludes(Some(*l))) {
            *excluded.entry((row.task_id.as_str(), lane)).or_default() += 1;
        }
    }
    let excluded: Vec<Value> = excluded
        .into_iter()
        .map(|((task, lane), rows)| json!({"task": task, "lane_status": lane, "rows": rows}))
        .collect();
    let score = cohort_score(rows, prices);
    json!({
        "excluded": excluded,
        "scored": score["scored"],
        "accepted": score["accepted"],
        "tokens": score["tokens"],
        "dollars_per_accepted": score["cost"]["dollars_per_accepted"],
    })
}

fn add_lanes(view: &mut Value, rows: &[Row], a: &str, b: &str, prices: Option<&Value>) {
    view["a"]["lane"] = lane_fold(&side_rows(rows, a), prices);
    view["b"]["lane"] = lane_fold(&side_rows(rows, b), prices);
}

fn lane_lines(view: &Value) -> Vec<String> {
    let mut out = Vec::new();
    for key in ["a", "b"] {
        let lane = &view[key]["lane"];
        for row in lane["excluded"].as_array().into_iter().flatten() {
            out.push(format!(
                "Excluded {key}: {} {} ({} rows, unscored)",
                row["task"].as_str().unwrap_or_default(),
                row["lane_status"].as_str().unwrap_or_default(),
                row["rows"],
            ));
        }
        let dollars = lane["dollars_per_accepted"]
            .as_f64()
            .map_or("unmeasured".to_string(), |d| format!("${d:.4}"));
        out.push(format!(
            "Cost {key}: {dollars} per accepted result ({} accepted of {} scored, tokens in={} out={})",
            lane["accepted"], lane["scored"], lane["tokens"]["input"], lane["tokens"]["output"],
        ));
    }
    out
}

fn render(view: &Value) -> String {
    let side_line = |key: &str| {
        let side = &view[key];
        let revs: Vec<String> = side["revs"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(Value::as_str)
                    .map(|r| r.chars().take(12).collect())
                    .collect()
            })
            .unwrap_or_default();
        format!(
            "{key}: {} (revs: {})",
            side["name"].as_str().unwrap_or_default(),
            revs.join(", ")
        )
    };
    let mut out = vec![side_line("a"), side_line("b")];
    for (title, key) in [
        (FIXED, "fixed"),
        (REGRESSIONS, "regressions"),
        (NOW_FLAKY, "now_flaky"),
        (STILL_FAILING, "still_failing"),
        (ERRORS, "errors"),
        (UNPROVED, "unproved"),
    ] {
        let rows = view[key].as_array().cloned().unwrap_or_default();
        out.push(format!("{title} ({}):", rows.len()));
        let status = |r: &Value, side: &str| {
            let name = r[side].as_str().unwrap_or_default();
            match r[format!("{side}_noise")].as_str() {
                Some(noise) => format!("{name}[{noise}]"),
                None => name.to_string(),
            }
        };
        for r in rows {
            out.push(format!(
                "  {}  {} ({} grades) -> {} ({} grades)",
                r["task"].as_str().unwrap_or_default(),
                status(&r, "a"),
                r["a_grades"],
                status(&r, "b"),
                r["b_grades"],
            ));
        }
    }
    for key in ["a", "b"] {
        let counts = &view[key]["counts"];
        let n = |status: &str| counts[status].as_u64().unwrap_or(0);
        out.push(format!(
            "Counts {key}: always_pass={} always_fail={} flaky={} error={}",
            n("always_pass"),
            n("always_fail"),
            n("flaky"),
            n("error"),
        ));
    }
    for key in ["a", "b"] {
        let rate = &view[key]["pass_rate"];
        let (p, d) = (
            rate["passes"].as_u64().unwrap_or(0),
            rate["grades"].as_u64().unwrap_or(0),
        );
        let pct = if d == 0 {
            "no grades".to_string()
        } else {
            format!("{:.1}%", p as f64 * 100.0 / d as f64)
        };
        out.push(format!("Pass rate {key}: {p}/{d} ({pct})"));
    }
    out.extend(lane_lines(view));
    out.join("\n")
}

pub(crate) fn run(args: &CompareArgs) -> i32 {
    if args.repeats == 0 {
        eprintln!("evals compare: --repeats must be at least 1");
        return EXIT_USAGE;
    }
    let rows = read_rows(args.history, None, args.since);
    let unproved = crate::evals::failure_types::unproved_tasks(args.bank).unwrap_or_else(|e| {
        // With no catalog nothing reads unproved, so every regression still fails.
        eprintln!("evals compare: no metric proofs read: {e}");
        BTreeSet::new()
    });
    let mut view = match fold(&rows, args.a, args.b, args.repeats, &unproved) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("{e}");
            return EXIT_USAGE;
        }
    };
    add_lanes(&mut view, &rows, args.a, args.b, args.prices);
    if args.json_out {
        println!(
            "{}",
            serde_json::to_string_pretty(&view).unwrap_or_default()
        );
    } else {
        println!("{}", render(&view));
    }
    if view["breaking"].as_bool().unwrap_or(false) {
        EXIT_REGRESSED
    } else {
        0
    }
}

/// The failure report: each named type, ranked by cost, then by how many of
/// its goldens fail on their newest `repeats` grades.
pub(crate) fn failure_fold(
    rows: &[Row],
    tasks: &[TaskSpec],
    catalog: &Catalog,
    proofs: &BTreeMap<String, Proof>,
    repeats: usize,
) -> Value {
    let all: Vec<&Row> = rows.iter().collect();
    let mut types: Vec<(usize, usize, Value)> = catalog
        .types
        .iter()
        .map(|t| {
            let goldens: Vec<&TaskSpec> = tasks
                .iter()
                .filter(|task| task.golden.as_ref().is_some_and(|g| g.failure_type == t.id))
                .collect();
            let status_of = |task: &TaskSpec| fold_side(&all, &task.id, repeats).status;
            let failing: Vec<&str> = goldens
                .iter()
                .filter(|task| matches!(status_of(task), Status::AlwaysFail | Status::Flaky))
                .map(|task| task.id.as_str())
                .collect();
            let no_result = goldens
                .iter()
                .filter(|task| status_of(task) == Status::Error)
                .count();
            let cost_rank = catalog
                .cost_order
                .iter()
                .position(|c| *c == t.cost)
                .unwrap_or(usize::MAX);
            let count = failing.len();
            let entry = json!({
                "type": t.id,
                "description": t.description,
                "cost": t.cost,
                "cost_rank": cost_rank,
                "goldens": goldens.len(),
                "failing": count,
                "no_result": no_result,
                "failing_tasks": failing,
                "proved": proofs.get(&t.id).is_some_and(|p| p.proved),
            });
            (cost_rank, count, entry)
        })
        .collect();
    types.sort_by(|a, b| {
        (a.0, std::cmp::Reverse(a.1), a.2["type"].as_str()).cmp(&(
            b.0,
            std::cmp::Reverse(b.1),
            b.2["type"].as_str(),
        ))
    });
    let types: Vec<Value> = types.into_iter().map(|(_, _, v)| v).collect();
    json!({"repeats": repeats, "types": types})
}

fn load_catalog(bank: &Path) -> Result<(Vec<TaskSpec>, Catalog), String> {
    let tasks = crate::evals::bank::discover_bank(bank).map_err(|e| e.0)?;
    let catalog = Catalog::load(bank)?;
    Ok((tasks, catalog))
}

/// `report --by-failure-type`: exit 0, or usage when the catalog cannot load.
pub(crate) fn run_failure_report(
    history: &str,
    bank: &Path,
    repeats: usize,
    since: Option<usize>,
    json_out: bool,
) -> i32 {
    let (tasks, catalog) = match load_catalog(bank) {
        Ok(loaded) => loaded,
        Err(e) => {
            eprintln!("evals failure report: {e}");
            return EXIT_USAGE;
        }
    };
    let rows = read_rows(history, Some("baseline"), since);
    let proofs = prove(&catalog, &tasks, bank);
    let view = failure_fold(&rows, &tasks, &catalog, &proofs, repeats);
    if json_out {
        println!(
            "{}",
            serde_json::to_string_pretty(&view).unwrap_or_default()
        );
        return 0;
    }
    for t in view["types"].as_array().into_iter().flatten() {
        println!(
            "{}  cost={} failing={} of {} goldens, no result={}, {}",
            t["type"].as_str().unwrap_or_default(),
            t["cost"].as_str().unwrap_or_default(),
            t["failing"],
            t["goldens"],
            t["no_result"],
            if t["proved"] == json!(true) {
                "proved"
            } else {
                "unproved"
            },
        );
    }
    0
}

/// `report --check-metrics`: exit 0 when every type is proved, else 1.
pub(crate) fn run_check_metrics(bank: &Path, json_out: bool) -> i32 {
    let (tasks, catalog) = match load_catalog(bank) {
        Ok(loaded) => loaded,
        Err(e) => {
            eprintln!("evals check-metrics: {e}");
            return EXIT_USAGE;
        }
    };
    let proofs = prove(&catalog, &tasks, bank);
    if json_out {
        let view: serde_json::Map<String, Value> = proofs
            .iter()
            .map(|(id, p)| {
                (
                    id.clone(),
                    json!({"proved": p.proved, "failures": p.failures}),
                )
            })
            .collect();
        println!(
            "{}",
            serde_json::to_string_pretty(&view).unwrap_or_default()
        );
    } else {
        for (id, proof) in &proofs {
            let word = if proof.proved { "proved" } else { "not proved" };
            println!("{id}: {word}");
            for failure in &proof.failures {
                println!("  {failure}");
            }
        }
    }
    if proofs.values().all(|p| p.proved) {
        0
    } else {
        EXIT_REGRESSED
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::evals::failure_types::FailureType;
    use std::io::Write as _;

    fn history(rows: &[(&str, &str, bool, &str)]) -> tempfile::NamedTempFile {
        let mut f = tempfile::NamedTempFile::new().unwrap();
        for (cohort, task, pass, status) in rows {
            let row = json!({
                "task_id": task, "tier": "capability", "pass": pass, "status": status,
                "variant": "baseline", "experiment_id": cohort, "bank_rev": cohort,
            });
            writeln!(f, "{row}").unwrap();
        }
        f
    }

    fn ids(view: &Value, key: &str) -> Vec<String> {
        view[key]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r["task"].as_str().unwrap().to_string())
            .collect()
    }

    fn three(
        cohort: &'static str,
        task: &'static str,
        passes: [bool; 3],
    ) -> Vec<(&'static str, &'static str, bool, &'static str)> {
        passes
            .iter()
            .map(|p| (cohort, task, *p, "graded"))
            .collect()
    }

    #[test]
    fn every_transition_lands_on_its_list_and_the_lists_print_before_the_rate() {
        let (t, f) = (true, false);
        let mut rows = Vec::new();
        for (task, a, b) in [
            ("fixed", [f, f, f], [t, t, t]),
            ("regressed", [t, t, t], [f, f, f]),
            ("wobbled", [t, t, t], [t, f, t]),
            ("stuck", [f, f, f], [f, t, f]),
            ("steady", [t, t, t], [t, t, t]),
        ] {
            rows.extend(three("a", task, a));
            rows.extend(three("b", task, b));
        }
        let h = history(&rows);
        let parsed = read_rows(h.path().to_str().unwrap(), None, None);
        let view = fold(&parsed, "a", "b", 3, &BTreeSet::new()).unwrap();
        assert_eq!(ids(&view, "fixed"), ["fixed"]);
        assert_eq!(ids(&view, "regressions"), ["regressed"]);
        assert_eq!(ids(&view, "now_flaky"), ["wobbled"]);
        assert_eq!(ids(&view, "still_failing"), ["stuck"]);
        assert_eq!(view["breaking"], json!(true));

        let text = render(&view);
        let at = |needle: &str| text.find(needle).unwrap();
        assert!(at("Fixed (") < at("Regressions ("));
        assert!(at("Regressions (") < at("Now flaky ("));
        assert!(at("Now flaky (") < at("Still failing ("));
        assert!(at("Still failing (") < at("Pass rate a"));
    }

    #[test]
    fn an_unavailable_row_is_no_grade_and_a_short_side_reads_error() {
        let mut rows = three("a", "lost", [true, true, true]);
        rows.push(("b", "lost", true, "graded"));
        rows.push(("b", "lost", true, "unavailable"));
        rows.push(("b", "lost", true, "unavailable"));
        let h = history(&rows);
        let parsed = read_rows(h.path().to_str().unwrap(), None, None);
        let view = fold(&parsed, "a", "b", 3, &BTreeSet::new()).unwrap();
        assert_eq!(ids(&view, "errors"), ["lost"]);
        assert_eq!(view["errors"][0]["b"], json!("error"));
        assert_eq!(view["b"]["pass_rate"], json!({"passes": 1, "grades": 1}));
        assert_eq!(
            view["breaking"],
            json!(true),
            "a had grades, b has an error"
        );
    }

    #[test]
    fn a_side_no_row_names_refuses_and_names_it() {
        let h = history(&three("a", "t", [true, true, true]));
        let parsed = read_rows(h.path().to_str().unwrap(), None, None);
        let err = fold(&parsed, "a", "typo", 3, &BTreeSet::new()).unwrap_err();
        assert!(err.contains("'typo'"), "{err}");
    }

    #[test]
    fn excluded_lanes_are_named_and_unscored_and_cost_prints_dollars_or_unmeasured() {
        let mut f = tempfile::NamedTempFile::new().unwrap();
        for (side, lane, model, pass) in [
            ("glm", "ok", "glm", true),
            ("glm", "substituted", "other", true),
            ("opus", "ok", "opus", true),
            ("opus", "account-dropped", "opus", true),
        ] {
            let row = json!({
                "task_id": "t", "tier": "capability", "pass": pass, "status": "graded",
                "experiment_id": side, "lane_status": lane, "observed_model": model,
                "usage": {"input": 1_000_000, "output": 0},
            });
            writeln!(f, "{row}").unwrap();
        }
        let rows = read_rows(f.path().to_str().unwrap(), None, None);
        let mut view = fold(&rows, "glm", "opus", 1, &BTreeSet::new()).unwrap();
        let prices = json!({"glm": {"input_per_m": 2.0, "output_per_m": 1.0}});
        add_lanes(&mut view, &rows, "glm", "opus", Some(&prices));
        let text = render(&view);
        assert!(
            text.contains("Excluded a: t substituted (1 rows, unscored)"),
            "{text}"
        );
        assert!(
            text.contains("Excluded b: t account-dropped (1 rows, unscored)"),
            "{text}"
        );
        assert!(
            text.contains("Cost a: $2.0000 per accepted result (1 accepted of 1 scored"),
            "{text}"
        );
        assert!(
            text.contains("Cost b: unmeasured per accepted result"),
            "{text}"
        );
    }

    #[test]
    fn a_flake_names_its_noise_from_the_tree_hashes() {
        let mut f = tempfile::NamedTempFile::new().unwrap();
        let mut row = |side: &str, task: &str, pass: bool, tree: Option<&str>| {
            let row = json!({
                "task_id": task, "tier": "capability", "pass": pass, "status": "graded",
                "experiment_id": side, "tree_hash": tree,
            });
            writeln!(f, "{row}").unwrap();
        };
        for (task, grades, trees) in [
            (
                "same-tree",
                [true, false, true],
                [Some("t"), Some("t"), Some("t")],
            ),
            (
                "two-trees",
                [true, true, false],
                [Some("x"), Some("x"), Some("y")],
            ),
            (
                "no-hash",
                [true, false, false],
                [Some("x"), None, Some("y")],
            ),
            ("mixed", [true, false, true], [Some("x"), Some("x"), None]),
        ] {
            for _ in 0..3 {
                row("a", task, true, Some("t"));
            }
            for (pass, tree) in grades.into_iter().zip(trees) {
                row("b", task, pass, tree);
            }
        }
        let parsed = read_rows(f.path().to_str().unwrap(), None, None);
        let view = fold(&parsed, "a", "b", 3, &BTreeSet::new()).unwrap();
        let noise: BTreeMap<&str, &str> = view["now_flaky"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| (r["task"].as_str().unwrap(), r["b_noise"].as_str().unwrap()))
            .collect();
        assert_eq!(noise["same-tree"], "grader_noise");
        assert_eq!(noise["two-trees"], "agent_noise");
        assert_eq!(noise["no-hash"], "unknown");
        assert_eq!(noise["mixed"], "grader_noise");
        assert!(render(&view).contains("flaky[grader_noise]"));
    }

    #[test]
    fn failure_types_rank_by_cost_then_count_and_an_unproved_golden_never_breaks() {
        let dir = tempfile::tempdir().unwrap();
        let golden = |id: &str, ftype: &str| {
            let path = dir.path().join(format!("{id}.yaml"));
            let body = format!(
                "id: {id}\ntier: capability\ngrade:\n  - kind: file-exists\n    path: x\ngolden:\n  source: {{kind: c, ref: r}}\n  expected: e\n  rule: r\n  failure_type: {ftype}\n  outcome: failed\n"
            );
            std::fs::write(&path, body).unwrap();
            crate::evals::bank::load_task(&path).unwrap()
        };
        let tasks = vec![
            golden("g-cheap-1", "cheap"),
            golden("g-cheap-2", "cheap"),
            golden("g-busy-1", "busy"),
            golden("g-busy-2", "busy"),
            golden("g-dear", "dear"),
        ];
        let kind = |id: &str, cost: &str| FailureType {
            id: id.into(),
            cost: cost.into(),
            description: "d".into(),
        };
        let catalog = Catalog {
            cost_order: vec!["trust".into(), "rework".into()],
            types: vec![
                kind("cheap", "rework"),
                kind("busy", "rework"),
                kind("dear", "trust"),
            ],
        };
        let mut rows = Vec::new();
        for (task, passes) in [
            ("g-cheap-1", [false; 3]),
            ("g-cheap-2", [true; 3]),
            ("g-busy-1", [false; 3]),
            ("g-busy-2", [true, false, true]),
            ("g-dear", [true; 3]),
        ] {
            rows.extend(three("a", task, passes));
        }
        let h = history(&rows);
        let parsed = read_rows(h.path().to_str().unwrap(), None, None);
        let view = failure_fold(&parsed, &tasks, &catalog, &BTreeMap::new(), 3);
        let order: Vec<(&str, u64)> = view["types"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| (t["type"].as_str().unwrap(), t["failing"].as_u64().unwrap()))
            .collect();
        assert_eq!(order, [("dear", 0), ("busy", 2), ("cheap", 1)]);
        assert_eq!(view["types"][0]["proved"], json!(false));

        let mut rows = three("a", "g-dear", [true; 3]);
        rows.extend(three("b", "g-dear", [false; 3]));
        let h = history(&rows);
        let parsed = read_rows(h.path().to_str().unwrap(), None, None);
        let unproved: BTreeSet<String> = ["g-dear".to_string()].into();
        let view = fold(&parsed, "a", "b", 3, &unproved).unwrap();
        assert_eq!(ids(&view, "unproved"), ["g-dear"]);
        assert!(ids(&view, "regressions").is_empty());
        assert_eq!(view["breaking"], json!(false));
    }
}
