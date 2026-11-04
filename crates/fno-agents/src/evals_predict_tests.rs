use super::*;
use std::io::Write as _;

const BASE: i64 = 1_791_158_400; // 2026-10-05T00:00:00Z

struct Hist {
    _dir: tempfile::TempDir,
    path: std::path::PathBuf,
    file: std::fs::File,
}

impl Hist {
    fn new() -> Hist {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("h.jsonl");
        let file = std::fs::File::create(&path).unwrap();
        Hist {
            _dir: dir,
            path,
            file,
        }
    }

    fn row(&mut self, task: &str, pass: bool, sha: &str, cohort: &str, rep: i64, sealed: bool) {
        let row = json!({"ts": "2026-10-05T00:00:00Z", "task_id": task, "tier": "capability",
            "pass": pass, "status": "graded", "variant": "v1", "bank_rev": sha,
            "experiment_id": cohort, "repeat_index": rep, "obs": {"worker_sealed": sealed}});
        writeln!(self.file, "{row}").unwrap();
    }

    /// One repeat of a tree: the first `passes` of 25 tasks pass.
    fn tree(&mut self, sha: &str, cohort: &str, rep: i64, passes: usize) {
        for i in 0..25 {
            self.row(&format!("t{i}"), i < passes, sha, cohort, rep, true);
        }
    }

    fn rows(&self) -> Vec<Row> {
        crate::evals_trend::read_rows(&self.path.to_string_lossy(), None, None)
    }
}

fn inputs(trees: usize, feeds: Vec<(String, Feed)>) -> Inputs {
    let shas: Vec<String> = (0..trees).map(|i| format!("s{i:02}")).collect();
    Inputs {
        tree_time: shas
            .iter()
            .enumerate()
            .map(|(i, s)| (s.clone(), BASE + i as i64 * DAY_S))
            .collect(),
        on_main: shas.into_iter().collect(),
        lead_tasks: BTreeSet::new(),
        now: BASE + trees as i64 * DAY_S,
        typed_turns: None,
        feeds,
    }
}

fn feed(name: &str, times: Times) -> (String, Feed) {
    (
        name.into(),
        Feed {
            times: Ok(times),
            coverage: Map::new(),
        },
    )
}

#[test]
fn noise_prints_same_tree_beside_between_tree_and_the_join_counts_what_it_drops() {
    let mut h = Hist::new();
    // One tree run three times reads 68, 76, 64: the gate-seal-head shape.
    for (rep, passes) in [(0, 17), (1, 19), (2, 16)] {
        h.tree("s01", "head", rep, passes);
    }
    for (sha, passes) in [("s00", 17), ("s02", 18), ("s03", 17)] {
        h.tree(sha, sha, 0, passes);
    }
    h.row("t0", true, "s02", "s02", 0, false);
    let view = fold(
        &h.rows(),
        &inputs(4, vec![feed("corrections", vec![BASE - 100, BASE + 100])]),
    );
    let noise = &view["noise"];
    let same = noise["same_tree_sd_points"].as_f64().unwrap();
    assert!((same - 6.11).abs() < 0.01, "{noise}");
    assert!(
        noise["between_tree_sd_points"].as_f64().unwrap() < same,
        "{noise}"
    );
    assert_eq!(noise["separable"], json!(false), "{noise}");
    assert_eq!(view["excluded"]["unsealed"], 1, "{view}");
    let corrections = &view["outcomes"]["corrections"];
    assert_eq!(corrections["before_first_tree"], 1, "{corrections}");
    assert_eq!(corrections["joined"], 1, "{corrections}");
}

#[test]
fn a_planted_task_predicts_and_two_passes_in_28_read_underpowered() {
    let mut h = Hist::new();
    let mut reverts = Vec::new();
    for i in 0..28 {
        let sha = format!("s{i:02}");
        let pass = i % 2 == 0;
        h.row("planted", pass, &sha, &sha, 0, true);
        h.row("rare", i < 2, &sha, &sha, 0, true);
        if !pass {
            for k in 0..3 {
                reverts.push(BASE + i * DAY_S + 3_600 + k);
            }
        }
    }
    let view = fold(&h.rows(), &inputs(28, vec![feed("reverts", reverts)]));
    let verdict = |id: &str| {
        view["tasks"]
            .as_array()
            .unwrap()
            .iter()
            .find(|t| t["task"] == id)
            .map(|t| t["verdict"].as_str().unwrap().to_string())
            .unwrap()
    };
    assert_eq!(verdict("planted"), "predicts", "{view}");
    assert_eq!(verdict("rare"), "underpowered", "{view}");
}

#[test]
fn bh_leaves_few_predicts_over_random_noise() {
    let mut h = Hist::new();
    let mut rng = Rng(7);
    let mut reverts = Vec::new();
    for i in 0..28 {
        let sha = format!("s{i:02}");
        for t in 0..100 {
            h.row(&format!("n{t}"), rng.next() % 2 == 0, &sha, &sha, 0, true);
        }
        for k in 0..(rng.next() % 4) as i64 {
            reverts.push(BASE + i * DAY_S + 3_600 + k);
        }
    }
    let view = fold(&h.rows(), &inputs(28, vec![feed("reverts", reverts)]));
    let predicts = view["verdict_counts"]["predicts"].as_u64().unwrap_or(0);
    assert!(predicts < 15, "{}", view["verdict_counts"]);
}

#[test]
fn the_report_names_the_change_it_could_have_seen() {
    let mut h = Hist::new();
    // Seven main trees, all in week 0: k = 7. One cohort run three times
    // on s00 reads 68, 76, 64 - sd 6.11 points, the gate-seal-head shape.
    for (rep, passes) in [(0, 17), (1, 19), (2, 16)] {
        h.tree("s00", "head", rep, passes);
    }
    let others = [
        ("s01", 18),
        ("s02", 18),
        ("s03", 18),
        ("s04", 18),
        ("s05", 18),
        ("s06", 18),
    ];
    for (sha, passes) in others {
        h.tree(sha, sha, 0, passes);
    }
    let view = fold(&h.rows(), &inputs(7, vec![feed("corrections", vec![])]));
    let noise = &view["noise"];
    let mdd_week = noise["mdd_week_points"].as_f64().unwrap();
    assert!((mdd_week - 9.14).abs() < 0.01, "{noise}");
    assert!(!noise["halves"].is_null(), "{noise}");
    assert_eq!(
        noise["halves"]["paired"]["tasks_informative"],
        json!(3),
        "{noise}"
    );
}

#[test]
fn a_report_with_one_main_tree_reads_halves_unmeasured() {
    let mut h = Hist::new();
    for (rep, passes) in [(0, 17), (1, 19), (2, 16)] {
        h.tree("s00", "head", rep, passes);
    }
    let view = fold(&h.rows(), &inputs(1, vec![feed("corrections", vec![])]));
    assert!(view["noise"]["halves"].is_null(), "{}", view);
    print_text(&view);
}
