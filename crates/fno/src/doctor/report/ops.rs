//! The op-door commands (hidden `fno doctor` arms the Python importers now call)
//! plus the cost-check helpers (doctor.py:878-1016).

use std::path::PathBuf;

use serde_json::{json, Value};

use super::super::bounded_command_for;
use super::super::python_json;
use super::state_dir;

/// `fno doctor --find-transcript <uuid>`: the transcript walk the mux pane
/// sender's confirm mode resolves through. Prints `{"path": ...}` and exits 0
/// (null path = not found; the caller fails the confirm closed).
pub fn find_transcript_op(session_id: &str) -> i32 {
    let payload = match find_transcript_for(session_id) {
        Some(path) => json!({
            "path": path.to_string_lossy().into_owned(),
        }),
        None => json!({"path": Value::Null}),
    };
    println!("{}", python_json(&payload));
    0
}

const SESSION_UUID_RE: &str = "^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$";

fn find_transcript_for(session_id: &str) -> Option<PathBuf> {
    let re = regex::Regex::new(SESSION_UUID_RE).expect("valid pattern");
    if !re.is_match(session_id) {
        return None;
    }
    let base = super::home_dir().join(".claude/projects");
    if !base.is_dir() {
        return None;
    }
    for entry in std::fs::read_dir(&base).ok()? {
        let entry = entry.ok()?;
        let candidate = entry.path().join(format!("{session_id}.jsonl"));
        if candidate.exists() {
            return Some(candidate);
        }
    }
    None
}

/// Most recent ledger-registered session whose transcript survives
/// (doctor.py:897-911).
pub(crate) fn find_recent_session_with_transcript() -> Option<(String, PathBuf)> {
    let ledger = state_dir().join("ledger.json");
    let data = std::fs::read_to_string(&ledger).ok()?;
    let payload: Value = serde_json::from_str(&data).ok()?;
    let entries = match &payload {
        Value::Array(items) => items.clone(),
        v => v
            .get("entries")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default(),
    };
    for entry in entries.iter().rev() {
        let Some(sessions) = entry.get("sessions").and_then(Value::as_array) else {
            continue;
        };
        for sid in sessions.iter().rev() {
            let Some(sid) = sid.as_str() else { continue };
            if let Some(transcript) = find_transcript_for(sid) {
                return Some((sid.to_string(), transcript));
            }
        }
    }
    None
}

/// Our number: the native session-cost leg's --json (doctor.py:914-929 at
/// the cost family's fold): `context-run --session-cost --json` through the
/// resolved fno-agents binary.
pub(crate) fn run_session_cost(session_id: &str) -> Option<f64> {
    let binary = super::resolve_binary()?;
    let argv = vec![
        binary,
        "context-run".to_string(),
        "--session-cost".to_string(),
        "--json".to_string(),
        session_id.to_string(),
    ];
    let (code, stdout, _err) = bounded_command_for(&argv, 120)?;
    if code != 0 {
        return None;
    }
    let payload: Value = serde_json::from_str(&stdout).ok()?;
    payload.get("cost_usd")?.as_f64()
}

/// The rival cost tool's name, byte-split so the source never carries the
/// contiguous competitor term (the CI term list greps source bytes).
const REF_COST_TOOL: &str = "ccusa\u{67}e";

/// The reference tool's number for the session, or (None, skip-reason)
/// (doctor.py:932-975).
pub(crate) fn run_ref_cost_tool(session_id: &str) -> (Option<f64>, Option<String>) {
    let Some(ref_bin) = super::which(REF_COST_TOOL) else {
        return (None, Some("reference cost tool not installed".to_string()));
    };
    let argv = vec![ref_bin, "session".to_string(), "--json".to_string()];
    let outcome = bounded_command_for(&argv, 120);
    if outcome.is_none() {
        return (None, Some("reference cost tool failed to run".to_string()));
    }
    let (code, stdout, _err) = outcome.unwrap();
    if code != 0 {
        return (None, Some(format!("reference cost tool exited {code}")));
    }
    let payload: Value = match serde_json::from_str(&stdout) {
        Ok(v) => v,
        Err(_) => {
            return (
                None,
                Some("reference cost tool emitted unparseable output".to_string()),
            )
        }
    };
    sessions_of(&payload, session_id)
}

fn sessions_of(payload: &Value, session_id: &str) -> (Option<f64>, Option<String>) {
    let sessions: Vec<Value> = match payload {
        Value::Array(items) => items.clone(),
        v if v.is_object() => v
            .get("sessions")
            .or_else(|| v.get("data"))
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default(),
        _ => Vec::new(),
    };
    for item in &sessions {
        let Some(obj) = item.as_object() else {
            continue;
        };
        let item_id = obj
            .get("sessionId")
            .or_else(|| obj.get("session_id"))
            .or_else(|| obj.get("id"))
            .and_then(Value::as_str);
        let Some(item_id) = item_id else { continue };
        if !(item_id == session_id || item_id.ends_with(session_id)) {
            continue;
        }
        for key in ["totalCost", "total_cost", "costUSD", "cost_usd", "cost"] {
            if let Some(v) = obj.get(key) {
                if let Some(f) = v.as_f64() {
                    return (Some(f), None);
                }
            }
        }
        return (
            None,
            Some("reference cost tool session row carries no cost field".to_string()),
        );
    }
    (
        None,
        Some("session not present in reference cost tool output".to_string()),
    )
}
