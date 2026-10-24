//! The argv door: `fno-agents resume-receipt write|validate|show ...`.
//!
//! The Python leaf (`cli/src/fno/resume/cli.py`) forwards here with the same
//! options; stdout bytes and exit codes are the contract, and the goldens
//! under `tests/golden/resume_receipt/` pin them. The verbs are hidden
//! infrastructure: handoff writes at a phase boundary, a successor session
//! validates before any write, and `show` prints parse-checked evidence.
//! The relay preserves the deleted typer leg's byte shapes exactly: compact
//! JSON with `", "` / `": "` separators in insertion order (write, validate),
//! sorted two-space pretty JSON (show), stderr only for usage.
#![allow(dead_code)] // skeleton: consumed when the verb bodies land, next commits

use serde_json::Value;
use std::path::{Path, PathBuf};

pub const USAGE: &str = "usage: fno-agents resume-receipt write|validate|show [options]";

pub fn run(args: &[String]) -> i32 {
    let Some(verb) = args.first() else {
        eprintln!("{USAGE}");
        return 2;
    };
    let rest = &args[1..];
    match verb.as_str() {
        "write" => run_write(rest),
        "validate" => run_validate(rest),
        "show" => run_show(rest),
        other => {
            eprintln!("{USAGE} (unknown verb: {other})");
            2
        }
    }
}

/// `--flag value` pairs with `--flag=value` folded in. Unknown positionals
/// refuse: the deleted typer leg had no positional surface.
fn parse_pairs(args: &[String]) -> Result<Vec<(String, String)>, String> {
    let mut pairs: Vec<(String, String)> = Vec::new();
    let mut it = args.iter();
    while let Some(tok) = it.next() {
        let (name, inline) = match tok.split_once('=') {
            Some((n, v)) => (n.to_string(), Some(v.to_string())),
            None => (tok.clone(), None),
        };
        if !name.starts_with("--") {
            return Err(format!("unexpected argument: {tok}"));
        }
        let value = match inline {
            Some(v) => v,
            None => match it.next() {
                Some(v) => v.clone(),
                None => return Err(format!("{name} needs a value")),
            },
        };
        pairs.push((name, value));
    }
    Ok(pairs)
}

fn value_of<'a>(pairs: &'a [(String, String)], name: &str) -> Option<&'a str> {
    pairs
        .iter()
        .rev()
        .find(|(n, _)| n == name)
        .map(|(_, v)| v.as_str())
}

fn has_flag(pairs: &[(String, String)], name: &str) -> bool {
    pairs.iter().any(|(n, _)| n == name)
}

/// The deprecated `--session` alias: canonical when legacy is absent, legacy
/// when canonical is absent, a refusal when both arrive.
fn merge_session_alias(pairs: &[(String, String)]) -> Result<Option<String>, String> {
    let canonical = value_of(pairs, "--session-id");
    let legacy = value_of(pairs, "--session");
    match (canonical, legacy) {
        (Some(_), Some(_)) => Err(
            "--session-id and --session are the same option; pass --session-id only".to_string(),
        ),
        (Some(s), None) => Ok(Some(s.to_string())),
        (None, Some(s)) => Ok(Some(s.to_string())),
        (None, None) => Ok(None),
    }
}

/// A newline-separated list option: blank lines drop, lines keep their inner
/// text verbatim (the deleted leg split on newlines and kept non-blank
/// parts, no trimming).
fn split_list(raw: Option<&str>) -> Vec<String> {
    raw.unwrap_or_default()
        .split('\n')
        .filter(|part| !part.trim().is_empty())
        .map(str::to_string)
        .collect()
}

