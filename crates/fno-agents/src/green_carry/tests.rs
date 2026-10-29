//! The green-carry contracts, over real scratch git repos: the carry rides a
//! clean main merge and refuses a real push, a conflict resolution, a
//! non-ancestor head, and a missing row; a carried read keeps the current
//! payload's github_block.

use super::carried_checks;
use crate::authorized_merge::ChecksRead;
use serde_json::json;
use std::path::{Path, PathBuf};
use std::process::Command;

fn scratch(label: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("fno-green-carry-{label}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn git(cwd: &Path, args: &[&str]) {
    let out = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@fno")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@fno")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn commit_all(cwd: &Path, msg: &str) -> String {
    git(cwd, &["add", "-A"]);
    git(cwd, &["commit", "-q", "-m", msg]);
    rev_parse(cwd, "HEAD")
}

/// A git run whose output the caller keeps: the merge is the one step whose
/// refusal mode varies by runner, so its failure must carry both streams.
fn git_out(cwd: &Path, args: &[&str]) -> std::process::Output {
    Command::new("git")
        .args(args)
        .current_dir(cwd)
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@fno")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@fno")
        .output()
        .expect("git spawns")
}

fn rev_parse(cwd: &Path, rev: &str) -> String {
    let out = Command::new("git")
        .args(["rev-parse", rev])
        .current_dir(cwd)
        .output()
        .unwrap();
    assert!(out.status.success(), "rev-parse {rev} failed");
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn repo_init(label: &str) -> PathBuf {
    let dir = scratch(label);
    git(&dir, &["init", "-q", "-b", "main"]);
    git(
        &dir,
        &["remote", "add", "origin", "https://github.com/o/r.git"],
    );
    std::fs::write(dir.join("seed.txt"), "seed\n").unwrap();
    commit_all(&dir, "seed");
    dir
}

/// Point `origin/main` at the local main branch: the identity's merge-base
/// read needs the remote ref a real checkout always has.
fn pin_origin_main(dir: &Path) {
    git(
        dir,
        &["update-ref", "refs/remotes/origin/main", "refs/heads/main"],
    );
}

/// Record a green row for `head` the way the live path writes it.
fn record_green(dir: &Path, head: &str) {
    let slug = crate::pr_status::cache::git_slug(dir).unwrap();
    let base = "main";
    let identity = crate::review_freshness::pr_code_diff_identity(
        "git",
        dir,
        &super::qualified_base(base),
        head,
    )
    .expect("identity resolves in the fixture");
    let row = json!({
        "ts": 0,
        "base": base,
        "patch_hash": identity.hash,
        "optional_unresolved": 0,
        "rerun_recovered": null,
        "rerun_failures": null,
    });
    let prefix = crate::pr_status::memo::memo_prefix(&slug, 7);
    crate::pr_status::memo::write(
        dir,
        crate::pr_status::memo::GREEN_VERDICT,
        &format!("{prefix}{head}"),
        &row,
        Some(&prefix),
    );
}

fn pending(_head: &str) -> ChecksRead {
    ChecksRead {
        verdict: "pending".to_string(),
        github_block: None,
        optional_unresolved: Some(Some(0)),
        rerun_recovered: None,
        rerun_failures: None,
    }
}

/// The env-carried cache dir: carried_checks resolves the memo through
/// pr_status_cache_dir, so each test points it at its own tempdir. The dir
/// sits OUTSIDE the scratch repo: a memo inside the repo gets staged by the
/// fixtures' `git add -A` and deleted by their checkouts, and a row's
/// presence then depends on which commit happened to resurrect it.
fn cache_guard(dir: &Path) -> std::sync::MutexGuard<'static, ()> {
    let guard = crate::pr_status::cache_env_lock();
    let cache = dir.parent().unwrap().join(format!(
        "{}-pr-status-cache",
        dir.file_name().unwrap().to_string_lossy()
    ));
    std::env::set_var("FNO_PR_STATUS_CACHE_DIR", cache);
    guard
}

#[test]
fn carry_rides_a_clean_main_merge() {
    let dir = repo_init("clean");
    let guard = cache_guard(&dir);
    git(&dir, &["checkout", "-q", "-b", "feature"]);
    std::fs::write(dir.join("src.rs"), "fn a() {}\n").unwrap();
    let green = commit_all(&dir, "c1");
    pin_origin_main(&dir);
    record_green(&dir, &green);
    git(&dir, &["checkout", "-q", "main"]);
    std::fs::write(dir.join("m.txt"), "mainline\n").unwrap();
    let _m1 = commit_all(&dir, "m1");
    git(&dir, &["checkout", "-q", "feature"]);
    git(&dir, &["merge", "-q", "--no-edit", "main"]);
    pin_origin_main(&dir);
    let head = rev_parse(&dir, "HEAD");
    let carried = carried_checks(&dir, 7, Some(&head), pending(&head));
    assert_eq!(
        carried.verdict, "green",
        "a comment-only main merge carries"
    );
    assert_eq!(carried.optional_unresolved, Some(Some(0)));
    drop(guard);
}

