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
use std::path::PathBuf;

use serde_json::Value;

use crate::announce::read_bus_segments;
use crate::mail_control_drain::{deliverable, read_cursor_file, withdrawn_ids};

const HELP: &str = "\
usage: fno-agents mail unread [--name NAME] [-n NAME] [--json] [-J] [--help]

Show unread bus messages addressed to NAME (cursor-filtered): the bus log
scanned oldest-first, rows addressed to NAME after its ack cursor.

options:
  -n, --name NAME  whose inbox to read (registry name or project; default fno)
  -J, --json       emit JSON regardless of TTY
  -h, --help       print this help
";

pub fn run(args: &[String]) -> i32 {
    let mut name = String::from("fno");
    let mut json_out = false;
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        if let Some(value) = arg.strip_prefix("--name=") {
            name = value.to_string();
            continue;
        }
        match arg.as_str() {
            "--help" | "-h" => {
                print!("{HELP}");
                return 0;
            }
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

    let bus = bus_dir();
    let msgs = read_bus_segments(&bus.join("messages.jsonl"));
    let (cursor, warnings) = read_cursor_file(&bus.join("cursors").join(format!("{name}.json")));
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

/// The bus directory as the Python oracle's `paths.bus_dir()` resolves it
/// once both env overrides are absent: `<state root>/bus`. `FNO_BUS_DIR` and
/// `FNO_INBOX_ROOT` live in `intel::bus_log_path`; the settings-file
/// overrides (`config.paths.bus_dir`, `config.state_dir`) stay that
/// function's documented gap. The bus hangs off the state root, not the
/// agents home.
fn bus_dir() -> PathBuf {
    let file = crate::intel::bus_log_path(&state_root());
    file.parent()
        .map(std::path::Path::to_path_buf)
        .unwrap_or_else(|| state_root().join("bus"))
}

/// The state root behind the Python `config.state_dir` default:
/// `$FNO_STATE_DIR`, else `$HOME/.fno`.
fn state_root() -> PathBuf {
    if let Some(dir) = std::env::var_os("FNO_STATE_DIR").filter(|v| !v.is_empty()) {
        return PathBuf::from(dir);
    }
    std::env::var_os("HOME")
        .filter(|h| !h.is_empty())
        .map(|h| PathBuf::from(h).join(".fno"))
        .unwrap_or_else(|| PathBuf::from(".fno"))
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
        .map(|(k, v)| format!("{}: {}", quote(k), v))
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
