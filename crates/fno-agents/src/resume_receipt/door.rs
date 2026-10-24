//! The argv door: `fno-agents resume-receipt write|validate|show ...`.
//!
//! The Python leaf (`cli/src/fno/resume/cli.py`) forwards here with the same
//! options; stdout bytes and exit codes are the contract, and the goldens
//! under `tests/golden/resume_receipt/` pin them. The verbs are hidden
//! infrastructure: handoff writes at a phase boundary, a successor session
//! validates before any write, and `show` prints parse-checked evidence.
//! The door preserves the deleted typer leg's byte shapes exactly: compact
//! JSON with `", "` / `": "` separators in insertion order (write, validate),
//! sorted two-space pretty JSON (show), Python's ensure_ascii escaping, and
//! json.JSONDecodeError-shaped parse errors.

use crate::claims::{self, ClaimState};
use crate::resume_receipt::model::{self, ReceiptError, ResumeReceipt};
use crate::resume_receipt::revalidate;
use crate::task_context::BoundBinding;
use serde_json::{json, Value};
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

/// The deprecated `--session` alias: canonical when legacy is absent, legacy
/// (with the deprecation warning) when canonical is absent, a refusal shaped
/// like the deleted click.UsageError when both arrive.
fn merge_session_alias(pairs: &[(String, String)]) -> Result<Option<String>, String> {
    let canonical = value_of(pairs, "--session-id");
    let legacy = value_of(pairs, "--session");
    match (canonical, legacy) {
        (Some(_), Some(_)) => {
            Err("pass either --session-id or --session (deprecated), not both".to_string())
        }
        (Some(s), None) | (None, Some(s)) => Ok(Some(s.to_string())),
        (None, None) => Ok(None),
    }
}

/// A newline-separated list option: blank lines drop, lines keep their inner
/// text verbatim (the deleted leg split on newlines and kept non-blank
/// parts, no trimming; a CRLF newline folds like Python's splitlines).
fn split_list(raw: Option<&str>) -> Vec<String> {
    raw.unwrap_or_default()
        .split('\n')
        .map(|part| part.strip_suffix('\r').unwrap_or(part))
        .filter(|part| !part.trim().is_empty())
        .map(str::to_string)
        .collect()
}

/// Serialize like `json.dumps` defaults: `", "` / `": "` separators,
/// insertion order, ensure_ascii escaping.
fn py_compact(value: &Value) -> String {
    match value {
        Value::Object(map) => {
            if map.is_empty() {
                return "{}".to_string();
            }
            let items: Vec<String> = map
                .iter()
                .map(|(k, v)| format!("{}: {}", model::py_json_string(k), py_compact(v)))
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
        Value::String(s) => model::py_json_string(s),
        other => other.to_string(),
    }
}

fn artifacts_dir(repo_root: &Path) -> PathBuf {
    repo_root.join(".fno").join("artifacts").join("handoff")
}

/// The repo root for artifact resolution: the FNO_REPO_ROOT pin, then `git
/// rev-parse --show-toplevel`, then cwd - the Python resolution's order.
/// The pin and the git answer realpath (Path.resolve), so /tmp answers
/// /private/tmp on macOS; the cwd fallback stays unresolved like Python's.
fn resolve_repo_root() -> PathBuf {
    if let Ok(pin) = std::env::var("FNO_REPO_ROOT") {
        if !pin.trim().is_empty() {
            return match Path::new(&pin).canonicalize() {
                Ok(resolved) => resolved,
                Err(_) => PathBuf::from(pin),
            };
        }
    }
    match git_out(Path::new("."), &["rev-parse", "--show-toplevel"]) {
        Some(root) if !root.is_empty() => PathBuf::from(root),
        _ => PathBuf::from("."),
    }
}

/// One bounded git read in `worktree`: stdout trimmed, empty on any failure
/// (the deleted leg's `_run`: `check=False`, 10s timeout, "" on miss). The
/// commands are fixed local reads, so a plain spawn keeps them
/// injection-free; the 10s bound polls try_wait and kills at the deadline.
fn git_out(worktree: &Path, args: &[&str]) -> Option<String> {
    let mut child = std::process::Command::new("git")
        .args(args)
        .current_dir(worktree)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .ok()?;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) if std::time::Instant::now() >= deadline => break None,
            Ok(None) => std::thread::sleep(std::time::Duration::from_millis(10)),
            Err(_) => break None,
        }
    };
    if status.is_none() {
        let _ = child.kill();
        let _ = child.wait();
        return None;
    }
    let out = child.wait_with_output().ok()?;
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
        // Leftmost `-g<digits>-` match in the name, like the deleted leg's
        // regex: digits must close with a dash, or the segment does not count.
        let mut gen = 0i64;
        let bytes = name.as_bytes();
        let mut i = 0;
        while i + 2 < bytes.len() {
            if bytes[i] == b'-' && bytes[i + 1] == b'g' && bytes[i + 2].is_ascii_digit() {
                let start = i + 2;
                let mut end = start;
                while end < bytes.len() && bytes[end].is_ascii_digit() {
                    end += 1;
                }
                if end < bytes.len() && bytes[end] == b'-' {
                    gen = name[start..end].parse().unwrap_or(0);
                }
                break;
            }
            i += 1;
        }
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

