//! parity-stage: characterization
//! parity-oracle: cli/src/fno/pr/cli.py (publish-review typer door)
//!
//! Characterization tests for the publish-review argv door (x-1d1e wave 4).
//! The Python typer command was captured into tests/golden/publish_review_argv/
//! before its deletion; each case rebuilds the same fixture (git repo, fake
//! gh, pinned HOME/FNO_STATE_DIR) and runs the BINARY's `publish-review`
//! argv door, asserting (exit, stdout, stderr) against the frozen golden.
//! The bot leg's skip paths touch no network before the config gate, and
//! the round leg's gh reads answer from the fake gh, so nothing here
//! reaches GitHub.

mod common;

use common::Golden;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

struct Fixture {
    root: PathBuf,
    repo: PathBuf,
    home: PathBuf,
    gh_dir: PathBuf,
}

impl Fixture {
    fn bin_path(&self) -> PathBuf {
        PathBuf::from(env!("CARGO_BIN_EXE_fno-agents"))
    }
}

// ── xtail: fixture + cases (appended below) ─────────────────────────────────

/// The fake gh: answers the pr-view read the round leg makes, keyed by
/// FGH_MODE like the capture's fake transport.
fn write_fake_gh(root: &Path) -> PathBuf {
    let dir = root.join("fakegh");
    fs::create_dir_all(&dir).unwrap();
    let gh = dir.join("gh");
    fs::write(
        &gh,
        "#!/bin/sh\necho '{\"number\":41,\"headRefOid\":\"a\",\"headRefName\":\"main\",\"url\":\"u\"}'\n",
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&gh, fs::Permissions::from_mode(0o755)).unwrap();
    }
    dir
}

fn fixture(tag: &str) -> Fixture {
    let tmp = tempfile::TempDir::new().unwrap().keep();
    let repo = tmp.join(tag);
    fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q", "-b", "main", "."]);
    fs::write(repo.join("file.txt"), "v1\n").unwrap();
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-qm", "c0"]);
    fs::create_dir_all(tmp.join("home")).unwrap();
    let home = tmp.join("home");
    let gh_dir = write_fake_gh(&tmp);
    Fixture {
        root: tmp,
        repo,
        home,
        gh_dir,
    }
}

fn git(cwd: &Path, args: &[&str]) {
    let out = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .env("GIT_AUTHOR_NAME", "golden")
        .env("GIT_AUTHOR_EMAIL", "golden@example.com")
        .env("GIT_COMMITTER_NAME", "golden")
        .env("GIT_COMMITTER_EMAIL", "golden@example.com")
        .output()
        .expect("git");
    assert!(
        out.status.success(),
        "git {args:?} failed: {:?}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Identity markers scrubbed like the capture, so nothing environmental
/// reaches the door.
const MARKERS: [&str; 7] = [
    "CODEX_THREAD_ID",
    "CLAUDE_CODE_SESSION_ID",
    "CODEX_SESSION_ID",
    "GEMINI_SESSION_ID",
    "OPENCODE_SESSION_ID",
    "CLAUDE_SESSION_ID",
    "FNO_HARNESS_SESSION_ID",
];

// ── xtail: run + tests (appended below) ─────────────────────────────────────

/// Run one `publish-review` argv invocation against the built binary with
/// the fixture env. Returns the normalized (exit, stdout, stderr).
fn run(fx: &Fixture, args: &[&str]) -> (i32, String, String) {
    let mut cmd = Command::new(fx.bin_path());
    cmd.arg("publish-review");
    cmd.args(args);
    cmd.current_dir(&fx.repo);
    cmd.envs(fno_agents::test_run::self_owner_env());
    cmd.env("HOME", &fx.home);
    cmd.env("FNO_STATE_DIR", fx.root.join("state"));
    cmd.env("FNO_EVENTS_PATH", fx.root.join("events.jsonl"));
    cmd.env("FGH_MODE", "open");
    let path = format!(
        "{}:{}",
        fx.gh_dir.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    cmd.env("PATH", path);
    for m in MARKERS {
        cmd.env_remove(m);
    }
    let out = cmd.output().expect("run publish-review");
    let code = out.status.code().unwrap_or(-1);
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    (code, stdout, stderr)
}

#[test]
fn publish_dry_run_no_identity() {
    let fx = fixture("repo");
    let (rc, out, err) = run(&fx, &["--dry-run", "--pr-number", "41"]);
    let rust = Golden {
        exit: Some(rc),
        streams: vec![out, err],
    };
    common::assert_golden(
        "publish_review_argv",
        "publish dry run no identity",
        &rust,
        None,
    );
}

#[test]
fn publish_deprecated_alias() {
    let fx = fixture("repo");
    let (rc, out, err) = run(&fx, &["--pr", "41", "--dry-run"]);
    let rust = Golden {
        exit: Some(rc),
        streams: vec![out, err],
    };
    common::assert_golden(
        "publish_review_argv",
        "publish deprecated alias",
        &rust,
        None,
    );
}

#[test]
fn publish_missing_pr_number() {
    let fx = fixture("repo");
    let (rc, out, err) = run(&fx, &[]);
    let rust = Golden {
        exit: Some(rc),
        streams: vec![out, err],
    };
    common::assert_golden(
        "publish_review_argv",
        "publish missing pr number",
        &rust,
        None,
    );
}
