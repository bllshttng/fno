//! `fno-agents pr-base-check`: the stale-base age guard, ported from
//! `cli/src/fno/pr/_preflight.py` `check_stale_base`. A branch whose
//! merge-base with its base is more than 24 hours of base history behind
//! ships phantom deletions, so the guard refuses before `gh pr create`.
//!
//! Two transports over one walk: argv (`--base <ref>`, the skill reads the
//! stderr message and the exit code) and one JSON payload on stdin
//! (`{"base": ..., "cwd": ...}` answered with `{"code", "message"}`, which
//! the Python ship paths read through `verb_call`).

use serde_json::Value;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::gate_probes::{run_probe, RunProbe};

const BASE_DEFAULT: &str = "origin/main";
const MAX_HOURS: f64 = 24.0;
const BYPASS_ENV: &str = "FNO_PR_BASE_OK";
const BYPASS_VALUE: &str = "stale-acknowledged";
const OK: i32 = 0;
const REFUSED_STALE: i32 = 3;
const UNRELATED: i32 = 4;

/// The argv transport: print any message to stderr, exit with the code.
pub fn run(argv: &[String]) -> i32 {
    let mut base = BASE_DEFAULT;
    let mut it = argv.iter().map(String::as_str);
    while let Some(arg) = it.next() {
        match arg {
            "--base" => match it.next() {
                Some(value) => base = value,
                None => {
                    eprintln!(
                        "usage: fno-agents pr-base-check --base <ref> (--base needs a value)"
                    );
                    return 2;
                }
            },
            other => {
                eprintln!("usage: fno-agents pr-base-check [--base <ref>] (got {other})");
                return 2;
            }
        }
    }
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let run_probe_fn = |args: &[&str], dir: &Path| {
        let mut cmd = Command::new(args[0]);
        cmd.args(&args[1..]).current_dir(dir);
        run_probe(cmd)
    };
    let (code, message) = check_stale_base(base, &cwd, &run_probe_fn, bypassed());
    if let Some(msg) = message.as_deref() {
        eprintln!("{msg}");
    }
    code
}

/// The stdin transport: one JSON payload in, one JSON answer on stdout, exit
/// 0 always (the code rides inside the answer, which is what `verb_call`
/// parses; a nonzero exit here would raise past the caller's own handling).
pub fn run_stdin() -> i32 {
    let mut payload = String::new();
    if std::io::stdin().read_to_string(&mut payload).is_err() {
        eprintln!("pr-base-check: cannot read stdin");
        return 2;
    }
    let parsed: Value = match serde_json::from_str(&payload) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("pr-base-check: stdin was not JSON: {e}");
            return 2;
        }
    };
    let base = parsed
        .get("base")
        .and_then(Value::as_str)
        .unwrap_or(BASE_DEFAULT);
    let cwd = parsed
        .get("cwd")
        .and_then(Value::as_str)
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
    let run_probe_fn = |args: &[&str], dir: &Path| {
        let mut cmd = Command::new(args[0]);
        cmd.args(&args[1..]).current_dir(dir);
        run_probe(cmd)
    };
    let (code, message) = check_stale_base(base, &cwd, &run_probe_fn, bypassed());
    println!(
        "{}",
        serde_json::json!({ "code": code, "message": message })
    );
    OK
}

