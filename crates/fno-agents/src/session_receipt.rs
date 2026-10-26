//! One session's receipt: who woke its last turn, what that turn did, and
//! which tool calls were refused. The reader slices the transcript tail at
//! the last user-shaped row and folds what follows with the same pieces the
//! intel fold uses: `classify_turn`, `wake_class`, `ActivityFold::row` and
//! `model_price::cost`. A store it cannot read answers `unmeasured: <why>`,
//! never zeros.

use crate::provenance::{
    classify_turn, is_meta_row, is_user_turn, turn_stamp, turn_text, turn_ts_epoch, BusIndex,
    Provenance, TranscriptSource,
};
use crate::session_activity::{
    block_text, codex_output_is_error, codex_output_text, codex_token_total, content_blocks,
    error_class, ActivityFold, Tokens,
};
use serde_json::{json, Map, Value};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// The most transcript a receipt reads, from the end of the file. A turn that
/// starts before the cut reports `unmeasured: turn exceeds the tail`.
pub(crate) const RECEIPT_TAIL_BYTES: u64 = 2 * 1024 * 1024;
/// The trailing tool-result window the denial counts read.
const TRAILING_WINDOW: usize = 50;
/// How many turn labels `recent_turns` keeps: the wake-storm signal.
const RECENT_TURNS: usize = 5;
/// The three refusal classes a receipt counts.
const DENIAL_CLASSES: [&str; 3] = ["permission_denied", "user_rejected", "hook_blocked"];

enum Store {
    /// A transcript file the harness writes in its own row shape.
    File(PathBuf),
    /// A store rendered claude-shaped (pi, opencode, agy): turns and call
    /// counts only, no tool inputs or results.
    Rendered(String),
}

fn rendered(src: &dyn TranscriptSource, session_id: &str) -> Result<Store, String> {
    let file = src
        .sessions(30)
        .into_iter()
        .find(|f| f.session_id == session_id)
        .ok_or_else(|| format!("no {} session {session_id} in its store", src.harness()))?;
    Ok(Store::Rendered(src.read(&file)))
}

fn locate(harness: &str, session_id: &str) -> Result<Store, String> {
    match harness {
        "claude" => crate::claude_drive::find_transcript(session_id)
            .map(Store::File)
            .ok_or_else(|| format!("no claude transcript for session {session_id}")),
        "codex" => crate::lead_history::hygiene_transcript_for_holder("codex", session_id)
            .map(Store::File)
            .ok_or_else(|| format!("no codex rollout for session {session_id}")),
        "pi" => {
            let src = crate::pi_transcript::PiSource {
                sessions_dir: None,
                roots: None,
            };
            src.probe()?;
            rendered(&src, session_id)
        }
        "opencode" => rendered(
            &crate::opencode_transcript::OpencodeSource {
                dbs: crate::opencode_transcript::opencode_stores(),
                roots: None,
            },
            session_id,
        ),
        "agy" => {
            let (summaries_db, history, conversations_dir) = crate::agy_transcript::agy_stores();
            let src = crate::agy_transcript::AgySource {
                summaries_db,
                history,
                conversations_dir,
                roots: None,
            };
            src.probe()?;
            rendered(&src, session_id)
        }
        other => Err(format!("no transcript store reader for harness {other}")),
    }
}

/// The last `max` bytes of `path`, and whether the read started mid-file.
/// A mid-file read drops its first line, which the cut may have split.
fn tail(path: &Path, max: u64) -> Result<(String, bool), String> {
    use std::io::{Read, Seek, SeekFrom};
    let mut f = std::fs::File::open(path).map_err(|e| format!("transcript unreadable: {e}"))?;
    let len = f
        .metadata()
        .map_err(|e| format!("transcript unreadable: {e}"))?
        .len();
    let start = len.saturating_sub(max);
    f.seek(SeekFrom::Start(start))
        .map_err(|e| format!("transcript unreadable: {e}"))?;
    let mut buf = Vec::new();
    f.read_to_end(&mut buf)
        .map_err(|e| format!("transcript unreadable: {e}"))?;
    let mut text = String::from_utf8_lossy(&buf).into_owned();
    if start > 0 {
        match text.find('\n') {
            Some(i) => {
                text.drain(..=i);
            }
            None => text.clear(),
        }
    }
    Ok((text, start > 0))
}

fn now_epoch() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

fn unmeasured(why: &str) -> Value {
    json!({ "basis": format!("unmeasured: {why}") })
}

