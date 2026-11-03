//! One path for a daemon-opened PR: scratch worktree off origin/main, one
//! commit, push, `gh pr create`, worktree removal. `main_revert` and the eval
//! arms call it so the sequence has a single owner.

use std::path::{Path, PathBuf};
use std::time::Duration;

const TIMEOUT: Duration = Duration::from_secs(120);

fn run(cwd: &Path, bin: &str, args: &[&str]) -> Result<String, String> {
    match crate::pr_push::run_labeled("bank-pr", bin, args, cwd, TIMEOUT)? {
        (true, out, _) => Ok(out.trim().to_string()),
        (false, _, err) => Err(format!("{bin} {}: {}", args.join(" "), err.trim())),
    }
}

/// URL of the open PR whose head is `branch`, if one exists.
pub(crate) fn existing_pr(root: &Path, branch: &str) -> Result<Option<String>, String> {
    let args = [
        "pr", "list", "--head", branch, "--state", "open", "--json", "url", "--jq", ".[0].url",
    ];
    let out = run(root, "gh", &args)?;
    Ok((!out.is_empty()).then_some(out))
}

/// Open a PR from `branch` with the commit `step` leaves in the worktree.
/// `step` runs inside the new worktree and must commit its own change. An
/// open PR on `branch` is returned as is: nothing is pushed twice.
pub fn open_with(
    root: &Path,
    branch: &str,
    title: &str,
    body: &str,
    step: impl FnOnce(&Path) -> Result<(), String>,
) -> Result<String, String> {
    // An unreadable list falls through: `gh pr create` itself refuses a
    // second PR on the same head.
    if let Ok(Some(url)) = existing_pr(root, branch) {
        return Ok(url);
    }
    let tree = std::env::temp_dir().join(format!("fno-bank-pr-{}", branch.replace('/', "-")));
    let tree_s = tree.to_string_lossy().into_owned();
    let _ = run(root, "git", &["worktree", "remove", "--force", &tree_s]);
    run(root, "git", &["fetch", "origin", "main"])?;
    // No open PR holds this branch, so a remote copy is the leftover of a run
    // whose `gh pr create` failed after its push. A fresh cut cannot push over it.
    let heads = ["ls-remote", "--heads", "origin", branch];
    if run(root, "git", &heads).is_ok_and(|out| !out.is_empty()) {
        run(root, "git", &["push", "origin", "--delete", branch])?;
    }
    run(
        root,
        "git",
        &["worktree", "add", "-b", branch, &tree_s, "origin/main"],
    )?;
    let create = [
        "pr", "create", "--base", "main", "--head", branch, "--title", title, "--body", body,
    ];
    let url = step(&tree)
        .and_then(|_| run(&tree, "git", &["push", "-u", "origin", branch]))
        .and_then(|_| run(&tree, "gh", &create));
    let _ = run(root, "git", &["worktree", "remove", "--force", &tree_s]);
    let _ = run(root, "git", &["branch", "-D", branch]);
    url
}

/// Write `edits` (repo-relative path, bytes) and commit them as one commit.
pub fn open(
    root: &Path,
    branch: &str,
    title: &str,
    body: &str,
    edits: &[(PathBuf, Vec<u8>)],
) -> Result<String, String> {
    open_with(root, branch, title, body, |tree| {
        let mut rels = Vec::new();
        for (rel, bytes) in edits {
            let path = tree.join(rel);
            if let Some(dir) = path.parent() {
                std::fs::create_dir_all(dir)
                    .map_err(|e| format!("mkdir {}: {e}", dir.display()))?;
            }
            std::fs::write(&path, bytes).map_err(|e| format!("write {}: {e}", path.display()))?;
            rels.push(rel.to_string_lossy().into_owned());
        }
        let mut add = vec!["add", "--"];
        add.extend(rels.iter().map(String::as_str));
        run(tree, "git", &add)?;
        run(tree, "git", &["commit", "-m", title])?;
        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    fn git(dir: &Path, args: &[&str]) {
        let ok = Command::new("git")
            .args(args)
            .current_dir(dir)
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@t")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@t")
            .status()
            .unwrap()
            .success();
        assert!(ok, "git {args:?}");
    }

    fn scratch() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let origin = dir.path().join("origin.git");
        let work = dir.path().join("work");
        std::fs::create_dir_all(&work).unwrap();
        git(dir.path(), &["init", "--bare", "-b", "main", "origin.git"]);
        git(&work, &["init", "-b", "main"]);
        std::fs::write(work.join("a.txt"), "a").unwrap();
        git(&work, &["add", "."]);
        git(&work, &["commit", "-m", "init"]);
        git(
            &work,
            &["remote", "add", "origin", origin.to_str().unwrap()],
        );
        git(&work, &["push", "-u", "origin", "main"]);
        (dir, work)
    }

    #[test]
    fn step_commits_and_worktree_is_removed() {
        let (_dir, work) = scratch();
        let branch = "evals/test-x";
        let seen = std::cell::RefCell::new(PathBuf::new());
        // No gh in the scratch repo: the call fails after the push, which is
        // the part under test.
        let _ = open_with(&work, branch, "t", "b", |tree| {
            *seen.borrow_mut() = tree.to_path_buf();
            std::fs::write(tree.join("one.txt"), "1").unwrap();
            std::fs::write(tree.join("two.txt"), "2").unwrap();
            git(tree, &["add", "."]);
            git(tree, &["commit", "-m", "both"]);
            Ok(())
        });
        assert!(!seen.borrow().exists(), "worktree removed");
        let log = Command::new("git")
            .args(["--git-dir", "../origin.git", "log", branch, "--format=%s"])
            .current_dir(&work)
            .output()
            .unwrap();
        let log = String::from_utf8_lossy(&log.stdout).to_string();
        assert_eq!(log.lines().next(), Some("both"));
    }
}
