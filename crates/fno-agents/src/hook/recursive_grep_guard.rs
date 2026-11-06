//! `fno-agents hook recursive-grep-guard` - refuse a recursive `grep` when
//! the current repository contains a Cargo build cache.
//!
//! Harness-native worktrees live inside the checkout and each carries its own
//! `crates/*/target` cache. `grep -r`/`-R` ignore `.gitignore`, so a search
//! scoped to `crates/` descends into every nested worktree's object files. On
//! 2026-09-09 nine concurrent `grep -r` processes ran on one 12-core machine
//! and the worst ran for 1 hour 56 minutes without returning.
//!
//! A directory named `target` is NOT evidence: this repo holds source at
//! `cli/src/fno/target`, `skills/target` and `tests/target`. A cache is
//! confirmed only by Cargo's own `CACHEDIR.TAG` marker, resolved through
//! symlinks, and the walk prunes a confirmed cache instead of entering it.
//!
//! Two stages, cheapest first: find a recursive grep in command position
//! (a pure token walk, every call), and only then inventory the repository.
//! Parse-only until a recursive grep is found, and it fails OPEN on anything
//! unexpected.

use serde_json::Value;
use std::path::{Path, PathBuf};

use super::bg_process_guard::{is_separator, shlex_tokens, strip_heredocs};
use super::test_run_guard::head_of;

const SHELLS: &[&str] = &["sh", "bash", "zsh", "dash", "ksh"];

const RECURSIVE_LONG: &[&str] = &["--recursive", "--dereference-recursive"];

/// grep options whose NEXT token is a value, never a flag: `grep -e -r file`
/// searches for the literal string "-r" and must not read as recursive.
const VALUE_OPTS: &[&str] = &[
    "-e",
    "-f",
    "-m",
    "-A",
    "-B",
    "-C",
    "-d",
    "--regexp",
    "--file",
    "--max-count",
    "--after-context",
    "--before-context",
    "--context",
    "--include",
    "--exclude",
    "--exclude-dir",
    "--label",
    "--directories",
    "--color",
    "--colour",
];

/// Entry: read the payload once, judge, print, always exit 0.
pub fn run(_args: &[String]) -> i32 {
    let payload: Value = serde_json::from_str(super::read_stdin().trim()).unwrap_or(Value::Null);
    let cwd = payload
        .get("cwd")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_else(|| PathBuf::from("."));
    if !crate::agents_config::guard_enabled(&cwd, "recursive-grep") {
        return super::emit_allow();
    }
    let refusal = judge(&payload);
    super::emit_guard_decision(&cwd, "recursive-grep-guard", "Bash", refusal.is_some());
    match refusal {
        Some(reason) => super::emit_block(&reason),
        None => super::emit_allow(),
    }
}

/// The whole verdict for one payload: a refusal, or None to allow. The
/// repository is the hook PROCESS's cwd, as it was for the Python guard.
pub(super) fn judge(payload: &Value) -> Option<String> {
    if payload.get("tool_name").and_then(Value::as_str) != Some("Bash") {
        return None;
    }
    let command = payload
        .pointer("/tool_input/command")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|c| !c.is_empty())?;
    decide(command, &std::env::current_dir().ok()?)
}

/// The whole verdict for one command string in one directory: a refusal, or
/// None to allow. Unbalanced quotes cannot tell command position, so they
/// allow.
fn decide(command: &str, cwd: &Path) -> Option<String> {
    let tokens = shlex_tokens(&strip_heredocs(command))?;
    let segment = recursive_grep_segment(&tokens, 0)?;
    let root = repo_root(cwd)?;
    if !repo_has_cargo_cache(&root) {
        return None;
    }
    Some(reason(&segment))
}

/// True when this grep argv carries a recursive option. The whole option
/// region is scanned, because grep permutes options after operands
/// (`grep foo -r .`). Value-taking options consume the next token and `--`
/// ends the region.
fn is_recursive_argv(argv: &[String]) -> bool {
    let mut skip_next = false;
    for tok in argv {
        if skip_next {
            skip_next = false;
            continue;
        }
        if tok == "--" {
            return false;
        }
        if VALUE_OPTS.contains(&tok.as_str()) {
            skip_next = true;
            continue;
        }
        if RECURSIVE_LONG.contains(&tok.as_str()) {
            return true;
        }
        if tok.starts_with('-') && !tok.starts_with("--") && tok.len() > 1 {
            // A bundle LED by a value-taking option is that flag plus its
            // attached value, never a flag list: `-fr patterns.txt` reads a
            // pattern FILE named r, and `-er` searches for the string "r".
            if tok[1..].starts_with(|c: char| "efmABCD".contains(c)) {
                continue;
            }
            if tok[1..].contains(['r', 'R']) {
                return true;
            }
        }
    }
    false
}

