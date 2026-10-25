//! The native `fno inbox outstanding` subcommands: `mine`, `ask`, `clear`,
//! `reindex`.
//!
//! The deleted Python package (`cli/src/fno/outstanding/`) answered these
//! behind the native listing; this module is the one leg now. Everything the
//! shells resolved Python-side - identity, live law rows, storage root,
//! session id, the lane store, the mail delivery - resolves here, so the
//! front door can claim the spellings outright with no forward shell.
//!
//! Parity contract: every verb spelling, refusal line and exit code matches
//! the deleted Python leg byte for byte; the characterization tests pin the
//! lines the operator or a script can be mid-read on.

use crate::graph_store::BoundedLock;
use crate::org_board::queues::parse_lane;
use serde_json::{json, Value};
use std::path::PathBuf;

/// Route one `fno inbox outstanding <word>` tail. `None` is not a
/// subcommand: the caller falls through to the listing parse.
pub fn dispatch(word: &str, rest: &[String]) -> Option<i32> {
    match word {
        "mine" => Some(run_mine(rest)),
        "reindex" => Some(run_reindex(rest)),
        _ => None,
    }
}

/// The reindex arm: fold every ask/close identity from graph-named project
/// journals into the machine index. A missing project journal degrades to
/// zero rows; the one fatal read is the machine index itself.
fn run_reindex(args: &[String]) -> i32 {
    for a in args {
        match a.as_str() {
            "-h" | "--help" => {
                println!("usage: fno inbox outstanding reindex");
                return 0;
            }
            other => {
                eprintln!("outstanding: unknown argument {other:?}");
                return 2;
            }
        }
    }
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let src = crate::outstanding::sources(&cwd);
    let index = src.questions.clone();
    let journals: Vec<PathBuf> = crate::outstanding::capture_project_roots(&src)
        .iter()
        .map(|root| crate::paths::project_log(root, "events.jsonl"))
        .collect();
    match reindex_fold(&index, &journals) {
        Ok((added, already, total)) => println!(
            "{} event(s) added; {} already indexed; {} total",
            added, already, total
        ),
        Err(e) => {
            eprintln!("outstanding: failed to read: {e}");
            return 1;
        }
    }
    0
}

/// The fold behind the reindex arm: every ask/close identity from the named
/// journals lands in the machine index exactly once. Missing or unreadable
/// project journals degrade to zero rows; the machine index is the one store
/// a failed read refuses, because reporting an unreadable index as an empty
/// one would tell the operator the fold is current when it never ran.
fn reindex_fold(
    index: &std::path::Path,
    journals: &[PathBuf],
) -> Result<(usize, usize, usize), String> {
    let query = crate::event_store::EventQuery::of_types(&[
        "operator_question",
        "operator_question_closed",
    ]);
    let existing = crate::event_store::journal_text_checked(index, &query)?;
    let mut known: std::collections::HashSet<(String, String)> =
        existing.lines().filter_map(question_identity).collect();
    let preexisting = known.clone();
    let mut added = 0usize;
    let mut already: std::collections::HashSet<(String, String)> = std::collections::HashSet::new();
    let mut seen: std::collections::HashSet<(u64, u64)> = std::collections::HashSet::new();
    for journal in journals {
        // The store beside the journal is the write boundary: a journal whose
        // bytes never landed still owns a readable store, so the dedupe key
        // stats the store when the live file is absent.
        let live = crate::event_store::live_journal(journal);
        let probe = if live.exists() {
            live
        } else {
            crate::event_store::store_path(journal)
        };
        let Ok(meta) = std::fs::metadata(&probe) else {
            continue;
        };
        let key = (meta.dev(), meta.ino());
        if !seen.insert(key) {
            continue;
        }
        let text = match crate::event_store::journal_text_checked(journal, &query) {
            Ok(text) => text,
            Err(_) => continue,
        };
        for line in text.lines() {
            let Some(identity) = question_identity(line) else {
                continue;
            };
            if known.contains(&identity) {
                if preexisting.contains(&identity) {
                    already.insert(identity);
                }
                continue;
            }
            if crate::event_store::append_envelope(index, line, None).is_err() {
                continue;
            }
            known.insert(identity);
            added += 1;
        }
    }
    Ok((added, already.len(), known.len()))
}

/// The ask/close identity one journal row folds into the dedupe set: the
/// event type plus the question id. Decision rows carry no identity here.
fn question_identity(line: &str) -> Option<(String, String)> {
    let rec: Value = serde_json::from_str(line).ok()?;
    let kind = rec.get("type").and_then(Value::as_str)?;
    if kind != "operator_question" && kind != "operator_question_closed" {
        return None;
    }
    let qid = rec
        .pointer("/data/question_id")
        .and_then(Value::as_str)
        .filter(|v| !v.is_empty())?;
    Some((kind.to_string(), qid.to_string()))
}

