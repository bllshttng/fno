//! `fno doctor evals report --compare A --to B`: one status per task on each
//! side, then what changed. The per-task lists print before the pass rate
//! because a rate hides which behaviour moved.
//!
//! A side names a cohort, else a variant. Only a scored row is a grade, so a
//! plumbing row can never read as a pass. A task needs `repeats` grades for a
//! status; with fewer it reads `error`.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::{json, Value};

use crate::evals_trend::{is_scored, read_rows, Row};

const EXIT_REGRESSED: i32 = 1;
const EXIT_USAGE: i32 = 2;

pub(crate) struct CompareArgs<'a> {
    pub history: &'a str,
    pub a: &'a str,
    pub b: &'a str,
    pub repeats: usize,
    pub json_out: bool,
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
    }
}

const FIXED: &str = "Fixed";
const REGRESSIONS: &str = "Regressions";
const NOW_FLAKY: &str = "Now flaky";
const STILL_FAILING: &str = "Still failing";
const ERRORS: &str = "Errors";
const LISTS: [&str; 5] = [FIXED, REGRESSIONS, NOW_FLAKY, STILL_FAILING, ERRORS];

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
pub(crate) fn fold(rows: &[Row], a: &str, b: &str, repeats: usize) -> Result<Value, String> {
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
        breaking |= (sa.status == Status::AlwaysPass && sb.status == Status::AlwaysFail)
            || (sa.grades > 0 && sb.status == Status::Error);
        if let Some(list) = bucket(sa.status, sb.status) {
            let entry = json!({
                "task": task,
                "a": sa.status.name(),
                "b": sb.status.name(),
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
        "breaking": breaking,
    }))
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
    ] {
        let rows = view[key].as_array().cloned().unwrap_or_default();
        out.push(format!("{title} ({}):", rows.len()));
        for r in rows {
            out.push(format!(
                "  {}  {} ({} grades) -> {} ({} grades)",
                r["task"].as_str().unwrap_or_default(),
                r["a"].as_str().unwrap_or_default(),
                r["a_grades"],
                r["b"].as_str().unwrap_or_default(),
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
    out.join("\n")
}

pub(crate) fn run(args: &CompareArgs) -> i32 {
    if args.repeats == 0 {
        eprintln!("evals compare: --repeats must be at least 1");
        return EXIT_USAGE;
    }
    let rows = read_rows(args.history, None, None);
    let view = match fold(&rows, args.a, args.b, args.repeats) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("{e}");
            return EXIT_USAGE;
        }
    };
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

#[cfg(test)]
mod tests {
    use super::*;
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
        let view = fold(&parsed, "a", "b", 3).unwrap();
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
        let view = fold(&parsed, "a", "b", 3).unwrap();
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
        let err = fold(&parsed, "a", "typo", 3).unwrap_err();
        assert!(err.contains("'typo'"), "{err}");
    }
}