/// The receipt for one session of one harness, read from its store now.
pub(crate) fn receipt(harness: &str, session_id: &str) -> Value {
    let store = match locate(harness, session_id) {
        Ok(store) => store,
        Err(why) => return unmeasured(&why),
    };
    let book = crate::model_price::price_book(&crate::model_price::state_dir());
    match store {
        Store::File(path) => match tail(&path, RECEIPT_TAIL_BYTES) {
            Ok((text, truncated)) => from_text(
                harness,
                session_id,
                &text,
                truncated,
                now_epoch(),
                book.as_deref(),
            ),
            Err(why) => unmeasured(&why),
        },
        Store::Rendered(text) => from_text(harness, session_id, &text, false, now_epoch(), None),
    }
}

/// The short word for who woke a turn, as `fno agents top` prints it.
pub(crate) fn wake_word(provenance: &str) -> &str {
    match provenance {
        p if p.starts_with("relay_") => "relay",
        "harness_stop_hook" => "hook",
        "harness_loop_wakeup" => "loop",
        p if p.starts_with("harness_") => "harness",
        p => p,
    }
}

fn woke_by(p: Provenance) -> &'static str {
    match crate::wake_meter::wake_class(p) {
        Some(true) => "machine",
        Some(false) => "person",
        None => "harness",
    }
}

fn is_sidechain(row: &Value) -> bool {
    crate::provenance::json_truthy(row.get("isSidechain"))
}

fn is_turn(harness: &str, row: &Value) -> bool {
    let shaped = if harness == "codex" {
        is_user_turn(row)
    } else {
        is_user_turn(row) || is_meta_row(row)
    };
    shaped && !turn_text(row).trim().is_empty()
}

/// A row that closes a turn: claude's final assistant stop or its
/// `turn_duration` system row, codex's `task_complete` event.
fn ends_turn(row: &Value) -> bool {
    let stop = row.pointer("/message/stop_reason").and_then(Value::as_str);
    matches!(stop, Some("end_turn" | "stop_sequence"))
        || row.get("subtype").and_then(Value::as_str) == Some("turn_duration")
        || row.pointer("/payload/type").and_then(Value::as_str) == Some("task_complete")
}

/// Tool calls and the distinct files edited, in both row shapes.
fn calls_and_files(rows: &[Value]) -> (u64, BTreeSet<String>) {
    let mut calls = 0u64;
    let mut files = BTreeSet::new();
    for row in rows {
        if let Some(p) = row.get("payload").filter(|p| p.is_object()) {
            let ptype = p.get("type").and_then(Value::as_str);
            if matches!(ptype, Some("function_call" | "custom_tool_call")) {
                calls += 1;
                let input = p
                    .get("input")
                    .or_else(|| p.get("arguments"))
                    .and_then(Value::as_str)
                    .unwrap_or("");
                for line in input.lines() {
                    let t = line.trim_start();
                    if let Some(path) = t
                        .strip_prefix("*** Add File:")
                        .or_else(|| t.strip_prefix("*** Update File:"))
                        .or_else(|| t.strip_prefix("*** Delete File:"))
                    {
                        files.insert(path.trim().to_string());
                    }
                }
            }
            continue;
        }
        for block in content_blocks(row) {
            if block.get("type").and_then(Value::as_str) != Some("tool_use") {
                continue;
            }
            calls += 1;
            let name = block.get("name").and_then(Value::as_str).unwrap_or("");
            if matches!(name, "Edit" | "MultiEdit" | "Write" | "NotebookEdit") {
                let input = block.get("input");
                let path = ["file_path", "notebook_path"]
                    .into_iter()
                    .find_map(|k| input.and_then(|i| i.get(k)).and_then(Value::as_str))
                    .filter(|p| !p.is_empty());
                if let Some(path) = path {
                    files.insert(path.to_string());
                }
            }
        }
    }
    (calls, files)
}

/// One entry per tool result in row order: the refusal or error class, or
/// `None` for a result that succeeded.
fn result_classes(rows: &[Value]) -> Vec<Option<&'static str>> {
    let mut out = Vec::new();
    for row in rows {
        if let Some(p) = row.get("payload").filter(|p| p.is_object()) {
            if matches!(
                p.get("type").and_then(Value::as_str),
                Some("function_call_output" | "custom_tool_call_output")
            ) {
                let failed = codex_output_is_error(&codex_output_text(p));
                out.push(failed.then_some("command_failed"));
            }
            continue;
        }
        for block in content_blocks(row) {
            if block.get("type").and_then(Value::as_str) != Some("tool_result") {
                continue;
            }
            let is_error = block.get("is_error").and_then(Value::as_bool) == Some(true);
            out.push(
                is_error.then(|| {
                    error_class(&block_text(block.get("content").unwrap_or(&Value::Null)))
                }),
            );
        }
    }
    out
}

