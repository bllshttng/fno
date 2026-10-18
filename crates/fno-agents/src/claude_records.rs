//! A Claude Code transcript as footnote session records: the adapter that
//! proves the record is a protocol a second harness can speak, so fno reads
//! one format for claude and footnote. Field lists live in
//! `docs/architecture/footnote-transcript-schema.md`.
//!
//! Two layers. [`records`] maps rows to `{type, ts, data}` records with no
//! envelope; it is line-local, so intel's rollup can feed it one appended
//! line at a time. [`convert`] adds the envelope, a `header` and a closing
//! `terminal`, the whole-file shape `read_records` accepts.

use serde_json::{json, Value};
use std::collections::HashSet;
use std::path::Path;

/// The spec's cap on `tool_result.model_text`.
const MODEL_TEXT_MAX: usize = 30_000;

/// Row-to-record state: the turn counter and the API message ids already
/// seen. Claude writes one API message over several rows that repeat its
/// `message.id` and usage; the first row opens the turn and carries the
/// usage, so a sum over `usage` records counts each message once.
#[derive(Default)]
struct Mapper {
    turn: u64,
    seen: HashSet<String>,
}

fn record(ty: &str, ts: &Value, data: Value) -> Value {
    json!({"type": ty, "ts": ts, "data": data})
}

/// A row the adapter does not map, kept whole so nothing is lost.
fn ignorable(row: &Value, ts: &Value) -> Value {
    let ty = row.get("type").and_then(Value::as_str).unwrap_or("row");
    json!({"type": format!("claude_{ty}"), "ts": ts, "ignorable": true, "data": row})
}

fn capped(text: &str) -> &str {
    let mut end = text.len().min(MODEL_TEXT_MAX);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

impl Mapper {
    fn row(&mut self, row: &Value) -> Vec<Value> {
        let ts = row.get("timestamp").cloned().unwrap_or(Value::Null);
        let out = match row.get("type").and_then(Value::as_str) {
            Some("user") => user(row, &ts),
            Some("assistant") => self.assistant(row, &ts),
            _ => Vec::new(),
        };
        if out.is_empty() {
            return vec![ignorable(row, &ts)];
        }
        out
    }

    fn assistant(&mut self, row: &Value, ts: &Value) -> Vec<Value> {
        let msg = &row["message"];
        let id = msg.get("id").and_then(Value::as_str);
        let fresh = id.is_none_or(|id| self.seen.insert(id.to_string()));
        let model = msg
            .get("model")
            .and_then(Value::as_str)
            .filter(|m| !m.is_empty() && *m != "<synthetic>");
        let mut out = Vec::new();
        if fresh {
            self.turn += 1;
            out.push(record(
                "turn_context",
                ts,
                json!({"turn": self.turn, "model": model, "cwd": row.get("cwd")}),
            ));
        }
        out.push(record(
            "model_response",
            ts,
            json!({"turn": self.turn, "provider_response_id": id, "reported_model": model,
                "content": msg.get("content"), "stop_reason": msg.get("stop_reason")}),
        ));
        // Usage reads route through the one owned parser; it carries no
        // output tokens, so that one field is read here.
        let usage = fresh
            .then(|| crate::context_window::parse_usage_record(row))
            .flatten();
        let output = msg.pointer("/usage/output_tokens").and_then(Value::as_u64);
        if let (Some(u), Some(output)) = (usage, output) {
            out.push(record(
                "usage",
                ts,
                json!({"turn": self.turn, "provider_response_id": id,
                    "input_tokens": u.input_tokens, "output_tokens": output,
                    "cache_read_tokens": u.cache_read_input_tokens,
                    "cache_write_tokens": u.cache_creation_input_tokens}),
            ));
        }
        for block in msg
            .get("content")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter(|b| b["type"] == "tool_use")
        {
            out.push(record(
                "tool_call",
                ts,
                json!({"turn": self.turn, "tool_call_uid": block.get("id"),
                    "provider_tool_call_id": block.get("id"), "name": block.get("name"),
                    "input": block.get("input")}),
            ));
        }
        out
    }
}

/// Typed text is origin `operator`; a meta row (hook output, skill body,
/// stop-hook feedback) is `hook`; a native peer row is `mail`. Tool results
/// ride user rows too and land as `tool_result`.
fn user(row: &Value, ts: &Value) -> Vec<Value> {
    let origin = if crate::provenance::json_truthy(row.get("isMeta")) {
        "hook"
    } else if row.pointer("/origin/kind").and_then(Value::as_str) == Some("peer") {
        "mail"
    } else {
        "operator"
    };
    let mut texts: Vec<&str> = Vec::new();
    let mut results = Vec::new();
    match row.pointer("/message/content") {
        Some(Value::String(s)) => texts.push(s),
        Some(Value::Array(blocks)) => {
            for block in blocks {
                match block.get("type").and_then(Value::as_str) {
                    Some("tool_result") => results.push(tool_result(block, ts)),
                    _ => texts.extend(block.get("text").and_then(Value::as_str)),
                }
            }
        }
        _ => {}
    }
    let mut out = Vec::new();
    if !texts.is_empty() {
        out.push(record(
            "user_input",
            ts,
            json!({"text": texts.join(" "), "origin": origin}),
        ));
    }
    out.extend(results);
    out
}

fn tool_result(block: &Value, ts: &Value) -> Value {
    let text = match block.get("content") {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(parts)) => parts
            .iter()
            .filter_map(|p| p.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    };
    record(
        "tool_result",
        ts,
        json!({"tool_call_uid": block.get("tool_use_id"),
            "is_error": block.get("is_error").and_then(Value::as_bool).unwrap_or(false),
            "model_text": capped(&text), "size": text.len()}),
    )
}

/// The records of claude transcript lines, without the envelope. A line
/// that is not JSON (a write caught mid-line) is skipped.
pub(crate) fn records<'a>(lines: impl IntoIterator<Item = &'a str>) -> Vec<Value> {
    let mut mapper = Mapper::default();
    lines
        .into_iter()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .flat_map(|row| mapper.row(&row))
        .collect()
}

