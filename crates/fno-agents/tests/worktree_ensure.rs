//! `fno-agents worktree-ensure` against real git repos: the base-selection
//! and reuse contracts the parity goldens do not pin. The binary prints the
//! worktree path as its sole stdout line, a receipt on stderr, and nothing on
//! stdout when it refuses.

use std::path::{Path, PathBuf};
use std::process::Command;

fn git(cwd: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .current_dir(cwd)
        .args(["-c", "user.email=t@t", "-c", "user.name=t"])
        .args(args)
        .output()
        .expect("git runs");
    assert!(
        out.status.success(),
        "git {args:?} failed in {cwd:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

struct Fixture {
    _tmp: tempfile::TempDir,
    root: PathBuf,
    repo: PathBuf,
}

/// A main checkout on `main` with one commit; HOME and the state root live
/// in the sandbox so the default worktree base does too.
fn fixture() -> Fixture {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path().canonicalize().expect("tempdir resolves");
    let repo = root.join("myrepo");
    std::fs::create_dir_all(&repo).expect("repo dir");
    git(&repo, &["init", "-q", "-b", "main"]);
    std::fs::write(repo.join("README.md"), "# repo\n").expect("readme writes");
    git(&repo, &["add", "README.md"]);
    git(&repo, &["commit", "-qm", "init"]);
    Fixture {
        _tmp: tmp,
        root,
        repo,
    }
}

impl Fixture {
    /// A bare origin holding `main`.
    fn origin(&self) {
        let origin = self.root.join("origin.git");
        git(
            &self.root,
            &[
                "init",
                "-q",
                "--bare",
                "-b",
                "main",
                origin.to_str().unwrap(),
            ],
        );
        git(
            &self.repo,
            &["remote", "add", "origin", origin.to_str().unwrap()],
        );
        git(&self.repo, &["push", "-q", "origin", "main"]);
    }

    /// Run `worktree-ensure --repo <repo> <args>`: (exit, stdout, stderr).
    fn ensure(&self, args: &[&str]) -> (i32, String, String) {
        let home = self.root.join("home");
        let _ = std::fs::create_dir_all(&home);
        let out = Command::new(env!("CARGO_BIN_EXE_fno-agents"))
            .arg("worktree-ensure")
            .arg("--repo")
            .arg(&self.repo)
            .args(args)
            .envs(fno_agents::test_run::self_owner_env())
            .env("HOME", &home)
            .env("FNO_STATE_DIR", self.root.join("state"))
            .env(
                "FNO_GLOBAL_SETTINGS_PATH",
                home.join("absent-global-settings.yaml"),
            )
            .env_remove("FNO_WORKTREE_POLICY")
            .output()
            .expect("fno-agents runs");
        (
            out.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&out.stdout).trim().to_string(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    }
}

#[test]
fn continues_an_origin_feature_branch_ahead_of_main() {
    let fx = fixture();
    fx.origin();
    git(&fx.repo, &["checkout", "-qb", "feature/x-1234"]);
    for (file, text) in [("a.txt", "one\n"), ("b.txt", "two\n")] {
        std::fs::write(fx.repo.join(file), text).expect("file writes");
        git(&fx.repo, &["add", file]);
        git(&fx.repo, &["commit", "-qm", file]);
    }
    git(&fx.repo, &["push", "-q", "origin", "feature/x-1234"]);
    let tip = git(&fx.repo, &["rev-parse", "feature/x-1234"]);
    // Only origin holds the work: the local branch is gone.
    git(&fx.repo, &["checkout", "-q", "main"]);
    git(&fx.repo, &["branch", "-qD", "feature/x-1234"]);

    let (code, out, err) = fx.ensure(&["--name", "x-1234"]);
    assert_eq!(code, 0, "{err}");
    let wt = PathBuf::from(&out);
    assert_eq!(git(&wt, &["rev-parse", "HEAD"]), tip);
    assert_eq!(
        git(
            &wt,
            &["rev-parse", "--abbrev-ref", "feature/x-1234@{upstream}"]
        ),
        "origin/feature/x-1234"
    );
    assert!(
        err.contains("base=continued:origin/feature/x-1234:+2"),
        "{err}"
    );
}

#[test]
fn salvages_a_remote_salvage_ref_when_no_branch_exists() {
    let fx = fixture();
    fx.origin();
    git(&fx.repo, &["checkout", "-qb", "spill"]);
    std::fs::write(fx.repo.join("c.txt"), "salvage me\n").expect("file writes");
    git(&fx.repo, &["add", "c.txt"]);
    git(&fx.repo, &["commit", "-qm", "unpushed work"]);
    let tip = git(&fx.repo, &["rev-parse", "HEAD"]);
    git(
        &fx.repo,
        &[
            "push",
            "-q",
            "origin",
            &format!("{tip}:refs/fno/salvage/x-5555"),
        ],
    );
    git(&fx.repo, &["checkout", "-q", "main"]);

    let (code, out, err) = fx.ensure(&["--name", "x-5555"]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(git(Path::new(&out), &["rev-parse", "HEAD"]), tip);
    assert!(
        err.contains("base=salvaged:refs/fno/salvage/x-5555:+1"),
        "{err}"
    );
}

#[test]
fn refuses_a_branch_checked_out_in_another_worktree() {
    let fx = fixture();
    fx.origin();
    let (code, _, err) = fx.ensure(&["--name", "switcher"]);
    assert_eq!(code, 0, "{err}");
    git(&fx.repo, &["branch", "-q", "feature/elsewhere"]);
    let (code, _, err) = fx.ensure(&["--name", "elsewhere", "--branch", "feature/elsewhere"]);
    assert_eq!(code, 0, "{err}");

    let (code, out, _) = fx.ensure(&["--name", "switcher", "--branch", "feature/elsewhere"]);
    assert_ne!(code, 0);
    assert_eq!(out, "", "a refusal prints nothing on stdout");
}

#[test]
fn reuses_the_branch_checkout_when_the_policy_path_moves() {
    let fx = fixture();
    fx.origin();
    let (code, original, err) = fx.ensure(&["--name", "lane-a"]);
    assert_eq!(code, 0, "{err}");

    // Same node, a harness whose policy resolves somewhere else.
    let (code, moved, err) = fx.ensure(&["--name", "lane-a", "--harness", "claude"]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(moved, original);
    assert!(err.contains("a branch has one checkout"), "{err}");
}

#[test]
fn never_hands_back_a_missing_or_canonical_checkout() {
    let fx = fixture();
    fx.origin();
    let (code, original, err) = fx.ensure(&["--name", "lane-p"]);
    assert_eq!(code, 0, "{err}");
    std::fs::remove_dir_all(&original).expect("tree removes");

    // The relocated call consults the branch, so only it can hand back the
    // prunable row.
    let (code, out, _) = fx.ensure(&["--name", "lane-p", "--harness", "claude"]);
    assert!(
        !(code == 0 && out == original),
        "handed back a path that does not exist"
    );

    git(&fx.repo, &["checkout", "-q", "-b", "feature/lane-c"]);
    let (code, out, _) = fx.ensure(&["--name", "lane-c"]);
    let canonical = fx.repo.canonicalize().expect("repo resolves");
    let handed = Path::new(if out.is_empty() { "." } else { &out })
        .canonicalize()
        .unwrap_or_default();
    assert!(
        !(code == 0 && handed == canonical),
        "handed back the canonical checkout"
    );
}
