//! Mint a work-product golden from a merged fix commit: its parent is the
//! fixture, the node it fixes is the prompt, and the tests it added are the
//! grade. A draft exists only after a gold solve (the fix itself) passes three
//! times in a sealed tree, so no task enters the bank on a grader nobody ran.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use super::tests_grade::{parse_libtest, run_cargo};

const GOLD_RUNS: usize = 3;
const RUN_TIMEOUT_S: i64 = 1800;

fn git(repo: &Path, args: &[&str]) -> Result<String, String> {
    let out = std::process::Command::new("git")
        .args(args)
        .current_dir(repo)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .output()
        .map_err(|e| e.to_string())?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
    }
}

/// The libtest module path a test file contributes: `src/a/tests.rs` -> `a::tests`.
fn module_prefix(test_file: &str) -> Option<String> {
    let (_, rest) = test_file.split_once("/src/")?;
    let path = rest.strip_suffix(".rs")?;
    let path = path.strip_suffix("/mod").unwrap_or(path);
    Some(path.replace('/', "::"))
}

fn in_files(id: &str, prefixes: &[String]) -> bool {
    prefixes.iter().any(|p| id.starts_with(&format!("{p}::")))
}

/// f2p: tests the fix made pass that did not pass before. p2p: tests that pass
/// both before and after. Both only from the re-applied test files.
fn split_ids(
    before: &BTreeMap<String, String>,
    after: &BTreeMap<String, String>,
    prefixes: &[String],
) -> (Vec<String>, Vec<String>) {
    let ok = |m: &BTreeMap<String, String>| -> BTreeSet<String> {
        m.iter()
            .filter(|(id, st)| st.as_str() == "ok" && in_files(id, prefixes))
            .map(|(id, _)| id.clone())
            .collect()
    };
    let (was, now) = (ok(before), ok(after));
    (
        now.difference(&was).cloned().collect(),
        now.intersection(&was).cloned().collect(),
    )
}

/// A prompt must not name the files it is graded by or the answer's location.
pub(crate) fn strip_paths(text: &str) -> String {
    let file = regex::Regex::new(r"[\w./-]*\.(rs|py|sh|md|toml|ya?ml|json)\b").expect("static");
    let dir =
        regex::Regex::new(r"\b(crates|cli|skills|hooks|scripts|docs)/[\w./-]*").expect("static");
    let flat = file.replace_all(text, "the relevant file");
    dir.replace_all(&flat, "the relevant directory")
        .into_owned()
}

fn fixes_id(message: &str) -> Option<String> {
    let rx = regex::Regex::new(r"(?i)fixes:?\s+([a-z]+-[0-9a-f]{3,})").expect("static");
    rx.captures(message).map(|c| c[1].to_string())
}

fn node_text(id: &str) -> Result<String, String> {
    let out = std::process::Command::new("fno")
        .args(["backlog", "get", id])
        .output()
        .map_err(|e| format!("fno backlog get {id}: {e}"))?;
    let node: Value = serde_json::from_slice(&out.stdout)
        .map_err(|_| format!("node {id} did not resolve (fno backlog get)"))?;
    let field = |k: &str| node.get(k).and_then(Value::as_str).unwrap_or("").trim();
    if field("title").is_empty() {
        return Err(format!("node {id} has no title"));
    }
    Ok(format!("{}\n\n{}", field("title"), field("details")))
}

fn package_of(repo: &Path, sha: &str, file: &str) -> Result<String, String> {
    let mut dir = Path::new(file).parent();
    while let Some(d) = dir {
        let manifest = d.join("Cargo.toml");
        if let Ok(text) = git(repo, &["show", &format!("{sha}:{}", manifest.display())]) {
            let name = text
                .lines()
                .skip_while(|l| l.trim() != "[package]")
                .find_map(|l| l.trim().strip_prefix("name"))
                .and_then(|l| l.split('"').nth(1));
            return name
                .map(str::to_string)
                .ok_or_else(|| format!("{} has no package name", manifest.display()));
        }
        dir = d.parent();
    }
    Err(format!("no Cargo.toml above {file}"))
}

fn tree_of(repo: &Path, rev: &str) -> Result<tempfile::TempDir, String> {
    let dir = tempfile::tempdir().map_err(|e| e.to_string())?;
    super::runner::extract_ref(repo, rev, &[], dir.path())?;
    Ok(dir)
}

fn results(tree: &Path, package: &str) -> Result<BTreeMap<String, String>, String> {
    run_cargo(tree, package, None, RUN_TIMEOUT_S).map(|out| parse_libtest(&out))
}

