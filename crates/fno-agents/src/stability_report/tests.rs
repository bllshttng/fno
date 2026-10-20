//! Tests build a real temp git repo (deterministic commit dates) and, for
//! the join, a two-column graph fixture. Assertions run against `compute`
//! and `aggregate` directly; one test drives `run()` for the usage exit.

use super::{aggregate, compute, removed_runs, Args, EXIT_USAGE};
use crate::git_test_helpers::git_bin;
use std::collections::BTreeMap;
use std::path::Path;
use std::process::Command;

/// git with pinned config and one deterministic commit date.
fn git(repo: &Path, args: &[&str], date: &str) {
    let out = Command::new(git_bin())
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("GIT_AUTHOR_DATE", date)
        .env("GIT_COMMITTER_DATE", date)
        .args(args)
        .current_dir(repo)
        .output()
        .expect("git runs");
    assert!(
        out.status.success(),
        "git {:?} failed: {}",
        args,
        String::from_utf8_lossy(&out.stderr)
    );
}

fn init_repo() -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempfile::tempdir().expect("tempdir");
    let repo = dir.path().join("repo");
    std::fs::create_dir(&repo).expect("mkdir");
    git(&repo, &["init", "-q", "-b", "main"], "");
    git(&repo, &["config", "user.email", "t@example.test"], "");
    git(&repo, &["config", "user.name", "t"], "");
    (dir, repo)
}

fn write(repo: &Path, rel: &str, body: &str) {
    let path = repo.join(rel);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("mkdir -p");
    }
    std::fs::write(path, body).expect("write");
}

fn kinds_from(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
    pairs
        .iter()
        .map(|(id, kind)| (id.to_string(), kind.to_string()))
        .collect()
}

fn graph_fixture(path: &Path, pairs: &[(&str, &str)]) {
    let connection = rusqlite::Connection::open(path).expect("open fixture");
    connection
        .execute_batch("CREATE TABLE nodes (id TEXT PRIMARY KEY, kind TEXT);")
        .expect("create nodes");
    for (id, kind) in pairs {
        connection
            .execute("INSERT INTO nodes VALUES (?1, ?2)", [id, kind])
            .expect("insert node");
    }
}

fn args(repo: &Path, days: i64, fix_window_days: i64, graph: Option<&Path>) -> Args {
    Args {
        repo: repo.to_path_buf(),
        days,
        fix_window_days,
        graph: graph.map(Path::to_path_buf),
        now: Some(super::DateTime::parse_from_rfc3339("2026-10-05T12:00:00Z").expect("now")),
        json: true,
    }
}

const OLD_DATE: &str = "2026-09-01T10:00:00+00:00";
const FEAT_DATE: &str = "2026-09-28T10:00:00+00:00";
const FIX_DATE: &str = "2026-10-02T10:00:00+00:00";

#[test]
fn young_squash_feat_fix_reports_fix_of_fix() {
    let (_dir, repo) = init_repo();
    write(&repo, "src/widget.rs", "a\nb\nc\nd\n");
    git(&repo, &["add", "."], OLD_DATE);
    git(&repo, &["commit", "-q", "-m", "init"], OLD_DATE);
    write(&repo, "src/widget.rs", "a\nb\nNEW\nd\n");
    git(&repo, &["add", "."], FEAT_DATE);
    git(
        &repo,
        &["commit", "-q", "-m", "feat: add widget line (#100)"],
        FEAT_DATE,
    );
    write(&repo, "src/widget.rs", "a\nb\nd\n");
    git(&repo, &["add", "."], FIX_DATE);
    git(
        &repo,
        &["commit", "-q", "-m", "fix: drop the widget line (#101)"],
        FIX_DATE,
    );

    let parsed = args(&repo, 30, 7, None);
    let now = parsed.now.expect("now");
    let report = compute(&parsed, now, &BTreeMap::new()).expect("report");
    let stats = aggregate(&report);
    assert_eq!(
        (stats.total, stats.fix, stats.feat, stats.other),
        (2, 1, 1, 0)
    );
    assert_eq!(stats.fix_of_fix, 1);
    let row = &report.rows[1];
    assert_eq!(row.delivery.pr, 101);
    assert_eq!(row.fix_of_fix, Some(true));
    assert_eq!(row.young_lines, 1);
}

