//! The bus mail log reader the dead-letter sweep reads (the envelope and
//! cursor shapes of fno.bus.log / fno.bus.cursor).

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use serde_json::Value;

use super::super::state_dir;

pub(crate) fn bus_dir() -> PathBuf {
    state_dir().join("bus")
}

fn bus_log_path() -> PathBuf {
    bus_dir().join("messages.jsonl")
}

fn segments_oldest_first() -> Vec<PathBuf> {
    let live = bus_log_path();
    let parent = live.parent().unwrap_or(Path::new("."));
    let stem = live
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let mut segments: Vec<PathBuf> = Vec::new();
    if let Ok(rd) = std::fs::read_dir(parent) {
        for entry in rd.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            let seg = match name.strip_prefix(&stem) {
                Some("") => true,
                Some(digits) => !digits.is_empty() && digits.chars().all(|c| c.is_ascii_digit()),
                None => false,
            };
            if seg {
                segments.push(entry.path());
            }
        }
    }
    let key = |p: &PathBuf| -> u64 {
        p.extension()
            .and_then(|e| e.to_str())
            .and_then(|e| e.parse::<u64>().ok())
            .unwrap_or(u64::MAX)
    };
    segments.sort_by_key(|p| key(p));
    segments
}

fn envelopes() -> Vec<Value> {
    let mut rows: Vec<Value> = Vec::new();
    for seg in segments_oldest_first() {
        let Ok(text) = std::fs::read_to_string(seg) else {
            continue;
        };
        for line in text.lines() {
            if line.trim().is_empty() {
                continue;
            }
            if let Ok(v) = serde_json::from_str::<Value>(line) {
                if v.is_object() {
                    rows.push(v);
                }
            }
        }
    }
    rows
}

pub(crate) fn drain_msg_ids() -> BTreeSet<String> {
    let mut ids: BTreeSet<String> = BTreeSet::new();
    for row in query_drained_rows() {
        let mut mid = row
            .get("msg_id")
            .and_then(Value::as_str)
            .map(str::to_string);
        if mid.is_none() {
            mid = row
                .get("data")
                .and_then(|d| d.get("msg_id"))
                .and_then(Value::as_str)
                .map(str::to_string);
        }
        if let Some(mid) = mid {
            ids.insert(mid);
        }
    }
    ids
}

/// The `agent_mail_drained` receipts, read raw from the project journal
/// (the store commit is the write boundary; a torn store reads empty).
fn query_drained_rows() -> Vec<Value> {
    let journal = super::super::project_events_path();
    let Ok(text) = std::fs::read_to_string(journal) else {
        return Vec::new();
    };
    let mut rows: Vec<Value> = Vec::new();
    for line in text.lines() {
        if line.trim().is_empty() {
            continue;
        }
        if let Ok(v) = serde_json::from_str::<Value>(line) {
            if v.get("type").and_then(Value::as_str) == Some("agent_mail_drained") {
                rows.push(v);
            }
        }
    }
    rows
}

pub(crate) fn envelopes_pub() -> Vec<Value> {
    envelopes()
}

/// The unread mail to one handle, after its drain cursor (the
/// `scan_unread` fallback: a cursor id rotated out of the retained
/// segments reads every retained message to the handle as unread).
pub(crate) fn scan_unread(handle: &str) -> Vec<Value> {
    let cursor = read_bus_cursor(handle);
    let mut unread: Vec<Value> = Vec::new();
    let mut seen: BTreeSet<String> = BTreeSet::new();
    let mut cursor_seen = false;
    for m in envelopes() {
        let id = m
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        if id.is_empty() {
            continue;
        }
        if m.get("to").and_then(Value::as_str) != Some(handle) {
            continue;
        }
        if Some(id.as_str()) == cursor.as_deref() {
            cursor_seen = true;
            continue;
        }
        if cursor.is_some() && !cursor_seen {
            continue;
        }
        if !seen.insert(id) {
            continue;
        }
        unread.push(m);
    }
    let rotated_out = cursor.is_some() && !cursor_seen;
    if rotated_out {
        let keep = |m: &Value| m.get("to").and_then(Value::as_str) == Some(handle);
        unread = envelopes().into_iter().filter(keep).collect();
    }
    unread
}

fn read_bus_cursor(handle: &str) -> Option<String> {
    let path = bus_dir().join("cursors").join(format!("{handle}.json"));
    let map = super::super::read_json_object(&path)?;
    map.get("last_seen_id")
        .and_then(Value::as_str)
        .map(str::to_string)
}
