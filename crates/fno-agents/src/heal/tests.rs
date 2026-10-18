//! Tests for the heal drive loop, in their own module: `heal.rs` sat pinned
//! at the file-budget cap and the starvation fix had no room to land. The
//! move is pure test motion; raw-string stub contents stay byte-exact and
//! the code lines shed the one indent level the brace nesting paid.

use super::*;
use crate::pr_push::{any_pending, rest_bucket};
use serde_json::json;
use std::path::Path;

/// A real `cargo fmt --check (pinned)` failure, timestamps and all.
const FMT_LOG: &str = concat!(
    "2026-09-03T03:06:02.1431957Z ##[group]Run cargo \"+$RUSTFMT_TOOLCHAIN\" fmt --all --check\n",
    "2026-09-03T03:06:03.2159034Z Diff in /home/runner/work/footnote/footnote/crates/fno/src/server.rs:17553:\n",
    "2026-09-03T03:06:03.2234373Z -            core.pane_send(\n",
    "2026-09-03T03:06:04.8929731Z ##[error]Process completed with exit code 1.\n",
);

fn ctx<'a>(check: &'a str, log: &'a str) -> Ctx<'a> {
    Ctx {
        check,
        log,
        bucket: "fail",
        link: "",
    }
}

fn cancelled_ctx<'a>(check: &'a str, log: &'a str) -> Ctx<'a> {
    Ctx {
        check,
        log,
        bucket: "cancel",
        link: "",
    }
}

fn cancelled_run_ctx<'a>(check: &'a str, link: &'a str) -> Ctx<'a> {
    Ctx {
        check,
        log: "",
        bucket: "cancel",
        link,
    }
}

#[test]
fn classify_rows() {
    let stripped = strip_timestamps(FMT_LOG);
    assert!(
        stripped.contains("Diff in /home/runner/work/footnote/footnote/crates/fno/src/"),
        "got: {stripped}"
    );
    assert!(!stripped.contains("2026-09-03T03:06"), "got: {stripped}");

    let log = strip_timestamps(FMT_LOG);
    let f = classify(&ctx("cargo fmt --check (pinned)", &log), false);
    assert_eq!(f.signature, "rustfmt-drift");
    match f.remedy {
        Remedy::Auto { run, verify } => {
            assert_eq!(run.len(), 1, "one crate drifted: {run:?}");
            assert_eq!(run[0].cwd, "crates/fno");
            assert_eq!(run[0].argv, vec!["cargo", "+1.94.1", "fmt", "--all"]);
            assert_eq!(verify[0].argv.last().unwrap(), "--check");
        }
        other => panic!("expected Auto, got {other:?}"),
    }

    let log = strip_timestamps(&format!(
        "{FMT_LOG}2026-09-03T03:06:03Z Diff in /home/runner/work/footnote/footnote/crates/fno-agents/src/heal.rs:1:\n"
    ));
    let f = classify(&ctx("cargo fmt --check (pinned)", &log), false);
    match f.remedy {
        Remedy::Auto { run, .. } => {
            let dirs: Vec<&str> = run.iter().map(|c| c.cwd.as_str()).collect();
            assert_eq!(dirs, vec!["crates/fno", "crates/fno-agents"]);
        }
        other => panic!("expected Auto, got {other:?}"),
    }

    let f = classify(&ctx("cargo fmt --check (pinned)", "log unavailable"), false);
    assert_eq!(f.signature, "rustfmt-drift");
    match f.remedy {
        Remedy::Auto { run, .. } => assert_eq!(run.len(), 2, "{run:?}"),
        other => panic!("expected Auto, got {other:?}"),
    }

    // The shape ruff actually prints: code first, location beneath.
    let log = strip_timestamps(concat!(
        "2026-09-03T07:15:29.7310174Z F821 Undefined name `Callable`\n",
        "2026-09-03T07:15:29.7310931Z    --> src/fno/lead/board.py:886:21\n",
        "2026-09-03T07:15:29.7322711Z Found 1 error.\n",
    ));
    let f = classify(&ctx("Python static correctness (495 sources)", &log), false);
    assert_eq!(f.signature, "ruff-lint");
    match f.remedy {
        Remedy::Auto { run, verify } => {
            assert_eq!(run.len(), 1, "one command, the gate's own: {run:?}");
            assert_eq!(run[0].cwd, "cli");
            assert!(run[0].argv.contains(&"--fix".to_string()), "{run:?}");
            // The gate reads src/ only and runs no formatter; a remedy
            // that reached wider would rewrite files nothing checks.
            assert!(!run[0].argv.iter().any(|a| a == "tests"), "{run:?}");
            assert!(!run[0].argv.iter().any(|a| a == "format"), "{run:?}");
            assert_eq!(verify[0].argv.last().unwrap(), "src/");
        }
        other => panic!("expected Auto, got {other:?}"),
    }

    let log = "src/fno/x.py:12: error: Incompatible return value type";
    let f = classify(&ctx("Python static correctness", log), false);
    assert_eq!(f.signature, "mypy");
    assert!(
        matches!(&f.remedy, Remedy::Escalate { repro } if repro.contains("mypy")),
        "{:?}",
        f.remedy
    );
}

#[test]
fn table_rows() {
    let text = playbook();
    for sig in SIGNATURES {
        assert!(
            text.contains(sig.name),
            "{} missing from playbook",
            sig.name
        );
    }
    assert!(text.contains("inherited"));
    assert!(text.contains("unknown"));

    let checks = json!([
        {"name": "a", "bucket": "fail", "link": "", "workflow": "w"},
        {"name": "b", "bucket": "pass", "link": "", "workflow": "w"},
        {"name": "c", "bucket": "cancel", "link": "", "workflow": "w"},
        {"name": "d", "bucket": "pending", "link": "", "workflow": "w"},
    ]);
    let names: Vec<String> = failing_rows(&checks)
        .iter()
        .map(|r| r["name"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(names, vec!["a", "c"]);

    let settled = json!([{"name": "a", "bucket": "fail"}, {"name": "b", "bucket": "pass"}]);
    assert!(!any_pending(&settled));
    let running = json!([{"name": "a", "bucket": "pass"}, {"name": "b", "bucket": "pending"}]);
    assert!(any_pending(&running));

    assert_eq!(
        job_id("https://github.com/o/r/actions/runs/123/job/456"),
        Some("456".to_string())
    );
    assert_eq!(job_id("https://example.test/build/7"), None);
}

// ── the verb: push discipline ───────────────────────────────────────────
// Stub `gh` and `git`, not a real remote: exactly one push, never a push
// over a run in flight, provable rather than argued.

use crate::write_exec_stub as write_exec;

/// A stub `gh` answering the four reads heal makes. `pending` decides
/// whether the SECOND check-runs read (the pre-push one) reports a run in
/// flight, which is how the in-flight guard is exercised.
fn stub_gh(dir: &Path, pending_on_second_read: bool) -> std::path::PathBuf {
    let flip = if pending_on_second_read {
        r#"if [ -f "$D/seen" ]; then B=in_progress; else touch "$D/seen"; fi"#
    } else {
        ""
    };
    write_exec(
        dir,
        "gh",
        &format!(
            r#"#!/bin/sh
D="$(dirname "$0")"
echo "gh $*" >> "$D/gh.log"
for a in "$@"; do case "$a" in
  */pulls/*) echo '{{"head":{{"sha":"deadbeef","ref":"feature/x","repo":{{"full_name":"o/r"}}}},"base":{{"ref":"main","repo":{{"full_name":"o/r"}}}},"author_association":"OWNER","body":"b"}}'; exit 0 ;;
  */check-runs) B=completed; {flip}
     if [ "$B" = in_progress ]; then
       echo '{{"check_runs":[{{"name":"cargo fmt --check (pinned)","status":"in_progress","conclusion":null,"html_url":"https://github.com/o/r/actions/runs/1/job/9"}}]}}'
     else
       echo '{{"check_runs":[{{"name":"cargo fmt --check (pinned)","status":"completed","conclusion":"failure","html_url":"https://github.com/o/r/actions/runs/1/job/9"}}]}}'
     fi
     exit 0 ;;
  */logs) echo "Diff in /w/w/crates/fno-agents/src/x.rs:1:"; exit 0 ;;
  */status) echo '{{"statuses":[]}}'; exit 0 ;;
esac; done
echo '[]'
"#
        ),
    )
}

