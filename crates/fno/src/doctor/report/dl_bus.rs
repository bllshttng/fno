//! The stale-bus-letter sweep (doctor.py:3619-3693). The log is read by
//! `fno-agents chats bus-read`, the one reader, so the doctor honors
//! `FNO_BUS_DIR` and `config.paths.bus_dir` the way every mail command does.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::{json, Value};

use super::dl_hooks;

const AGE_HOURS: f64 = 24.0;

fn parse_bus_ts(ts: &str) -> Option<i64> {
    let parsed = chrono::NaiveDateTime::parse_from_str(ts, "%Y-%m-%dT%H:%M:%SZ");
    parsed.ok().map(|d| d.and_utc().timestamp())
}

fn stranded(m: &Value, cutoff: i64, ref_ts: i64) -> bool {
    let meta = m.get("meta").cloned().unwrap_or(Value::Null);
    let ttl_raw = meta.get("ttl_at").and_then(Value::as_str);
    if let Some(ttl_raw) = ttl_raw {
        let ttl = parse_bus_ts(ttl_raw);
        if ttl.is_some() {
            return ref_ts >= ttl.unwrap();
        }
    }
    let ts = parse_bus_ts(m.get("ts").and_then(Value::as_str).unwrap_or(""));
    match ts {
        Some(t) => t <= cutoff,
        None => false,
    }
}

fn handle_pattern() -> regex::Regex {
    let pattern = r"^(?:(?:claude|codex|agy|opencode|gemini|cursor|droid|zcode|pi|grok|kimi|glm)-)?[0-9a-fA-F]{6,}$";
    regex::Regex::new(pattern).expect("a valid handle pattern")
}

pub(crate) fn dead_letter_report() -> Value {
    let rows = envelopes_pub();
    let recipients = recipients(&rows);
    let mut unread = scan_unread_all(&recipients, &rows);
    let drained = drain_msg_ids();
    let now = chrono::Utc::now().timestamp();
    let cutoff = now - (AGE_HOURS * 3600.0) as i64;
    let mut findings: Vec<Value> = Vec::new();
    for handle in recipients {
        for m in unread.remove(&handle).unwrap_or_default() {
            let id = m.get("id").and_then(Value::as_str).unwrap_or("");
            let already = drained.contains(id);
            if already {
                continue;
            }
            if stranded(&m, cutoff, now) {
                findings.push(one_finding(&m, &handle, id));
            }
        }
    }
    json!({
        "drain_hook_wired": dl_hooks::drain_hook_wired().map(Value::Bool).unwrap_or(Value::Null),
        "stale_unread": findings,
    })
}

fn one_finding(m: &Value, handle: &str, id: &str) -> Value {
    let ts = m.get("ts").cloned().unwrap_or(Value::Null);
    let owner = m
        .get("meta")
        .and_then(|x| x.get("owner"))
        .cloned()
        .unwrap_or(Value::Null);
    json!({
        "handle": handle,
        "msg_id": id,
        "ts": ts,
        "owner": owner,
    })
}

fn recipients(rows: &[Value]) -> Vec<String> {
    let re = handle_pattern();
    let mut out: Vec<String> = Vec::new();
    for m in rows {
        let to = m.get("to").and_then(Value::as_str).unwrap_or("");
        if to.is_empty() {
            continue;
        }
        if m.get("to_kind").and_then(Value::as_str) == Some("project") {
            continue;
        }
        let has_owner = m
            .get("meta")
            .map(|x| x.get("owner").is_some())
            .unwrap_or(false);
        if has_owner || re.is_match(to) {
            out.push(to.to_string());
        }
    }
    out.sort();
    out.dedup();
    out
}

/// One `fno-agents chats bus-read <mode> ...` answer. `None` when the binary
/// is missing, the call exceeds its bound or the answer is not JSON; the sweep
/// then reads an empty bus.
fn bus_read(args: &[&str]) -> Option<Value> {
    let mut argv = vec![
        super::resolve_binary()?,
        "chats".to_string(),
        "bus-read".to_string(),
    ];
    argv.extend(args.iter().map(|a| a.to_string()));
    let (code, stdout, _stderr) = super::super::bounded_command_for(&argv, 15)?;
    if code != 0 {
        return None;
    }
    serde_json::from_str(&stdout).ok()
}

/// Every retained envelope, oldest first.
fn envelopes_pub() -> Vec<Value> {
    bus_read(&["rows"])
        .and_then(|v| v.get("rows").and_then(Value::as_array).cloned())
        .unwrap_or_default()
}

fn drain_msg_ids() -> BTreeSet<String> {
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
    let journal = super::project_events_path();
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

/// The unread mail per handle, after each handle's drain cursor. The door
/// refuses a cursor name with a path separator, so those handles read every
/// retained message addressed to them, as an absent cursor does.
fn scan_unread_all(handles: &[String], rows: &[Value]) -> BTreeMap<String, Vec<Value>> {
    let valid = |h: &str| !h.is_empty() && !h.contains('/') && !h.contains('\\') && h != "..";
    let mut out: BTreeMap<String, Vec<Value>> = BTreeMap::new();
    let named: Vec<&String> = handles.iter().filter(|h| valid(h)).collect();
    if !named.is_empty() {
        let mut args: Vec<&str> = vec!["unread"];
        for h in &named {
            args.push("--name");
            args.push(h);
        }
        if let Some(Value::Object(map)) = bus_read(&args) {
            for (handle, list) in map {
                if let Some(list) = list.as_array() {
                    out.insert(handle, list.clone());
                }
            }
        }
    }
    for h in handles.iter().filter(|h| !valid(h)) {
        let all = rows
            .iter()
            .filter(|m| m.get("to").and_then(Value::as_str) == Some(h.as_str()))
            .cloned()
            .collect();
        out.insert(h.clone(), all);
    }
    out
}
