//! Receipt policy: the journal discovery, generation, equivalence and
//! local-verification policy behind `fno do pr evidence-*` (the receipts wave).
//!
//! Ports `cli/src/fno/pr/_preflight.py` (deleted in the same change). The
//! strict receipt decision itself was already native
//! (`verify_evidence::receipt_decision`); this module adds the pieces the
//! Python file owned alone: WHERE the journals live, the next generation
//! number, the rebase-equivalence arm, and the opt-in policy. The scope
//! lists stay only in verify_evidence.rs - one owner, no keep-in-step
//! comment (d-e11b2b3e).

use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::process::Command;

/// The default base for patch identity: `origin/main`, the Python module
/// constant.
pub(crate) const BASE_DEFAULT: &str = "origin/main";

/// Whether the strict evidence gate demands a local preflight run, and why.
pub(crate) fn local_verification_required(
    cwd: &Path,
    base_ref: &str,
    env: Option<&std::collections::HashMap<String, String>>,
) -> (bool, String) {
    if env_is(env, "FNO_SKIP_PREFLIGHT", "1") {
        return (false, "explicit-skip".to_string());
    }
    if !preflight_required_by_config(cwd) {
        // CI is the gate. A full local preflight is an opt-in rehearsal of
        // it, so a stock config never blocks a ready PR on a job that takes
        // 22 to 47 minutes. config.preflight.required = true restores it.
        return (false, "policy-opt-in".to_string());
    }
    let runner = cwd.join("scripts/ci/preflight.sh");
    let base_runner = git_out(cwd, &["ls-tree", base_ref, "--", "scripts/ci/preflight.sh"]);
    let Some(base_runner) = base_runner else {
        return (true, "base-policy-unavailable".to_string());
    };
    let base_configured = !base_runner.trim().is_empty();
    let candidate_configured = runner.is_file() && file_is_executable(&runner);
    if base_configured && !candidate_configured {
        return (true, "runner-removed".to_string());
    }
    if !candidate_configured {
        return (false, "runner-not-configured".to_string());
    }
    let changed = git_out(cwd, &["diff", "--name-only", &format!("{base_ref}...HEAD")]);
    let Some(changed) = changed else {
        return (true, "diff-unavailable".to_string());
    };
    let non_docs = changed
        .lines()
        .filter(|path| {
            !path.is_empty()
                && *path != "README.md"
                && !path.starts_with("docs/")
                && !path.starts_with("internal/")
        })
        .count();
    if non_docs > 0 {
        (true, "required".to_string())
    } else {
        (false, "docs-only".to_string())
    }
}

fn env_is(env: Option<&std::collections::HashMap<String, String>>, key: &str, val: &str) -> bool {
    match env {
        Some(map) => map.get(key).map(|v| v.as_str()) == Some(val),
        None => std::env::var(key).as_deref() == Ok(val),
    }
}