/// A stub `git` recording every invocation. `dirty` decides what
/// `status --porcelain` answers.
fn stub_git(dir: &Path, branch: &str, dirty: bool) -> std::path::PathBuf {
    let porcelain = if dirty { "echo ' M src/x.rs'" } else { ":" };
    write_exec(
        dir,
        "git",
        &format!(
            r#"#!/bin/sh
D="$(dirname "$0")"
echo "git $*" >> "$D/git.log"
case "$1 $2" in
  "rev-parse --abbrev-ref") echo {branch}; exit 0 ;;
  "status --porcelain") if [ -f "$D/fixed" ]; then echo ' M src/x.rs'; else {porcelain}; fi; exit 0 ;;
esac
exit 0
"#
        ),
    )
}

/// A stub `cargo` on PATH that "fixes" the drift: the first run dirties
/// the tree, and the verify then passes. The crate directory is created
/// because a remedy runs IN it, and a missing cwd fails the spawn -- which
/// heal correctly reads as "the fix did not succeed".
fn stub_cargo(dir: &Path) {
    std::fs::create_dir_all(dir.join("crates/fno-agents")).unwrap();
    write_exec(
        dir,
        "cargo",
        r#"#!/bin/sh
D="$(dirname "$0")"
echo "cargo $*" >> "$D/cargo.log"
for a in "$@"; do [ "$a" = "--check" ] && exit 0; done
touch "$D/fixed"
exit 0
"#,
    );
}

/// A stub `cargo` that always succeeds and never touches the tree. It
/// stands for a remedy whose red does not reproduce locally.
fn stub_cargo_noop(dir: &Path) {
    std::fs::create_dir_all(dir.join("crates/fno-agents")).unwrap();
    std::fs::create_dir_all(dir.join("crates/fno")).unwrap();
    write_exec(
        dir,
        "cargo",
        r#"#!/bin/sh
D="$(dirname "$0")"
echo "cargo $*" >> "$D/cargo.log"
exit 0
"#,
    );
}

/// A stub `git` whose `commit` always fails, standing for a rejecting
/// pre-commit hook or a signing failure.
fn stub_git_commit_fails(dir: &Path, branch: &str) -> std::path::PathBuf {
    write_exec(
        dir,
        "git",
        &format!(
            r#"#!/bin/sh
D="$(dirname "$0")"
echo "git $*" >> "$D/git.log"
case "$1 $2" in
  "rev-parse --abbrev-ref") echo {branch}; exit 0 ;;
  "status --porcelain") if [ -f "$D/fixed" ]; then echo ' M src/x.rs'; fi; exit 0 ;;
esac
case "$1" in
  commit) echo "pre-commit hook refused" >&2; exit 1 ;;
esac
exit 0
"#
        ),
    )
}

fn log_of(dir: &Path, name: &str) -> String {
    // The events journal is store-committed: read committed rows, not
    // journal bytes. Other stub logs (gh.log, fno.log) stay raw files.
    if name == "events.jsonl" {
        return crate::events::committed_journal_text(&dir.join(name));
    }
    std::fs::read_to_string(dir.join(name)).unwrap_or_default()
}

fn args_for(dir: &Path, extra: &[&str]) -> Vec<String> {
    let mut v = vec![
        "1".to_string(),
        "--gh-bin".to_string(),
        dir.join("gh").to_string_lossy().into_owned(),
        "--git-bin".to_string(),
        dir.join("git").to_string_lossy().into_owned(),
        "--cwd".to_string(),
        dir.to_string_lossy().into_owned(),
    ];
    v.push("--bin-dir".to_string());
    v.push(dir.to_string_lossy().into_owned());
    // The journal rides the same test seam as drive_args below: without it
    // the default resolves through $HOME and the hermetic guard panics.
    v.push("--events-file".to_string());
    v.push(dir.join("events.jsonl").to_string_lossy().into_owned());
    v.extend(extra.iter().map(|s| s.to_string()));
    v
}

#[test]
fn remedy_rows() {
    let tmp = tempfile::tempdir().unwrap();
    let d = tmp.path();
    stub_gh(d, false);
    stub_git(d, "feature/x", false);
    stub_cargo(d);
    let code = run_heal(&args_for(d, &[]));
    assert_eq!(code, EXIT_ESCALATIONS.max(EXIT_CLEAN), "dry run reports");
    assert_eq!(log_of(d, "cargo.log"), "", "no remedy ran");
    assert!(!log_of(d, "git.log").contains("push"), "no push");

    let tmp = tempfile::tempdir().unwrap();
    let d = tmp.path();
    stub_gh(d, false);
    stub_git(d, "feature/x", true);
    stub_cargo(d);
    let code = run_heal(&args_for(d, &["--apply"]));
    assert_eq!(code, EXIT_CWD_REFUSAL);
    assert_eq!(log_of(d, "cargo.log"), "", "no remedy ran");
    assert!(!log_of(d, "git.log").contains("push"));

    let tmp = tempfile::tempdir().unwrap();
    let d = tmp.path();
    stub_gh(d, false);
    stub_git(d, "main", false);
    stub_cargo(d);
    assert_eq!(run_heal(&args_for(d, &["--apply"])), EXIT_CWD_REFUSAL);
    assert_eq!(log_of(d, "cargo.log"), "");

    let tmp = tempfile::tempdir().unwrap();
    let d = tmp.path();
    stub_gh(d, false);
    stub_git(d, "feature/x", false);
    stub_cargo(d);
    run_heal(&args_for(d, &["--apply"]));
    let git = log_of(d, "git.log");
    assert_eq!(git.matches("git commit").count(), 1, "{git}");
    assert_eq!(git.matches("git push").count(), 1, "{git}");

    let tmp = tempfile::tempdir().unwrap();
    let d = tmp.path();
    // The pre-push re-read reports a check still running.
    stub_gh(d, true);
    stub_git(d, "feature/x", false);
    stub_cargo(d);
    let code = run_heal(&args_for(d, &["--apply"]));
    assert_eq!(code, EXIT_IN_FLIGHT);
    let git = log_of(d, "git.log");
    assert_eq!(git.matches("git commit").count(), 1, "the fix is kept");
    assert!(!git.contains("git push"), "but never pushed: {git}");

    // `dirty` is a WHOLE-WORKTREE question and the remedies share one
    // worktree, so an earlier remedy's uncommitted edit made a later
    // no-op remedy read as dirty and take credit for work it did not do.
    // Each remedy is measured against its own before/after now.
    let tmp = tempfile::tempdir().unwrap();
    let d = tmp.path();
    stub_gh(d, false);
    stub_git(d, "feature/x", false);
    stub_cargo_noop(d);

    let mut findings = vec![classify(
        &ctx("cargo fmt --check (pinned)", "log unavailable"),
        false,
    )];
    let args = parse_args(&args_for(d, &["--apply"])).unwrap();
    // The tree is already dirty when the remedy runs, exactly as it would
    // be after a previous finding's fix.
    std::fs::write(d.join("fixed"), "").unwrap();

    let healed = apply_auto(&args, &mut findings);
    assert!(
        healed.is_empty(),
        "a no-op remedy is never healed: {healed:?}"
    );
    assert!(
        matches!(&findings[0].remedy, Remedy::Escalate { repro } if repro.contains("changed nothing")),
        "{:?}",
        findings[0].remedy
    );

    // The fix is in the worktree but no commit and no push happened, so
    // the check is still red on the remote. Exiting 0 there is the false
    // green the no-op arm was written to close, left open on this path.
    let tmp = tempfile::tempdir().unwrap();
    let d = tmp.path();
    stub_gh(d, false);
    stub_git_commit_fails(d, "feature/x");
    stub_cargo(d);

    let code = run_heal(&args_for(d, &["--apply"]));
    assert_ne!(code, EXIT_CLEAN, "a failed commit is not a clean PR");
    assert!(
        !log_of(d, "git.log").contains("git push"),
        "and nothing is pushed"
    );

    // --dry-run rehearses --all --apply; on the single-PR path it would
    // silently mean "the report you already get without --apply".
    assert_eq!(
        run_heal(&[
            "1".to_string(),
            "--apply".to_string(),
            "--dry-run".to_string()
        ]),
        EXIT_READ_ERROR
    );
}

// ── the drive loop ─────────────────────────────────────────────────────
//
// Driven against stub gh/git/cargo/fno executables, so the four refusals
// and the tick row are provable rather than argued.