/// Mint one draft; returns its path. Every refusal names the commit.
pub fn mint(repo: &Path, sha: &str, drafts: &Path) -> Result<PathBuf, String> {
    let refuse = |why: &str| format!("{sha}: {why}");
    let full = git(repo, &["rev-parse", &format!("{sha}^{{commit}}")])?;
    let parent = git(repo, &["rev-parse", &format!("{full}^")])?;
    let changed = git(repo, &["diff", "--name-only", &parent, &full])?;
    let test_files: Vec<String> = changed
        .lines()
        .filter(|f| crate::evals_candidates::is_test_file(f))
        .map(str::to_string)
        .collect();
    if test_files.is_empty() {
        return Err(refuse("no separate test file under src/ in the fix"));
    }
    let message = git(repo, &["log", "-1", "--format=%B", &full])?;
    let node =
        fixes_id(&message).ok_or_else(|| refuse("no `Fixes <id>` line to build a prompt from"))?;
    let task_text = node_text(&node).map_err(|e| refuse(&e))?;
    let package = package_of(repo, &full, &test_files[0]).map_err(|e| refuse(&e))?;
    let prefixes: Vec<String> = test_files.iter().filter_map(|f| module_prefix(f)).collect();

    let before =
        results(&tree_of(repo, &parent)?.path().to_path_buf(), &package).map_err(|e| refuse(&e))?;
    let gold = tree_of(repo, &full)?;
    let after = results(gold.path(), &package).map_err(|e| refuse(&e))?;
    let (f2p, mut p2p) = split_ids(&before, &after, &prefixes);
    if f2p.is_empty() {
        return Err(refuse(
            "f2p empty: the fix adds no test that passes only with it",
        ));
    }

    let ids: Vec<String> = f2p.iter().chain(&p2p).cloned().collect();
    let mut stable: BTreeSet<String> = ids.iter().cloned().collect();
    for _ in 0..GOLD_RUNS {
        let out =
            run_cargo(gold.path(), &package, Some(&ids), RUN_TIMEOUT_S).map_err(|e| refuse(&e))?;
        let seen = parse_libtest(&out);
        stable.retain(|id| seen.get(id).is_some_and(|s| s == "ok"));
    }
    if let Some(bad) = f2p.iter().find(|id| !stable.contains(*id)) {
        return Err(refuse(&format!(
            "gold solve not green {GOLD_RUNS} of {GOLD_RUNS}: {bad}"
        )));
    }
    p2p.retain(|id| stable.contains(id));

    let short = &full[..8];
    let subject = message.lines().next().unwrap_or("");
    let draft = json!({
        "id": format!("wp-{short}"),
        "tier": "capability",
        "prompt": format!(
            "You are working in this repository. Follow its rules: AGENTS.md and the docs and skills it points to.\n\nMake the code change below. Change the source: its tests are checked separately and any edit to them is discarded. Leave your change in the working tree.\n\n{}",
            strip_paths(&task_text)
        ),
        "grade": [{
            "kind": "tests", "runner": "cargo", "tests_ref": full,
            "test_files": test_files, "package": package, "f2p": f2p, "p2p": p2p,
        }],
        "repo_fixture": parent,
        "timeout_minutes": 45,
        "tags": ["golden", "work-product"],
        "golden": {
            "source": {"kind": "fix-pr", "ref": full},
            "quote": subject,
            "expected": "The fix commit's added tests pass and the tests it kept green stay green.",
            "rule": subject,
            "failure_type": "wrong-assumption",
            "outcome": "failed",
        },
        "qualified": {
            "gold_rev": full, "f2p": f2p.len(), "p2p": p2p.len(), "runs": GOLD_RUNS,
            "at": chrono::Utc::now().to_rfc3339(),
        },
    });
    std::fs::create_dir_all(drafts).map_err(|e| e.to_string())?;
    let path = drafts.join(format!("wp-{short}.yaml"));
    let body = serde_yaml_ng::to_string(&draft).map_err(|e| e.to_string())?;
    std::fs::write(&path, body).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn status(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(a, b)| (a.to_string(), b.to_string()))
            .collect()
    }

    #[test]
    fn f2p_is_new_passing_tests_in_the_files_and_an_unchanged_pass_is_p2p() {
        let prefixes = vec![module_prefix("crates/x/src/evals/eval_attempt/tests.rs").unwrap()];
        assert_eq!(prefixes, ["evals::eval_attempt::tests"]);
        let before = status(&[
            ("evals::eval_attempt::tests::kept", "ok"),
            ("evals::eval_attempt::tests::broken", "FAILED"),
            ("other::tests::unrelated", "ok"),
        ]);
        let after = status(&[
            ("evals::eval_attempt::tests::kept", "ok"),
            ("evals::eval_attempt::tests::broken", "ok"),
            ("evals::eval_attempt::tests::added", "ok"),
            ("evals::eval_attempt::tests::flaky", "FAILED"),
            ("other::tests::unrelated", "ok"),
        ]);
        let (f2p, p2p) = split_ids(&before, &after, &prefixes);
        assert_eq!(
            f2p,
            [
                "evals::eval_attempt::tests::added",
                "evals::eval_attempt::tests::broken"
            ]
        );
        assert_eq!(p2p, ["evals::eval_attempt::tests::kept"]);
        let same = split_ids(&after, &after, &prefixes);
        assert!(
            same.0.is_empty(),
            "a test that passes before too is no f2p: {same:?}"
        );
        let text = strip_paths("Fix crates/fno-agents/src/evals/bank.rs so tests.rs reads ok");
        assert!(!text.contains(".rs") && !text.contains("crates/"), "{text}");
        assert_eq!(
            fixes_id("subject\n\nFixes x-5b5f\n").as_deref(),
            Some("x-5b5f")
        );
    }
}