#[cfg(unix)]
fn file_is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path)
        .map(|m| m.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

#[cfg(not(unix))]
fn file_is_executable(path: &Path) -> bool {
    path.is_file()
}

/// The event journals one verification read consults, plus the discovery
/// errors that make coverage uncertain: the global journal, the repo's
/// space journal, and every delivery-root (and its salvage) journal the
/// ledger names. Ports `_preflight.verification_event_paths`.
pub(crate) fn verification_event_paths(cwd: &Path) -> (Vec<PathBuf>, Vec<String>) {
    let mut errors: Vec<String> = Vec::new();
    let repo = std::fs::canonicalize(cwd).unwrap_or_else(|_| cwd.to_path_buf());
    let root = git_out(&repo, &["rev-parse", "--show-toplevel"])
        .map(|out| PathBuf::from(out.trim()))
        .filter(|p| !p.as_os_str().is_empty())
        .map(|p| std::fs::canonicalize(&p).unwrap_or(p))
        .unwrap_or_else(|| repo.clone());
    let mut paths = vec![
        crate::paths_cli::global_events_json(&repo),
        crate::paths::events_path(&root),
    ];
    let ledger = crate::paths_cli::ledger_json(&repo);
    let Ok(text) = std::fs::read_to_string(&ledger) else {
        // FileNotFoundError: pass, exactly like the Python leg.
        return (paths, errors);
    };
    match delivery_journals_from_ledger(&text) {
        Ok(extra) => paths.extend(extra),
        Err(e) => errors.push(format!("delivery journal discovery failed: {e}")),
    }
    (paths, errors)
}

/// The delivery-root and salvage journals the ledger names. A row carries
/// `root_path` (its space journal joins the scan) and
/// `canonical_root_path` (each salvage checkout's journal joins). Malformed
/// structure refuses loudly: uncertain coverage must say so.
fn delivery_journals_from_ledger(text: &str) -> Result<Vec<PathBuf>, String> {
    let raw: Value = serde_json::from_str(text).map_err(|e| e.to_string())?;
    let rows: Vec<Value> = match &raw {
        Value::Array(items) => items.clone(),
        Value::Object(map) => match map.get("entries") {
            Some(Value::Array(items)) => items.clone(),
            _ => return Err("ledger must be a list or an object with an entries list".into()),
        },
        _ => return Err("ledger must be a list or an object with an entries list".into()),
    };
    let mut paths = Vec::new();
    for row in &rows {
        let Some(map) = row.as_object() else {
            return Err("ledger entries must be objects".into());
        };
        for field in ["root_path", "canonical_root_path"] {
            if let Some(value) = map.get(field) {
                // An explicit null is the writer's own no-root shape
                // (cost/_register.py writes `root_path or None`): only a
                // present, non-null, non-string or empty string refuses.
                if value.is_null() {
                    continue;
                }
                let s = value.as_str().map(str::trim).unwrap_or("");
                if s.is_empty() {
                    return Err(format!("ledger {field} must be a non-empty string or null"));
                }
            }
        }
        if let Some(root) = map
            .get("root_path")
            .and_then(Value::as_str)
            .filter(|s| !s.trim().is_empty())
        {
            paths.push(crate::paths::events_path(&PathBuf::from(root)));
        }
        if let Some(canonical) = map
            .get("canonical_root_path")
            .and_then(Value::as_str)
            .filter(|s| !s.trim().is_empty())
        {
            let salvage = PathBuf::from(canonical).join(".fno/salvage");
            let mut entries: Vec<PathBuf> = std::fs::read_dir(&salvage)
                .map_err(|e| {
                    format!(
                        "salvage journal discovery failed for {}: {e}",
                        salvage.display()
                    )
                })?
                .filter_map(|e| e.ok())
                .map(|e| e.path())
                .filter(|p| p.is_dir())
                .collect();
            entries.sort();
            for dir in entries {
                let journal = dir.join("events.jsonl");
                if journal.exists() {
                    paths.push(journal);
                }
            }
        }
    }
    Ok(paths)
}

/// One above every discovered valid exact-SHA receipt generation. Ports
/// `_preflight.next_verification_generation` including its two authority
/// arms: the canonical journal decides when it holds the candidate, and a
/// mirror that carries a HIGHER generation than the canonical one is an
/// error, not a number to beat.
pub(crate) fn next_verification_generation(cwd: &Path, candidate_sha: &str) -> Result<i64, String> {
    if !is_full_sha(candidate_sha) {
        return Err("candidate_sha must be a full 40-hex commit".into());
    }
    let (paths, errors) = verification_event_paths(cwd);
    if !errors.is_empty() {
        return Err(errors.join("; "));
    }
    let (canonical, found) = highest_generation(&paths[..1], candidate_sha, false)?;
    let highest = if found {
        let (discovered, _) = highest_generation(&paths, candidate_sha, true)?;
        if discovered > canonical {
            return Err("mirror generation exceeds canonical authority".into());
        }
        canonical
    } else {
        highest_generation(&paths, candidate_sha, false)?.0
    };
    if highest >= MAX_SAFE_EVENT_INTEGER {
        return Err("receipt generation exhausted".into());
    }
    Ok(highest + 1)
}

/// The generator ceiling the events judge enforces.
const MAX_SAFE_EVENT_INTEGER: i64 = 9_007_199_254_740_991;

fn is_full_sha(s: &str) -> bool {
    s.len() == 40 && s.bytes().all(|b| b.is_ascii_hexdigit())
}

/// Highest valid generation among exact-candidate receipts across the
/// paths. `skip_unreadable` mirrors the Python flag; strict mode refuses
/// on an unreadable journal. Receipt validity rides the same shape test
/// the strict decision uses.
fn highest_generation(
    paths: &[PathBuf],
    candidate_sha: &str,
    skip_unreadable: bool,
) -> Result<(i64, bool), String> {
    let mut highest: i64 = 0;
    let mut found = false;
    for path in paths {
        let text = match crate::event_store::journal_text_checked(
            path,
            &crate::event_store::EventQuery::of_types(&["verification_receipt"]),
        ) {
            Ok(text) => text,
            Err(_) => {
                if skip_unreadable {
                    continue;
                }
                return Err(format!("receipt journal unreadable: {}", path.display()));
            }
        };
        for line in text.lines().map(str::trim).filter(|l| !l.is_empty()) {
            let Ok(event) = serde_json::from_str::<Value>(line) else {
                return Err(format!(
                    "receipt journal malformed: {}: unparseable line",
                    path.display()
                ));
            };
            if event.get("type").and_then(Value::as_str) != Some("verification_receipt") {
                continue;
            }
            let sha = event
                .pointer("/data/candidate_sha")
                .and_then(Value::as_str)
                .unwrap_or("");
            if sha.is_empty() {
                return Err(format!(
                    "receipt journal malformed: {}: candidate SHA missing",
                    path.display()
                ));
            }
            if !sha.eq_ignore_ascii_case(candidate_sha) {
                continue;
            }
            let generation = event
                .pointer("/data/generation")
                .and_then(Value::as_i64)
                .ok_or_else(|| {
                    format!(
                        "receipt journal malformed: {}: bad generation",
                        path.display()
                    )
                })?;
            found = true;
            highest = highest.max(generation);
        }
    }
    Ok((highest, found))
}

/// The strict decision wired to discovery and the equivalence arm: resolve
/// HEAD, discover the journals, run `verify_evidence::receipt_decision`,
/// and on an allowed miss borrow a patch-identical ancestor's green. Ports
/// `_preflight.check_verification_evidence` (+ `run_evidence_check`'s
/// stderr line lives in the verb door).
pub(crate) fn check_verification_evidence(
    cwd: &Path,
    candidate_sha: Option<&str>,
    allow_equivalent: bool,
) -> Value {
    let sha = match candidate_sha {
        Some(sha) => sha.to_string(),
        None => git_out(cwd, &["rev-parse", "HEAD"])
            .map(|s| s.trim().to_string())
            .unwrap_or_default(),
    };
    if !is_full_sha(&sha) {
        return json!({
            "satisfied": false, "mode": Value::Null, "result": "unavailable",
            "receipt": Value::Null,
            "coverage": {"complete": false, "error": "candidate SHA unavailable"},
        });
    }
    let (paths, discovery_errors) = verification_event_paths(cwd);
    let str_paths: Vec<String> = paths
        .iter()
        .map(|p| p.to_string_lossy().into_owned())
        .collect();
    let mut decision = crate::verify_evidence::receipt_decision(&sha, &str_paths);
    if !discovery_errors.is_empty() {
        decision["coverage"]["complete"] = Value::Bool(false);
        decision["coverage"]["discovery_errors"] = json!(discovery_errors);
        decision["satisfied"] = Value::Bool(false);
    }
    let result = decision.get("result").and_then(Value::as_str).unwrap_or("");
    let coverage_complete = decision.pointer("/coverage/complete") == Some(&Value::Bool(true));
    if allow_equivalent
        && decision.get("satisfied") != Some(&Value::Bool(true))
        && discovery_errors.is_empty()
        // A failed or still-running verdict for HEAD itself is authoritative:
        // the same patches can fail against a newer main, so an older green
        // for a patch-equal ancestor must never override a fresh red, and an
        // unfinished attempt deserves the same protection.
        && result != "failed"
        && result != "pending"
        // The strict path refuses on incomplete coverage; equivalence must
        // not relax that (malformed lines, unreadable journals).
        && coverage_complete
    {
        if let Some(equivalent) =
            rebase_equivalent_evidence(cwd, &sha, &paths, &decision, BASE_DEFAULT)
        {
            decision = equivalent;
        }
    }
    decision
}

/// Sorted patch identity of the commits `sha` adds over its merge-base:
/// `git patch-id --verbatim` per commit, `None` on any failure. `None`
/// never matches anything, so every failure mode refuses. Verbatim ids
/// survive a clean rebase while counting whitespace, so a re-indent that
/// changes semantics hashes differently.
fn patch_identity(cwd: &Path, sha: &str, base_ref: &str) -> Option<Vec<String>> {
    let mb = git_out(cwd, &["merge-base", base_ref, sha])?;
    let mb = mb.trim().to_string();
    if mb.is_empty() {
        return None;
    }
    let log = git_out(cwd, &["log", "--no-merges", "-p", &format!("{mb}..{sha}")])?;
    let mut child = Command::new("git")
        .args(["patch-id", "--verbatim"])
        .current_dir(cwd)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .ok()?;
    // Write the patch text from a thread: patch-id emits ids as it parses,
    // so a long log can fill its stdout pipe while we are still writing
    // stdin, and a same-thread write-then-read deadlocks on both pipes.
    let mut stdin = child.stdin.take()?;
    let log_bytes = log.into_bytes();
    std::thread::spawn(move || {
        use std::io::Write;
        let _ = stdin.write_all(&log_bytes);
    });
    let out = child.wait_with_output().ok()?;
    if !out.status.success() {
        return None;
    }
    let mut ids: Vec<String> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|line| line.split_whitespace().next())
        .map(|id| format!("patch-id:{id}"))
        .collect();
    ids.sort();
    ids.dedup();
    if ids.is_empty() {
        None
    } else {
        Some(ids)
    }
}