/// A stub `gh` answering the drive loop's reads: an open-PR listing with
/// PR 1 (branch feature/x-1111, free) and PR 2 (feature/x-2222, whose
/// node the test holds a live claim on), each with one rustfmt-drift
/// failure. `mystery` swaps PR 1's check for one no signature matches.
fn stub_gh_drive(dir: &Path, mystery: bool) -> std::path::PathBuf {
    let check = if mystery {
        r#"{"name":"mystery-check","status":"completed","conclusion":"failure","html_url":"https://github.com/o/r/actions/runs/1/job/9"}"#
    } else {
        r#"{"name":"cargo fmt --check (pinned)","status":"completed","conclusion":"failure","html_url":"https://github.com/o/r/actions/runs/1/job/9"}"#
    };
    // A mystery PR's log must match nothing in the table either: the
    // rustfmt Diff line would classify it rustfmt-drift however the check
    // is named, and the escalation under test would never fire.
    let log_line = if mystery {
        "totally novel failure output"
    } else {
        "Diff in /w/w/crates/fno-agents/src/x.rs:1:"
    };
    write_exec(
        dir,
        "gh",
        &format!(
            r#"#!/bin/sh
D="$(dirname "$0")"
echo "gh $*" >> "$D/gh.log"
for a in "$@"; do case "$a" in
  *'pulls?state=open'*)
     echo '[{{"number":1,"head":{{"sha":"aaa1","ref":"feature/x-1111"}},"body":"b"}},{{"number":2,"head":{{"sha":"bbb2","ref":"feature/x-2222"}},"body":"b"}},{{"number":3,"head":{{"sha":"ccc3","ref":"patch-1","repo":{{"full_name":"stranger/footnote"}}}},"base":{{"ref":"main","repo":{{"full_name":"o/r"}}}},"author_association":"FIRST_TIMER","body":"b"}}]'
     exit 0 ;;
  *pulls/1*) echo '{{"head":{{"sha":"aaa1","ref":"feature/x-1111","repo":{{"full_name":"o/r"}}}},"base":{{"ref":"main","repo":{{"full_name":"o/r"}}}},"author_association":"OWNER","body":"b"}}'; exit 0 ;;
  *pulls/2*) echo '{{"head":{{"sha":"bbb2","ref":"feature/x-2222","repo":{{"full_name":"o/r"}}}},"base":{{"ref":"main","repo":{{"full_name":"o/r"}}}},"author_association":"OWNER","body":"b"}}'; exit 0 ;;
  *pulls/3*) echo '{{"head":{{"sha":"ccc3","ref":"patch-1","repo":{{"full_name":"stranger/footnote"}}}},"base":{{"ref":"main","repo":{{"full_name":"o/r"}}}},"author_association":"FIRST_TIMER","body":"b"}}'; exit 0 ;;
  *check-runs) echo '{{"check_runs":[{check}]}}'; exit 0 ;;
  */logs) echo "{log_line}"; exit 0 ;;
  */status) echo '{{"statuses":[]}}'; exit 0 ;;
esac; done
echo '[]'
"#
        ),
    )
}

/// A stub `git` that both lists one worktree (on feature/x-1111) and
/// answers run_one's branch/porcelain/commit/push questions inside it.
fn stub_git_drive(dir: &Path) -> std::path::PathBuf {
    let wt = dir.join("wt");
    let wt = wt.to_string_lossy().into_owned();
    write_exec(
        dir,
        "git",
        &format!(
            r#"#!/bin/sh
D="$(dirname "$0")"
echo "git $*" >> "$D/git.log"
case "$1 $2" in
  "worktree list") printf 'worktree {wt}\nHEAD aaa\nbranch refs/heads/feature/x-1111\n\n'; exit 0 ;;
  "rev-parse --abbrev-ref") echo feature/x-1111; exit 0 ;;
  "status --porcelain") if [ -f "$D/fixed" ]; then echo ' M src/x.rs'; fi; exit 0 ;;
esac
exit 0
"#
        ),
    )
}

/// The drive-loop stub gh with PR 1's one red check a CANCELLED run
/// (Actions run 777): the class the rerun remedy exists for.
fn stub_gh_drive_rerun(dir: &Path) -> std::path::PathBuf {
    write_exec(
        dir,
        "gh",
        r#"#!/bin/sh
D="$(dirname "$0")"
echo "gh $*" >> "$D/gh.log"
for a in "$@"; do case "$a" in
  *'pulls?state=open'*)
     echo '[{"number":1,"head":{"sha":"aaa1","ref":"feature/x-1111"},"body":"b"},{"number":2,"head":{"sha":"bbb2","ref":"feature/x-2222"},"body":"b"}]'
     exit 0 ;;
  *pulls/1*) echo '{"head":{"sha":"aaa1","ref":"feature/x-1111","repo":{"full_name":"o/r"}},"base":{"ref":"main","repo":{"full_name":"o/r"}},"author_association":"OWNER","body":"b"}'; exit 0 ;;
  *pulls/2*) echo '{"head":{"sha":"bbb2","ref":"feature/x-2222","repo":{"full_name":"o/r"}},"base":{"ref":"main","repo":{"full_name":"o/r"}},"author_association":"OWNER","body":"b"}'; exit 0 ;;
  *check-runs) echo '{"check_runs":[{"name":"ci","status":"completed","conclusion":"cancelled","html_url":"https://github.com/o/r/actions/runs/777/job/9"}]}'; exit 0 ;;
  */logs) echo "the run was cancelled before any step ran"; exit 0 ;;
  */status) echo '{"statuses":[]}'; exit 0 ;;
esac; done
echo '[]'
"#,
    )
}

/// A stub `fno` logging every invocation. Heal files fleet tasks by
/// writing the store directly, so the inbox lanes are gone; the stub
/// exists for the verbs the seams still shell (`do pr push`, `backlog`).
fn stub_fno(dir: &Path) {
    write_exec(
        dir,
        "fno",
        r#"#!/bin/sh
D="$(dirname "$0")"
echo "fno $*" >> "$D/fno.log"
exit 0
"#,
    );
}

/// Hold a live claim on `x-2222` under the given root, as a worker would.
fn hold_claim(dir: &Path) {
    let opts = crate::claims::AcquireOpts {
        pid: Some(std::process::id()),
        root: Some(dir.to_path_buf()),
        events_dir: Some(dir.to_path_buf()),
        ..Default::default()
    };
    match crate::claims::acquire("node:x-2222", "worker:t-x", opts) {
        crate::claims::AcquireOutcome::Acquired(_) => {}
        other => panic!("claim setup failed: {other:?}"),
    }
}

/// Drive-loop argv with the claim root and the events file pinned to the
/// test dir, so the two side effects never touch the real `~/.fno`.
fn drive_args(dir: &Path, extra: &[&str]) -> Vec<String> {
    let mut v = args_for(dir, &["--all", "--apply"]);
    v.push("--claims-root".to_string());
    v.push(dir.to_string_lossy().into_owned());
    v.push("--events-file".to_string());
    v.push(dir.join("events.jsonl").to_string_lossy().into_owned());
    v.extend(extra.iter().map(|s| s.to_string()));
    v
}

