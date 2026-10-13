//! Read the git common dir without trusting `--path-format=absolute`.
//!
//! git older than 2.31 does not know the flag. It prints the flag back as the
//! first output line, then a relative `.git`, and exits 0, so a reader that
//! takes stdout whole names a repo root `--path-format=absolute`. The first
//! read counts only when it is one absolute path. Anything else reads plain
//! `--git-common-dir` and joins it to `cwd`.
//!
//! Twin of `fno::git_paths` (this crate links `fno` as a dev-dependency only);
//! `tests/git_paths_twin.rs` pins the two to the same answers.

use std::path::{Path, PathBuf};
use std::process::Command;

/// The common dir of `cwd`, with `rev_parse` running `git rev-parse <args>` in
/// `cwd` and returning stdout on exit 0. `None` outside a repo.
pub fn common_dir_with<F>(cwd: &Path, mut rev_parse: F) -> Option<PathBuf>
where
    F: FnMut(&[&str]) -> Option<String>,
{
    // A failed read is final: old git exits 0 on the unknown flag, so a
    // non-zero exit means no repo and the plain read would only spawn again.
    let out = rev_parse(&["--path-format=absolute", "--git-common-dir"][..])?;
    if let Some(line) = single_line(&out).filter(|l| Path::new(l).is_absolute()) {
        return Some(PathBuf::from(line));
    }
    let plain = rev_parse(&["--git-common-dir"][..])?;
    let line = single_line(&plain)?;
    Some(if line == "." {
        cwd.to_path_buf()
    } else {
        cwd.join(line)
    })
}

/// The common dir of `cwd` from the `git` on PATH; `None` outside a repo or on
/// any failure.
pub fn common_dir(cwd: &Path) -> Option<PathBuf> {
    common_dir_with(cwd, |args| {
        let out = Command::new("git")
            .arg("-C")
            .arg(cwd)
            .arg("rev-parse")
            .args(args)
            .output()
            .ok()?;
        out.status
            .success()
            .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
    })
}

fn single_line(out: &str) -> Option<&str> {
    let line = out.trim();
    (!line.is_empty() && !line.contains('\n')).then_some(line)
}
