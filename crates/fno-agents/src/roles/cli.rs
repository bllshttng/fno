//! The `fno agents roles <op>` commands, nested under the existing
//! `spawn-overlay` door as `fno-agents spawn-overlay roles <op>`: the
//! fno-agents root sits at its menu cap, so the roles ops add no root
//! command. The `fno` front classifies each ported op and execs this door.
//!
//! The group options precede the op, as the Python group's callback read
//! them: `--json`/`-J`, `--root PATH` and the repeatable
//! `--source LAYER=PATH`. Refusals keep the click wording and exit 2.

use super::registry::{self, Discovered, DiscoveryInputError};
use serde_json::{json, Map, Value};
use std::path::PathBuf;

const GROUP: &str = "fno agents roles";

const LS_HELP: &str = "Usage: fno agents roles ls [OPTIONS]

  List source-attributed definitions without resolving context.

Options:
  -J, --json  Emit stable structured JSON.
  --help      Show this message and exit.
";

#[derive(Default)]
struct GroupOptions {
    json: bool,
    root: Option<PathBuf>,
    sources: Vec<String>,
}

/// A click usage error: the usage line, the help hint, the message.
fn usage_error(command: &str, usage: &str, message: &str) -> i32 {
    eprintln!("Usage: {command} {usage}\nTry '{command} --help' for help.\n\nError: {message}");
    2
}

/// `json.dumps(value, indent=2, sort_keys=True)`: serde's pretty layout
/// matches Python's, and every non-ASCII char escapes as Python's default
/// `ensure_ascii` writes it. Keys arrive sorted from the caller, since the
/// crate keeps insertion order.
pub(crate) fn python_json(value: &Value) -> String {
    let pretty = serde_json::to_string_pretty(value).expect("a Value always serializes");
    let mut out = String::with_capacity(pretty.len());
    for ch in pretty.chars() {
        if ch.is_ascii() {
            out.push(ch);
        } else {
            let mut units = [0u16; 2];
            for unit in ch.encode_utf16(&mut units) {
                out.push_str(&format!("\\u{unit:04x}"));
            }
        }
    }
    out
}

/// Split the argv into the group options, the op, and the op's own tail.
fn parse_group(args: &[String]) -> Result<(GroupOptions, Option<&str>, &[String]), String> {
    let mut options = GroupOptions::default();
    let mut i = 0;
    while i < args.len() {
        let arg = args[i].as_str();
        let (flag, inline) = match arg.split_once('=') {
            Some((flag, value)) if flag.starts_with("--") => (flag, Some(value.to_string())),
            _ => (arg, None),
        };
        match flag {
            "--json" | "-J" => options.json = true,
            "--root" | "--source" => {
                let value = match inline {
                    Some(value) => value,
                    None => {
                        i += 1;
                        args.get(i)
                            .cloned()
                            .ok_or_else(|| format!("Option '{flag}' requires an argument."))?
                    }
                };
                if flag == "--root" {
                    options.root = Some(PathBuf::from(value));
                } else {
                    options.sources.push(value);
                }
            }
            _ if arg.starts_with('-') => return Err(format!("No such option: {arg}")),
            op => return Ok((options, Some(op), &args[i + 1..])),
        }
        i += 1;
    }
    Ok((options, None, &[]))
}

/// `fno-agents spawn-overlay roles <group options> <op> ...`.
pub fn run(args: &[String]) -> i32 {
    let (options, op, tail) = match parse_group(args) {
        Ok(parsed) => parsed,
        Err(message) => return usage_error(GROUP, "[OPTIONS] COMMAND [ARGS]...", &message),
    };
    match op {
        Some("ls") => ls(options, tail),
        Some(other) => usage_error(
            GROUP,
            "[OPTIONS] COMMAND [ARGS]...",
            &format!("No such command '{other}'."),
        ),
        None => usage_error(GROUP, "[OPTIONS] COMMAND [ARGS]...", "Missing command."),
    }
}

fn discover(options: &GroupOptions, command: &str) -> Result<Vec<Discovered>, i32> {
    registry::discover(options.root.as_deref(), &options.sources).map_err(
        |DiscoveryInputError(message)| {
            usage_error(
                command,
                "[OPTIONS]",
                &format!("Invalid value for --source: {message}"),
            )
        },
    )
}

/// The `ls` row: the sorted keys Python's `_row` emitted.
fn ls_row(record: &Discovered) -> Value {
    let mut row = Map::new();
    row.insert("error".into(), json!(record.error));
    row.insert(
        "function_id".into(),
        json!(record.role.as_ref().map(|r| &r.function_id)),
    );
    row.insert("layer".into(), json!(record.layer.as_str()));
    row.insert("role_id".into(), json!(record.role.as_ref().map(|r| &r.id)));
    row.insert("source".into(), json!(record.source_id));
    row.insert("status".into(), json!(record.status));
    Value::Object(row)
}

/// List source-attributed definitions without resolving context.
fn ls(mut options: GroupOptions, tail: &[String]) -> i32 {
    let command = format!("{GROUP} ls");
    for arg in tail {
        match arg.as_str() {
            "--json" | "-J" => options.json = true,
            "--help" => {
                print!("{LS_HELP}");
                return 0;
            }
            other if other.starts_with('-') => {
                return usage_error(&command, "[OPTIONS]", &format!("No such option: {other}"))
            }
            other => {
                return usage_error(
                    &command,
                    "[OPTIONS]",
                    &format!("Got unexpected extra argument ({other})"),
                )
            }
        }
    }
    let records = match discover(&options, &command) {
        Ok(records) => records,
        Err(code) => return code,
    };
    if options.json {
        let rows: Vec<Value> = records.iter().map(ls_row).collect();
        println!("{}", python_json(&Value::Array(rows)));
        return 0;
    }
    for record in &records {
        let unavailable = "<unavailable>";
        let role_id = record.role.as_ref().map_or(unavailable, |r| r.id.as_str());
        let function_id = record
            .role
            .as_ref()
            .map_or(unavailable, |r| r.function_id.as_str());
        println!(
            "{role_id} function={function_id} layer={} source={} status={}",
            record.layer.as_str(),
            record.source_id,
            record.status
        );
    }
    0
}