/// A whole claude transcript as a session record: a `header` naming
/// `session_id` and the first row's cwd, every row's records, and a
/// `terminal` with the totals. A transcript carries no price, so no record
/// claims a cost. Ids are fresh v4 UUIDs, never hashes, so no randomness
/// means no record.
pub fn convert(raw: &str, session_id: &str) -> Result<Vec<Value>, String> {
    let rows: Vec<Value> = raw
        .lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect();
    let first = |k: &str| rows.iter().find_map(|r| r.get(k).and_then(Value::as_str));
    let first_ts = first("timestamp").map_or_else(
        || chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
        str::to_string,
    );
    let mut body = vec![record(
        "header",
        &json!(first_ts),
        json!({"session_id": session_id, "cwd": first("cwd"), "harness": "claude",
            "branch": first("gitBranch"), "fno_version": Value::Null}),
    )];
    let mut mapper = Mapper::default();
    body.extend(rows.iter().flat_map(|row| mapper.row(row)));
    let count = |ty: &str| body.iter().filter(|r| r["type"] == ty).count();
    let sum = |k: &str| -> u64 {
        body.iter()
            .filter(|r| r["type"] == "usage")
            .filter_map(|r| r["data"][k].as_u64())
            .sum()
    };
    // Claude often writes no stop_reason, so a call left without its
    // result is the one interrupt the transcript can show.
    let answered: HashSet<&str> = body
        .iter()
        .filter(|r| r["type"] == "tool_result")
        .filter_map(|r| r["data"]["tool_call_uid"].as_str())
        .collect();
    let open = body
        .iter()
        .filter(|r| r["type"] == "tool_call")
        .filter(|r| {
            r["data"]["tool_call_uid"]
                .as_str()
                .is_none_or(|uid| !answered.contains(uid))
        })
        .count();
    let (state, reason) = match open {
        0 => ("done", "every tool call has its result".to_string()),
        n => ("interrupted", format!("{n} tool call(s) have no result")),
    };
    let terminal = json!({"state": state, "reason": reason, "turns": count("turn_context"),
        "tool_calls": count("tool_call"), "input_tokens": sum("input_tokens"),
        "output_tokens": sum("output_tokens")});
    let last_ts = body
        .iter()
        .rev()
        .find_map(|r| r["ts"].as_str())
        .unwrap_or(first_ts.as_str())
        .to_string();
    body.push(record("terminal", &json!(last_ts), terminal));
    // A row without a stamp (a summary, a snapshot) takes the one before it.
    let mut ts = first_ts;
    for (seq, rec) in body.iter_mut().enumerate() {
        match rec["ts"].as_str().map(str::to_string) {
            Some(t) => ts = t,
            None => rec["ts"] = json!(ts),
        }
        rec["v"] = json!(crate::footnote_transcript::SCHEMA_V);
        rec["seq"] = json!(seq);
        rec["id"] = json!(crate::identity::mint_fno_id()?);
        rec["session_id"] = json!(session_id);
    }
    Ok(body)
}

/// One transcript file converted. The file stem names the session: it is
/// the id resume and bus rows address, while a row's `sessionId` can carry
/// history copied in from another session.
pub fn convert_file(path: &Path) -> Result<Vec<Value>, String> {
    let raw = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let stem = path.file_stem().unwrap_or_default().to_string_lossy();
    convert(&raw, &stem)
}

