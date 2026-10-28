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
    let msgs = load_messages(&bus);

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

    match advance_cursor(&bus, &msgs, name, msg_id) {
        Ok(true) => {
            println!("cursor for '{name}' advanced to {msg_id}");
            for m in &acked {
                emit_drain_marker(m, name);
            }
        }
        Ok(false) => println!("cursor for '{name}' already at or past {msg_id}; unchanged"),
        // The oracle raises out of cmd_bus_ack: traceback, exit 1, no
        // advance line and no drain receipts. Mirrored as one stderr line.
        Err(error) => {
            eprintln!("error: could not write cursor for '{name}': {error}");
            return 1;
        }
    }
    0
}

/// The bus directory holding messages.jsonl, the shared reader env order
/// (`intel::bus_log_path`): FNO_BUS_DIR, then FNO_INBOX_ROOT/.bus, then the
/// state root's bus.
/// `pub(crate)`: the reply leaf reads the same bus log through it.
pub(crate) fn bus_dir() -> Option<PathBuf> {
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

/// The retained bus rows, oldest first: `bus.log.iter_messages` over
/// `_segment_paths_oldest_first`. Rotated `messages.jsonl.<N>` segments sort
/// by descending N and the live log goes last; reading only the live file
/// would refuse a target retained in a rotated segment and silently skip
/// rotated rows when collecting the acked set. Malformed lines are skipped
/// (the oracle warns; this reader stays silent).
/// `pub(crate)`: the reply leaf reads the same retained segments through it.
pub(crate) fn load_messages(bus: &Path) -> Vec<Value> {
    let mut segments: Vec<(u64, PathBuf)> = Vec::new();
    if let Ok(entries) = std::fs::read_dir(bus) {
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            let Some(suffix) = name.strip_prefix("messages.jsonl.") else {
                continue;
            };
            if let Ok(n) = suffix.parse::<u64>() {
                segments.push((n, entry.path()));
            }
        }
    }
    segments.sort_by(|a, b| b.0.cmp(&a.0));
    let live = bus.join("messages.jsonl");
    if live.exists() {
        segments.push((u64::MAX, live));
    }
    let mut msgs: Vec<Value> = Vec::new();
    for (_, path) in segments {
        if let Ok(text) = std::fs::read_to_string(path) {
            for line in text.lines() {
                if let Ok(v) = serde_json::from_str::<Value>(line) {
                    msgs.push(v);
                }
            }
        }
    }
    msgs
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
    format!(
        "message '{msg_id}' already reached the recipient's harness (hosted); cursor not advanced"
    )
}

/// `bus.cursor.advance_cursor`, verbatim: forward-only on FIRST-occurrence
/// positions. An absent, corrupt or rotated-out cursor reseeds; a target the
/// log no longer holds never moves it; so does a target at or before the
/// cursor. `Ok(true)` means the cursor moved and the acked receipts may fire;
/// the write error propagates, so a failed persistence never reads as an
/// advance.
fn advance_cursor(bus: &Path, msgs: &[Value], name: &str, msg_id: &str) -> std::io::Result<bool> {
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
        write_cursor(bus, name, msg_id)?;
        return Ok(true);
    };
    if current == msg_id {
        return Ok(false);
    }
    let Some(cur_pos) = first_pos(&current) else {
        write_cursor(bus, name, msg_id)?;
        return Ok(true);
    };
    let Some(new_pos) = first_pos(msg_id) else {
        return Ok(false);
    };
    if new_pos <= cur_pos {
        return Ok(false);
    }
    write_cursor(bus, name, msg_id)?;
    Ok(true)
}

