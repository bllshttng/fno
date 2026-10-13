//! The withdraw leaf: `mail withdraw <msg-id>` (append a withdrawal
//! tombstone so no later drain delivers the message).
//!
//! Port of the Python `mail withdraw` body, deleted in the same change; the
//! parity goldens in `tests/golden/mail_withdraw` are the contract. Reader
//! divergences from the Python leg (both follow the drain-reader idiom):
//! malformed bus lines and corrupt cursor files are skipped silently instead
//! of warned to stderr, and cursor filenames use the raw address, not the
//! Python `_safe_name` spelling.

use crate::mail_control_drain::{withdrawn_ids, WITHDRAW_KIND};
use crate::paths::AgentsHome;
use crate::spawn_context::resolve_self_identity;
use serde_json::{json, Map, Value};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

const HOSTED_DELIVERY: &str = "hosted";
const TYPED_DELIVERY: &str = "typed";

/// Python `stamp_from`: an explicit `--from-name`, even the empty string,
/// wins verbatim; otherwise the resolved self handle, floored to the bare
/// fleet name. Unlike hold there is no identity refusal here: an
/// unprovable self stamps as `fno`.
fn stamp_from(explicit: Option<&str>) -> String {
    if let Some(name) = explicit {
        return name.to_string();
    }
    let home = AgentsHome::from_env();
    let ident = resolve_self_identity(&|name| std::env::var(name).ok(), None, None, &home);
    ident
        .session_id
        .as_deref()
        .map(crate::identity::canonical_handle)
        .filter(|h| !h.is_empty())
        .unwrap_or_else(|| "fno".to_string())
}

/// Bus messages, oldest first. Lines that are not envelopes (missing a
/// string id/from/to/kind) are skipped silently, matching the drain reader.
fn read_bus(bus: &Path) -> Vec<Value> {
    let Ok(text) = std::fs::read_to_string(bus.join("messages.jsonl")) else {
        return Vec::new();
    };
    text.lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .filter(|m| {
            ["id", "from", "to", "kind"]
                .iter()
                .all(|k| m.get(k).and_then(Value::as_str).is_some())
        })
        .collect()
}

fn bus_dir(home: &AgentsHome) -> PathBuf {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let root = crate::agents_config::state_dir(&cwd).unwrap_or_else(|| home.root().to_path_buf());
    crate::intel::bus_dir_of(&root)
}

fn str_field(m: &Value, key: &str, blank: &str) -> String {
    m.get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .unwrap_or(blank)
        .to_string()
}

/// The target's own delivery cursor (the cursor filename IS the address),
/// read silently like the drain does.
fn read_cursor(bus: &Path, name: &str) -> Option<String> {
    let text = std::fs::read_to_string(bus.join("cursors").join(format!("{name}.json"))).ok()?;
    let v: Value = serde_json::from_str(&text).ok()?;
    v.get("last_seen_id")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

pub fn run(args: &[String]) -> i32 {
    let mut msg_id: Option<String> = None;
    let mut from_name: Option<String> = None;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--from-name" => match it.next() {
                Some(v) if !v.starts_with("--") => from_name = Some(v.clone()),
                _ => {
                    eprintln!("error: --from-name wants a value");
                    return 2;
                }
            },
            other => {
                if msg_id.is_none() && !other.starts_with('-') {
                    msg_id = Some(other.to_string());
                } else {
                    eprintln!("withdraw: unknown argument {other:?}");
                    return 2;
                }
            }
        }
    }
    let Some(msg_id) = msg_id else {
        eprintln!("withdraw: a message id is required");
        return 2;
    };

    let handle = stamp_from(from_name.as_deref());
    let bus = bus_dir(&AgentsHome::from_env());
    let msgs = read_bus(&bus);

    let Some(target) = msgs
        .iter()
        .find(|m| m.get("id").and_then(Value::as_str) == Some(msg_id.as_str()))
    else {
        eprintln!("no such message on the bus: {msg_id}");
        return 1;
    };
    let from = str_field(target, "from", "");
    // The ownership check IS the authorization: `handle` comes from the same
    // resolver that stamped the send, and `--from-name` widens nothing (a
    // label is unauthenticated on the send side too).
    if from != handle {
        eprintln!(
            "{msg_id} was sent by {from}, not you ({handle}); only its sender can withdraw it"
        );
        return 1;
    }
    if withdrawn_ids(&msgs).contains(&msg_id) {
        println!("{msg_id} is already withdrawn");
        return 0;
    }
    let delivery = str_field(target, "delivery", "");
    if delivery == HOSTED_DELIVERY {
        eprintln!("{msg_id} was already delivered (hosted); it cannot be withdrawn");
        return 1;
    }
    // A typed row is bytes already at somebody's prompt: a tombstone here
    // would print success while retracting nothing.
    if delivery == TYPED_DELIVERY {
        eprintln!("{msg_id} was typed into a pane; the bytes are already at the recipient's prompt and cannot be recalled. Send a correction.");
        return 1;
    }
    let to = str_field(target, "to", "");
    if let Some(cursor) = read_cursor(&bus, &to) {
        let pos: HashMap<&str, usize> = msgs
            .iter()
            .enumerate()
            .filter_map(|(i, m)| m.get("id").and_then(Value::as_str).map(|id| (id, i)))
            .collect();
        if let (Some(cursor_pos), Some(target_pos)) = (
            pos.get(cursor.as_str()).copied(),
            pos.get(msg_id.as_str()).copied(),
        ) {
            if target_pos <= cursor_pos {
                eprintln!("{msg_id} was already claimed by {to}; a withdrawal now would hide it from you, not from them");
                return 1;
            }
        }
    }

    let mid = crate::announce::new_msg_id();
    let thread = str_field(target, "thread", "");
    let thread = if thread.is_empty() {
        mid.clone()
    } else {
        thread
    };
    let to_kind = str_field(target, "to_kind", "");
    let mut obj = Map::new();
    obj.insert("v".into(), Value::from(1));
    obj.insert("id".into(), Value::from(mid));
    obj.insert("ts".into(), Value::from(crate::announce::now_iso()));
    obj.insert("thread".into(), Value::from(thread));
    obj.insert("from".into(), Value::from(handle));
    obj.insert("to".into(), Value::from(to.clone()));
    obj.insert("kind".into(), Value::from(WITHDRAW_KIND));
    if !to_kind.is_empty() {
        obj.insert("to_kind".into(), Value::from(to_kind));
    }
    obj.insert("meta".into(), json!({ "withdraws": msg_id }));
    obj.insert("body".into(), Value::from(format!("withdrawn: {msg_id}")));
    let live = bus.join("messages.jsonl");
    if let Err(e) =
        crate::announce::append_line_open(&live, &Value::Object(obj), &crate::chats::chats_dir())
    {
        eprintln!("withdraw: could not append the tombstone: {e}");
        return 1;
    }
    // The cursor read above and this append are not one atomic step: a drain
    // in flight may already hold the body, so the receipt names the boundary
    // the tombstone actually guarantees.
    println!("withdrew {msg_id} (to {to}); no drain will deliver it from now on. A drain already in flight when this ran may have delivered it: `fno agents peek {to}` to check.");
    0
}
