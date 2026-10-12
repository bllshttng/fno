//! Read the git common dir without trusting `--path-format=absolute`.
//!
//! Twin of `fno_agents::git_paths`, which holds the full rationale (fno-agents
//! links this crate as a dev-dependency only); `fno-agents/tests/git_paths_twin.rs`
//! pins the two to the same answers.

use std::path::{Path, PathBuf};

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

fn single_line(out: &str) -> Option<&str> {
    let line = out.trim();
    (!line.is_empty() && !line.contains('\n')).then_some(line)
}