#[test]
fn drive_rows() {
    // Parity cases from the Python half and the CI gate: a plain branch
    // names nothing, a node branch names its node, and a trailing segment
    // never re-glues into a second, bogus candidate.
    assert_eq!(branch_node_ids("main"), Vec::<String>::new());
    assert_eq!(branch_node_ids("fix/respawn-race"), Vec::<String>::new());
    assert_eq!(
        branch_node_ids("feature/x-bbbb"),
        vec!["x-bbbb".to_string()]
    );
    assert_eq!(
        branch_node_ids("feature/x-cccc-1234"),
        vec!["x-cccc".to_string()],
        "the all-hex suffix must not re-glue into cdef-1234"
    );
    // Fixed-width hex makes a prefix of x-5b667: only the
    // delimiter-bounded one counts.
    assert_eq!(
        branch_node_ids("feature/x-5b667"),
        vec!["x-5b667".to_string()]
    );

    let tmp = tempfile::tempdir().unwrap();
    let d = tmp.path();
    stub_gh_drive(d, false);
    stub_git_drive(d);
    stub_cargo(d);
    stub_fno(d);
    hold_claim(d);
    std::fs::create_dir_all(d.join("wt/crates/fno-agents")).unwrap();
    let code = run_heal(&drive_args(d, &["--dry-run"]));
    assert_eq!(
        code, EXIT_ESCALATIONS,
        "a rehearsal over a would-heal PR reports the work, exactly like the report-only --all"
    );
    let out = log_of(d, "gh.log");
    assert!(out.contains("pulls/2"), "the claimed PR was read: {out}");
    assert_eq!(log_of(d, "cargo.log"), "", "a dry run runs no remedy");
    assert!(!log_of(d, "git.log").contains("push"), "and pushes nothing");
    // The tick row lands with the dry_run marker and both verdicts.
    let events = log_of(d, "events.jsonl");
    assert!(events.contains("pr_heal_tick"), "{events}");
    assert!(events.contains("\"dry_run\":true"), "{events}");
    assert!(events.contains("\"skip_claim_held\":1"), "{events}");
    assert!(events.contains("\"skip_outside\":1"), "{events}");
    assert!(events.contains("\"would_heal\":1"), "{events}");

    let tmp = tempfile::tempdir().unwrap();
    let d = tmp.path();
    stub_gh_drive(d, false);
    stub_git_drive(d);
    stub_cargo(d);
    stub_fno(d);
    hold_claim(d);
    std::fs::create_dir_all(d.join("wt/crates/fno-agents")).unwrap();
    let code = run_heal(&drive_args(d, &[]));
    assert_eq!(code, EXIT_CLEAN, "the free PR healed: {code}");
    let git = log_of(d, "git.log");
    assert_eq!(git.matches("git push").count(), 1, "one push: {git}");
    assert_ne!(log_of(d, "cargo.log"), "", "the remedy ran");
    let git_fork = log_of(d, "git.log");
    assert!(
        !git_fork.contains("patch-1"),
        "no git work for the fork: {git_fork}"
    );
    let events = log_of(d, "events.jsonl");
    assert!(events.contains("\"healed\":1"), "{events}");
    assert!(events.contains("\"steward_off_no_law\":1"), "{events}");
    assert!(events.contains("\"skip_claim_held\":1"), "{events}");
    assert!(events.contains("\"skip_outside\":1"), "{events}");

    // The single-PR path skips an outside PR before any worktree
    // read: exit clean, no git call.
    let tmp = tempfile::tempdir().unwrap();
    let d = tmp.path();
    stub_gh_drive(d, false);
    stub_git_drive(d);
    stub_cargo(d);
    stub_fno(d);
    let code = run_heal(&args_for(d, &["3", "--apply"]));
    assert_eq!(code, EXIT_CLEAN, "the fork PR skips clean: {code}");
    assert_eq!(log_of(d, "git.log"), "", "no git call for the fork PR");

    let tmp = tempfile::tempdir().unwrap();
    let d = tmp.path();
    stub_gh_drive(d, false);
    // No stub_git_drive: `git worktree list` fails, so no worktree is
    // found for the free PR's branch.
    stub_git(d, "feature/x-1111", false);
    stub_cargo(d);
    stub_fno(d);
    hold_claim(d);
    std::fs::create_dir_all(d.join("wt/crates/fno-agents")).unwrap();
    let code = run_heal(&drive_args(d, &[]));
    assert_ne!(code, EXIT_CLEAN, "the red PR is still red");
    assert_eq!(log_of(d, "cargo.log"), "", "no remedy ran anywhere");
    let events = log_of(d, "events.jsonl");
    assert!(events.contains("skip_no_worktree"), "{events}");

    let tmp = tempfile::tempdir().unwrap();
    let d = tmp.path();
    stub_gh(d, false);
    stub_git(d, "feature/x", false);
    assert_eq!(run_heal(&args_for(d, &["--playbook"])), EXIT_CLEAN);
    assert_eq!(log_of(d, "gh.log"), "", "no read");

    assert_eq!(run_heal(&["--nope".to_string()]), EXIT_READ_ERROR);

    // --slurp hands back an array of pages, so nothing has to find a
    // boundary. The old hand-rolled `split("][")` cut inside any PR body
    // carrying a markdown reference link and dropped those PRs silently.
    let one: Vec<Value> = vec![json!([{"number": 7}, {"number": 9}])];
    assert_eq!(open_pr_numbers(&one), vec!["7", "9"]);

    let many: Vec<Value> = vec![json!([{"number": 7}]), json!([{"number": 9}])];
    assert_eq!(open_pr_numbers(&many), vec!["7", "9"]);

    let with_bracket_pair: Vec<Value> = vec![json!([{"number": 7, "body": "see [the doc][ref]"}])];
    assert_eq!(open_pr_numbers(&with_bracket_pair), vec!["7"]);

    // Keeping the LAST non-clean code let a later escalation mask an
    // earlier failure to read the world at all.
    assert_eq!(worse_of(EXIT_READ_ERROR, EXIT_ESCALATIONS), EXIT_READ_ERROR);
    assert_eq!(worse_of(EXIT_ESCALATIONS, EXIT_IN_FLIGHT), EXIT_ESCALATIONS);
    assert_eq!(worse_of(EXIT_CLEAN, EXIT_IN_FLIGHT), EXIT_IN_FLIGHT);
    assert_eq!(worse_of(EXIT_CLEAN, EXIT_CLEAN), EXIT_CLEAN);

    // The guard joins candidates with commas and NO space, so a capture
    // that cannot cross a comma matched nothing and the remedy became a
    // silent no-op on exactly the branches that needed it most.
    let log = concat!(
        "check-pr-node-closure: HEAD ref 'feature/x-a1-x-b2' names x-a1,x-b2, ",
        "and the exact trailer claims none of them.\n",
    );
    let f = classify(&ctx("check-pr-node-closure", log), false);
    assert_eq!(
        f.remedy,
        Remedy::EditBody {
            nodes: vec!["x-a1".to_string(), "x-b2".to_string()]
        }
    );

    let unknown = json!({"status": "completed", "conclusion": "action_required"});
    assert_eq!(rest_bucket(&unknown), "fail");
    assert_eq!(
        rest_bucket(&json!({"status": "completed", "conclusion": "success"})),
        "pass"
    );
    assert_eq!(
        rest_bucket(&json!({"status": "queued", "conclusion": null})),
        "pending"
    );
}