fn git_out(cwd: &Path, args: &[&str]) -> Option<String> {
    let out = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Satisfy a refused strict decision from a receipt whose patches match.
/// Walks the discovered receipts for a full/passed one that clears the
/// trusted-producer test, whose commit still resolves, and whose patch
/// identity equals HEAD's. The no-rescue rule runs INSIDE the walk: a
/// failed or unfinished attempt for HEAD anywhere outranks an ancestor's
/// green, so the scan completes before any match. Ports
/// `_preflight.rebase_equivalent_evidence`.
pub(crate) fn rebase_equivalent_evidence(
    cwd: &Path,
    candidate_sha: &str,
    paths: &[PathBuf],
    decision: &Value,
    base_ref: &str,
) -> Option<Value> {
    let head_ids = patch_identity(cwd, candidate_sha, base_ref)?;
    let mut events: Vec<Value> = Vec::new();
    for path in paths {
        let Ok(text) = crate::event_store::journal_text_checked(
            path,
            &crate::event_store::EventQuery::of_types(&["verification_receipt"]),
        ) else {
            continue;
        };
        for line in text.lines().map(str::trim).filter(|l| !l.is_empty()) {
            let Ok(event) = serde_json::from_str::<Value>(line) else {
                continue;
            };
            if event.get("type").and_then(Value::as_str) != Some("verification_receipt") {
                continue;
            }
            events.push(event);
        }
    }
    let is_head = |event: &Value| {
        event
            .pointer("/data/candidate_sha")
            .and_then(Value::as_str)
            .is_some_and(|s| s.eq_ignore_ascii_case(candidate_sha))
    };
    // First pass, everywhere: the no-rescue rule.
    for event in &events {
        let result = event
            .pointer("/data/result")
            .and_then(Value::as_str)
            .unwrap_or("");
        if is_head(event) && (result == "failed" || result == "pending") {
            return None;
        }
    }
    // Second pass: the patch-equal ancestor.
    for event in &events {
        let mode = event
            .pointer("/data/mode")
            .and_then(Value::as_str)
            .unwrap_or("");
        let result = event
            .pointer("/data/result")
            .and_then(Value::as_str)
            .unwrap_or("");
        if mode != "full" || result != "passed" {
            continue;
        }
        if !crate::verify_evidence::gate_eligible_receipt(event) {
            continue;
        }
        let sha = event
            .pointer("/data/candidate_sha")
            .and_then(Value::as_str)
            .unwrap_or("");
        if sha.is_empty() || sha.eq_ignore_ascii_case(candidate_sha) {
            continue;
        }
        if git_out(cwd, &["cat-file", "-e", &format!("{sha}^{{commit}}")]).is_none() {
            continue;
        }
        if patch_identity(cwd, sha, base_ref) == Some(head_ids.clone()) {
            let mut matched = decision.clone();
            matched["satisfied"] = Value::Bool(true);
            matched["result"] = Value::String("equivalent".into());
            matched["coverage"]["matched_sha"] = json!(sha);
            matched["coverage"]["equivalence"] = Value::String("patch-id-verbatim".into());
            return Some(matched);
        }
    }
    None
}

/// `config.preflight.required` for the checkout, fail OPEN (false): the
/// failure this policy exists to remove is a gate no one can satisfy
/// stopping a green commit from reaching CI, so an unreadable or malformed
/// config means not-required, never a refusal. `$FNO_CONFIG` pins an
/// explicit file and is read directly.
fn preflight_required_by_config(cwd: &Path) -> bool {
    let read_required = |path: &Path| -> Option<bool> {
        let content = std::fs::read_to_string(path).ok()?;
        let root: toml::Value = toml::from_str(&content).ok()?;
        root.get("preflight")
            .and_then(|block| block.get("required"))
            .and_then(|v| v.as_bool())
    };
    if let Ok(pin) = std::env::var("FNO_CONFIG") {
        if !pin.trim().is_empty() {
            return read_required(Path::new(&pin)).unwrap_or(false);
        }
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
    let global = read_required(&PathBuf::from(home).join(".fno/config.toml"));
    let local = read_required(&cwd.join(".fno/config.toml"));
    local.or(global).unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generation_starts_at_one_on_empty_journals() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (rc, gen) = {
            let result = next_verification_generation(tmp.path(), &"a".repeat(40));
            match result {
                Ok(n) => (0, n),
                Err(_) => (1, 0),
            }
        };
        assert_eq!(rc, 0);
        assert_eq!(gen, 1);
    }

    #[test]
    fn bad_candidate_sha_refuses() {
        let tmp = tempfile::TempDir::new().unwrap();
        assert!(next_verification_generation(tmp.path(), "zzz").is_err());
    }
}