/// The mine arm: list the operator's ranked lane, or apply one collapsed
/// mutation through `do`.
fn run_mine(args: &[String]) -> i32 {
    let path = lane_path();
    mine_at(&path, args)
}

/// The mine arm against one lane path; tests drive sandboxes through this
/// instead of moving the process cwd.
fn mine_at(path: &std::path::Path, args: &[String]) -> i32 {
    match args.split_first() {
        None => mine_list_at(path, false),
        Some((word, rest)) if word == "do" => mine_do_at(path, rest),
        Some((_, _)) => {
            let mut as_json = false;
            for a in args {
                match a.as_str() {
                    "--json" | "-J" => as_json = true,
                    "-h" | "--help" => {
                        println!("usage: fno inbox outstanding mine [--json|-J] | do add TEXT | do done N | do drop N | do link N NODE_ID");
                        return 0;
                    }
                    other => {
                        eprintln!("mine: unknown argument {other:?}");
                        return 2;
                    }
                }
            }
            mine_list_at(path, as_json)
        }
    }
}

/// The collapsed mutation door: `do add TEXT | done N | drop N | link N ID`.
/// One dispatched leaf instead of four: the CLI-surface ratchet caps total
/// registered leaves, and four commands doubled by the bare listing would
/// blow that budget.
fn mine_do_at(path: &std::path::Path, args: &[String]) -> i32 {
    let Some((action, rest)) = args.split_first() else {
        eprintln!("mine: failed: unknown action '' (add|done|drop|link)");
        return 2;
    };
    let want: usize = match action.as_str() {
        "add" | "done" | "drop" => 1,
        "link" => 2,
        other => {
            eprintln!(
                "mine: failed: unknown action {} (add|done|drop|link)",
                crate::mail_cli::send_raw::py_repr(other)
            );
            return 2;
        }
    };
    if rest.len() != want {
        eprintln!(
            "mine: failed: {action} takes {want} argument(s), got {}",
            rest.len()
        );
        return 2;
    }
    // Format validation runs before the lock, exactly as the Python leg did:
    // a malformed argument refuses without touching the file.
    match action.as_str() {
        "add" => {
            let text = &rest[0];
            if text.trim().is_empty() || text.contains('\n') || text.contains('\r') {
                eprintln!("mine: failed: item text must be one non-empty line");
                return 2;
            }
        }
        "done" | "drop" | "link" => {
            if parse_index(&rest[0]).is_none() {
                return 2;
            }
            if action == "link" && !is_node_id(&rest[1]) {
                eprintln!("mine: failed: invalid node id: {}", rest[1]);
                return 2;
            }
        }
        _ => unreachable!(),
    }
    match mutate_lane_at(path, action, rest) {
        Ok(count) => {
            println!("{count}");
            0
        }
        Err(err) => {
            eprintln!("mine: failed: {err}");
            1
        }
    }
}

fn parse_index(raw: &str) -> Option<usize> {
    match raw.parse::<usize>() {
        Ok(n) => Some(n),
        Err(_) => {
            eprintln!("mine: failed: not a number: {raw}");
            None
        }
    }
}

fn is_node_id(node: &str) -> bool {
    let body = crate::org_board::queues::NODE_ID_BODY;
    regex::Regex::new(&format!("^(?:{body})$"))
        .ok()
        .is_some_and(|re| re.is_match(node))
}

fn lane_path() -> PathBuf {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    crate::org_board::scope::operator_lane_path(&cwd)
}