#[test]
fn fix_on_old_code_is_not_a_fix_of_fix() {
    let (_dir, repo) = init_repo();
    write(&repo, "old.rs", "one\ntwo\n");
    git(&repo, &["add", "."], OLD_DATE);
    git(&repo, &["commit", "-q", "-m", "feat: seed (#1)"], OLD_DATE);
    write(&repo, "old.rs", "one\n");
    git(&repo, &["add", "."], FIX_DATE);
    git(&repo, &["commit", "-q", "-m", "fix: trim (#2)"], FIX_DATE);

    let parsed = args(&repo, 30, 7, None);
    let now = parsed.now.expect("now");
    let report = compute(&parsed, now, &BTreeMap::new()).expect("report");
    let stats = aggregate(&report);
    assert_eq!((stats.fix, stats.fix_of_fix), (1, 0));
    assert_eq!(report.rows[1].fix_of_fix, Some(false));
}

#[test]
fn merge_delivery_joins_the_backlog_node_kind() {
    let (_dir, repo) = init_repo();
    let graph = tempfile::tempdir().expect("graphdir");
    let graph_db = graph.path().join("graph.db");
    graph_fixture(&graph_db, &[("x-aaaa", "bug")]);

    write(&repo, "crates/fleet.rs", "x\n");
    git(&repo, &["add", "."], OLD_DATE);
    git(&repo, &["commit", "-q", "-m", "init"], OLD_DATE);
    git(
        &repo,
        &["checkout", "-q", "-b", "bugfix/x-aaaa-crash"],
        FEAT_DATE,
    );
    write(&repo, "crates/fleet.rs", "y\n");
    git(&repo, &["add", "."], FEAT_DATE);
    git(&repo, &["commit", "-q", "-m", "stop the crash"], FEAT_DATE);
    git(&repo, &["checkout", "-q", "main"], FEAT_DATE);
    git(
        &repo,
        &[
            "merge",
            "-q",
            "--no-ff",
            "-m",
            "Merge pull request #5 from owener/bugfix/x-aaaa-crash",
            "bugfix/x-aaaa-crash",
        ],
        FIX_DATE,
    );

    let parsed = args(&repo, 30, 7, Some(&graph_db));
    let now = parsed.now.expect("now");
    let report = compute(&parsed, now, &super::load_node_kinds(&graph_db)).expect("report");
    assert_eq!(report.rows.len(), 1);
    let row = &report.rows[0];
    assert_eq!(row.delivery.kind, "fix");
    assert_eq!(row.delivery.classified_by, "node-kind");
    // crates/ reads fleet.
    assert_eq!(row.surface, "fleet");
}

#[test]
fn without_the_graph_the_branch_prefix_classifies() {
    let (_dir, repo) = init_repo();
    write(&repo, "f.rs", "x\n");
    git(&repo, &["add", "."], OLD_DATE);
    git(&repo, &["commit", "-q", "-m", "init"], OLD_DATE);
    git(
        &repo,
        &["checkout", "-q", "-b", "feature/x-bbbb-thing"],
        FEAT_DATE,
    );
    write(&repo, "f.rs", "y\n");
    git(&repo, &["add", "."], FEAT_DATE);
    git(&repo, &["commit", "-q", "-m", "add it"], FEAT_DATE);
    git(&repo, &["checkout", "-q", "main"], FEAT_DATE);
    git(
        &repo,
        &[
            "merge",
            "-q",
            "--no-ff",
            "-m",
            "Merge pull request #6 from owener/feature/x-bbbb-thing",
            "feature/x-bbbb-thing",
        ],
        FIX_DATE,
    );

    // Unknown node id: the join degrades to the branch prefix.
    let graph = tempfile::tempdir().expect("graphdir");
    let graph_db = graph.path().join("graph.db");
    graph_fixture(&graph_db, &[("x-zzzz", "bug")]);
    let parsed = args(&repo, 30, 7, Some(&graph_db));
    let now = parsed.now.expect("now");
    let report = compute(&parsed, now, &super::load_node_kinds(&graph_db)).expect("report");
    assert_eq!(report.rows[0].delivery.kind, "feat");
    assert_eq!(report.rows[0].delivery.classified_by, "branch-prefix");
    // skills/ reads product.
    write(&repo, "placeholder", "");
    let _ = write(&repo, "skills/x.txt", "x\n");
    git(&repo, &["add", "."], FIX_DATE);
    git(
        &repo,
        &["commit", "-q", "-m", "feat: skill doc (#7)"],
        FIX_DATE,
    );
    let report = compute(&parsed, now, &BTreeMap::new()).expect("report");
    let skill_row = report
        .rows
        .iter()
        .find(|r| r.delivery.pr == 7)
        .expect("row 7");
    assert_eq!(skill_row.surface, "product");
}