#[test]
fn carry_refuses_a_real_push() {
    let dir = repo_init("push");
    let guard = cache_guard(&dir);
    git(&dir, &["checkout", "-q", "-b", "feature"]);
    std::fs::write(dir.join("src.rs"), "fn a() {}\n").unwrap();
    let green = commit_all(&dir, "c1");
    pin_origin_main(&dir);
    record_green(&dir, &green);
    std::fs::write(dir.join("src.rs"), "fn b() {}\n").unwrap();
    let head = commit_all(&dir, "c2");
    let checks = carried_checks(&dir, 7, Some(&head), pending(&head));
    assert_eq!(checks.verdict, "pending", "a branch-authored commit reruns");
    drop(guard);
}

#[test]
fn carry_refuses_a_conflict_resolution() {
    let dir = repo_init("conflict");
    let guard = cache_guard(&dir);
    git(&dir, &["checkout", "-q", "-b", "feature"]);
    std::fs::write(dir.join("seed.txt"), "branch\n").unwrap();
    let green = commit_all(&dir, "c1");
    pin_origin_main(&dir);
    record_green(&dir, &green);
    git(&dir, &["checkout", "-q", "main"]);
    std::fs::write(dir.join("seed.txt"), "main\n").unwrap();
    commit_all(&dir, "m1");
    git(&dir, &["checkout", "-q", "feature"]);
    // Both sides edit seed.txt off the shared seed commit, so the 3-way merge
    // conflicts on every git and the resolution below always runs.
    let merged = git_out(&dir, &["merge", "--no-edit", "main"]);
    let streams = format!(
        "{}{}",
        String::from_utf8_lossy(&merged.stdout),
        String::from_utf8_lossy(&merged.stderr)
    );
    assert!(
        !merged.status.success(),
        "the both-sides edit must conflict: {streams}"
    );
    // A conflicted merge leaves MERGE_HEAD and MERGE_MSG behind; commit only
    // in that state, so a runner whose merge refused some other way reports
    // its own refusal instead of an empty-message abort from a stray commit.
    assert!(
        dir.join(".git").join("MERGE_HEAD").exists(),
        "the conflicted merge left no MERGE_HEAD: {streams}"
    );
    std::fs::write(dir.join("seed.txt"), "resolved\n").unwrap();
    git(&dir, &["add", "seed.txt"]);
    git(&dir, &["commit", "-q", "--no-edit"]);
    pin_origin_main(&dir);
    let head = rev_parse(&dir, "HEAD");
    let checks = carried_checks(&dir, 7, Some(&head), pending(&head));
    assert_eq!(checks.verdict, "pending", "a hand-resolved merge reruns");
    drop(guard);
}

#[test]
fn carry_refuses_non_ancestor_missing_row_and_keeps_github_block() {
    let dir = repo_init("guards");
    let guard = cache_guard(&dir);
    git(&dir, &["checkout", "-q", "-b", "feature"]);
    std::fs::write(dir.join("src.rs"), "fn a() {}\n").unwrap();
    let green = commit_all(&dir, "c1");
    pin_origin_main(&dir);
    record_green(&dir, &green);
    // A detached rewrite: the recorded head is no longer an ancestor.
    git(&dir, &["checkout", "-q", "--detach", "main"]);
    std::fs::write(dir.join("src.rs"), "fn a() {}\n").unwrap();
    let rewritten = commit_all(&dir, "orphan");
    let checks = carried_checks(&dir, 7, Some(&rewritten), pending(&rewritten));
    assert_eq!(
        checks.verdict, "pending",
        "a non-ancestor head never carries"
    );
    // No row for another slug's PR numbering: same repo, no row for pr 8.
    let checks = carried_checks(&dir, 8, Some(&rewritten), pending(&rewritten));
    assert_eq!(
        checks.verdict, "pending",
        "a PR with no recorded green reruns"
    );
    // A carry keeps the CURRENT payload's ruleset hold, never carries one away.
    git(&dir, &["checkout", "-q", "feature"]);
    git(&dir, &["merge", "-q", "--no-edit", "main"]);
    pin_origin_main(&dir);
    let head = rev_parse(&dir, "HEAD");
    let mut held = pending(&head);
    held.github_block = Some("required-check-missing".to_string());
    let checks = carried_checks(&dir, 7, Some(&head), held);
    assert_eq!(checks.verdict, "green");
    assert_eq!(
        checks.github_block.as_deref(),
        Some("required-check-missing")
    );
    // A non-pending verdict is returned untouched: only pending consults rows.
    let mut green_read = pending(&head);
    green_read.verdict = "green".to_string();
    let checks = carried_checks(&dir, 7, Some(&head), green_read);
    assert_eq!(checks.verdict, "green");
    drop(guard);
}
