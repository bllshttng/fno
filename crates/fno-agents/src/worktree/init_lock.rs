//! The `initializing` lock `git worktree add` puts on a linked worktree while
//! it builds it. Git drops the lock when the add finishes. An add that dies
//! midway (a timeout kill, a crash) leaves it, and a locked worktree refuses
//! `git worktree remove` even with `--force`.
//!
//! Setup releases its own lock on every exit; the removal paths release a
//! stale one before they remove. A lock with any other reason is on purpose and
//! is never touched.

use super::{git_out, resolve_lenient};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

/// Age past which an `initializing` lock is a leftover, not a setup in flight.
pub(crate) const STALE_AFTER: Duration = Duration::from_secs(3600);

const INIT_REASON: &str = "initializing";

/// The admin dir under `<common>/worktrees/` whose `gitdir` file points at
/// `wt/.git`. Works when the worktree dir is half built and has no `.git` yet.
fn admin_dir(repo_hint: &Path, wt: &Path) -> Option<PathBuf> {
    let out = git_out(
        repo_hint,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    )?;
    if !out.status.success() {
        return None;
    }
    let common = PathBuf::from(String::from_utf8_lossy(&out.stdout).trim());
    let want = resolve_lenient(&wt.join(".git"));
    std::fs::read_dir(common.join("worktrees"))
        .ok()?
        .flatten()
        .map(|e| e.path())
        .find(|dir| {
            std::fs::read_to_string(dir.join("gitdir"))
                .map(|s| resolve_lenient(Path::new(s.trim())) == want)
                .unwrap_or(false)
        })
}

/// Unlock `wt` when its lock reads `initializing` and is at least `min_age`
/// old. `repo_hint` is any directory inside the repository (the worktree
/// itself works). True when the tree is not left init-locked.
pub(crate) fn release(repo_hint: &Path, wt: &Path, min_age: Duration) -> bool {
    let Some(admin) = admin_dir(repo_hint, wt) else {
        return true;
    };
    let lock = admin.join("locked");
    let Ok(reason) = std::fs::read_to_string(&lock) else {
        return true;
    };
    if reason.trim() != INIT_REASON {
        return false;
    }
    let age = std::fs::metadata(&lock)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| SystemTime::now().duration_since(t).ok());
    if min_age > Duration::ZERO && age.map_or(true, |a| a < min_age) {
        return false;
    }
    std::fs::remove_file(&lock).is_ok()
}

/// Release a stale `initializing` lock on the worktree at `cwd` before a
/// removal; a younger or foreign lock stays.
pub(crate) fn release_stale(cwd: &str) {
    let wt = Path::new(cwd);
    release(wt, wt, STALE_AFTER);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn git(dir: &Path, args: &[&str]) {
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .output()
            .expect("git runs");
        assert!(out.status.success(), "git {args:?}: {out:?}");
    }

    fn repo_with_worktree(tag: &str) -> (PathBuf, PathBuf) {
        let base = std::env::temp_dir().join(format!("fno-init-lock-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let repo = base.join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        git(&repo, &["init", "-q", "-b", "main"]);
        git(
            &repo,
            &[
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@t",
                "commit",
                "-q",
                "--allow-empty",
                "-m",
                "x",
            ],
        );
        let wt = base.join("wt");
        git(
            &repo,
            &["worktree", "add", "-q", "-b", "f", &wt.to_string_lossy()],
        );
        (repo, wt)
    }

    #[test]
    fn releases_a_stale_init_lock_so_remove_works_and_keeps_other_locks() {
        let (repo, wt) = repo_with_worktree("stale");
        let admin = admin_dir(&repo, &wt).expect("admin dir");

        std::fs::write(admin.join("locked"), "initializing").unwrap();
        assert!(!release(&repo, &wt, STALE_AFTER), "a young lock stays");
        assert!(admin.join("locked").exists());
        assert!(
            release(&repo, &wt, Duration::ZERO),
            "setup releases its own"
        );
        assert!(!admin.join("locked").exists());

        std::fs::write(admin.join("locked"), "on purpose").unwrap();
        assert!(!release(&repo, &wt, Duration::ZERO), "foreign lock stays");
        assert!(admin.join("locked").exists());
        std::fs::remove_file(admin.join("locked")).unwrap();

        std::fs::write(admin.join("locked"), "initializing").unwrap();
        release(&repo, &wt, Duration::ZERO);
        git(
            &repo,
            &["worktree", "remove", "--force", &wt.to_string_lossy()],
        );
        let _ = std::fs::remove_dir_all(repo.parent().unwrap());
    }
}