/// The one lock-and-rewrite transaction: parse, read raw, apply the line
/// change, atomic replace in the same directory, re-read. The bounded lock
/// holds the lane's sidecar `<lane>.lock` for ten seconds, the same budget
/// the Python `filelock.FileLock` took.
fn mutate_lane_at(path: &std::path::Path, action: &str, rest: &[String]) -> Result<usize, String> {
    let path = path.to_path_buf();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("cannot write operator lane {}: {e}", path.display()))?;
    }
    let _lock = BoundedLock::acquire(&path, std::time::Duration::from_secs(10))
        .map_err(|e| format!("cannot write operator lane {}: {e}", path.display()))?;
    let items = parse_lane(&path)?;
    let content = match std::fs::read_to_string(&path) {
        Ok(c) => c,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(format!("cannot read operator lane {}: {e}", path.display())),
    };
    let mut lines: Vec<String> = split_keepends(&content);
    match action {
        "add" => {
            let text = rest[0].trim();
            if let Some(last) = lines.last_mut() {
                if !last.ends_with(['\n', '\r']) {
                    last.push('\n');
                }
            }
            lines.push(format!("- [ ] {text}\n"));
        }
        "done" | "drop" | "link" => {
            let index = rest[0].parse::<usize>().unwrap_or(0);
            let Some(item) = items.get(index.wrapping_sub(1)) else {
                return Err(format!(
                    "mine item {index} does not exist; choose 1 through {}",
                    items.len()
                ));
            };
            let idx = item.line - 1;
            let raw = lines.get(idx).ok_or_else(|| {
                format!(
                    "cannot read operator lane {}: line {index} is gone",
                    path.display()
                )
            })?;
            let owned = raw.clone();
            match action {
                "done" => {
                    let mark = if item.done { " " } else { "x" };
                    let mut flipped = String::with_capacity(owned.len());
                    flipped.push_str(&owned[..3]);
                    flipped.push_str(mark);
                    flipped.push_str(&owned[4..]);
                    lines[idx] = flipped;
                }
                "drop" => {
                    let _ = lines.remove(idx);
                }
                _ => {
                    // A link rewrites the whole line, so a parked reason on it
                    // is lost - the same overwrite the Python leg performed.
                    let ending = line_ending(&owned);
                    let mark = if item.done { " " } else { "x" };
                    lines[idx] = format!("- [{}] {} -> {}{}", mark, item.text, rest[1], ending);
                }
            }
        }
        _ => unreachable!(),
    }
    atomic_replace(&path, &lines.join(""))?;
    // The post-write read is the proof: a lane that cannot parse after the
    // rewrite is a failed write, not a quiet one.
    let updated = parse_lane(&path)?;
    Ok(updated.len())
}

/// `content` split with line endings kept, the `splitlines(keepends=True)`
/// shape the mutation edits.
fn split_keepends(content: &str) -> Vec<String> {
    let mut lines = Vec::new();
    let mut current = String::new();
    for ch in content.chars() {
        current.push(ch);
        if ch == '\n' || ch == '\r' {
            lines.push(std::mem::take(&mut current));
        }
    }
    if !current.is_empty() {
        lines.push(current);
    }
    lines
}

/// `\r\n`, `\n`, `\r`, or nothing: the ending the raw line carried.
fn line_ending(raw: &str) -> &str {
    if raw.ends_with("\r\n") {
        "\r\n"
    } else if raw.ends_with('\n') {
        "\n"
    } else if raw.ends_with('\r') {
        "\r"
    } else {
        ""
    }
}

/// Write through a same-directory temp file and rename: an interrupted
/// writer leaves the old lane, never a truncated one.
fn atomic_replace(path: &std::path::Path, content: &str) -> Result<(), String> {
    let dir = path.parent().unwrap_or_else(|| std::path::Path::new("."));
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "lane".to_string());
    let tmp = dir.join(format!(".{name}.{}.tmp", std::process::id()));
    std::fs::write(&tmp, content)
        .map_err(|e| format!("cannot write operator lane {}: {e}", path.display()))?;
    match std::fs::rename(&tmp, path) {
        Ok(()) => Ok(()),
        Err(e) => {
            let _ = std::fs::remove_file(&tmp);
            Err(format!(
                "cannot write operator lane {}: {e}",
                path.display()
            ))
        }
    }
}

/// The lane read for the list arms: missing file reads as empty; an
/// unreadable one fails with the parse error.
fn mine_list_at(path: &std::path::Path, as_json: bool) -> i32 {
    let items = match parse_lane(path) {
        Ok(items) => items,
        Err(e) => {
            eprintln!("mine: failed: {e}");
            return 1;
        }
    };
    if as_json {
        let rows: Vec<Value> = items
            .iter()
            .enumerate()
            .map(|(i, item)| {
                json!({
                    "n": i + 1,
                    "text": item.text,
                    "done": item.done,
                    "node": item.node,
                })
            })
            .collect();
        println!(
            "{}",
            serde_json::to_string(&json!({ "mine": rows })).unwrap_or_default()
        );
        return 0;
    }
    for (i, item) in items.iter().enumerate() {
        let mark = if item.done { "x" } else { " " };
        let node = item
            .node
            .as_deref()
            .map(|n| format!(" -> {n}"))
            .unwrap_or_default();
        println!("{}. [{}] {}{}", i + 1, mark, item.text, node);
    }
    0
}

#[cfg(test)]
#[path = "outstanding_cmds_tests.rs"]
mod tests;