fn fail_line_as(out: Value, code: i32) -> i32 {
    println!("{}", py_compact(&out));
    code
}

fn fail_line(out: Value) -> i32 {
    fail_line_as(out, 1)
}

/// `date -u +%Y-%m-%dT%H:%M:%SZ` - the deleted default writer stamp.
fn now_utc() -> String {
    chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string()
}

/// The binding rides the declared env path, the same carrier the gate and the
/// spawner read; embedding it marks the receipt bound. An unreadable or
/// unparseable file refuses by name, like the deleted leg.
fn load_task_context() -> Result<Option<Value>, String> {
    let declared = std::env::var("FNO_TASK_CONTEXT_FILE")
        .unwrap_or_default()
        .trim()
        .to_string();
    if declared.is_empty() {
        return Ok(None);
    }
    match std::fs::read_to_string(&declared) {
        Ok(raw) => match serde_json::from_str::<Value>(&raw) {
            Ok(v) => Ok(Some(v)),
            Err(e) => Err(format!("task_context file unreadable: {e}")),
        },
        Err(e) => Err(format!("task_context file unreadable: {e}")),
    }
}

/// Options shared by write and validate: the merged session (with the
/// deprecation warning on the legacy spelling) and the required check.
fn common_options(
    args: &[String],
    required: &[&str],
) -> Result<(Vec<(String, String)>, Option<String>), i32> {
    let pairs = match parse_pairs(args) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("{e}");
            return Err(2);
        }
    };
    for name in required {
        if value_of(&pairs, name).is_none() {
            eprintln!("error: missing required option {name}");
            return Err(2);
        }
    }
    let session = match merge_session_alias(&pairs) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("{e}");
            return Err(2);
        }
    };
    if value_of(&pairs, "--session-id").is_none() && value_of(&pairs, "--session").is_some() {
        eprintln!(
            "warning: --session is deprecated; use --session-id instead. \
The alias will be removed in a future release."
        );
    }
    Ok((pairs, session))
}

fn run_write(args: &[String]) -> i32 {
    let (pairs, session) = match common_options(
        args,
        &[
            "--node",
            "--phase",
            "--generation",
            "--repo",
            "--worktree",
            "--branch",
            "--head",
            "--next-command",
        ],
    ) {
        Ok(v) => v,
        Err(code) => return code,
    };
    let generation = match value_of(&pairs, "--generation")
        .unwrap_or_default()
        .parse::<i64>()
    {
        Ok(g) => g,
        Err(_) => {
            eprintln!(
                "error: Invalid value for '--generation': '{}' is not a valid integer",
                value_of(&pairs, "--generation").unwrap_or_default()
            );
            return 2;
        }
    };
    let task_context = match load_task_context() {
        Ok(v) => v,
        Err(e) => {
            return fail_line_as(
                json!({"ok": false, "reason": "invalid_input", "error": e}),
                2,
            );
        }
    };
    let written_at = value_of(&pairs, "--written-at")
        .map(str::to_string)
        .unwrap_or_else(now_utc);
    let receipt = match model::build_receipt(
        value_of(&pairs, "--node").unwrap_or_default(),
        session.as_deref().unwrap_or(""),
        value_of(&pairs, "--phase").unwrap_or_default(),
        generation,
        value_of(&pairs, "--repo").unwrap_or_default(),
        value_of(&pairs, "--worktree").unwrap_or_default(),
        value_of(&pairs, "--branch").unwrap_or_default(),
        value_of(&pairs, "--head").unwrap_or_default(),
        value_of(&pairs, "--next-command").unwrap_or_default(),
        value_of(&pairs, "--next-target"),
        &written_at,
        split_list(value_of(&pairs, "--completed-tasks")),
        split_list(value_of(&pairs, "--remaining-tasks")),
        split_list(value_of(&pairs, "--open-findings")),
        split_list(value_of(&pairs, "--known-reds")),
        split_list(value_of(&pairs, "--watchers")),
        split_list(value_of(&pairs, "--idempotency-keys")),
        task_context,
    ) {
        Ok(r) => r,
        Err(e) => {
            return fail_line_as(
                json!({
                    "ok": false, "reason": "invalid_input", "error": e.to_string()
                }),
                2,
            );
        }
    };
    let root = resolve_repo_root();
    match model::write_receipt(&receipt, &artifacts_dir(&root)) {
        Ok(path) => {
            println!(
                "{}",
                py_compact(&json!({
                    "ok": true, "path": path.display().to_string(),
                    "content_sha": receipt.content_sha,
                }))
            );
            0
        }
        Err(ReceiptError::AlreadyExists(m)) => fail_line(json!({
            "ok": false, "reason": "already_exists", "error": m
        })),
        Err(e) => {
            eprintln!("{e}");
            1
        }
    }
}