/// The main-cursor write (the `advance_main_cursor` idiom, not
/// `mail_control_drain::write_cursor`, which is the control lane):
/// `{last_seen_id, ts}`, trailing newline, tmp-then-rename. The error
/// propagates (`bus.cursor.write_cursor` unlinks the tmp and reraises): a
/// cursor that did not persist must not read as an advance.
fn write_cursor(bus: &Path, name: &str, msg_id: &str) -> std::io::Result<()> {
    let path = bus.join("cursors").join(format!("{name}.json"));
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let payload = json!({"last_seen_id": msg_id, "ts": crate::graph_store::now_isoformat()});
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, format!("{payload}\n"))
        .and_then(|()| std::fs::rename(&tmp, &path))
        .inspect_err(|_| {
            let _ = std::fs::remove_file(&tmp);
        })
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

    fn send_row(id: &str) -> String {
        format!(
            "{{\"id\":\"{id}\",\"thread\":\"t\",\"from\":\"lead\",\"to\":\"a1b2c3d4\",\
             \"kind\":\"send\",\"body\":\"\",\"ts\":\"2026-10-12T09:00:00Z\"}}\n"
        )
    }

    fn vec_args(args: &[&str]) -> Vec<String> {
        args.iter().map(|s| s.to_string()).collect()
    }

    /// The ack scans all retained segments, oldest first: a live ack drains
    /// the rotated unread ahead of it (`.2` before `.1`, live last), and a
    /// target retained only in a rotated segment is a known id. A
    /// live-file-only reader refuses that target and under-reports the
    /// acked set; an ascending sort under-reports it too.
    #[test]
    fn ack_spans_rotated_segments_oldest_first() {
        let _guard = crate::claims::test_env_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().unwrap();
        let _state = crate::claims::EnvVarGuard::set("FNO_STATE_DIR", dir.path().to_str().unwrap());
        let bus = dir.path().join("bus");
        let _bus = crate::claims::EnvVarGuard::set("FNO_BUS_DIR", bus.to_str().unwrap());
        std::fs::create_dir_all(&bus).unwrap();
        std::fs::write(bus.join("messages.jsonl.2"), send_row("m-rot-a")).unwrap();
        std::fs::write(bus.join("messages.jsonl.1"), send_row("m-rot-b")).unwrap();
        std::fs::write(bus.join("messages.jsonl"), send_row("m-live")).unwrap();

        assert_eq!(run_ack(&vec_args(&["m-live", "--name", "a1b2c3d4"])), 0);
        let journal = dir.path().join("events.jsonl");
        let rows = crate::event_store::query_events(
            &journal,
            &crate::event_store::EventQuery::of_types(&["agent_mail_drained"]),
        )
        .unwrap();
        let drained: Vec<String> = rows
            .iter()
            .map(|r| {
                let row: Value = serde_json::from_str(&r.line).unwrap();
                row["data"]["msg_id"].as_str().unwrap().to_string()
            })
            .collect();
        assert_eq!(
            drained,
            vec!["m-rot-a", "m-rot-b", "m-live"],
            "acked set spans segments, oldest first"
        );
        let cursor = std::fs::read_to_string(bus.join("cursors").join("a1b2c3d4.json")).unwrap();
        assert!(cursor.contains("m-live"), "cursor lands on the live id");
    }

    /// A failed cursor write fails the ack: exit 1, no advance line, no
    /// drain receipts. Persistence precedes receipts, as the oracle's
    /// reraise out of `cmd_bus_ack`.
    #[test]
    fn cursor_write_failure_fails_before_receipts() {
        let _guard = crate::claims::test_env_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().unwrap();
        let _state = crate::claims::EnvVarGuard::set("FNO_STATE_DIR", dir.path().to_str().unwrap());
        let bus = dir.path().join("bus");
        let _bus = crate::claims::EnvVarGuard::set("FNO_BUS_DIR", bus.to_str().unwrap());
        std::fs::create_dir_all(bus.join("cursors")).unwrap();
        std::fs::write(bus.join("messages.jsonl"), send_row("m-1")).unwrap();
        // A directory where the cursor file belongs: the tmp write
        // succeeds, the rename over it fails.
        std::fs::create_dir_all(bus.join("cursors").join("a1b2c3d4.json")).unwrap();

        assert_eq!(
            run_ack(&vec_args(&["m-1", "--name", "a1b2c3d4"])),
            1,
            "write failure exits 1"
        );
        assert!(
            !dir.path().join("events.db").exists(),
            "no drain receipts on a failed write"
        );
        assert!(
            !bus.join("cursors").join("a1b2c3d4.json.tmp").exists(),
            "the tmp file is cleaned up"
        );
    }
}
