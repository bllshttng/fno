//! `fno-agents mail-threads retry <id>`: run the live ladder again for one
//! sent message that never reached the recipient. The retry reuses the
//! staged envelope and its id, so the thread shows one message with a new
//! state, never a second copy.

use super::engine::{deliver_live, load_and_resolve_target, registered_family1_state};
use crate::mail_delivery_state::{newest_state, DeliveryState};
use serde_json::Value;

/// The retry of message `id`: `Err` names why it did not run at all.
pub(crate) fn retry_message(id: &str) -> Result<DeliveryState, String> {
    let bus = crate::mail_cli::ack::bus_dir().ok_or("no mail bus to read")?;
    let text = std::fs::read_to_string(bus.join("messages.jsonl"))
        .map_err(|e| format!("mail bus unreadable: {e}"))?;
    let rows: Vec<Value> = text
        .lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect();
    let field = |r: &Value, k: &str| r.get(k).and_then(Value::as_str).unwrap_or("").to_string();
    let row = rows
        .iter()
        .find(|r| field(r, "id") == id && field(r, "kind") == "send")
        .ok_or_else(|| format!("{id} is not a sent message on the bus"))?;
    // A message whose bytes sit with the recipient would arrive twice.
    let current = newest_state(&rows, id);
    if current.reached() {
        return Err(format!(
            "{id} is {}: the recipient already holds it, so a retry would deliver it twice",
            current.word()
        ));
    }
    let (to, from, wrapped) = (field(row, "to"), field(row, "from"), field(row, "body"));
    let (_, entry) = load_and_resolve_target(&to).map_err(|(message, _)| message)?;
    let mut reasons = Vec::new();
    let state = deliver_live(
        &entry,
        &wrapped,
        &from,
        None,
        &mut reasons,
        &registered_family1_state(&entry),
    );
    // The ladder can stop with a bare reason while its rungs said more.
    let state = match state {
        DeliveryState::Failed { reason } if !reasons.is_empty() && !reasons.contains(&reason) => {
            DeliveryState::failed(format!("{reason}; {}", reasons.join("; ")))
        }
        other => other,
    };
    crate::mail_delivery_state::record(id, &state, &from, &to);
    if state.reached() {
        let mut hosted = row.clone();
        if let Some(obj) = hosted.as_object_mut() {
            obj.insert("delivery".to_string(), Value::from("hosted"));
        }
        if let Err(e) = super::lanes::record_hosted_delivery(
            &hosted,
            entry.get("harness").and_then(Value::as_str),
            entry.get("harness_session_id").and_then(Value::as_str),
        ) {
            eprintln!("retry: delivered, but the hosted row failed: {e}");
        }
    }
    Ok(state)
}

/// The door: one JSON line, the state the retry reached plus the next step
/// a still-failed message offers. Exit 0 when the bytes reached the
/// recipient, 1 when they did not, 2 when the retry could not run.
pub(crate) fn run(args: &[String]) -> i32 {
    let Some(id) = args.first().filter(|a| !a.starts_with('-')) else {
        eprintln!("usage: fno-agents mail-threads retry <msg-id>");
        return 2;
    };
    match retry_message(id) {
        Ok(state) => {
            let mut out = state.to_json();
            if !state.reached() {
                out["next"] = Value::from(
                    "the retry failed too: press y to copy the message and paste it into the recipient's session",
                );
            }
            println!("{out}");
            if state.reached() {
                0
            } else {
                1
            }
        }
        Err(reason) => {
            println!(
                "{}",
                serde_json::json!({"state": "refused", "reason": reason})
            );
            2
        }
    }
}