fn run_validate(args: &[String]) -> i32 {
    let (pairs, session) = match common_options(args, &["--node"]) {
        Ok(v) => v,
        Err(code) => return code,
    };
    let node = value_of(&pairs, "--node").unwrap_or_default();
    let root = resolve_repo_root();
    let Some(latest) = find_latest_receipt(node, &artifacts_dir(&root)) else {
        return fail_line(json!({"ok": false, "reason": "no_receipt", "node": node}));
    };
    let receipt: ResumeReceipt = match model::load_receipt(&latest) {
        Ok(r) => r,
        Err(ReceiptError::Malformed(m)) => {
            return fail_line(json!({
                "ok": false, "reason": "malformed_receipt",
                "error": m, "path": latest.display().to_string(),
            }));
        }
        Err(e) => {
            eprintln!("{e}");
            return 1;
        }
    };

    let wt = value_of(&pairs, "--worktree")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(&receipt.worktree));

    // A receipt that carries a binding revalidates it natively BEFORE the
    // authority checks; identity expectations ride only when named. The gate
    // command owns the declared semantics; this door maps refusals to exits.
    let mut context_checked = false;
    if let Some(tc) = &receipt.task_context {
        // `session_id` here is the CANONICAL flag's presence, not the merged
        // value: the deprecated --session spelling never feeds the expect.
        let expect = if value_of(&pairs, "--session-id").is_some() {
            json!({"node": node, "session": session.as_deref().unwrap_or_default()})
        } else {
            json!({"node": node})
        };
        let answer = crate::task_context::gate_request(&json!({
            "node": node,
            "root": wt.display().to_string(),
            "binding": tc,
            "expect": expect,
        }));
        if !answer.get("ok").and_then(Value::as_bool).unwrap_or(false) {
            let detail = answer.get("detail").and_then(Value::as_str).unwrap_or("{}");
            let binding: Value = serde_json::from_str(detail).unwrap_or(Value::Null);
            return fail_line(json!({
                "ok": false,
                "reason": answer.get("reason").cloned().unwrap_or(Value::Null),
                "node": node,
                "binding": binding,
            }));
        }
        context_checked = answer.get("answer").is_some();
    }

    let (live_head, live_branch) = if wt.exists() {
        git_head_and_branch(&wt)
    } else {
        (String::new(), String::new())
    };
    let claims_root: Option<PathBuf> = value_of(&pairs, "--claims-root").map(expand_tilde);
    let (state, record) = claims::status(&format!("node:{node}"), claims_root.as_deref());
    // Fail CLOSED on a corrupted lockfile: an unreadable claim cannot confirm
    // ownership, so it must not collapse to "free" and grant an ok verdict.
    if state == ClaimState::Corrupted {
        return fail_line(json!({
            "ok": false,
            "reason": "corrupted_claim",
            "node": node,
            "receipt": latest.display().to_string(),
            "claim_error": Value::Null,
            "checked": {"live_claim_state": "corrupted"},
        }));
    }
    let live_holder = if matches!(state, ClaimState::Free | ClaimState::Corrupted) {
        None
    } else {
        record.map(|r| r.holder).filter(|h| !h.is_empty())
    };

    let events_path = value_of(&pairs, "--events")
        .map(PathBuf::from)
        .unwrap_or_else(|| wt.join(".fno").join("events.jsonl"));
    // The store beside the journal answers even when no journal file exists.
    let node_events: Vec<Value> = revalidate::read_node_events(&[events_path])
        .into_iter()
        .filter(|e| revalidate::event_node(e) == Some(node))
        .collect();

    let harness = value_of(&pairs, "--harness");
    let res = revalidate::revalidate(
        &receipt,
        &live_head,
        &live_branch,
        wt.exists(),
        live_holder.as_deref(),
        session.as_deref(),
        &node_events,
        harness,
    );
    let mut checked = res.checked;
    checked.insert("live_claim_state".into(), json!(state.as_str()));
    println!(
        "{}",
        py_compact(&json!({
            "ok": res.ok,
            "reason": res.reason,
            "node": node,
            "receipt": latest.display().to_string(),
            "next_action": {"verb": receipt.next_action.command, "target": receipt.next_action.target},
            "idempotency_keys": receipt.idempotency_keys,
            "checked": Value::Object(checked),
            "context_checked": context_checked,
            "task_context_bound": receipt.task_context.is_some(),
        }))
    );
    if res.ok {
        0
    } else {
        1
    }
}

