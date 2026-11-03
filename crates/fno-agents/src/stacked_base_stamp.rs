//! Post the required `stacked-base-guard` status from the CLI when a PR's
//! base is the default branch. That verdict needs no checkout, but the CI job
//! that posts it waits for a runner like every other job. With a deep Actions
//! queue the one required context held green PRs blocked for an hour. The CI
//! job stays as the cover for hand pushes and for stacked bases, which need
//! the real lineage probe.

use crate::gate_probes::{default_branch_cached, run_probe, Run, RunProbe};
use crate::pr_list::parse_origin_slug;
use serde_json::Value;
use std::path::Path;
use std::process::Command;

pub(crate) const CONTEXT: &str = "stacked-base-guard";

pub(crate) fn origin_slug(cwd: &Path, run: RunProbe) -> Option<String> {
    run(&["git", "remote", "get-url", "origin"], cwd)
        .filter(|r| r.ok)
        .and_then(|r| parse_origin_slug(r.stdout.trim()))
}

/// Post success on `sha` when `base_ref` is the default branch. True when a
/// status was posted. Best-effort: any miss leaves the CI job to post it.
pub(crate) fn stamp(cwd: &Path, base_ref: &str, sha: &str, run: RunProbe) -> bool {
    if base_ref.is_empty() || sha.is_empty() {
        return false;
    }
    let Some(slug) = origin_slug(cwd, run) else {
        return false;
    };
    match default_branch_cached(cwd, &slug, run) {
        Some(default) if default == base_ref => {
            let endpoint = format!("repos/{slug}/statuses/{sha}");
            let context = format!("context={CONTEXT}");
            let desc = format!("description=base is the default branch ({default})");
            run(
                &[
                    "gh",
                    "api",
                    "--method",
                    "POST",
                    &endpoint,
                    "-f",
                    "state=success",
                    "-f",
                    &context,
                    "-f",
                    &desc,
                ],
                cwd,
            )
            .is_some_and(|r| r.ok)
        }
        _ => false,
    }
}

/// Stamp `sha` just pushed to `branch`, reading the base of the open PR whose
/// head is `branch`. No open PR posts nothing: `pr create` stamps its own.
pub(crate) fn stamp_pushed(cwd: &Path, branch: &str, sha: &str, run: RunProbe) -> bool {
    let Some(slug) = origin_slug(cwd, run) else {
        return false;
    };
    let owner = slug.split('/').next().unwrap_or_default();
    let endpoint = format!(
        "repos/{slug}/pulls?state=open&head={owner}:{}",
        crate::claims::encode_key(branch)
    );
    let base = run(&["gh", "api", &endpoint], cwd)
        .filter(|r| r.ok)
        .and_then(|r| serde_json::from_str::<Value>(&r.stdout).ok())
        .and_then(|prs| {
            prs.as_array()?
                .iter()
                .find(|pr| pr.pointer("/head/ref").and_then(Value::as_str) == Some(branch))
                .and_then(|pr| pr.pointer("/base/ref").and_then(Value::as_str))
                .map(str::to_string)
        })
        .unwrap_or_default();
    stamp(cwd, &base, sha, run)
}

/// The live probe seam with `git` and `gh` resolved to the caller's binaries,
/// so a test stub for either one also catches the stamp's calls.
pub(crate) fn runner<'a>(
    git_bin: &'a str,
    gh_bin: &'a str,
) -> impl Fn(&[&str], &Path) -> Option<Run> + 'a {
    move |args: &[&str], dir: &Path| {
        let bin = match args[0] {
            "git" => git_bin,
            "gh" => gh_bin,
            other => other,
        };
        let mut cmd = Command::new(bin);
        cmd.args(&args[1..]).current_dir(dir);
        run_probe(cmd)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    fn ok(stdout: &str) -> Option<Run> {
        Some(Run {
            ok: true,
            code: Some(0),
            stdout: stdout.into(),
            stderr: String::new(),
        })
    }

    #[test]
    fn only_a_default_based_pr_gets_the_status_posted() {
        let dir = tempfile::tempdir().unwrap();
        for (base, posts) in [("main", true), ("feature/parent", false)] {
            let calls = RefCell::new(Vec::<String>::new());
            let run = |args: &[&str], _: &Path| {
                let line = args.join(" ");
                calls.borrow_mut().push(line.clone());
                if line.starts_with("git remote get-url") {
                    ok("git@github.com:acme/repo.git\n")
                } else if line == "gh api repos/acme/repo" {
                    ok(r#"{"default_branch":"main"}"#)
                } else if line.contains("/pulls?state=open&head=acme:feat") {
                    ok(&format!(
                        r#"[{{"head":{{"ref":"feat"}},"base":{{"ref":"{base}"}}}}]"#
                    ))
                } else {
                    ok("{}")
                }
            };
            assert_eq!(
                stamp_pushed(dir.path(), "feat", "abc123", &run),
                posts,
                "{base}"
            );
            let posted = calls.borrow().iter().any(|c| {
                c.contains("POST repos/acme/repo/statuses/abc123")
                    && c.contains("context=stacked-base-guard")
            });
            assert_eq!(posted, posts, "{base}: {:?}", calls.borrow());
        }
    }
}