/// Serialize like `json.dumps` defaults: `", "` and `": "` separators,
/// insertion order preserved.
fn py_compact(value: &Value) -> String {
    match value {
        Value::Object(map) => {
            if map.is_empty() {
                return "{}".to_string();
            }
            let items: Vec<String> = map
                .iter()
                .map(|(k, v)| {
                    format!(
                        "{}: {}",
                        serde_json::to_string(k).unwrap_or_default(),
                        py_compact(v)
                    )
                })
                .collect();
            format!("{{{}}}", items.join(", "))
        }
        Value::Array(items) => {
            if items.is_empty() {
                return "[]".to_string();
            }
            let items: Vec<String> = items.iter().map(py_compact).collect();
            format!("[{}]", items.join(", "))
        }
        other => other.to_string(),
    }
}

fn artifacts_dir(repo_root: &Path) -> PathBuf {
    repo_root.join(".fno").join("artifacts").join("handoff")
}

/// The repo root for artifact resolution: the FNO_REPO_ROOT pin, then `git
/// rev-parse --show-toplevel`, then cwd - the Python resolution's order.
fn resolve_repo_root() -> PathBuf {
    if let Ok(pin) = std::env::var("FNO_REPO_ROOT") {
        if !pin.trim().is_empty() {
            if let Ok(resolved) = Path::new(&pin).canonicalize() {
                return resolved;
            }
            return PathBuf::from(pin);
        }
    }
    git_out(Path::new("."), &["rev-parse", "--show-toplevel"])
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}

/// One bounded git read in `worktree`: stdout trimmed, empty on any failure
/// (the deleted leg's `_run`: `check=False`, 10s timeout, "" on miss).
fn git_out(worktree: &Path, args: &[&str]) -> Option<String> {
    let out = std::process::Command::new("git")
        .args(args)
        .current_dir(worktree)
        .output()
        .ok()?;
    if out.status.success() {
        Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
    } else {
        None
    }
}

fn git_head_and_branch(worktree: &Path) -> (String, String) {
    let head = git_out(worktree, &["rev-parse", "HEAD"]).unwrap_or_default();
    let branch = git_out(worktree, &["rev-parse", "--abbrev-ref", "HEAD"]).unwrap_or_default();
    (head, branch)
}

/// Latest receipt for a node: highest `-gN-` generation in the filename, then
/// newest mtime. Selection does NOT load the receipt: a corrupt latest must
/// still be SELECTED (so the caller reports `malformed_receipt`), not
/// silently skipped in favor of an older good one.
fn find_latest_receipt(node: &str, dir: &Path) -> Option<PathBuf> {
    let entries = std::fs::read_dir(dir).ok()?;
    let prefix = format!("receipt-{node}-");
    let mut best: Option<(i64, std::time::SystemTime, PathBuf)> = None;
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if !name.starts_with(&prefix) || !name.ends_with(".json") {
            continue;
        }
        let gen = name
            .split("-g")
            .nth(1)
            .and_then(|rest| rest.split('-').next())
            .and_then(|digits| digits.parse::<i64>().ok())
            .unwrap_or(0);
        let mtime = entry
            .metadata()
            .and_then(|m| m.modified())
            .unwrap_or(std::time::UNIX_EPOCH);
        let better = match &best {
            None => true,
            Some((best_gen, best_mtime, _)) => (gen, mtime) > (*best_gen, *best_mtime),
        };
        if better {
            best = Some((gen, mtime, path));
        }
    }
    best.map(|(_, _, path)| path)
}

fn fail_line(out: Value) -> i32 {
    println!("{}", py_compact(&out));
    1
}

/// `date -u +%Y-%m-%dT%H:%M:%SZ` - the deleted default writer stamp.
fn now_utc() -> String {
    chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string()
}

fn run_write(args: &[String]) -> i32 {
    let _pairs = match parse_pairs(args) {
        Ok(pairs) => pairs,
        Err(e) => {
            eprintln!("{e}");
            return 2;
        }
    };
    eprintln!("resume-receipt write: the body lands in this PR's next commits");
    2
}

fn run_validate(_args: &[String]) -> i32 {
    eprintln!("resume-receipt validate: the body lands in this PR's next commits");
    2
}

fn run_show(_args: &[String]) -> i32 {
    eprintln!("resume-receipt show: the body lands in this PR's next commits");
    2
}
