//! `mail ack` front port (Python `mail.cli.cmd_bus_ack`, now a forward
//! leaf): move one recipient's read cursor to a message id, refusing ids
//! that are not the caller's mail or are control receipts. Everything at or
//! before the id that is still unread counts as acked and gets one
//! `agent_mail_drained` receipt, so a sender can join events.jsonl to a
//! terminal drained state.

use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

pub fn run_ack(args: &[String]) -> i32 {
    let mut msg_id: Option<&str> = None;
    let mut name = "fno";
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--name" | "-n" => match it.next() {
                Some(v) => name = v.as_str(),
                None => {
                    eprintln!("error: --name wants a value");
                    return 2;
                }
            },
            other => {
                if msg_id.is_none() {
                    msg_id = Some(other);
                } else {
                    eprintln!("ack: unknown argument {other:?}");
                    return 2;
                }
            }
        }
    }
    let Some(msg_id) = msg_id else {
        eprintln!("ack: a message id is required");
        return 2;
    };
    let Some(bus) = bus_dir() else {
        eprintln!("error: no fno state directory; bus log not found");
        return 2;
    };
    let mut msgs: Vec<Value> = Vec::new();
    if let Ok(text) = std::fs::read_to_string(bus.join("messages.jsonl")) {
        for line in text.lines() {
            if let Ok(v) = serde_json::from_str::<Value>(line) {
                msgs.push(v);
            }
        }
    }

    // Refusals in cmd_bus_ack order, each against the id's FIRST row.
    let Some(target) = msgs
        .iter()
        .find(|m| m.get("id").and_then(Value::as_str) == Some(msg_id))
    else {
        eprintln!(
            "unknown message id '{msg_id}': not found in the retained bus log; \
             cursor not advanced (run `fno agents mail unread --name {name}` to see ids)"
        );
        return 2;
    };
    let to = target.get("to").and_then(Value::as_str).unwrap_or("");
    if to != name {
        eprintln!(
            "message '{msg_id}' is addressed to '{to}', not '{name}'; \
             cursor not advanced (ack only your own messages)"
        );
        return 2;
    }
    if !crate::mail_control_drain::deliverable(target) {
        eprintln!("{}", refusal_line(target, msg_id));
        return 2;
    }

    // scan_unread over the same rows (cursor walk, to/name, deliverability,
    // retractions), then the superseded mask - any row whose id carries a
    // hosted delivery - and first-row dedupe.
    let superseded: std::collections::HashSet<&str> = msgs
        .iter()
        .filter(|m| m.get("delivery").and_then(Value::as_str) == Some("hosted"))
        .filter_map(|m| m.get("id").and_then(Value::as_str))
        .collect();
    let mut seen = std::collections::HashSet::new();
    let unread: Vec<&Value> = crate::mail_control_drain::unread_messages(&bus, &msgs, name)
        .into_iter()
        .filter(|m| {
            m.get("id")
                .and_then(Value::as_str)
                .is_some_and(|id| !superseded.contains(id) && seen.insert(id))
        })
        .collect();

    // cmd_bus_ack positions by LAST occurrence (the dict-comprehension
    // insert order), where advance_cursor below walks by FIRST - mirror each.
    let mut pos: HashMap<&str, usize> = HashMap::new();
    for (i, m) in msgs.iter().enumerate() {
        if let Some(id) = m.get("id").and_then(Value::as_str) {
            pos.insert(id, i);
        }
    }
    let Some(&target_pos) = pos.get(msg_id) else {
        eprintln!(
            "unknown message id '{msg_id}': not found in the retained bus log; \
             cursor not advanced (run `fno agents mail unread --name {name}` to see ids)"
        );
        return 2;
    };
    let acked: Vec<&Value> = unread
        .into_iter()
        .filter(|m| {
            m.get("id")
                .and_then(Value::as_str)
                .and_then(|id| pos.get(id).copied())
                .is_some_and(|p| p <= target_pos)
        })
        .collect();

    if advance_cursor(&bus, &msgs, name, msg_id) {
        println!("cursor for '{name}' advanced to {msg_id}");
        for m in &acked {
            emit_drain_marker(m, name);
        }
    } else {
        println!("cursor for '{name}' already at or past {msg_id}; unchanged");
    }
    0
}

/// The bus directory holding messages.jsonl, the shared reader env order
/// (`intel::bus_log_path`): FNO_BUS_DIR, then FNO_INBOX_ROOT/.bus, then the
/// state root's bus.
fn bus_dir() -> Option<PathBuf> {
    if let Some(dir) = std::env::var("FNO_BUS_DIR").ok().filter(|v| !v.is_empty()) {
        return Some(PathBuf::from(dir));
    }
    if let Some(root) = std::env::var("FNO_INBOX_ROOT")
        .ok()
        .filter(|v| !v.is_empty())
    {
        return Some(PathBuf::from(root).join(".bus"));
    }
    crate::agents_config::state_dir(Path::new(".")).map(|dir| dir.join("bus"))
}