fn denial_counts<'a>(classes: impl Iterator<Item = &'a str>) -> Map<String, Value> {
    let mut counts: Map<String, Value> = DENIAL_CLASSES
        .iter()
        .map(|c| (c.to_string(), json!(0)))
        .collect();
    for class in classes {
        if let Some(n) = counts.get_mut(class) {
            *n = json!(n.as_u64().unwrap_or(0) + 1);
        }
    }
    counts
}

/// The receipt over one transcript's text. `truncated` says the text is a
/// tail that starts mid-file; `book` prices the last turn's tokens.
pub(crate) fn from_text(
    harness: &str,
    session_id: &str,
    text: &str,
    truncated: bool,
    now: f64,
    book: Option<&crate::model_price::PriceBook>,
) -> Value {
    let rows: Vec<Value> = text
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .filter(|r| !is_sidechain(r) && !crate::session_activity::is_request_echo(r))
        .collect();
    let full = matches!(harness, "claude" | "codex");
    let bounds: Vec<usize> = (0..rows.len())
        .filter(|&i| is_turn(harness, &rows[i]))
        .collect();
    let bus = BusIndex::empty();
    let label = |i: usize| classify_turn(&rows[i], &bus, session_id);
    let recent: Vec<Value> = bounds[bounds.len().saturating_sub(RECENT_TURNS)..]
        .iter()
        .map(|&i| json!({"at": turn_stamp(&rows[i]), "provenance": label(i).label()}))
        .collect();

    let last_turn = match bounds.last() {
        None if truncated => json!({"unmeasured": "turn exceeds the 2 MiB tail"}),
        None => json!({"unmeasured": "no user-shaped turn in the transcript"}),
        Some(&b) => {
            let prov = label(b);
            let turn = &rows[b + 1..];
            let start = turn_ts_epoch(&rows[b]);
            let end = turn
                .iter()
                .filter_map(turn_ts_epoch)
                .fold(start, |acc, ts| Some(acc.map_or(ts, |a: f64| a.max(ts))));
            let (calls, files) = calls_and_files(turn);
            let mut out = json!({
                "started_at": turn_stamp(&rows[b]),
                "age_s": start.map(|s| (now - s).max(0.0).round() as u64),
                "provenance": prov.label(),
                "woke_by": woke_by(prov),
                "duration_s": start.zip(end).map(|(s, e)| (e - s).max(0.0).round() as u64),
                "tool_calls": (harness != "agy").then_some(calls),
            });
            if full {
                let mut fold = ActivityFold::default();
                for row in turn {
                    fold.row(row);
                }
                let saw_tokens = fold.saw_tokens();
                let act = fold.finish();
                // A codex token_count row is a running total; the turn's
                // spend is the delta from the last total before it.
                let tokens = if harness == "codex" {
                    let base = rows[..=b]
                        .iter()
                        .rev()
                        .find_map(codex_token_total)
                        .unwrap_or_default();
                    if saw_tokens {
                        act.tokens.since(base)
                    } else {
                        Tokens::default()
                    }
                } else {
                    act.tokens
                };
                let rates = match (book, act.model.as_deref()) {
                    (None, _) => Err("unmeasured: no models-dev price catalog".to_string()),
                    (_, None) => Err("unmeasured: the turn names no model".to_string()),
                    (Some(book), Some(m)) => book
                        .rates(m, None)
                        .ok_or_else(|| format!("unmeasured: no rate for {m}")),
                };
                let (cost_usd, cost_basis) = match rates {
                    Ok(r) => match crate::model_price::cost(tokens, &r) {
                        Some(c) => (Some((c * 100.0).round() / 100.0), "models-dev".to_string()),
                        None => (None, "unmeasured: a token kind has no rate".to_string()),
                    },
                    Err(why) => (None, why),
                };
                out["edits"] = json!({
                    "files": files.len(),
                    "lines_added": act.lines_added,
                    "lines_removed": act.lines_removed,
                });
                out["errors"] = json!(act.tool_errors);
                out["tokens"] = json!(tokens);
                out["cost_usd"] = json!(cost_usd);
                out["cost_basis"] = json!(cost_basis);
                out["in_progress"] = json!(!turn.iter().any(ends_turn));
            } else {
                out["activity"] = json!({
                    "unmeasured": format!("the {harness} render carries no tool inputs or results")
                });
            }
            out
        }
    };

    let denied = if full {
        let all = result_classes(&rows);
        let trailing = &all[all.len().saturating_sub(TRAILING_WINDOW)..];
        let mut trailing_counts = denial_counts(trailing.iter().flatten().copied());
        trailing_counts.insert("window".into(), json!(trailing.len()));
        let last = match bounds.last() {
            Some(&b) => json!(denial_counts(
                result_classes(&rows[b + 1..]).into_iter().flatten()
            )),
            None => last_turn.clone(),
        };
        json!({"last_turn": last, "trailing": trailing_counts})
    } else {
        json!({"unmeasured": format!("the {harness} render carries no tool results")})
    };

    json!({
        "session_id": session_id,
        "harness": harness,
        "basis": if full { "transcript" } else { "render" },
        "last_turn": last_turn,
        "recent_turns": recent,
        "denied": denied,
    })
}