/// The walk, in the Python order with the Python messages: bypass, fetch,
/// merge-base, behind count, then the commit-date span. Every unevaluated
/// step fails open with the exact `...; stale-base check skipped` warning.
/// `bypassed` rides a parameter so the case table never touches process env.
fn check_stale_base(
    base: &str,
    cwd: &Path,
    run: RunProbe,
    bypassed: bool,
) -> (i32, Option<String>) {
    if bypassed {
        emit_bypass(cwd);
        return (OK, None);
    }
    let (remote, ref_name) = split_base(base, cwd, run);
    let compare = format!("{remote}/{ref_name}");
    // Fail-open: a network flake must not block a ship, and gh pr create is
    // about to make the real network test anyway.
    let Some(fetch) = run(&["git", "fetch", remote, ref_name, "--quiet"], cwd) else {
        return (
            OK,
            Some("could not run git; stale-base check skipped".into()),
        );
    };
    if !fetch.ok {
        return (
            OK,
            Some(format!(
                "could not refresh {compare}; stale-base check skipped"
            )),
        );
    }
    let mb_sha = run(&["git", "merge-base", "HEAD", compare.as_str()], cwd)
        .map(|r| r.stdout.trim().to_string())
        .unwrap_or_default();
    if mb_sha.is_empty() {
        return (UNRELATED, Some(unrelated_message(&compare)));
    }
    let behind = run(
        &[
            "git",
            "rev-list",
            "--count",
            &format!("{mb_sha}..{compare}"),
        ],
        cwd,
    );
    // Fail-open on an unexpected rev-list error rather than misreading empty
    // stdout as behind-count 0 (a false "up to date" pass).
    let Some(behind) = behind.filter(|r| r.ok) else {
        return (
            OK,
            Some("could not determine behind count; stale-base check skipped".into()),
        );
    };
    if behind.stdout.trim() == "0" {
        return (OK, None); // up to date; merge-base age is irrelevant
    }
    let (Some(tip_ts), Some(mb_ts)) = (
        committer_epoch(&compare, cwd, run),
        committer_epoch(&mb_sha, cwd, run),
    ) else {
        return (
            OK,
            Some("could not read commit dates; stale-base check skipped".into()),
        );
    };
    let span_hours = (tip_ts - mb_ts) as f64 / 3600.0;
    if span_hours > MAX_HOURS {
        return (REFUSED_STALE, Some(stale_message(span_hours, &compare)));
    }
    (OK, None)
}

fn bypassed() -> bool {
    std::env::var(BYPASS_ENV).ok().as_deref() == Some(BYPASS_VALUE)
}

/// ``origin/main`` -> ``("origin", "main")``. A leading segment is treated as
/// the remote ONLY when it matches a real git remote; otherwise the whole ref
/// is fetched from ``origin``, so a slash-containing branch (``releases/v1``)
/// is not mis-split into a phantom remote ``releases`` (which would fail the
/// fetch and silently skip the guard).
fn split_base(base: &str, cwd: &Path, run: RunProbe) -> (String, String) {
    if let Some((remote, ref_name)) = base.split_once('/') {
        if let Some(out) = run(&["git", "remote"], cwd) {
            if out.ok && out.stdout.lines().any(|ln| ln.trim() == remote) {
                return (remote.to_string(), ref_name.to_string());
            }
        }
    }
    ("origin".into(), base.to_string())
}

fn committer_epoch(ref_name: &str, cwd: &Path, run: RunProbe) -> Option<i64> {
    let out = run(&["git", "show", "-s", "--format=%ct", ref_name], cwd)?;
    let text = out.stdout.trim();
    if !out.ok || text.is_empty() {
        return None;
    }
    text.parse::<i64>().ok()
}

fn stale_message(span_hours: f64, compare: &str) -> String {
    let days = span_hours / 24.0;
    format!(
        "stale base: your branch's merge-base with {compare} is {days:.1} days \
         behind {compare}.\n\
         PRs from stale bases show phantom deletions (changes you never made \
         appear as reverts).\n\
         fix:    git merge origin/main, then fno do pr push   (a merge \
         integrates the base for any branch shape; resolve conflicts, then the \
         push repushes)\n\
         bypass: FNO_PR_BASE_OK=stale-acknowledged  (deliberate old-base PR; \
         emits gate_escape)"
    )
}

fn unrelated_message(compare: &str) -> String {
    format!(
        "unrelated histories: your branch has no common ancestor with {compare}. \
         That is a bigger problem than a stale base - check you branched from the \
         right place before opening a PR."
    )
}