fn run_show(args: &[String]) -> i32 {
    let pairs = match parse_pairs(args) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("{e}");
            return 2;
        }
    };
    if value_of(&pairs, "--node").is_none() {
        eprintln!("error: missing required option --node");
        return 2;
    }
    let node = value_of(&pairs, "--node").unwrap_or_default();
    let path = match value_of(&pairs, "--path") {
        Some(p) => PathBuf::from(p),
        None => match find_latest_receipt(node, &artifacts_dir(&resolve_repo_root())) {
            Some(p) => p,
            None => return fail_line(json!({"ok": false, "reason": "no_receipt", "node": node})),
        },
    };
    if !path.exists() {
        return fail_line(json!({"ok": false, "reason": "no_receipt", "node": node}));
    }
    let receipt = match model::load_receipt(&path) {
        Ok(r) => r,
        Err(ReceiptError::Malformed(m)) => {
            // show's malformed echo carries no path field, unlike validate's.
            return fail_line(json!({"ok": false, "reason": "malformed_receipt", "error": m}));
        }
        Err(e) => {
            eprintln!("{e}");
            return 1;
        }
    };
    let mut out = model::receipt_to_value(&receipt);
    // Honesty labels: legacy receipts are explicitly UNBOUND; a declared one
    // is verified natively, a corrupt one refuses by name.
    out["task_context_bound"] = json!(receipt.task_context.is_some());
    if let Some(tc) = &receipt.task_context {
        match BoundBinding::load(tc) {
            Ok(bound) => {
                out["task_context_stage"] = json!(bound.binding.stage.as_str());
            }
            Err(reason) => {
                return fail_line(json!({
                    "ok": false, "reason": "corrupt_binding",
                    "detail": {"ok": false, "reason": reason},
                }));
            }
        }
    }
    println!("{}", model::pretty_sorted_json(&out, 0));
    0
}

/// `Path.expanduser()`: a leading `~` becomes $HOME (unix). No home, no
/// expansion - the raw path stands.
fn expand_tilde(raw: &str) -> PathBuf {
    if raw == "~" {
        if let Ok(home) = std::env::var("HOME") {
            return PathBuf::from(home);
        }
    } else if let Some(rest) = raw.strip_prefix("~/") {
        if let Ok(home) = std::env::var("HOME") {
            return PathBuf::from(home).join(rest);
        }
    }
    PathBuf::from(raw)
}

/// Unused in the happy paths but load-bearing for the door's shape: keeps the
/// module's serde Map import honest when the verbs compile standalone.
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Map;

    #[test]
    fn parse_pairs_reads_inline_and_spaced_forms() {
        let args: Vec<String> = ["--node", "x-1", "--generation=2"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let pairs = parse_pairs(&args).expect("parses");
        assert_eq!(value_of(&pairs, "--node"), Some("x-1"));
        assert_eq!(value_of(&pairs, "--generation"), Some("2"));
    }

    #[test]
    fn py_parse_error_matches_the_json_decode_error_shape() {
        let err = serde_json::from_str::<Value>("{\"version\": 1, \"broken\": ")
            .expect_err("incomplete JSON");
        let text = model::py_parse_error("{\"version\": 1, \"broken\": ", &err);
        assert_eq!(text, "Expecting value: line 1 column 26 (char 25)");
    }

    #[test]
    fn py_compact_keeps_insertion_order_and_escapes_like_python() {
        let mut m = Map::new();
        m.insert("ok".into(), json!(false));
        m.insert("s".into(), json!("caf\u{e9}"));
        assert_eq!(
            py_compact(&Value::Object(m)),
            "{\"ok\": false, \"s\": \"caf\\u00e9\"}"
        );
    }
}