#[test]
fn rerun_rows() {
    // The rerun comes first, exactly once per (sha, run id); only a
    // second red on the same pair is the verdict. The escalation names
    // the double failure, so the operator reads a verdict, not a guess.
    let tmp = tempfile::tempdir().unwrap();
    let d = tmp.path();
    stub_gh_drive(d, true);
    stub_git_drive(d);
    stub_fno(d);
    hold_claim(d);
    std::fs::create_dir_all(d.join("wt/crates/fno-agents")).unwrap();
    let code1 = run_heal(&drive_args(d, &[]));
    assert_eq!(
        code1, EXIT_CLEAN,
        "the rerun acted: first sighting exits clean"
    );
    let gh = log_of(d, "gh.log");
    assert_eq!(gh.matches("run rerun 1 --failed").count(), 1, "{gh}");
    let store_before =
        crate::event_store::journal_text(&d.join("questions.jsonl"), &["fleet_task"]);
    assert!(
        !store_before.contains("fleet_task"),
        "no task before the rerun answers: {store_before}"
    );
    run_heal(&drive_args(d, &[]));
    let store = crate::event_store::journal_text(&d.join("questions.jsonl"), &["fleet_task"]);
    assert_eq!(
        store.matches(r#""type":"fleet_task""#).count(),
        1,
        "exactly one task: {store}"
    );
    assert!(
        store.contains("no playbook signature") && store.contains("mystery-check"),
        "{store}"
    );
    assert!(store.contains(r#""run":"fno do pr heal 1""#), "{store}");
    // The next tick re-files nothing: the open task dedups on its key.
    run_heal(&drive_args(d, &[]));
    let store = crate::event_store::journal_text(&d.join("questions.jsonl"), &["fleet_task"]);
    assert_eq!(
        store.matches(r#""type":"fleet_task""#).count(),
        1,
        "the open task is never re-filed: {store}"
    );
    let events = log_of(d, "events.jsonl");
    assert!(events.contains("\"unknown\":1"), "{events}");

    let f = classify(
        &cancelled_run_ctx("ci", "https://github.com/o/r/actions/runs/777/job/9"),
        false,
    );
    assert_eq!(f.signature, "cancelled");
    assert_eq!(
        f.remedy,
        Remedy::Rerun {
            run_id: "777".to_string()
        }
    );
    assert_eq!(f.action(), "rerun");
    assert_eq!(f.detail(), "gh run rerun 777");
    assert!(f.counts_against_pr());

    // A link that names no run (a StatusContext, or no link at all)
    // leaves nothing to rerun; the escalation is the honest answer.
    let f = classify(&cancelled_ctx("ci", ""), false);
    assert_eq!(f.signature, "cancelled");
    assert!(
        matches!(&f.remedy, Remedy::Escalate { repro } if repro.contains("cancelled")),
        "{:?}",
        f.remedy
    );

    let tmp = tempfile::tempdir().unwrap();
    let d = tmp.path();
    stub_gh_drive_rerun(d);
    stub_git_drive(d);
    stub_fno(d);
    hold_claim(d);
    std::fs::create_dir_all(d.join("wt/crates/fno-agents")).unwrap();
    let code = run_heal(&drive_args(d, &[]));
    assert_eq!(code, EXIT_CLEAN, "the rerun acted: the PR exits clean");
    let gh = log_of(d, "gh.log");
    assert_eq!(
        gh.matches("run rerun 777").count(),
        1,
        "exactly one rerun: {gh}"
    );
    let events = log_of(d, "events.jsonl");
    assert!(events.contains("\"reran\":1"), "{events}");
    assert!(events.contains("\"rerun_keys\":[\"aaa1:777\"]"), "{events}");

    // The guard reads rerun_shas off pr_heal_tick rows; the drive loop
    // writes its own. A manual `pr-heal <n> --apply` writes one too, so
    // a hand rerun is never issued a second time by the next cycle.
    let tmp = tempfile::tempdir().unwrap();
    let d = tmp.path();
    stub_gh_drive_rerun(d);
    stub_git_drive(d);
    stub_fno(d);
    hold_claim(d);
    std::fs::create_dir_all(d.join("wt/crates/fno-agents")).unwrap();
    let mut args = args_for(d, &["--apply"]);
    args.push("--claims-root".to_string());
    args.push(d.to_string_lossy().into_owned());
    args.push("--events-file".to_string());
    args.push(d.join("events.jsonl").to_string_lossy().into_owned());
    run_heal(&args);
    run_heal(&args);
    let gh = log_of(d, "gh.log");
    assert_eq!(
        gh.matches("run rerun 777").count(),
        1,
        "the second manual apply never re-runs the sha: {gh}"
    );

    // The journal is the once-guard: a prior pr_heal_tick row naming the
    // sha in rerun_shas means the rerun was already issued; a second
    // cancelled verdict on the same sha reached a real result.
    let tmp = tempfile::tempdir().unwrap();
    let d = tmp.path();
    stub_gh_drive_rerun(d);
    stub_git_drive(d);
    stub_fno(d);
    hold_claim(d);
    std::fs::create_dir_all(d.join("wt/crates/fno-agents")).unwrap();
    run_heal(&drive_args(d, &[]));
    run_heal(&drive_args(d, &[]));
    let gh = log_of(d, "gh.log");
    assert_eq!(
        gh.matches("run rerun 777").count(),
        1,
        "the second cycle never re-runs the sha: {gh}"
    );
    let events = log_of(d, "events.jsonl");
    assert!(
        events.contains("\"still_red\":1"),
        "the demoted rerun is still red, never healed: {events}"
    );
}

// ── the rebase triggers ───────────────────────────────────────────────

/// A stub `gh` for the rebase tests: PR 1 carries `mergeable` (the test
/// names its value) and a `base` of main. Its one red check is
/// rustfmt-drift, so if the loop ever reached the heal path the remedy
/// would run and cargo.log would name it.
fn stub_gh_drive_rebase(dir: &Path, pr1_mergeable: &str) {
    let body = r#"#!/bin/sh
D="$(dirname "$0")"
echo "gh $*" >> "$D/gh.log"
for a in "$@"; do case "$a" in
  *'pulls?state=open'*)
     echo '[{"number":1,"head":{"sha":"aaa1","ref":"feature/x-1111"},"base":{"ref":"main"},"mergeable":MERGEABLE,"body":"b"},{"number":2,"head":{"sha":"bbb2","ref":"feature/x-2222"},"base":{"ref":"main"},"mergeable":null,"body":"b"}]'
     exit 0 ;;
  *pulls/1*) echo '{"head":{"sha":"aaa1","ref":"feature/x-1111","repo":{"full_name":"o/r"}},"base":{"ref":"main","repo":{"full_name":"o/r"}},"author_association":"OWNER","mergeable":MERGEABLE,"body":"b"}'; exit 0 ;;
  *pulls/2*) echo '{"head":{"sha":"bbb2","ref":"feature/x-2222","repo":{"full_name":"o/r"}},"base":{"ref":"main","repo":{"full_name":"o/r"}},"author_association":"OWNER","mergeable":null,"body":"b"}'; exit 0 ;;
  *check-runs) echo '{"check_runs":[{"name":"cargo fmt --check (pinned)","status":"completed","conclusion":"failure","html_url":"https://github.com/o/r/actions/runs/1/job/9"}]}'; exit 0 ;;
  */logs) echo "Diff in /w/w/crates/fno-agents/src/x.rs:1:"; exit 0 ;;
  */status) echo '{"statuses":[]}'; exit 0 ;;
esac; done
echo '[]'
"#
    .replace("MERGEABLE", pr1_mergeable);
    write_exec(dir, "gh", &body);
}

/// A stub `fno` whose `do pr push` seam answers a caller-named
/// stdout/exit/stderr triple; the backlog lane logs and answers.
fn stub_fno_push(dir: &Path, push_stdout: &str, push_exit: u8, push_stderr: &str) {
    let body = r#"#!/bin/sh
D="$(dirname "$0")"
echo "fno $*" >> "$D/fno.log"
case "$*" in
  *"do pr push"*) printf '%s' 'PUSH_STDOUT' ; printf '%s' 'PUSH_STDERR' >&2; exit PUSH_EXIT ;;
  *"backlog idea"*) echo "backlog node fno-abc9 created"; exit 0 ;;
esac
exit 0
"#
    .replace("PUSH_STDOUT", push_stdout)
    .replace("PUSH_EXIT", &push_exit.to_string())
    .replace("PUSH_STDERR", push_stderr);
    write_exec(dir, "fno", &body);
}

/// Hold a live `merge-slot:main` claim naming `pr`, as the sweep would.
fn hold_slot_claim(dir: &Path, pr: u64) {
    let opts = crate::claims::AcquireOpts {
        pid: Some(std::process::id()),
        root: Some(dir.to_path_buf()),
        events_dir: Some(dir.to_path_buf()),
        ..Default::default()
    };
    match crate::claims::acquire("merge-slot:main", &format!("pr:{pr}"), opts) {
        crate::claims::AcquireOutcome::Acquired(_) => {}
        other => panic!("slot claim setup failed: {other:?}"),
    }
}

#[test]
fn rebase_rows() {
    let tmp = tempfile::tempdir().unwrap();
    let d = tmp.path();
    stub_gh_drive_rebase(d, "false");
    stub_git_drive(d);
    stub_cargo(d);
    std::fs::create_dir_all(d.join("wt/crates/fno-agents")).unwrap();
    stub_fno_push(
        d,
        "pr-push: origin/main behind-before=9 behind-after=0 preflight=full ci=settled sha=abc pushed=1",
        0,
        "",
    );
    let code = run_heal(&drive_args(d, &[]));
    // PR 2 has no worktree and reads as an escalation; PR 1's rebase
    // itself exits clean.
    assert_eq!(code, EXIT_ESCALATIONS);
    let fno = log_of(d, "fno.log");
    assert_eq!(fno.matches("do pr push").count(), 1, "one push: {fno}");
    assert_eq!(log_of(d, "cargo.log"), "", "no heal in the same pass");
    let events = log_of(d, "events.jsonl");
    assert!(events.contains("\"rebased\":1"), "{events}");
    assert!(events.contains("pr_heal_pr"), "{events}");
    // The emitter's own row, read back the way --status reads it: the
    // acted list must survive the string-to-number trip.
    let sa = parse_args(&[
        "--status".to_string(),
        "--armed".to_string(),
        "--events-file".to_string(),
        d.join("events.jsonl").to_string_lossy().into_owned(),
    ])
    .unwrap();
    assert!(
        status_line(&sa).contains("acted on PR 1"),
        "{}",
        status_line(&sa)
    );

    // `mergeable` reads null while GitHub computes; the merge slot is the
    // other trigger, and the ONLY reason this PR rebases. PR 2 conflicts
    // but holds no slot and has no worktree, so nothing else is rebased.
    let tmp = tempfile::tempdir().unwrap();
    let d = tmp.path();
    stub_gh_drive_rebase(d, "null");
    stub_git_drive(d);
    stub_cargo(d);
    std::fs::create_dir_all(d.join("wt/crates/fno-agents")).unwrap();
    stub_fno_push(
        d,
        "pr-push: origin/main behind-before=3 behind-after=0 preflight=skipped sha=abc pushed=1",
        0,
        "",
    );
    hold_slot_claim(d, 1);
    run_heal(&drive_args(d, &[]));
    let fno = log_of(d, "fno.log");
    assert_eq!(fno.matches("do pr push").count(), 1, "{fno}");
    let events = log_of(d, "events.jsonl");
    assert!(events.contains("\"rebased\":1"), "{events}");
    assert!(events.contains("\"skip_no_worktree\":1"), "{events}");

    // Refusal 1 runs FIRST: a conflicting PR whose node a live worker
    // holds is never rebased by the healer. PR 1 holds no trigger
    // (mergeable null, no slot), PR 2's node is claim-held and
    // conflicting, so no push may happen at all.
    let tmp = tempfile::tempdir().unwrap();
    let d = tmp.path();
    stub_gh_drive_rebase(d, "null");
    stub_git_drive(d);
    stub_cargo(d);
    std::fs::create_dir_all(d.join("wt/crates/fno-agents")).unwrap();
    stub_fno_push(d, "", 0, "");
    hold_claim(d);
    run_heal(&drive_args(d, &[]));
    let fno = log_of(d, "fno.log");
    assert!(
        !fno.contains("do pr push"),
        "the held PR is never rebased: {fno}"
    );
    let events = log_of(d, "events.jsonl");
    assert!(events.contains("\"skip_claim_held\":1"), "{events}");

    let tmp = tempfile::tempdir().unwrap();
    let d = tmp.path();
    // Seven conflicting PRs, all on the one branch the git stub lists a
    // worktree for: the cap binds on the 7th, and PR 7 gets the skip.
    let mut prs = Vec::new();
    for n in 1..=7 {
        prs.push(format!(
            "{{\"number\":{n},\"head\":{{\"sha\":\"aaa1\",\"ref\":\"feature/x-1111\"}},\"base\":{{\"ref\":\"main\"}},\"mergeable\":false}}"
        ));
    }
    let listing = format!("[{}]", prs.join(","));
    let body = r#"#!/bin/sh
D="$(dirname "$0")"
echo "gh $*" >> "$D/gh.log"
for a in "$@"; do case "$a" in
  *'pulls?state=open'*) echo 'LISTING'
     exit 0 ;;
  *pulls/*) echo '{"head":{"sha":"aaa1","ref":"feature/x-1111","repo":{"full_name":"o/r"}},"base":{"ref":"main","repo":{"full_name":"o/r"}},"author_association":"OWNER","mergeable":false,"body":"b"}'; exit 0 ;;
  *check-runs) echo '{"check_runs":[]}'; exit 0 ;;
  */status) echo '{"statuses":[]}'; exit 0 ;;
