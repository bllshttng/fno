//! The stale-bus-letter sweep (doctor.py:3619-3693).

use serde_json::{json, Value};

use super::bus;
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
    let recipients = recipients();
    let drained = bus::drain_msg_ids();
    let now = chrono::Utc::now().timestamp();
    let cutoff = now - (AGE_HOURS * 3600.0) as i64;
    let mut findings: Vec<Value> = Vec::new();
    for handle in recipients {
        for m in bus::scan_unread(&handle) {
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

fn recipients() -> Vec<String> {
    let re = handle_pattern();
    let mut out: Vec<String> = Vec::new();
    for m in bus::envelopes_pub() {
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
