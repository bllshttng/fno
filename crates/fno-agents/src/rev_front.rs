//! The native `fno do show` and `fno do diff` spellings: one file at a
//! revision, and the difference between two revisions, read straight from git.
//! The front door claims both lexically and execs this binary. They replace
//! the scratch copy-to-/tmp-then-diff habit with two commands that need no
//! shell chaining, so a worktree session runs them without a compound-command
//! refusal.

use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

const HELP_ROWS: &[(&str, &str)] = &[
    (
        "show <rev>:<path>",
        "Print one file as it stands at a revision (a branch, tag, sha, or origin/main). Bytes pass through unchanged. exit 0 printed, 3 usage, 4 git could not read it.",
    ),
    (
        "diff <rev-a> <rev-b> [-- <path>...]",
        "Print the unified diff from rev-a to rev-b, limited to the paths after `--` when given. An empty diff prints nothing and exits 0. exit 0 read, 3 usage, 4 git could not read it.",
    ),
];

fn print_help() {
    println!("Revision reads (a file at a revision, the diff between two revisions)");
    println!();
    println!("Usage: fno do <show|diff> [args...]");
    println!();
    for (spelling, wording) in HELP_ROWS {
        println!("  {spelling}");
        println!("      {wording}");
        println!();
    }
}

fn print_row_help(index: usize) {
    let (spelling, wording) = HELP_ROWS[index];
    println!("Usage: fno do {spelling}");
    println!();
    println!("    {wording}");
}

/// A revision word git could take for an option: refused up front so a
/// hand-typed `--output=...` never reaches git as a flag.
fn rev_is_safe(rev: &str) -> bool {
    !rev.is_empty() && !rev.starts_with('-') && !rev.contains('\0')
}

/// Split `<rev>:<path>` at the first colon. The rev side never holds one, so
/// the first colon is the separator; a path may hold more.
fn split_spec(spec: &str) -> Option<(&str, &str)> {
    let (rev, path) = spec.split_once(':')?;
    (rev_is_safe(rev) && !path.is_empty()).then_some((rev, path))
}

fn git_passthrough(cwd: &Path, args: &[&str], label: &str, out: &mut dyn Write) -> i32 {
    let result = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stderr(Stdio::inherit())
        .output();
    match result {
        Ok(o) if o.status.success() => {
            // A closed pipe (`| head`) ends the read quietly, like git itself.
            let _ = out.write_all(&o.stdout);
            0
        }
        Ok(_) => {
            eprintln!("{label}: git could not read that revision or path");
            4
        }
        Err(e) => {
            eprintln!("{label}: cannot run git: {e}");
            4
        }
    }
}

pub fn run(args: &[String]) -> i32 {
    let cwd = std::env::current_dir().unwrap_or_else(|_| ".".into());
    run_in(&cwd, args, &mut std::io::stdout())
}

fn run_in(cwd: &Path, args: &[String], out: &mut dyn Write) -> i32 {
    let Some(sub) = args.first() else {
        print_help();
        return 0;
    };
    if sub == "--help" || sub == "-h" {
        print_help();
        return 0;
    }
    let (index, tail) = match sub.as_str() {
        "show" => (0, &args[1..]),
        "diff" => (1, &args[1..]),
        _ => {
            print_help();
            return 2;
        }
    };
    if tail.iter().any(|a| a == "--help" || a == "-h") {
        print_row_help(index);
        return 0;
    }
    if index == 0 {
        let [spec] = tail else {
            eprintln!("fno do show: one <rev>:<path> argument is required");
            return 3;
        };
        let Some((rev, path)) = split_spec(spec) else {
            eprintln!("fno do show: wants <rev>:<path>, got {spec:?}");
            return 3;
        };
        return git_passthrough(cwd, &["show", &format!("{rev}:{path}")], "fno do show", out);
    }
    let (revs, paths) = match tail.iter().position(|a| a == "--") {
        Some(at) => (&tail[..at], &tail[at + 1..]),
        None => (tail, &tail[tail.len()..]),
    };
    let [rev_a, rev_b] = revs else {
        eprintln!("fno do diff: two revisions are required: <rev-a> <rev-b> [-- <path>...]");
        return 3;
    };
    if !rev_is_safe(rev_a) || !rev_is_safe(rev_b) {
        eprintln!("fno do diff: a revision is empty or starts with '-'");
        return 3;
    }
    let mut git_args: Vec<&str> = vec!["diff", rev_a, rev_b, "--"];
    git_args.extend(paths.iter().map(String::as_str));
    git_passthrough(cwd, &git_args, "fno do diff", out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(w: &[&str]) -> Vec<String> {
        w.iter().map(|s| s.to_string()).collect()
    }

    fn repo_with_two_commits() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let git = |a: &[&str]| {
            let ok = Command::new("git")
                .args(a)
                .current_dir(dir.path())
                .env("GIT_AUTHOR_NAME", "t")
                .env("GIT_AUTHOR_EMAIL", "t@t")
                .env("GIT_COMMITTER_NAME", "t")
                .env("GIT_COMMITTER_EMAIL", "t@t")
                .stdout(Stdio::null())
                .status()
                .unwrap()
                .success();
            assert!(ok, "git {a:?}");
        };
        git(&["init", "-q", "-b", "main"]);
        std::fs::write(dir.path().join("f.txt"), "one\n").unwrap();
        git(&["add", "f.txt"]);
        git(&["commit", "-q", "-m", "a"]);
        git(&["tag", "v1"]);
        std::fs::write(dir.path().join("f.txt"), "two\n").unwrap();
        git(&["commit", "-q", "-am", "b"]);
        dir
    }

    #[test]
    fn spec_splits_at_the_first_colon_and_refuses_flags() {
        assert_eq!(split_spec("v1:a/b:c.txt"), Some(("v1", "a/b:c.txt")));
        assert_eq!(split_spec("v1:"), None);
        assert_eq!(split_spec(":f.txt"), None);
        assert_eq!(split_spec("--output=x:f"), None);
        assert_eq!(split_spec("nocolon"), None);
    }

    #[test]
    fn show_and_diff_read_real_history() {
        let repo = repo_with_two_commits();
        let run = |a: &[&str]| {
            let mut out = Vec::new();
            let code = run_in(repo.path(), &words(a), &mut out);
            (code, String::from_utf8(out).unwrap())
        };
        assert_eq!(run(&["show", "v1:f.txt"]), (0, "one\n".to_string()));
        assert_eq!(run(&["show", "v1:missing"]).0, 4);
        let (code, diff) = run(&["diff", "v1", "HEAD", "--", "f.txt"]);
        assert_eq!(code, 0);
        assert!(diff.contains("-one\n+two\n"), "diff body: {diff}");
        assert_eq!(
            run(&["diff", "v1", "HEAD", "--", "other.txt"]),
            (0, String::new())
        );
        assert_eq!(run(&["diff", "v1", "nope"]).0, 4);
    }

    #[test]
    fn usage_errors_exit_three_and_unknown_word_exits_two() {
        let run = |a: &[&str]| run_in(Path::new("."), &words(a), &mut Vec::new());
        assert_eq!(run(&["show"]), 3);
        assert_eq!(run(&["show", "a", "b"]), 3);
        assert_eq!(run(&["diff", "only-one"]), 3);
        assert_eq!(run(&["diff", "-x", "HEAD"]), 3);
        assert_eq!(run(&["frobnicate"]), 2);
        assert_eq!(run(&["show", "--help"]), 0);
    }
}