esac; done
echo '[]'
"#
    .replace("LISTING", &listing);
    write_exec(d, "gh", &body);
    stub_git_drive(d);
    stub_cargo(d);
    std::fs::create_dir_all(d.join("wt/crates/fno-agents")).unwrap();
    stub_fno_push(
        d,
        "pr-push: origin/main behind-before=1 behind-after=0 preflight=absent sha=abc pushed=1",
        0,
        "",
    );
    run_heal(&drive_args(d, &[]));
    let fno = log_of(d, "fno.log");
    assert_eq!(fno.matches("do pr push").count(), 6, "{fno}");
    let events = log_of(d, "events.jsonl");
    assert!(events.contains("\"skip_rebase_budget\":1"), "{events}");

    // A push that moved nothing is receipted skip_rebase_noop, never
    // rebased: no acted row, no per-run budget spent, and the checks of
    // the live sha classify below.
    let tmp = tempfile::tempdir().unwrap();
    let d = tmp.path();
    stub_gh_drive_rebase(d, "false");
    stub_git_drive(d);
    stub_cargo(d);
    std::fs::create_dir_all(d.join("wt/crates/fno-agents")).unwrap();
    stub_fno_push(
        d,
        "pr-push: origin/main behind-before=3 behind-after=3 integrate=none sha=abc pushed=0 nothing to push",
        0,
        "",
    );
    run_heal(&drive_args(d, &[]));
    let fno = log_of(d, "fno.log");
    assert_eq!(fno.matches("do pr push").count(), 1, "{fno}");
    let events = log_of(d, "events.jsonl");
    assert!(events.contains("\"skip_rebase_noop\":1"), "{events}");
    assert!(
        events.contains("\"action\":\"skip_rebase_noop\""),
        "{events}"
    );
    assert!(!events.contains("\"rebased\":1"), "{events}");
}

// ── the detached drive loop ───────────────────────────────────────────

fn detach_args(dir: &Path) -> Vec<String> {
    drive_args(dir, &["--detach"])
}

/// The clear verdict, as a closure the detach runner reads; the armed
/// test passes its own.
fn clear_pause() -> crate::loops_pause::DispatchPause {
    crate::loops_pause::DispatchPause::Clear
}

#[test]
fn detach_rows() {
    let tmp = tempfile::tempdir().unwrap();
    let d = tmp.path();
    let a = parse_args(&detach_args(d)).unwrap();
    let argv_sent = detach_args(d);
    let spawned = std::cell::RefCell::new(Vec::<Vec<String>>::new());
    let spawn = |argv: &[String]| -> std::io::Result<u32> {
        spawned.borrow_mut().push(argv.to_vec());
        Ok(std::process::id())
    };
    let code = run_detached(&a, &argv_sent, &clear_pause, &spawn);
    assert_eq!(code, EXIT_CLEAN);
    let pid_file = heal_pid_file(&a);
    assert_eq!(
        read_pid_file(&pid_file),
        Some(std::process::id()),
        "the pid file names the child"
    );
    let calls = spawned.borrow();
    assert_eq!(calls.len(), 1, "one spawn: {calls:?}");
    let child = &calls[0];
    assert_eq!(child[1], "pr-heal", "the child argv re-adds the verb");
    assert!(
        !child.iter().any(|s| s == "--detach"),
        "the child runs the real loop: {child:?}"
    );
    assert!(
        child
            .windows(2)
            .any(|w| w[0] == "--cwd" && Path::new(&w[1]) == d),
        "the child heals this root: {child:?}"
    );
    drop(calls);
    let events = log_of(d, "events.jsonl");
    assert!(events.contains("control_plane_tick"), "{events}");
    assert!(events.contains("\"arm\":\"heal\""), "{events}");
    assert!(events.contains("\"acted\":1"), "{events}");

    // A drive loop still running when the next tick fires is skipped,
    // never double-spawned (the in-flight case the pid file exists for).
    let tmp = tempfile::tempdir().unwrap();
    let d = tmp.path();
    let a = parse_args(&detach_args(d)).unwrap();
    let pid_file = heal_pid_file(&a);
    std::fs::create_dir_all(pid_file.parent().unwrap()).unwrap();
    std::fs::write(&pid_file, format!("{}\n", std::process::id())).unwrap();
    let spawned = std::cell::RefCell::new(0);
    let spawn = |_: &[String]| -> std::io::Result<u32> {
        *spawned.borrow_mut() += 1;
        Ok(std::process::id())
    };
    let code = run_detached(&a, &detach_args(d), &clear_pause, &spawn);
    assert_eq!(code, EXIT_CLEAN);
    assert_eq!(*spawned.borrow(), 0, "never double-spawned");
    let events = log_of(d, "events.jsonl");
    assert!(events.contains("\"acted\":0"), "{events}");
    assert!(events.contains("in_flight"), "{events}");

    // The breaker exists to stop spawns: the drive loop obeys it on its
    // own arm row instead of feeding the admission gate a spawn to
    // refuse, and the tick still completes its other legs.
    let tmp = tempfile::tempdir().unwrap();
    let d = tmp.path();
    let a = parse_args(&detach_args(d)).unwrap();
    let spawned = std::cell::RefCell::new(0);
    let spawn = |_: &[String]| -> std::io::Result<u32> {
        *spawned.borrow_mut() += 1;
        Ok(std::process::id())
    };
    let armed = || crate::loops_pause::DispatchPause::FleetIncident {
        generation: 9,
        reason: "rustc storm".to_string(),
        holds: vec!["spawns".to_string(), "tests".to_string()],
    };
    let code = run_detached(&a, &detach_args(d), &armed, &spawn);
    assert_eq!(code, EXIT_CLEAN);
    assert_eq!(*spawned.borrow(), 0, "nothing spawned under the breaker");
    assert!(
        read_pid_file(&heal_pid_file(&a)).is_none(),
        "no pid file under an armed breaker"
    );
    let events = log_of(d, "events.jsonl");
    assert!(events.contains("\"arm\":\"heal\""), "{events}");
    assert!(events.contains("\"acted\":0"), "{events}");
    assert!(events.contains("fleet_stop"), "{events}");
    assert!(events.contains("generation 9"), "{events}");

    let tmp = tempfile::tempdir().unwrap();
    let d = tmp.path();
    // A real, now-dead pid: spawned, waited, gone.
    let dead = {
        let mut c = std::process::Command::new("/usr/bin/true").spawn().unwrap();
        let pid = c.id();
        c.wait().unwrap();
        pid
    };
    let a = parse_args(&detach_args(d)).unwrap();
    let pid_file = heal_pid_file(&a);
    std::fs::create_dir_all(pid_file.parent().unwrap()).unwrap();
    std::fs::write(&pid_file, format!("{dead}\n")).unwrap();
    let spawned = std::cell::RefCell::new(0);
    let spawn = |_: &[String]| -> std::io::Result<u32> {
        *spawned.borrow_mut() += 1;
        Ok(std::process::id())
    };
    let code = run_detached(&a, &detach_args(d), &clear_pause, &spawn);
    assert_eq!(code, EXIT_CLEAN);
    assert_eq!(
        *spawned.borrow(),
        1,
        "a dead pid is never mistaken for a live loop"
    );
    assert_eq!(
        read_pid_file(&heal_pid_file(&a)),
        Some(std::process::id()),
        "the pid file names the new child"
    );

    // The per-root Python loop is gone: one spawn whose child argv
    // carries every --cwd, one pid file. The second root rides
    // drive_args' own --cwd plus one more.
    let tmp = tempfile::tempdir().unwrap();
    let d = tmp.path();
    let other = tempfile::tempdir().unwrap();
    let other_s = other.path().to_str().unwrap();
    let argv = drive_args(d, &["--detach", "--cwd", other_s]);
    let a = parse_args(&argv).unwrap();
    assert_eq!(
        a.roots,
        vec![d.to_path_buf(), other.path().to_path_buf()],
        "both roots parsed, deduplicated"
    );
    let spawned = std::cell::RefCell::new(Vec::<Vec<String>>::new());
    let spawn = |argv: &[String]| -> std::io::Result<u32> {
        spawned.borrow_mut().push(argv.to_vec());
        Ok(std::process::id())
    };
    let code = run_detached(&a, &argv, &clear_pause, &spawn);
    assert_eq!(code, EXIT_CLEAN);
    let calls = spawned.borrow();
    assert_eq!(calls.len(), 1, "one spawn: {calls:?}");
    let child = &calls[0];
    for root in [d, other.path()] {
        assert!(
            child
                .windows(2)
                .any(|w| w[0] == "--cwd" && Path::new(&w[1]) == root),
            "the child carries --cwd {root:?}: {child:?}"
        );
    }
    drop(calls);
    assert_eq!(
        heal_pid_file(&a),
        events_dir(&a).join("heal").join("pr-heal.pid"),
        "one root-free pid file, in its own subfolder"
    );
    assert_eq!(
        read_pid_file(&heal_pid_file(&a)),
        Some(std::process::id()),
        "one pid file written"
    );

    // The in-flight guard reads the ONE pid file whatever root asked:
    // the per-root files never let it see a loop started for a root
    // other than its own.
    let tmp = tempfile::tempdir().unwrap();
    let d = tmp.path();
    let other = tempfile::tempdir().unwrap();
    let other_s = other.path().to_str().unwrap();
    let argv = drive_args(d, &["--detach", "--cwd", other_s]);
    let a = parse_args(&argv).unwrap();
    let pid_file = heal_pid_file(&a);
    std::fs::create_dir_all(pid_file.parent().unwrap()).unwrap();
    std::fs::write(&pid_file, format!("{}\n", std::process::id())).unwrap();
    let spawned = std::cell::RefCell::new(0);
    let spawn = |_: &[String]| -> std::io::Result<u32> {
        *spawned.borrow_mut() += 1;
        Ok(std::process::id())
    };
    let code = run_detached(&a, &argv, &clear_pause, &spawn);
    assert_eq!(code, EXIT_CLEAN);
    assert_eq!(*spawned.borrow(), 0, "never double-spawned");
    let events = log_of(d, "events.jsonl");
    assert!(events.contains("\"acted\":0"), "{events}");
    assert!(events.contains("in_flight"), "{events}");

    assert_eq!(
        run_heal(&["--detach".to_string()]),
        EXIT_READ_ERROR,
        "--detach without --all --apply has nothing to detach"
    );
}