#[test]
fn fix_renaming_the_file_blames_the_pre_image_path() {
    let (_dir, repo) = init_repo();
    write(&repo, "cli/src/fno/old.txt", "keep\nfarm\n");
    git(&repo, &["add", "."], OLD_DATE);
    git(&repo, &["commit", "-q", "-m", "feat: seed (#10)"], OLD_DATE);
    git(
        &repo,
        &["mv", "cli/src/fno/old.txt", "cli/src/fno/new.txt"],
        FIX_DATE,
    );
    write(&repo, "cli/src/fno/new.txt", "keep\n");
    git(&repo, &["add", "."], FIX_DATE);
    git(
        &repo,
        &["commit", "-q", "-m", "fix: rename and trim (#11)"],
        FIX_DATE,
    );

    let parsed = args(&repo, 30, 7, None);
    let now = parsed.now.expect("now");
    let report = compute(&parsed, now, &BTreeMap::new()).expect("report");
    let row = &report.rows[1];
    assert_eq!(row.fix_of_fix, Some(true));
    assert_eq!(row.surface, "product");
}

#[test]
fn removed_runs_groups_contiguous_lines_per_pre_image_path() {
    let diff = "\
diff --git a/f1 b/f1
index 111..222 100644
--- a/f1
+++ b/f1
@@ -1,3 +1,2 @@
-line one
 line two
-line three
+line three edited
diff --git a/f2 b/f2
index 333..444 100644
--- a/f2
+++ b/f2
@@ -4,3 +4,3 @@
 ctx
-gone one
-gone two
+back
 skip
";
    let runs = removed_runs(diff);
    assert_eq!(runs.len(), 2);
    assert_eq!(runs[0], ("f1".to_string(), vec![(1, 1), (3, 3)]));
    assert_eq!(runs[1], ("f2".to_string(), vec![(5, 6)]));
}

#[test]
fn run_rejects_unknown_flags_with_the_usage_exit() {
    assert_eq!(super::run(&["--nope".to_string()]), EXIT_USAGE);
    assert_eq!(super::run(&["--days".to_string()]), EXIT_USAGE);
}

#[test]
fn json_and_text_renders_carry_the_definitions() {
    let (_dir, repo) = init_repo();
    write(&repo, "a.rs", "x\n");
    git(&repo, &["add", "."], OLD_DATE);
    git(&repo, &["commit", "-q", "-m", "feat: seed (#20)"], OLD_DATE);
    let parsed = args(&repo, 30, 7, None);
    let now = parsed.now.expect("now");
    let report = compute(&parsed, now, &BTreeMap::new()).expect("report");
    let text = super::render_text(&parsed, now, &report, false);
    assert!(text.contains("fix youth window: 7d"), "{text}");
    let json_value: serde_json::Value =
        serde_json::from_str(&super::render_json(&parsed, now, &report, false)).expect("json");
    assert_eq!(json_value["totals"]["merged"], 1);
    assert_eq!(json_value["fix_of_fix"]["window_days"], 7);
    assert_eq!(json_value["backlog"]["available"], false);
    assert!(json_value["rows"].as_array().expect("rows").len() == 1);
}
