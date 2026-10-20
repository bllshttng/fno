//! Tests build a real temp git repo (deterministic commit dates) and, for
//! the join, a two-column graph fixture. Assertions run against `compute`
//! and `aggregate` directly; one test drives `run()` for the usage exit.

use super::{aggregate, compute, removed_runs, Args};
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

fn graph_fixture(path: &Path, pairs: &[(&str, &str)]) -> std::path::PathBuf {
    // Seed through the store's own fixture door: SQL writes to nodes outside
    // the backlog owner fail the table-ownership contract. The row needs the
    // representable shape (title, type, priority): a row the model cannot
    // parse is carried verbatim in nodes_raw and never reaches the typed
    // nodes table the kind loader reads. The store seeds through the layout
    // resolver, so the caller reads the path it returns.
    let rows: Vec<serde_json::Value> = pairs
        .iter()
        .map(|(id, kind)| {
            serde_json::json!({
                "id": id, "slug": id, "title": id, "type": kind,
                "status": "done", "priority": "p2",
            })
        })
        .collect();
    crate::graph_store::seed_rows(path, &rows).expect("seed graph fixture");
    crate::backlog::database_path(path)
}

fn args(repo: &Path, days: i64, fix_window_days: i64, graph: Option<&Path>) -> Args {
    Args {
        repo: repo.to_path_buf(),
        days,
        fix_window_days,
        graph: graph.map(Path::to_path_buf),
        graph_explicit: false,
        now: Some(
            super::DateTime::parse_from_rfc3339("2026-10-05T12:00:00Z")
                .expect("now")
                .with_timezone(&super::Utc),
        ),
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
    // An add-only fix is measured clean and reported apart.
    write(&repo, "added.rs", "x\n");
    git(&repo, &["add", "."], FIX_DATE);
    git(&repo, &["commit", "-q", "-m", "fix: add (#3)"], FIX_DATE);

    let parsed = args(&repo, 30, 7, None);
    let now = parsed.now.expect("now");
    let report = compute(&parsed, now, &BTreeMap::new()).expect("report");
    let stats = aggregate(&report);
    assert_eq!(stats.fix, 2);
    assert_eq!(stats.fix_of_fix, 0);
    assert_eq!(
        (stats.measured, stats.unmeasured, stats.no_removed),
        (2, 0, 1)
    );
    assert_eq!(report.rows[0].fix_of_fix, Some(false));
    assert_eq!(report.rows[1].fix_of_fix, Some(false));
    // Render smoke: the JSON parses and the text names the youth window.
    let json_value: serde_json::Value =
        serde_json::from_str(&super::render_json(&parsed, now, &report, false)).expect("json");
    assert_eq!(json_value["totals"]["merged"], 2);
    assert_eq!(json_value["fix_of_fix"]["window_days"], 7);
    assert_eq!(json_value["backlog"]["available"], false);
    assert!(super::render_text(&parsed, now, &report, false).contains("fix youth window: 7d"));
    // Arg-surface exits: usage (2) and an explicit unreadable graph (3).
    assert_eq!(super::run(&["--nope".to_string()]), super::EXIT_USAGE);
    assert_eq!(
        super::run(&[
            "--graph".to_string(),
            "/nonexistent/fno-test/graph.db".to_string()
        ]),
        3
    );
}

#[test]
fn merge_delivery_joins_the_backlog_node_kind() {
    let (_dir, repo) = init_repo();
    let graph = tempfile::tempdir().expect("graphdir");
    let graph_seed = graph.path().join("graph.json");
    let graph_db = graph_fixture(&graph_seed, &[("x-aaaa", "bug")]);

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
    let kinds = super::load_node_kinds(&graph_db).expect("graph kinds");
    let report = compute(&parsed, now, &kinds).expect("report");
    assert_eq!(report.rows.len(), 1);
    let row = &report.rows[0];
    assert_eq!(row.delivery.kind, "fix");
    assert_eq!(row.delivery.classified_by, "node-kind");
    // crates/ reads fleet.
    assert_eq!(row.surface, "fleet");
    // With no graph loaded the same branch reads through the prefix rung
    // (the body carries no title here, so nothing outranks it).
    let report = compute(&parsed, now, &BTreeMap::new()).expect("report");
    assert_eq!(report.rows[0].delivery.kind, "fix");
    assert_eq!(report.rows[0].delivery.classified_by, "branch-prefix");
}

#[test]
fn merge_title_outranks_the_node_kind() {
    let (_dir, repo) = init_repo();
    let graph = tempfile::tempdir().expect("graphdir");
    let graph_seed = graph.path().join("graph.json");
    let graph_db = graph_fixture(&graph_seed, &[("x-cccc", "feature")]);

    write(&repo, "gate.rs", "x\n");
    git(&repo, &["add", "."], OLD_DATE);
    git(&repo, &["commit", "-q", "-m", "init"], OLD_DATE);
    git(
        &repo,
        &["checkout", "-q", "-b", "feature/x-cccc-stop-gate"],
        FEAT_DATE,
    );
    write(&repo, "gate.rs", "y\n");
    git(&repo, &["add", "."], FEAT_DATE);
    git(&repo, &["commit", "-q", "-m", "stop the gate"], FEAT_DATE);
    git(&repo, &["checkout", "-q", "main"], FEAT_DATE);
    // A fix-titled merge body over a feature-kind node: the title wins. The
    // merge message is the GitHub shape: subject, blank, title, bullets.
    git(
        &repo,
        &[
            "merge",
            "-q",
            "--no-ff",
            "-m",
            "Merge pull request #8 from owener/feature/x-cccc-stop-gate\n\nfix(stop-gate): repair the gate\n\n* stop the gate\n",
            "feature/x-cccc-stop-gate",
        ],
        FIX_DATE,
    );

    let parsed = args(&repo, 30, 7, Some(&graph_db));
    let now = parsed.now.expect("now");
    let kinds = super::load_node_kinds(&graph_db).expect("graph kinds");
    let report = compute(&parsed, now, &kinds).expect("report");
    assert_eq!(report.rows.len(), 1);
    assert_eq!(report.rows[0].delivery.kind, "fix");
    assert_eq!(report.rows[0].delivery.classified_by, "merge-title");
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
    // The seed commit sits outside the window; the rename fix is the only row.
    assert_eq!(report.rows.len(), 1);
    let row = &report.rows[0];
    // The removed line blames the 31-day-old seed, so the row reads measured
    // and not a fix-of-fix. `Some` is the point: the blame found the line
    // through the pre-image path (old.txt), where the post-image path would
    // have failed the read and marked the row unmeasured.
    assert_eq!(row.fix_of_fix, Some(false));
    assert_eq!(row.young_lines, 0);
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