/// The `ack_refusal.refusal_line` order: landed, withdraw, typed, else the
/// hosted wording (which also covers cross-session). `record_landed` always
/// writes `meta.landed`, so the fallback only guards a hand-edited log; it
/// renders `'None'`, not Python's bare-`None` repr.
fn refusal_line(m: &Value, msg_id: &str) -> String {
    let kind = m.get("kind").and_then(Value::as_str).unwrap_or("");
    let delivery = m.get("delivery").and_then(Value::as_str).unwrap_or("");
    if kind == "landed" {
        let landed = m
            .get("meta")
            .and_then(|v| v.get("landed"))
            .and_then(Value::as_str)
            .unwrap_or("None");
        return format!(
            "message '{msg_id}' is a landed receipt, not mail; \
             it acknowledges '{landed}'; cursor not advanced"
        );
    }
    if kind == "withdraw" {
        return format!(
            "message '{msg_id}' is a withdrawal tombstone, not mail; cursor not advanced"
        );
    }
    if delivery == "typed" {
        return format!(
            "message '{msg_id}' was typed into a pane (delivery unconfirmed); cursor not advanced"
        );
    }
    format!("message '{msg_id}' was already delivered (hosted); cursor not advanced")
}

/// `bus.cursor.advance_cursor`, verbatim: forward-only on FIRST-occurrence
/// positions. An absent, corrupt or rotated-out cursor reseeds; a target the
/// log no longer holds never moves it; so does a target at or before the
/// cursor. True means the cursor moved and the acked receipts may fire.
fn advance_cursor(bus: &Path, msgs: &[Value], name: &str, msg_id: &str) -> bool {
    let first_pos = |id: &str| {
        msgs.iter()
            .position(|m| m.get("id").and_then(Value::as_str) == Some(id))
    };
    let current = std::fs::read_to_string(bus.join("cursors").join(format!("{name}.json")))
        .ok()
        .and_then(|text| serde_json::from_str::<Value>(&text).ok())
        .and_then(|v| {
            v.get("last_seen_id")
                .and_then(Value::as_str)
                .map(str::to_string)
        });
    let Some(current) = current else {
        write_cursor(bus, name, msg_id);
        return true;
    };
    if current == msg_id {
        return false;
    }
    let Some(cur_pos) = first_pos(&current) else {
        write_cursor(bus, name, msg_id);
        return true;
    };
    let Some(new_pos) = first_pos(msg_id) else {
        return false;
    };
    if new_pos <= cur_pos {
        return false;
    }
    write_cursor(bus, name, msg_id);
    true
}

/// The main-cursor write (the `advance_main_cursor` idiom, not
/// `mail_control_drain::write_cursor`, which is the control lane):
/// `{last_seen_id, ts}`, trailing newline, tmp-then-rename.
fn write_cursor(bus: &Path, name: &str, msg_id: &str) {
    let path = bus.join("cursors").join(format!("{name}.json"));
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let payload = json!({"last_seen_id": msg_id, "ts": crate::graph_store::now_isoformat()});
    let tmp = path.with_extension("json.tmp");
    if std::fs::write(&tmp, format!("{payload}\n")).is_ok() {
        let _ = std::fs::rename(&tmp, &path);
    }
}

/// Best-effort `agent_mail_drained` receipt, one per acked id, in the
/// `emit_release_marker` shape: a hybrid row in the state-root events
/// journal. Swallowed on failure: a missing receipt degrades to the cursor.
fn emit_drain_marker(m: &Value, name: &str) {
    let data = json!({
        "msg_id": m.get("id").and_then(Value::as_str).unwrap_or(""),
        "recipient": name,
        "address_form": name,
        "sender": m.get("from").and_then(Value::as_str).unwrap_or(""),
        "reason": "acked",
    });
    let mut row = data.clone();
    row["ts"] = json!(chrono::Utc::now().to_rfc3339());
    row["kind"] = json!("agent_mail_drained");
    row["type"] = json!("agent_mail_drained");
    row["source"] = json!("agents");
    row["data"] = data;
    let journal = match crate::agents_config::state_dir(Path::new(".")) {
        Some(root) => root.join("events.jsonl"),
        None => {
            eprintln!("warning: no state dir; agent_mail_drained not committed");
            return;
        }
    };
    if let Err(error) = crate::event_store::append_envelope(&journal, &row.to_string(), None) {
        eprintln!("warning: could not commit agent_mail_drained: {error}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The manual-ack drain receipt: an acked id leaves one
    /// `agent_mail_drained` row in the state-root events journal with the
    /// acked reason, or sweep/drain accounting loses the terminal event.
    #[test]
    fn acked_id_commits_drain_marker() {
        let _guard = crate::claims::test_env_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().unwrap();
        let _state = crate::claims::EnvVarGuard::set("FNO_STATE_DIR", dir.path().to_str().unwrap());
        emit_drain_marker(&json!({"id": "m-1", "from": "lead"}), "a1b2c3d4");
        let journal = std::fs::read_to_string(dir.path().join("events.jsonl")).unwrap();
        let rows: Vec<Value> = journal
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(rows.len(), 1, "one ack, one receipt row");
        assert_eq!(rows[0]["kind"], "agent_mail_drained");
        assert_eq!(rows[0]["data"]["reason"], "acked");
        assert_eq!(rows[0]["data"]["msg_id"], "m-1");
        assert_eq!(rows[0]["data"]["recipient"], "a1b2c3d4");
    }
}