fn n(v: &Value) -> u64 {
    v.as_u64().unwrap_or(0)
}

/// The two text lines `fno whoami` appends.
pub(crate) fn render_lines(r: &Value) -> Vec<String> {
    if let Some(basis) = r["basis"].as_str().filter(|b| b.starts_with("unmeasured")) {
        let why = basis.trim_start_matches("unmeasured: ");
        return vec![format!("turns:    unmeasured ({why})")];
    }
    let lt = &r["last_turn"];
    let turns = match lt["unmeasured"].as_str() {
        Some(why) => format!("turns:    last turn unmeasured ({why})"),
        None => {
            let age = lt["age_s"]
                .as_i64()
                .map(|a| format!("{} ago", crate::claims::fmt_age_s(a)))
                .unwrap_or_else(|| "at an unstamped time".into());
            let live = if lt["in_progress"] == json!(true) {
                " (in progress)"
            } else {
                ""
            };
            let mut line = format!(
                "turns:    last turn {age}{live}, woke by {} ({})",
                lt["provenance"].as_str().unwrap_or("unknown"),
                lt["woke_by"].as_str().unwrap_or("unknown"),
            );
            let mut parts: Vec<String> = Vec::new();
            if let Some(calls) = lt["tool_calls"].as_u64() {
                parts.push(format!("{calls} calls"));
            }
            if lt["edits"].is_object() {
                let e = &lt["edits"];
                parts.push(format!(
                    "{} files +{}/-{}",
                    n(&e["files"]),
                    n(&e["lines_added"]),
                    n(&e["lines_removed"])
                ));
                let errors: u64 = lt["errors"]
                    .as_object()
                    .map(|m| m.values().map(n).sum())
                    .unwrap_or(0);
                parts.push(format!(
                    "{errors} error{}",
                    if errors == 1 { "" } else { "s" }
                ));
                parts.push(match lt["cost_usd"].as_f64() {
                    Some(c) => format!("${c:.2}"),
                    None => "cost unpriced".into(),
                });
            }
            if !parts.is_empty() {
                line.push_str(&format!(": {}", parts.join(", ")));
            }
            line
        }
    };
    let d = &r["denied"];
    let denied = match d["unmeasured"].as_str() {
        Some(why) => format!("denied:   unmeasured ({why})"),
        None => {
            let last: u64 = d["last_turn"]
                .as_object()
                .map(|m| m.values().map(n).sum())
                .unwrap_or(0);
            let t = &d["trailing"];
            format!(
                "denied:   last turn {last} | last {} calls: {} classifier, {} user, {} hook",
                n(&t["window"]),
                n(&t["permission_denied"]),
                n(&t["user_rejected"]),
                n(&t["hook_blocked"]),
            )
        }
    };
    vec![turns, denied]
}

/// `fno-agents intel --receipt [--session <id>] [--harness <h>] [--json]`.
/// With no `--session`, the ambient session reads itself. Exit 0 on every
/// reading, unmeasured included; 2 on a bad flag.
pub(crate) fn run(args: &[String]) -> i32 {
    let mut session: Option<String> = None;
    let mut harness: Option<String> = None;
    let mut json_out = false;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--receipt" => {}
            "--json" | "-J" => json_out = true,
            flag @ ("--session" | "-H" | "--harness") => {
                i += 1;
                let Some(v) = args.get(i).filter(|v| !v.is_empty()) else {
                    eprintln!("fno-agents intel --receipt: {flag} needs a value");
                    return 2;
                };
                if flag == "--session" {
                    session = Some(v.clone());
                } else {
                    harness = Some(v.clone());
                }
            }
            other => {
                eprintln!("fno-agents intel --receipt: unknown flag {other}");
                return 2;
            }
        }
        i += 1;
    }
    let (ambient_sid, ambient_harness) = crate::claims::resolve_identity();
    let harness = harness
        .or(ambient_harness)
        .unwrap_or_else(|| "claude".into());
    let reading = match session.or(ambient_sid) {
        Some(sid) => receipt(&harness, &sid),
        None => unmeasured("no session id resolved from the ambient environment"),
    };
    if json_out {
        println!("{reading}");
    } else {
        for line in render_lines(&reading) {
            println!("{line}");
        }
    }
    0
}