/// `fno-agents intel --convert <path> --from claude`: the records, one JSON
/// line each, on stdout.
pub fn run_convert(args: &[String]) -> i32 {
    let value = |flag: &str| {
        args.iter()
            .position(|a| a == flag)
            .and_then(|i| args.get(i + 1))
    };
    let (Some(path), Some(from)) = (value("--convert"), value("--from")) else {
        eprintln!("usage: fno-agents intel --convert <transcript.jsonl> --from claude");
        return 2;
    };
    if from != "claude" {
        eprintln!("fno-agents intel --convert: no adapter for --from {from:?}; claude is the one");
        return 2;
    }
    match convert_file(Path::new(path)) {
        Ok(records) => {
            for rec in records {
                println!("{rec}");
            }
            0
        }
        Err(e) => {
            eprintln!("fno-agents intel --convert: {e}");
            1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::footnote_transcript::read_records;

    fn row(v: Value) -> String {
        v.to_string()
    }

    /// One prompt, two API messages (the second split over two rows that
    /// repeat its id and usage), three answered tool calls, a meta row, and
    /// one row type the adapter does not map. No row says end_turn.
    fn fixture() -> String {
        let ts = "2026-10-16T10:00:00.000Z";
        let assistant = |id: &str, blocks: Value, stop: &str| {
            json!({"type": "assistant", "timestamp": ts, "cwd": "/w", "sessionId": "s-claude",
                "message": {"id": id, "role": "assistant", "model": "claude-opus-5-5",
                    "content": blocks, "stop_reason": stop,
                    "usage": {"input_tokens": 10, "output_tokens": 4,
                        "cache_read_input_tokens": 3, "cache_creation_input_tokens": 1}}})
        };
        let tool = |id: &str| json!({"type": "tool_use", "id": id, "name": "Read", "input": {}});
        [
            row(json!({"type": "file-history-snapshot", "snapshot": {}})),
            row(
                json!({"type": "user", "timestamp": ts, "cwd": "/w", "sessionId": "s-claude",
                "gitBranch": "main", "message": {"role": "user", "content": "read two files"}}),
            ),
            row(assistant("m1", json!([tool("t1"), tool("t2")]), "tool_use")),
            row(json!({"type": "user", "timestamp": ts, "message": {"role": "user", "content": [
                {"type": "tool_result", "tool_use_id": "t1", "content": "one"},
                {"type": "tool_result", "tool_use_id": "t2", "content": "two", "is_error": true}]}})),
            row(json!({"type": "user", "timestamp": ts, "isMeta": true,
                "message": {"role": "user", "content": "Stop hook feedback: keep going"}})),
            row(assistant(
                "m2",
                json!([{"type": "text", "text": "and a third"}]),
                "tool_use",
            )),
            row(assistant("m2", json!([tool("t3")]), "tool_use")),
            row(json!({"type": "user", "timestamp": ts, "message": {"role": "user", "content": [
                {"type": "tool_result", "tool_use_id": "t3", "content": "three"}]}})),
            "{not json".to_string(),
        ]
        .join("\n")
    }

    /// AC3-HP and AC3-ERR: the converted record passes the reader with the
    /// fixture's turns, tool calls and deduped token totals; the unmapped
    /// row lands ignorable; the classifier reads the same turns it read off
    /// the raw rows.
    #[test]
    fn a_claude_transcript_converts_to_records_the_reader_accepts() {
        let records = convert(&fixture(), "s-claude").unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("s-claude.jsonl");
        let lines: Vec<String> = records.iter().map(Value::to_string).collect();
        std::fs::write(&path, lines.join("\n")).unwrap();
        let read = read_records(&path).expect("the reader accepts the conversion");
        let header = &read[0];
        assert_eq!(
            (header["type"].as_str(), header["data"]["harness"].as_str()),
            (Some("header"), Some("claude"))
        );
        assert_eq!(header["session_id"], "s-claude");
        assert!(read
            .iter()
            .enumerate()
            .all(|(n, r)| r["seq"] == n && r["v"] == 1));
        let end = &read.last().unwrap()["data"];
        assert_eq!(
            (end["state"].as_str(), end["turns"].as_u64()),
            (Some("done"), Some(2))
        );
        assert_eq!(
            (end["tool_calls"].as_u64(), end["input_tokens"].as_u64()),
            (Some(3), Some(20))
        );
        let snapshot = read
            .iter()
            .find(|r| r["type"] == "claude_file-history-snapshot")
            .unwrap();
        assert_eq!(snapshot["ignorable"], true);
        let results: Vec<_> = read.iter().filter(|r| r["type"] == "tool_result").collect();
        assert_eq!(results[1]["data"]["is_error"], true);

        let turns = crate::provenance::record_turns(records.clone());
        let raw_turns = crate::provenance::claude_shaped_turns(&fixture());
        let texts = |t: &[crate::provenance::Turn]| -> Vec<String> {
            t.iter()
                .filter(|t| !t.text.is_empty())
                .map(|t| t.text.clone())
                .collect()
        };
        assert_eq!(texts(turns.as_slice()), texts(raw_turns.as_slice()));
        let bus = crate::provenance::BusIndex::empty();
        let labels = |t: &[crate::provenance::Turn]| -> Vec<&'static str> {
            t.iter()
                .filter(|t| !t.text.is_empty())
                .map(|t| crate::provenance::classify_turn(&t.obj, &bus, "s").label())
                .collect()
        };
        assert_eq!(labels(turns.as_slice()), labels(raw_turns.as_slice()));
        assert_eq!(crate::provenance::record_tool_uses(records), 3);
    }
}