#[test]
fn tick_rows() {
    // Deadline already spent: no PR is read, every PR gets one
    // skip_deadline receipt, and the tick row carries the count.
    let tmp = tempfile::tempdir().unwrap();
    let d = tmp.path();
    let listing = format!(
        "[{},{},{}]",
        r#"{"number":1,"head":{"sha":"aaa1","ref":"feature/x-1111"},"base":{"ref":"main"},"mergeable":null}"#,
        r#"{"number":2,"head":{"sha":"bbb2","ref":"feature/x-2222"},"base":{"ref":"main"},"mergeable":null}"#,
        r#"{"number":3,"head":{"sha":"ccc3","ref":"feature/x-3333"},"base":{"ref":"main"},"mergeable":null}"#,
    );
    let body = r#"#!/bin/sh
D="$(dirname "$0")"
echo "gh $*" >> "$D/gh.log"
for a in "$@"; do case "$a" in
  *'pulls?state=open'*) echo 'LISTING'; exit 0 ;;
esac; done
echo '[]'
"#
    .replace("LISTING", &listing);
    write_exec(d, "gh", &body);
    stub_git_drive(d);
    stub_cargo(d);
    let mut a = parse_args(&drive_args(d, &[])).unwrap();
    a.deadline = Some(std::time::Instant::now() - std::time::Duration::from_secs(1));
    run_all_apply(&a, false);
    let gh = log_of(d, "gh.log");
    assert!(
        !gh.contains("pulls/"),
        "no PR is read past a spent deadline: {gh}"
    );
    let events = log_of(d, "events.jsonl");
    assert_eq!(
        events.matches("\"action\":\"skip_deadline\"").count(),
        3,
        "one receipt per unreached PR: {events}"
    );
    assert!(events.contains("\"skip_deadline\":3"), "{events}");

    // One process, one pr_heal_tick row per root, folded to the worst
    // verdict. Instant stubs and a generous budget: no timing dependence
    // (the skip itself is covered by the spent-deadline tests). The budget
    // sits above `read_bound` for every root, since the admission gate
    // refuses a root whose remaining share cannot fit one pre-loop read.
    let tmp = tempfile::tempdir().unwrap();
    let d = tmp.path();
    let roots: Vec<std::path::PathBuf> = (0..3)
        .map(|i| {
            let p = d.join(format!("root-{i}"));
            std::fs::create_dir_all(p.join(".git")).unwrap();
            p
        })
        .collect();
    let body = r#"#!/bin/sh
D="$(dirname "$0")"
echo "gh $*" >> "$D/gh.log"
pwd >> "$D/gh.order"
for a in "$@"; do case "$a" in
  *'pulls?state=open'*) echo '[]'; exit 0 ;;
