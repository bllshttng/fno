//! Prunes quiet-recovery salvage refs (`refs/heads/recovery/<session>-<epoch>`)
//! whose stray changes already sit on the base branch. A ref that holds any
//! path whose content differs from the base is kept: its commit is the only
//! copy of that work.

use std::path::Path;
use std::process::Command;

/// A salvage ref younger than this may belong to a salvage still in flight.
const GRACE_SECS: i64 = 3600;

#[derive(Debug, PartialEq, Eq)]
pub(super) struct Pruned {
    pub(super) refname: String,
    pub(super) sha: String,
}

/// Runs git in `repo`; `Ok` carries the exit code and stdout.
fn git(repo: &Path, args: &[&str]) -> Result<(i32, String), String> {
    let out = Command::new("git")
        .current_dir(repo)
        .args(args)
        .output()
        .map_err(|e| e.to_string())?;
    Ok((
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
    ))
}

/// The epoch the salvage stamped into the ref name. `None` for any other shape.
fn salvage_epoch(refname: &str) -> Option<i64> {
    refname
        .strip_prefix("refs/heads/recovery/")?
        .rsplit_once('-')
        .and_then(|(_, epoch)| epoch.parse().ok())
}

/// True when every path the salvage commit changed already matches `base`.
/// Any doubt (a root commit, a git error, a missing base) keeps the ref.
fn stray_on_base(repo: &Path, base: &str, sha: &str) -> bool {
    let parent = format!("{sha}^");
    let Ok((0, names)) = git(
        repo,
        &[
            "--literal-pathspecs",
            "diff",
            "--no-renames",
            "-z",
            "--name-only",
            &parent,
            sha,
        ],
    ) else {
        return false;
    };
    let paths: Vec<&str> = names.split('\0').filter(|p| !p.is_empty()).collect();
    if paths.is_empty() {
        return true;
    }
    let mut args = vec!["--literal-pathspecs", "diff", "--quiet", base, sha, "--"];
    args.extend(paths);
    matches!(git(repo, &args), Ok((0, _)))
}

/// Deletes each salvage ref that is past the grace window and whose stray
/// content is already on `base`. The delete pins the SHA it read, so a ref
/// moved since the listing survives.
pub(super) fn prune(repo: &Path, base: &str, now: i64) -> Result<Vec<Pruned>, String> {
    let (code, listing) = git(
        repo,
        &[
            "for-each-ref",
            "--format=%(refname) %(objectname)",
            "refs/heads/recovery/",
        ],
    )?;
    if code != 0 {
        return Err(format!("for-each-ref exited {code}"));
    }
    let mut pruned = Vec::new();
    for line in listing.lines() {
        let Some((refname, sha)) = line.split_once(' ') else {
            continue;
        };
        let Some(epoch) = salvage_epoch(refname) else {
            continue;
        };
        if now - epoch < GRACE_SECS || !stray_on_base(repo, base, sha) {
            continue;
        }
        let (code, _) = git(repo, &["update-ref", "-d", refname, sha])?;
        if code == 0 {
            pruned.push(Pruned {
                refname: refname.to_string(),
                sha: sha.to_string(),
            });
        }
    }
    Ok(pruned)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(repo: &Path, args: &[&str]) -> String {
        let (code, out) = git(repo, args).expect("spawn git");
        assert_eq!(code, 0, "git {args:?} failed");
        out.trim().to_string()
    }

    fn commit(repo: &Path, file: &str, body: &str) {
        std::fs::write(repo.join(file), body).expect("write file");
        run(repo, &["add", file]);
        run(
            repo,
            &[
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@t",
                "commit",
                "-q",
                "-m",
                file,
            ],
        );
    }

    #[test]
    fn epoch_comes_from_the_salvage_ref_shape_only() {
        assert_eq!(
            salvage_epoch("refs/heads/recovery/2a2ce146-d598-47c2-93bb-513c3ddc7eb0-1792331288"),
            Some(1792331288)
        );
        assert_eq!(salvage_epoch("refs/heads/feature/x-1"), None);
        assert_eq!(salvage_epoch("refs/heads/recovery/no-epoch-here"), None);
    }

    #[test]
    fn a_failed_listing_is_an_error_not_an_empty_prune() {
        let dir = tempfile::tempdir().expect("tempdir");
        assert!(prune(dir.path(), "main", 0).is_err());
    }

    #[test]
    fn prunes_only_stray_content_already_on_base() {
        let dir = tempfile::tempdir().expect("tempdir");
        let repo = dir.path();
        run(repo, &["init", "-q", "-b", "main"]);
        commit(repo, "a.txt", "one");
        let base_sha = run(repo, &["rev-parse", "HEAD"]);

        // Stray change `b.txt`, then the same content lands on main.
        run(repo, &["checkout", "-q", "-b", "tmp-safe"]);
        commit(repo, "b.txt", "x");
        let safe = run(repo, &["rev-parse", "HEAD"]);
        run(repo, &["checkout", "-q", "main"]);
        commit(repo, "b.txt", "x");

        // Stray change `c.txt` never reaches main.
        run(repo, &["checkout", "-q", "-b", "tmp-unsafe", &base_sha]);
        commit(repo, "c.txt", "y");
        let unsafe_sha = run(repo, &["rev-parse", "HEAD"]);
        run(repo, &["checkout", "-q", "main"]);
        run(repo, &["branch", "-q", "-D", "tmp-safe", "tmp-unsafe"]);

        let now = 1_800_000_000;
        let old = now - 7200;
        let safe_ref = format!("refs/heads/recovery/s-{old}");
        let unsafe_ref = format!("refs/heads/recovery/u-{old}");
        let fresh_ref = format!("refs/heads/recovery/f-{now}");
        run(repo, &["update-ref", &safe_ref, &safe]);
        run(repo, &["update-ref", &unsafe_ref, &unsafe_sha]);
        run(repo, &["update-ref", &fresh_ref, &safe]);

        let pruned = prune(repo, "main", now).expect("prune");
        assert_eq!(
            pruned,
            vec![Pruned {
                refname: safe_ref.clone(),
                sha: safe.clone(),
            }]
        );
        assert_eq!(
            run(repo, &["rev-parse", "-q", "--verify", &unsafe_ref]),
            unsafe_sha
        );
        assert_eq!(
            run(repo, &["rev-parse", "-q", "--verify", &fresh_ref]),
            safe
        );
    }
}