/// The script text a shell was handed with `-c`. Short options bundle:
/// `bash -lc '...'` is the same call.
fn shell_payload(head: &str, argv: &[String]) -> Option<String> {
    if !SHELLS.contains(&head) {
        return None;
    }
    argv.iter().enumerate().find_map(|(idx, tok)| {
        let is_c_bundle = tok.len() >= 2
            && tok.starts_with('-')
            && tok.ends_with('c')
            && tok[1..tok.len() - 1]
                .chars()
                .all(|c| c.is_ascii_alphabetic());
        if is_c_bundle {
            argv.get(idx + 1).cloned()
        } else {
            None
        }
    })
}

/// First command string that runs a recursive grep, or None. Read in command
/// position, per pipeline stage, with both wrapper-flag readings. A
/// `bash -c` payload recurses as its own command text, bounded by depth.
/// `xargs grep -r` is a known fail-open: the grep is not in this shell's
/// command position, and the find that feeds it names its own scope.
fn recursive_grep_segment(tokens: &[String], depth: usize) -> Option<String> {
    if depth > 2 {
        return None;
    }
    let mut segments: Vec<Vec<String>> = Vec::new();
    let mut current: Vec<String> = Vec::new();
    for tok in tokens {
        if is_separator(tok) {
            if !current.is_empty() {
                segments.push(std::mem::take(&mut current));
            }
        } else {
            current.push(tok.clone());
        }
    }
    if !current.is_empty() {
        segments.push(current);
    }
    for segment in &segments {
        // Splitting on `|` inside a `case` pattern alternation misreads the
        // arm, but neither misread part resolves to a grep head: the miss is
        // in the fail-open direction this guard accepts.
        let mut parts: Vec<Vec<String>> = Vec::new();
        let mut stage: Vec<String> = Vec::new();
        for tok in segment {
            if tok == "|" || tok == "|&" {
                if !stage.is_empty() {
                    parts.push(std::mem::take(&mut stage));
                }
            } else {
                stage.push(tok.clone());
            }
        }
        if !stage.is_empty() {
            parts.push(stage);
        }
        for part in &parts {
            for greedy in [false, true] {
                let Some((head, argv, _)) = head_of(part, greedy) else {
                    continue;
                };
                if head == "grep" && is_recursive_argv(&argv) {
                    return Some(part.join(" "));
                }
                if let Some(payload) = shell_payload(&head, &argv).filter(|p| !p.is_empty()) {
                    let Some(inner) = shlex_tokens(&strip_heredocs(&payload)) else {
                        continue;
                    };
                    if let Some(found) = recursive_grep_segment(&inner, depth + 1) {
                        return Some(found);
                    }
                }
            }
        }
    }
    None
}

/// The enclosing git repository root, found by walking up for `.git` (a
/// directory, or the file a linked worktree carries) instead of starting
/// `git rev-parse`.
fn repo_root(cwd: &Path) -> Option<PathBuf> {
    cwd.ancestors()
        .find(|dir| dir.join(".git").exists())
        .map(Path::to_path_buf)
}

/// True when a directory below `root` carries Cargo's CACHEDIR.TAG. Identity
/// is the MARKER at the resolved directory, never the basename. A confirmed
/// cache is never descended into, and a symlink counts only when it resolves
/// inside the repository.
fn repo_has_cargo_cache(root: &Path) -> bool {
    let Ok(root) = std::fs::canonicalize(root) else {
        return false;
    };
    if root.join("CACHEDIR.TAG").is_file() {
        return true;
    }
    let mut stack = vec![root.clone()];
    while let Some(dir) = stack.pop() {
        // An unreadable directory invents no cache.
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            // `is_dir` follows symlinks: a link to a directory is checked for
            // the marker but never walked into.
            if !path.is_dir() {
                continue;
            }
            let Ok(resolved) = std::fs::canonicalize(&path) else {
                continue;
            };
            if resolved.starts_with(&root) && resolved.join("CACHEDIR.TAG").is_file() {
                return true;
            }
            if !entry.file_type().is_ok_and(|t| t.is_symlink()) {
                stack.push(path);
            }
        }
    }
    false
}