esac; done
echo '[]'
"#;
    write_exec(d, "gh", body);
    stub_git_drive(d);
    stub_cargo(d);
    let mut a = parse_args(&drive_args(d, &[])).unwrap();
    a.roots = roots.clone();
    a.deadline = Some(std::time::Instant::now() + std::time::Duration::from_secs(480));
    let code = run_roots_apply(&a, false);
    assert_eq!(code, EXIT_CLEAN);
    let events = log_of(d, "events.jsonl");
    for root in &roots {
        let row = events
            .lines()
            .find(|l| l.contains("pr_heal_tick") && l.contains(root.to_str().unwrap()))
            .unwrap_or_else(|| panic!("no tick row for {root:?}: {events}"));
        assert!(row.contains("\"dry_run\":false"), "{row}");
    }
    // Empty journal: heal order is the `--cwd` order.
    let order = log_of(d, "gh.order");
    let listed: Vec<&str> = order
        .lines()
        .filter(|l| !l.is_empty())
        .map(|l| l.rsplit('/').next().unwrap())
        .collect();
    assert_eq!(
        listed,
        vec!["root-0", "root-1", "root-2"],
        "input order on a fresh journal: {order}"
    );

    let tmp = tempfile::tempdir().unwrap();
    let a = parse_args(&args_for(tmp.path(), &[])).unwrap();
    assert_eq!(remedy_timeout(&a), REMEDY_TIMEOUT, "no deadline: unchanged");
    let mut short = a.clone();
    short.deadline = Some(std::time::Instant::now() + std::time::Duration::from_secs(10));
    assert!(
        remedy_timeout(&short) <= std::time::Duration::from_secs(10),
        "10s left: never 300s"
    );
    let mut spent = a.clone();
    spent.deadline = Some(std::time::Instant::now() - std::time::Duration::from_secs(1));
    assert_eq!(
        remedy_timeout(&spent),
        std::time::Duration::from_secs(1),
        "spent deadline: the 1s floor"
    );

    // One --cwd behaves as today, except the pr_heal_tick row now names
    // its root: the field the two-probe done-check reads.
    let tmp = tempfile::tempdir().unwrap();
    let d = tmp.path();
    stub_gh_drive_rebase(d, "null");
    stub_git_drive(d);
    stub_cargo(d);
    std::fs::create_dir_all(d.join("wt/crates/fno-agents")).unwrap();
    stub_fno_push(d, "", 0, "");
    run_heal(&drive_args(d, &[]));
    let events = log_of(d, "events.jsonl");
    assert!(
        events.contains(&format!("\"root\":\"{}\"", d.to_str().unwrap())),
        "{events}"
    );

    // Migration guard: a pre-upgrade loop still alive in an old
    // pr-heal.<root>.pid file holds the tick, so a binary swap cannot
    // double-spawn the healer while the old loop lives.
    let tmp = tempfile::tempdir().unwrap();
    let d = tmp.path();
    let a = parse_args(&detach_args(d)).unwrap();
    std::fs::write(
        events_dir(&a).join("pr-heal.some-root.pid"),
        format!("{}\n", std::process::id()),
    )
    .unwrap();
    let spawned = std::cell::RefCell::new(0);
    let spawn = |_: &[String]| -> std::io::Result<u32> {
        *spawned.borrow_mut() += 1;
        Ok(std::process::id())
    };
    let code = run_detached(&a, &detach_args(d), &clear_pause, &spawn);
    assert_eq!(code, EXIT_CLEAN);
    assert_eq!(
        *spawned.borrow(),
        0,
        "a live stale-file loop holds the tick"
    );
    let events = log_of(d, "events.jsonl");
    assert!(events.contains("in_flight"), "{events}");

    // A stale pid file outlives its loop and pids are recycled: the pid
    // in an old file may belong to any unrelated process. `ps` names the
    // owner; a non-healer owner never holds the tick (the P1 codex
    // finding: otherwise one stale file disables healing forever).
    let tmp = tempfile::tempdir().unwrap();
    let d = tmp.path();
    let a = parse_args(&detach_args(d)).unwrap();
    let sleeper = {
        let c = std::process::Command::new("/bin/sleep")
            .arg("15")
            .spawn()
            .unwrap();
        let pid = c.id();
        // Leave it running under its own pid for the ps read.
        std::mem::forget(c);
        pid
    };
    std::fs::write(
        events_dir(&a).join("pr-heal.old-root.pid"),
        format!("{sleeper}\n"),
    )
    .unwrap();
    let spawned = std::cell::RefCell::new(0);
    let spawn = |_: &[String]| -> std::io::Result<u32> {
        *spawned.borrow_mut() += 1;
        Ok(std::process::id())
    };
    let code = run_detached(&a, &detach_args(d), &clear_pause, &spawn);
    assert_eq!(code, EXIT_CLEAN);
    assert_eq!(
        *spawned.borrow(),
        1,
        "a recycled pid naming no healer never holds the tick"
    );

    // Zero slice, zero work: an exhausted budget runs no listing, no
    // worktree scan, no main-HEAD read for any root, and writes no tick
    // row (AC2 corner codex raised: the reads cost up to 60s each).
    let tmp = tempfile::tempdir().unwrap();
    let d = tmp.path();
    let ra = d.join("root-a");
    let rb = d.join("root-b");
    std::fs::create_dir_all(&ra).unwrap();
    std::fs::create_dir_all(&rb).unwrap();
    let body = r#"#!/bin/sh
D="$(dirname "$0")"
echo "gh $*" >> "$D/gh.log"
exit 0
"#;
    write_exec(d, "gh", body);
    stub_git_drive(d);
    stub_cargo(d);
    let mut a = parse_args(&drive_args(d, &[])).unwrap();
    a.roots = vec![ra, rb];
    a.deadline = Some(std::time::Instant::now() - std::time::Duration::from_secs(1));
    let code = run_roots_apply(&a, false);
    assert_eq!(code, EXIT_CLEAN);
    assert_eq!(log_of(d, "gh.log"), "", "no pre-loop read past the budget");
    let events = log_of(d, "events.jsonl");
    assert!(
        !events.contains("pr_heal_tick"),
        "no tick row for a root that never ran: {events}"
    );
}

#[test]
fn status_rows() {
    let a = parse_args(&["--status".to_string()]).unwrap();
    assert_eq!(
        status_line(&a),
        "Heal: unarmed (auto_heal.enabled=false; arm with: fno config set auto_heal.enabled true)"
    );

    let tmp = tempfile::tempdir().unwrap();
    let a = parse_args(&[
        "--status".to_string(),
        "--armed".to_string(),
        "--events-file".to_string(),
        tmp.path()
            .join("events.jsonl")
            .to_string_lossy()
            .into_owned(),
    ])
    .unwrap();
    assert_eq!(status_line(&a), "Heal: armed; never ran");

    let tmp = tempfile::tempdir().unwrap();
    let events = tmp.path().join("events.jsonl");
    let ts = (chrono::Utc::now() - chrono::Duration::minutes(12) - chrono::Duration::seconds(5))
        .to_rfc3339();
    std::fs::write(
        &events,
        format!(
            "{{\"ts\":\"{ts}\",\"type\":\"pr_heal_tick\",\"data\":{{\"healed\":1,\"escalated\":3}}}}\n"
        ),
    )
    .unwrap();
    let a = parse_args(&[
        "--status".to_string(),
        "--armed".to_string(),
        "--events-file".to_string(),
        events.to_string_lossy().into_owned(),
    ])
    .unwrap();
    let line = status_line(&a);
    assert!(line.starts_with("Heal: armed; last run "), "{line}");
    assert!(line.contains("(12m ago, root unknown)"), "{line}");
    assert!(
        line.contains("healed 1, rebased 0, reran 0, escalated 3"),
        "{line}"
    );
    assert!(line.contains("acted on nothing"), "{line}");
    assert!(line.contains("in-flight none"), "{line}");

    let tmp = tempfile::tempdir().unwrap();
    let events = tmp.path().join("events.jsonl");
    let ts = chrono::Utc::now().to_rfc3339();
    std::fs::write(
        &events,
        format!(
            "{{\"ts\":\"{ts}\",\"type\":\"pr_heal_tick\",\"data\":{{\"healed\":0,\"rebased\":1,\"reran\":2,\"escalated\":1,\"acted_prs\":[2155,2162]}}}}\n"
        ),
    )
    .unwrap();
    let a = parse_args(&[
        "--status".to_string(),
        "--armed".to_string(),
        "--events-file".to_string(),
        events.to_string_lossy().into_owned(),
    ])
    .unwrap();
    let line = status_line(&a);
    assert!(
        line.contains("healed 0, rebased 1, reran 2, escalated 1; acted on PR 2155, 2162"),
        "{line}"
    );

    let tmp = tempfile::tempdir().unwrap();
    let events = tmp.path().join("events.jsonl");
    let ts = (chrono::Utc::now() - chrono::Duration::hours(30)).to_rfc3339();
    std::fs::write(
        &events,
        format!("{{\"ts\":\"{ts}\",\"type\":\"pr_heal_tick\",\"data\":{{}}}}\n"),
    )
    .unwrap();
    let a = parse_args(&[
        "--status".to_string(),
        "--armed".to_string(),
        "--events-file".to_string(),
        events.to_string_lossy().into_owned(),
    ])
    .unwrap();
    assert!(
        status_line(&a).contains("(1d ago, root unknown)"),
        "{}",
        status_line(&a)
    );

    let tmp = tempfile::tempdir().unwrap();
    let events = tmp.path().join("events.jsonl");
    let ts = chrono::Utc::now().to_rfc3339();
    std::fs::write(
        &events,
        format!("{{\"ts\":\"{ts}\",\"type\":\"pr_heal_tick\",\"data\":{{}}}}\n"),
    )
    .unwrap();
    let a = parse_args(&[
        "--status".to_string(),
        "--armed".to_string(),
        "--events-file".to_string(),
        events.to_string_lossy().into_owned(),
    ])
    .unwrap();
    let pid_file = heal_pid_file(&a);
    std::fs::create_dir_all(pid_file.parent().unwrap()).unwrap();
    std::fs::write(&pid_file, format!("{}\n", std::process::id())).unwrap();
    assert!(
        status_line(&a).contains(&format!("in-flight {}", std::process::id())),
        "{}",
        status_line(&a)
    );

    // One row names one root's run: the line labels whose counts these
    // are instead of letting the last root's row read as the whole tick.
    let tmp = tempfile::tempdir().unwrap();
    let events = tmp.path().join("events.jsonl");
    let ts = chrono::Utc::now().to_rfc3339();
    std::fs::write(
        &events,
        format!(
            "{{\"ts\":\"{ts}\",\"type\":\"pr_heal_tick\",\"data\":{{\"root\":\"/srv/repo\",\"healed\":2}}}}\n"
        ),
    )
    .unwrap();
    let a = parse_args(&[
        "--status".to_string(),
        "--armed".to_string(),
        "--events-file".to_string(),
        events.to_string_lossy().into_owned(),
    ])
    .unwrap();
    let line = status_line(&a);
    assert!(line.contains("root /srv/repo"), "{line}");
    assert!(line.contains("healed 2"), "{line}");
}