/// Fire-and-forget gate_escape on a deliberate bypass. Telemetry never blocks.
fn emit_bypass(cwd: &Path) {
    let _ = Command::new(crate::scrape::fno_bin())
        .args(["doctor", "event", "gate-escape", "stale-base"])
        .args(["--detail", "pre-pr base-check bypass"])
        .current_dir(cwd)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One fake probe answering the guard's whole git world from a case row:
    /// whether the fetch succeeds, the merge-base sha (None fails it), the
    /// behind count, and the two commit dates by ref.
    struct Case {
        fetch_ok: bool,
        merge_base: Option<&'static str>,
        behind: &'static str,
        compare: &'static str,
        tip_epoch: Option<i64>,
        mb_epoch: Option<i64>,
        remotes: &'static str,
    }

    fn ok(stdout: String) -> Option<Run> {
        Some(Run {
            ok: true,
            code: Some(0),
            stdout,
            stderr: String::new(),
        })
    }

    fn run_of(c: &Case) -> impl Fn(&[&str], &Path) -> Option<Run> + '_ {
        move |args: &[&str], _dir: &Path| match (args.first(), args.get(1)) {
            (Some(&"git"), Some(&"remote")) => ok(c.remotes.into()),
            (Some(&"git"), Some(&"fetch")) => {
                if c.fetch_ok {
                    ok(String::new())
                } else {
                    Some(Run {
                        ok: false,
                        code: Some(128),
                        stdout: String::new(),
                        stderr: "fetch failed\n".into(),
                    })
                }
            }
            (Some(&"git"), Some(&"merge-base")) => c.merge_base.map(|sha| Run {
                ok: true,
                code: Some(0),
                stdout: format!("{sha}\n"),
                stderr: String::new(),
            }),
            (Some(&"git"), Some(&"rev-list")) => ok(format!("{}\n", c.behind)),
            (Some(&"git"), Some(&"show")) => {
                let ref_name = *args.last().expect("git show carries a ref");
                if ref_name == c.compare {
                    c.tip_epoch
                } else {
                    c.mb_epoch
                }
                .map(|e| format!("{e}\n"))
                .and_then(ok)
            }
            _ => None,
        }
    }

    fn answer(c: &Case, base: &str) -> (i32, Option<String>) {
        let probe = run_of(c);
        check_stale_base(base, Path::new("/repo"), &probe, false)
    }

    const SHA: &str = "0123456789abcdef0123456789abcdef01234567";

    #[test]
    fn fresh_branch_passes() {
        let c = Case {
            fetch_ok: true,
            merge_base: Some(SHA),
            behind: "0",
            compare: "origin/main",
            tip_epoch: Some(1_000_000),
            mb_epoch: Some(1_000_000),
            remotes: "origin\n",
        };
        assert_eq!(answer(&c, "origin/main"), (0, None));
    }

    #[test]
    fn stale_branch_refuses_with_the_four_line_message() {
        let c = Case {
            fetch_ok: true,
            merge_base: Some(SHA),
            behind: "7",
            compare: "origin/main",
            tip_epoch: Some(1_000_000_000),
            mb_epoch: Some(900_000_000),
            remotes: "origin\n",
        };
        let (code, msg) = answer(&c, "origin/main");
        assert_eq!(code, 3);
        let msg = msg.expect("stale carries a message");
        assert!(msg.contains("days behind"), "{msg}");
        assert!(msg.contains("phantom deletions"), "{msg}");
        assert!(msg.contains("fno do pr push"), "{msg}");
        assert!(msg.contains("FNO_PR_BASE_OK=stale-acknowledged"), "{msg}");
    }

    #[test]
    fn failed_fetch_fails_open_with_the_skipped_warning() {
        let c = Case {
            fetch_ok: false,
            merge_base: Some(SHA),
            behind: "0",
            compare: "origin/main",
            tip_epoch: None,
            mb_epoch: None,
            remotes: "origin\n",
        };
        let (code, msg) = answer(&c, "origin/main");
        assert_eq!(code, 0);
        assert!(msg.unwrap_or_default().contains("skipped"));
    }

    #[test]
    fn zero_behind_passes_regardless_of_age() {
        let c = Case {
            fetch_ok: true,
            merge_base: Some(SHA),
            behind: "0",
            compare: "origin/main",
            tip_epoch: Some(1_000_000_000),
            mb_epoch: Some(0),
            remotes: "origin\n",
        };
        assert_eq!(answer(&c, "origin/main"), (0, None));
    }

    #[test]
    fn no_merge_base_refuses_distinctly() {
        let c = Case {
            fetch_ok: true,
            merge_base: None,
            behind: "0",
            compare: "origin/main",
            tip_epoch: None,
            mb_epoch: None,
            remotes: "origin\n",
        };
        let (code, msg) = answer(&c, "origin/main");
        assert_eq!(code, 4);
        assert!(msg.unwrap_or_default().contains("unrelated histories"));
    }

    #[test]
    fn unreadable_commit_dates_fail_open() {
        let c = Case {
            fetch_ok: true,
            merge_base: Some(SHA),
            behind: "7",
            compare: "origin/main",
            tip_epoch: None,
            mb_epoch: Some(0),
            remotes: "origin\n",
        };
        let (code, msg) = answer(&c, "origin/main");
        assert_eq!(code, 0);
        assert!(msg.unwrap_or_default().contains("skipped"));
    }

    #[test]
    fn bare_ref_compares_the_remote_not_the_local_branch() {
        let c = Case {
            fetch_ok: true,
            merge_base: Some(SHA),
            behind: "7",
            compare: "origin/main",
            tip_epoch: Some(1_000_000_000),
            mb_epoch: Some(900_000_000),
            remotes: "origin\n",
        };
        // The fetch argv carries the bare ref beside its remote: the guard
        // compared `origin/main`, never a local `main` the fetch never touched.
        let probe = run_of(&c);
        let mut fetch_args: Option<Vec<String>> = None;
        let spy = |args: &[&str], dir: &Path| {
            if args.get(1) == Some(&"fetch") {
                fetch_args = Some(args.iter().map(|s| s.to_string()).collect());
            }
            probe(args, dir)
        };
        let (code, _) = check_stale_base("main", Path::new("/repo"), &spy, false);
        assert_eq!(code, 3);
        assert_eq!(
            fetch_args,
            Some(vec![
                "git".into(),
                "fetch".into(),
                "origin".into(),
                "main".into(),
                "--quiet".into()
            ])
        );
    }

    #[test]
    fn slash_base_resolves_to_origin_not_a_phantom_remote() {
        let c = Case {
            fetch_ok: true,
            merge_base: Some(SHA),
            behind: "7",
            compare: "origin/releases/v1",
            tip_epoch: Some(1_000_000_000),
            mb_epoch: Some(900_000_000),
            remotes: "origin\nupstream\n",
        };
        // `releases` is not a git remote, so the whole ref fetches from origin.
        let probe = run_of(&c);
        let mut fetch_args: Option<Vec<String>> = None;
        let spy = |args: &[&str], dir: &Path| {
            if args.get(1) == Some(&"fetch") {
                fetch_args = Some(args.iter().map(|s| s.to_string()).collect());
            }
            probe(args, dir)
        };
        let (code, msg) = check_stale_base("releases/v1", Path::new("/repo"), &spy, false);
        assert_eq!(code, 3);
        assert!(msg.unwrap_or_default().contains("days behind"));
        assert_eq!(
            fetch_args,
            Some(vec![
                "git".into(),
                "fetch".into(),
                "origin".into(),
                "releases/v1".into(),
                "--quiet".into()
            ])
        );
    }

    #[test]
    fn a_wrong_bypass_value_still_refuses() {
        let c = Case {
            fetch_ok: true,
            merge_base: Some(SHA),
            behind: "7",
            compare: "origin/main",
            tip_epoch: Some(1_000_000_000),
            mb_epoch: Some(900_000_000),
            remotes: "origin\n",
        };
        let probe = run_of(&c);
        // Only the literal `stale-acknowledged` bypasses; anything else falls
        // through and refuses.
        assert_eq!(
            check_stale_base("origin/main", Path::new("/repo"), &probe, false).0,
            3
        );
    }
}