/// The receipt fields `fno agents top` puts on a worker row: `denied` and
/// `last_turn`, each an `{"unmeasured": why}` object when the read failed.
pub(crate) fn top_fields(reading: &Value) -> (Value, Value) {
    if let Some(basis) = reading["basis"]
        .as_str()
        .filter(|b| b.starts_with("unmeasured"))
    {
        let why = json!({"unmeasured": basis.trim_start_matches("unmeasured: ")});
        return (why.clone(), why);
    }
    (reading["denied"].clone(), reading["last_turn"].clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(rows: &[Value]) -> String {
        rows.iter().map(|r| format!("{r}\n")).collect()
    }

    #[test]
    fn a_relay_turn_counts_its_edits_and_its_classifier_denial() {
        // AC3-HP
        let edit = |id: &str, path: &str| {
            json!({"type": "assistant", "timestamp": "2026-10-25T10:01:00Z",
                   "message": {"role": "assistant", "id": id, "content": [
                       {"type": "tool_use", "id": id, "name": "Edit",
                        "input": {"file_path": path, "old_string": "a", "new_string": "b\nc"}}]}})
        };
        let rows = [
            json!({"type": "user", "timestamp": "2026-10-25T09:00:00Z",
                   "message": {"role": "user", "content": "hello"}}),
            json!({"type": "assistant", "timestamp": "2026-10-25T09:00:05Z",
                   "message": {"role": "assistant", "stop_reason": "end_turn", "content": []}}),
            json!({"type": "user", "timestamp": "2026-10-25T10:00:00Z",
                   "message": {"role": "user", "content": "<fno_mail from=\"henry\">go</fno_mail>"}}),
            edit("m1", "a.rs"),
            edit("m2", "b.rs"),
            json!({"type": "assistant", "timestamp": "2026-10-25T10:02:00Z",
                   "message": {"role": "assistant", "id": "m3", "content": [
                       {"type": "tool_use", "id": "t3", "name": "Bash", "input": {"command": "gh pr create"}}]}}),
            json!({"type": "user", "timestamp": "2026-10-25T10:02:01Z",
                   "message": {"role": "user", "content": [
                       {"type": "tool_result", "tool_use_id": "t3", "is_error": true,
                        "content": "Permission for this action was denied by the Claude Code auto mode classifier. Reason: [External System Writes]."}]}}),
        ];
        let now = turn_ts_epoch(&rows[2]).unwrap() + 240.0;
        let r = from_text("claude", "s1", &lines(&rows), false, now, None);
        let lt = &r["last_turn"];
        assert_eq!(lt["provenance"], "relay_fno_mail", "{r}");
        assert_eq!(lt["woke_by"], "machine");
        assert_eq!(lt["edits"]["files"], 2);
        assert_eq!(lt["tool_calls"], 3);
        assert_eq!(lt["age_s"], 240);
        assert_eq!(lt["in_progress"], true);
        assert_eq!(r["denied"]["last_turn"]["permission_denied"], 1);
        assert_eq!(r["denied"]["trailing"]["window"], 1);
        assert_eq!(r["recent_turns"].as_array().unwrap().len(), 2);
        let text = render_lines(&r).join("\n");
        assert!(
            text.contains("woke by relay_fno_mail (machine): 3 calls, 2 files"),
            "{text}"
        );
        assert!(text.contains("1 classifier, 0 user, 0 hook"), "{text}");
    }

    #[test]
    fn a_harness_with_no_store_reads_unmeasured() {
        // AC4-ERR: the receipt never invents zeros for a store it cannot read.
        let r = receipt("kiro", "s1");
        assert!(
            r["basis"].as_str().unwrap().starts_with("unmeasured:"),
            "{r}"
        );
        assert_eq!(r.as_object().unwrap().len(), 1);
        let (denied, last) = top_fields(&r);
        assert!(denied["unmeasured"].is_string());
        assert!(last["unmeasured"].is_string());
        assert_eq!(
            render_lines(&r),
            vec!["turns:    unmeasured (no transcript store reader for harness kiro)"]
        );
    }
}