fn reason(cmd: &str) -> String {
    format!(
        "[fno recursive-grep guard] `{cmd}` is a recursive grep, and this repository contains a CACHEDIR.TAG-confirmed Cargo cache. `grep -r` and `grep -R` ignore .gitignore, so from the repo root they descend into every nested worktree's build artifacts: on 2026-09-09 nine concurrent `grep -r` processes ran here and the worst ran for 1 hour 56 minutes without returning.\n\nUse the harness Grep tool for ordinary searches (it is ripgrep and honors .gitignore), or `rg <pattern> <path>`. For an intentional ignored-file sweep, use `RIPGREP_CONFIG_PATH= rg -uu <pattern>` with an explicit scope."
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::fs;

    fn repo(dir: &Path) {
        fs::create_dir_all(dir.join(".git")).unwrap();
    }

    fn make_cache(dir: &Path) {
        fs::create_dir_all(dir).unwrap();
        fs::write(
            dir.join("CACHEDIR.TAG"),
            "Signature: 8a477f597d28d172789f06886806bc55\n",
        )
        .unwrap();
    }

    /// A repo shaped like this one: nested worktree caches, a crate cache,
    /// and untagged `target` source directories that must never read as one.
    fn tagged_repo() -> tempfile::TempDir {
        let tmp = tempfile::tempdir().unwrap();
        repo(tmp.path());
        make_cache(
            &tmp.path()
                .join("nested/worktrees/wt/crates/fno-agents/target"),
        );
        make_cache(&tmp.path().join("crates/fno/target"));
        for source in ["cli/src/fno/target", "skills/target", "tests/target"] {
            fs::create_dir_all(tmp.path().join(source)).unwrap();
        }
        tmp
    }

    #[test]
    fn denies_each_recursive_grep_over_a_confirmed_cache() {
        let tmp = tagged_repo();
        for cmd in [
            "grep -rn ownership_defect cli/src/fno/ crates/ scripts/ tests/",
            "grep -r -l -E _clear_completion_fields crates",
            "grep -R token .",
            "grep --recursive token .",
            "grep --dereference-recursive token .",
            "/usr/bin/grep -rn token crates/",
            "grep -nr token .",
            "grep -rn token /tmp",
            "cd cli && grep -rn token .",
            "echo hi\ngrep -rn token crates/",
            "case $x in y) grep -rn token .;; esac",
            "cat list | grep -r token",
            "bash -c 'grep -rn token .'",
            "sudo -u me grep -rn token crates/",
            "caffeinate -t 3600 grep -rn token .",
            "FOO=1 grep -rn token .",
            "timeout 300 grep -rn token crates/",
            "grep -rn token .  # scoped to source, should be fine",
            "cat > f.txt <<'EOF'\nhello\nEOF\ngrep -rn token .",
        ] {
            assert!(decide(cmd, tmp.path()).is_some(), "should refuse: {cmd:?}");
        }
    }

    #[test]
    fn allows_benign_commands_over_a_confirmed_cache() {
        let tmp = tagged_repo();
        for cmd in [
            "rg -n token .",
            "grep -n token crates/",
            "grep -e -r file.txt",
            "grep -- -r file.txt",
            "grep -fr patterns.txt files/",
            "grep -m5r token files/",
            "echo grep -r",
            "find . -name '*.py' | xargs grep -r token",
            "case $x in y|yes) echo pick;; esac",
            "grep -rn 'unterminated .",
        ] {
            assert!(decide(cmd, tmp.path()).is_none(), "should allow: {cmd:?}");
        }
    }

    #[test]
    fn a_bare_target_basename_is_not_a_cache() {
        let tmp = tempfile::tempdir().unwrap();
        repo(tmp.path());
        for source in ["cli/src/fno/target", "crates/target", "tests/target"] {
            fs::create_dir_all(tmp.path().join(source)).unwrap();
        }
        assert!(decide("grep -rn token crates/", tmp.path()).is_none());
    }

    #[cfg(unix)]
    #[test]
    fn a_symlink_counts_only_when_it_resolves_inside_the_repository() {
        use std::os::unix::fs::symlink;
        let inside = tempfile::tempdir().unwrap();
        repo(inside.path());
        make_cache(&inside.path().join(".cache-store/wt-target"));
        symlink(
            inside.path().join(".cache-store/wt-target"),
            inside.path().join("target-link"),
        )
        .unwrap();
        assert!(decide("grep -rn token .", inside.path()).is_some());

        let root = tempfile::tempdir().unwrap();
        repo(root.path());
        let external = tempfile::tempdir().unwrap();
        make_cache(&external.path().join("target"));
        symlink(
            external.path().join("target"),
            root.path().join("target-link"),
        )
        .unwrap();
        assert!(decide("grep -rn token .", root.path()).is_none());
    }

    #[test]
    fn the_repo_root_itself_carrying_the_marker_is_refused() {
        let tmp = tempfile::tempdir().unwrap();
        repo(tmp.path());
        make_cache(tmp.path());
        assert!(decide("grep -rn token .", tmp.path()).is_some());
    }

    #[test]
    fn a_nested_directory_resolves_to_its_repository_root() {
        let tmp = tagged_repo();
        let nested = tmp.path().join("cli/src/fno");
        assert!(decide("grep -rn token .", &nested).is_some());
    }

    #[test]
    fn other_tools_and_blank_input_allow() {
        assert!(judge(&Value::Null).is_none());
        assert!(
            judge(&json!({"tool_name": "Read", "tool_input": {"command": "grep -r x ."}}))
                .is_none()
        );
        assert!(judge(&json!({"tool_name": "Bash", "tool_input": {"command": "  "}})).is_none());
    }
}
