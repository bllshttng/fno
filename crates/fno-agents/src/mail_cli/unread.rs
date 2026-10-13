//! The native `mail unread` leaf: the cursor-bounded unread listing over the
//! bus log. Ported from the Python leaf once characterization goldens
//! (tests/golden/mail_unread) froze its contract: the scan spans every
//! retained segment oldest-first, keeps the first row per id, drops
//! retracted (withdraw), superseded (a hosted-delivery twin) and
//! non-deliverable rows, then lists rows addressed to the reader and after
//! its cursor - and when the cursor id has rotated out of the retained log,
//! rescans every retained row rather than silently skipping unprocessed
//! mail. JSON output mirrors the oracle's
//! `json.dumps(..., ensure_ascii=False)` byte for byte.

use std::collections::HashSet;
use std::io::IsTerminal;

use serde_json::Value;

use crate::announce::read_bus_segments;
use crate::mail_cli::hold::bus_dir;
use crate::mail_control_drain::{deliverable, withdrawn_ids};
use crate::paths::AgentsHome;

pub fn run(args: &[String]) -> i32 {
    let mut name = String::from("fno");
    let mut json_out = false;
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--name" | "-n" => match it.next() {
                Some(value) => name = value.clone(),
                None => {
                    eprintln!("unread: --name wants a value");
                    return 2;
                }
            },
            "--json" | "-J" => json_out = true,
            other => {
                eprintln!("unread: unknown argument {other:?}");
                return 2;
            }
        }
    }
    if name.is_empty() || name.contains('/') || name.contains('\\') || name == ".." {
        eprintln!("unread: invalid cursor name: {name:?}");
        return 2;
    }

    let home = AgentsHome::from_env();
    let bus = bus_dir(&home);
    let msgs = read_bus_segments(&bus.join("messages.jsonl"));
    let (cursor, warnings) = read_cursor(&bus.join("cursors").join(format!("{name}.json")));
    for warning in &warnings {
        eprintln!("{warning}");
    }

    // The withdrawal and hosted-twin sets come off the RAW rows (a twin may
    // share its id with an earlier deliverable row); dedupe keeps the first
    // row per id in log order and drops id-less rows, which the oracle's
    // envelope coercion also refused.
    let retracted = withdrawn_ids(&msgs);
    let superseded: HashSet<String> = msgs
        .iter()
        .filter(|m| m.get("delivery").and_then(Value::as_str) == Some("hosted"))
        .filter_map(|m| m.get("id").and_then(Value::as_str))
        .map(str::to_string)
        .collect();

    let mut seen: HashSet<&str> = HashSet::new();
    let mut deduped: Vec<&Value> = Vec::new();
    for m in &msgs {
        let Some(id) = m.get("id").and_then(Value::as_str) else {
            continue;
        };
        if seen.insert(id) {
            deduped.push(m);
        }
    }

    let mine = |m: &Value| {
        let Some(id) = m.get("id").and_then(Value::as_str) else {
            return false;
        };
        m.get("to").and_then(Value::as_str) == Some(name.as_str())
            && deliverable(m)
            && !retracted.contains(id)
            && !superseded.contains(id)
    };

    // The oracle appends before flipping `passed` in the same iteration, so
    // the cursor message itself never lists.
    let mut unread: Vec<&Value> = Vec::new();
    let mut passed = cursor.is_none();
    for m in &deduped {
        if passed && mine(m) {
            unread.push(m);
        }
        if m.get("id").and_then(Value::as_str) == cursor.as_deref() {
            passed = true;
        }
    }
    if !passed {
        // The cursor id rotated out of the retained log: the oracle rescans
        // the retained rows rather than silently skipping unprocessed mail.
        unread = deduped.iter().copied().filter(|m| mine(m)).collect();
    }

    if json_out || !std::io::stdout().is_terminal() {
        println!("{}", json_array(&unread));
        return 0;
    }
    if unread.is_empty() {
        println!("inbox empty for {name:?} (no unread bus messages)");
        return 0;
    }
    for m in &unread {
        let excerpt: String = str_field(m, "body")
            .replace('\n', " ")
            .chars()
            .take(100)
            .collect();
        println!(
            "{}  {} -> {}  [{}]  {}",
            str_field(m, "id"),
            str_field(m, "from"),
            str_field(m, "to"),
            str_field(m, "kind"),
            excerpt
        );
    }
    println!();
    println!("to answer one: fno agents mail reply --to <id> --body \"...\"");
    0
}

fn str_field<'a>(m: &'a Value, key: &str) -> &'a str {
    m.get(key).and_then(Value::as_str).unwrap_or("")
}

fn quote(v: &str) -> String {
    serde_json::to_string(v).unwrap_or_default()
}

/// One envelope re-projected in the oracle's key order with its exact
/// `json.dumps` spacing (`", "` between pairs, `": "` after keys). Absent
/// strings read as empty, the thread falls back to the id, and a non-string
/// `in_reply_to` prints as null.
fn row_json(m: &Value) -> String {
    let id = str_field(m, "id");
    let thread = match m.get("thread") {
        Some(Value::String(t)) => quote(t),
        _ => quote(id),
    };
    let in_reply_to = match m.get("in_reply_to") {
        Some(Value::String(t)) => quote(t),
        _ => "null".to_string(),
    };
    let pairs = [
        ("id", quote(id)),
        ("thread", thread),
        ("from", quote(str_field(m, "from"))),
        ("to", quote(str_field(m, "to"))),
        ("kind", quote(str_field(m, "kind"))),
        ("ts", quote(str_field(m, "ts"))),
        ("in_reply_to", in_reply_to),
        ("body", quote(str_field(m, "body"))),
    ];
    let inner: Vec<String> = pairs
        .iter()
        .map(|(k, v)| format!("{}, {}", quote(k), v))
        .collect();
    format!("{{{}}}", inner.join(", "))
}

fn json_array(rows: &[&Value]) -> String {
    if rows.is_empty() {
        return "[]".to_string();
    }
    let inner: Vec<String> = rows.iter().map(|m| row_json(m)).collect();
    format!("[{}]", inner.join(", "))
}

/// The consumer's cursor, plus any stderr warnings. Absent file: a silent
/// `None` (full rescan). Unreadable or unparseable file: the oracle's warning
/// verbatim, naming the Python exception class in the `{type}` slot. A
/// non-object cursor reads as no cursor (`Value::get` answers `None`), and
/// the truthiness gate mirrors the oracle: empty string and numeric zero
/// advance nothing.
fn read_cursor(path: &std::path::Path) -> (Option<String>, Vec<String>) {
    let file_name = path
        .file_name()
        .and_then(std::ffi::OsStr::to_str)
        .unwrap_or_default()
        .to_string();
    if !path.exists() {
        return (None, Vec::new());
    }
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(_) => {
            return (
                None,
                vec![format!(
                    "bus cursor: ignoring corrupt cursor {file_name} (OSError); rescanning retained segments"
                )],
            )
        }
    };
    let parsed: Value = match serde_json::from_str(&text) {
        Ok(v) => v,
        Err(_) => {
            return (
                None,
                vec![format!(
                    "bus cursor: ignoring corrupt cursor {file_name} (JSONDecodeError); rescanning retained segments"
                )],
            )
        }
    };
    let cursor = match parsed.get("last_seen_id") {
        Some(Value::String(s)) if !s.is_empty() => Some(s.clone()),
        Some(Value::Number(n)) if n.as_f64().map(|f| f != 0.0).unwrap_or(false) => {
            Some(n.to_string())
        }
        _ => None,
    };
    (cursor, Vec::new())
}
