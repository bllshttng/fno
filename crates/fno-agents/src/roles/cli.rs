//! The `fno agents roles <op>` commands, nested under the existing
//! `spawn-overlay` door as `fno-agents spawn-overlay roles <op>`: the
//! fno-agents root sits at its menu cap, so the roles ops add no root
//! command. The `fno` front classifies each ported op and execs this door.
//!
//! The group options precede the op, as the Python group's callback read
//! them: `--json`/`-J`, `--root PATH` and the repeatable
//! `--source LAYER=PATH`. Refusals keep the click wording and exit 2.

use super::context;
use super::models::{self, RoleRef, WorkOrderRef};
use super::registry::{self, Discovered, DiscoveryInputError};
use super::resolver::{self, Resolution, Unchecked};
use crate::client_commands::py_repr_str;
use serde_json::{json, Map, Value};
use std::path::PathBuf;

const GROUP: &str = "fno agents roles";

const LS_HELP: &str = "Usage: fno agents roles ls [OPTIONS]

  List source-attributed definitions without resolving context.

Options:
  -J, --json  Emit stable structured JSON.
  --help      Show this message and exit.
";

const SHOW_HELP: &str = "Usage: fno agents roles show [OPTIONS] ROLE

  Show ordered raw definitions, validity, and shadowing.

Arguments:
  ROLE  ROLE or FUNCTION/ROLE.  [required]

Options:
  -J, --json  Emit stable structured JSON.
  --help      Show this message and exit.
";

const RESOLVE_HELP: &str = "Usage: fno agents roles resolve [OPTIONS] ROLE

  Resolve exactly one immutable role or one typed blocked result.

Arguments:
  ROLE  ROLE or FUNCTION/ROLE.  [required]

Options:
  --work-order TEXT    Authorized graph node identity.  [required]
  --attempt TEXT       Immutable attempt identity.  [required]
  --capabilities PATH  Read-only JSON array of CapabilityFact observations.
  --context PATH       Read-only JSON array of ContextReference observations.
  --snapshot TEXT      Expected input snapshot revision.
  -J, --json           Emit stable structured JSON.
  --help               Show this message and exit.
";

const CONTEXT_HELP: &str = "Usage: fno agents roles context [OPTIONS]

  Emit honest ContextReference observations for configured artifacts.

  Reads the generic ``[context.artifacts]`` map and produces one observation
  per entry: a real sha256 and byte size for a readable file, or
  ``readable=false`` with a naming reason for an unreadable one. Composes with
  ``resolve``: ``fno agents roles resolve --snapshot <rev> --context <(fno
  agents roles context --json --snapshot <rev>) ...``.

Options:
  --snapshot TEXT  Alignment revision stamped on every reference (pass to
                   `resolve --snapshot`).
  -J, --json       Emit a stable JSON array (pipes into `resolve --context`).
  --help           Show this message and exit.
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
    ensure_ascii(&serde_json::to_string_pretty(value).expect("a Value always serializes"))
}

/// `json.dumps(value, sort_keys=True, separators=(",", ":"))`, keys
/// sorted at every depth.
fn python_compact_json(value: &Value) -> String {
    let sorted = crate::backlog::decisions_cli::sort_keys(value);
    ensure_ascii(&serde_json::to_string(&sorted).expect("a Value always serializes"))
}

pub(crate) fn ensure_ascii(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
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
        Some("show") => show(options, tail),
        Some("resolve") => resolve(options, tail),
        Some("context") => context(options, tail),
        Some(other) => usage_error(
            GROUP,
            "[OPTIONS] COMMAND [ARGS]...",
            &format!("No such command '{other}'."),
        ),
        None => usage_error(GROUP, "[OPTIONS] COMMAND [ARGS]...", "Missing command."),
    }
}

fn discover(options: &GroupOptions, command: &str, usage: &str) -> Result<Vec<Discovered>, i32> {
    registry::discover(options.root.as_deref(), &options.sources).map_err(
        |DiscoveryInputError(message)| {
            usage_error(
                command,
                usage,
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
    let mut options_done = false;
    for arg in tail {
        match arg.as_str() {
            "--" if !options_done => options_done = true,
            "--json" | "-J" if !options_done => options.json = true,
            "--help" if !options_done => {
                print!("{LS_HELP}");
                return 0;
            }
            other if !options_done && other.starts_with('-') && other.len() > 1 => {
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
    let records = match discover(&options, &command, "[OPTIONS]") {
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

/// `ROLE` or `FUNCTION/ROLE`, split at the first slash.
fn token_parts(token: &str) -> Result<(Option<&str>, &str), &'static str> {
    match token.split_once('/') {
        None => Ok((None, token)),
        Some((function, role))
            if !function.is_empty() && !role.is_empty() && !role.contains('/') =>
        {
            Ok((Some(function), role))
        }
        Some(_) => Err("role must be ROLE or FUNCTION/ROLE"),
    }
}

/// The `show` row. A role-less source is never valid, so it stays
/// `unchecked`; of the valid rows only the last in precedence order is
/// `contributing`.
fn show_row(record: &Discovered, contributing: bool) -> Value {
    let disposition = if record.status != "valid" || record.definition.is_none() {
        "unchecked"
    } else if contributing {
        "contributing"
    } else {
        "shadowed"
    };
    let mut row = Map::new();
    row.insert("disposition".into(), json!(disposition));
    row.insert("error".into(), json!(record.error));
    row.insert(
        "function_id".into(),
        json!(record.role.as_ref().map(|r| &r.function_id)),
    );
    row.insert("layer".into(), json!(record.layer.as_str()));
    row.insert(
        "raw_definition".into(),
        record
            .raw_definition
            .as_ref()
            .map_or(Value::Null, crate::backlog::decisions_cli::sort_keys),
    );
    row.insert("role_id".into(), json!(record.role.as_ref().map(|r| &r.id)));
    row.insert("source".into(), json!(record.source_id));
    row.insert("status".into(), json!(record.status));
    Value::Object(row)
}

/// Show ordered raw definitions, validity, and shadowing. The rows are
/// the sources naming the role, plus every role-less invalid or
/// unreadable source, since any of them might have named it.
fn show(mut options: GroupOptions, tail: &[String]) -> i32 {
    const USAGE: &str = "[OPTIONS] ROLE";
    let command = format!("{GROUP} show");
    let mut positionals: Vec<&str> = Vec::new();
    // A bare `--` ends the options, as click reads it.
    let mut options_done = false;
    for arg in tail {
        match arg.as_str() {
            "--" if !options_done => options_done = true,
            "--json" | "-J" if !options_done => options.json = true,
            "--help" if !options_done => {
                print!("{SHOW_HELP}");
                return 0;
            }
            other if !options_done && other.starts_with('-') && other.len() > 1 => {
                return usage_error(&command, USAGE, &format!("No such option: {other}"))
            }
            other => positionals.push(other),
        }
    }
    let token = match positionals.as_slice() {
        [] => return usage_error(&command, USAGE, "Missing argument 'ROLE'."),
        [token] => *token,
        [_, extra] => {
            return usage_error(
                &command,
                USAGE,
                &format!("Got unexpected extra argument ({extra})"),
            )
        }
        [_, extras @ ..] => {
            return usage_error(
                &command,
                USAGE,
                &format!("Got unexpected extra arguments ({})", extras.join(" ")),
            )
        }
    };
    // Discovery runs before the token parse, the order the Python command
    // read them in, so a bad `--source` wins over a bad token.
    let records = match discover(&options, &command, USAGE) {
        Ok(records) => records,
        Err(code) => return code,
    };
    let (function_id, role_id) = match token_parts(token) {
        Ok(parts) => parts,
        Err(message) => {
            return usage_error(
                &command,
                USAGE,
                &format!("Invalid value for role: {message}"),
            )
        }
    };
    let matched: Vec<&Discovered> = records
        .iter()
        .filter(|record| match &record.role {
            None => matches!(record.status, "invalid" | "unreadable"),
            Some(role) => role.id == role_id && function_id.is_none_or(|f| role.function_id == f),
        })
        .collect();
    let highest_valid = matched
        .iter()
        .rposition(|r| r.status == "valid" && r.definition.is_some());
    let rows: Vec<Value> = matched
        .iter()
        .enumerate()
        .map(|(i, record)| show_row(record, Some(i) == highest_valid))
        .collect();
    if options.json {
        println!("{}", python_json(&Value::Array(rows)));
        return 0;
    }
    for row in &rows {
        let field = |key: &str| row[key].as_str().unwrap_or("None").to_string();
        println!(
            "{} function={} layer={} source={} status={} disposition={}",
            field("role_id"),
            field("function_id"),
            field("layer"),
            field("source"),
            field("status"),
            field("disposition")
        );
        if let Some(error) = row["error"].as_str().filter(|e| !e.is_empty()) {
            println!("  error: {error}");
        }
        println!("  raw: {}", python_compact_json(&row["raw_definition"]));
    }
    0
}

const RESOLVE_USAGE: &str = "[OPTIONS] ROLE";
const RESOLVE_VALUES: [&str; 5] = [
    "--work-order",
    "--attempt",
    "--capabilities",
    "--context",
    "--snapshot",
];

/// The requested role: `FUNCTION/ROLE` as given, or `ROLE` in the one
/// function that defines it (`unavailable` when none does).
fn role_ref(command: &str, token: &str, records: &[Discovered]) -> Result<RoleRef, i32> {
    let refuse = |message: String| {
        usage_error(
            command,
            RESOLVE_USAGE,
            &format!("Invalid value for role: {message}"),
        )
    };
    let (requested, role_id) = token_parts(token).map_err(|m| refuse(m.to_string()))?;
    if let Some(function_id) = requested {
        return Ok(RoleRef {
            id: role_id.to_string(),
            function_id: function_id.to_string(),
        });
    }
    let mut functions: Vec<&str> = records
        .iter()
        .filter_map(|r| r.role.as_ref())
        .filter(|r| r.id == role_id)
        .map(|r| r.function_id.as_str())
        .collect();
    functions.sort_unstable();
    functions.dedup();
    if functions.len() > 1 {
        return Err(refuse(format!(
            "role {} exists in multiple functions; use FUNCTION/ROLE",
            py_repr_str(role_id)
        )));
    }
    Ok(RoleRef {
        id: role_id.to_string(),
        function_id: functions
            .first()
            .copied()
            .unwrap_or("unavailable")
            .to_string(),
    })
}

/// A read-only JSON array of observations, or the usage error naming it.
fn read_models<T>(
    command: &str,
    path: Option<&String>,
    parse: fn(&Value, &str) -> Result<T, String>,
) -> Result<Vec<T>, i32> {
    let Some(path) = path else {
        return Ok(Vec::new());
    };
    let fail = |error: String| {
        usage_error(
            command,
            RESOLVE_USAGE,
            &format!("Invalid value for {path}: cannot validate {path}: {error}"),
        )
    };
    let text = std::fs::read_to_string(path).map_err(|e| fail(e.to_string()))?;
    let raw: Value = serde_json::from_str(&text).map_err(|e| fail(e.to_string()))?;
    let items = raw
        .as_array()
        .ok_or_else(|| fail("input should be a valid tuple".into()))?;
    items
        .iter()
        .enumerate()
        .map(|(i, item)| parse(item, &i.to_string()))
        .collect::<Result<Vec<T>, String>>()
        .map_err(fail)
}

/// Resolve exactly one immutable role or one typed blocked result. A
/// blocked result exits 1.
fn resolve(mut options: GroupOptions, tail: &[String]) -> i32 {
    let command = format!("{GROUP} resolve");
    let mut positionals: Vec<&str> = Vec::new();
    let mut values: std::collections::HashMap<&str, String> = Default::default();
    let mut options_done = false;
    let mut i = 0;
    while i < tail.len() {
        let arg = tail[i].as_str();
        i += 1;
        if options_done || !arg.starts_with('-') || arg == "-" {
            positionals.push(arg);
            continue;
        }
        let (flag, inline) = match arg.split_once('=') {
            Some((flag, value)) if flag.starts_with("--") => (flag, Some(value.to_string())),
            _ => (arg, None),
        };
        match flag {
            "--" => options_done = true,
            "--json" | "-J" => options.json = true,
            "--help" => {
                print!("{RESOLVE_HELP}");
                return 0;
            }
            _ if RESOLVE_VALUES.contains(&flag) => {
                let value = match inline {
                    Some(value) => value,
                    None => match tail.get(i) {
                        Some(value) => {
                            i += 1;
                            value.clone()
                        }
                        None => {
                            return usage_error(
                                &command,
                                RESOLVE_USAGE,
                                &format!("Option '{flag}' requires an argument."),
                            )
                        }
                    },
                };
                values.insert(flag, value);
            }
            _ => return usage_error(&command, RESOLVE_USAGE, &format!("No such option: {arg}")),
        }
    }
    let token = match positionals.as_slice() {
        [] => return usage_error(&command, RESOLVE_USAGE, "Missing argument 'ROLE'."),
        [token] => *token,
        [_, extras @ ..] => {
            let plural = if extras.len() > 1 { "s" } else { "" };
            return usage_error(
                &command,
                RESOLVE_USAGE,
                &format!(
                    "Got unexpected extra argument{plural} ({})",
                    extras.join(" ")
                ),
            );
        }
    };
    for required in ["--work-order", "--attempt"] {
        if !values.contains_key(required) {
            return usage_error(
                &command,
                RESOLVE_USAGE,
                &format!("Missing option '{required}'."),
            );
        }
    }
    let records = match discover(&options, &command, RESOLVE_USAGE) {
        Ok(records) => records,
        Err(code) => return code,
    };
    let role = match role_ref(&command, token, &records) {
        Ok(role) => role,
        Err(code) => return code,
    };
    let definitions: Vec<models::DefinitionSource> = records
        .iter()
        .filter(|r| r.role.as_ref() == Some(&role))
        .filter_map(|r| r.definition.as_ref())
        .filter_map(|d| models::definition_source(d).ok())
        .collect();
    let snapshot_revision = values.get("--snapshot").cloned().unwrap_or_else(|| {
        definitions
            .first()
            .map_or("unavailable".into(), |d| d.snapshot_revision.clone())
    });
    let capability_facts = match read_models(
        &command,
        values.get("--capabilities"),
        models::capability_fact,
    ) {
        Ok(facts) => facts,
        Err(code) => return code,
    };
    let context_catalog =
        match read_models(&command, values.get("--context"), models::context_reference) {
            Ok(catalog) => catalog,
            Err(code) => return code,
        };
    let work_order = models::work_order_ref(
        &json!({
            "node_id": values["--work-order"],
            "attempt_id": values["--attempt"],
            "role_id": role.id,
        }),
        "WorkOrderRef",
    );
    let work_order: WorkOrderRef = match work_order {
        Ok(work_order) => work_order,
        Err(message) => {
            return usage_error(
                &command,
                RESOLVE_USAGE,
                &format!("Invalid value for --work-order: {message}"),
            )
        }
    };
    let request = resolver::Request {
        role: role.clone(),
        definitions,
        capability_facts,
        context_catalog,
        work_order,
        clock: chrono::Utc::now().fixed_offset(),
        snapshot_revision,
        bounds: models::ContextBundleBounds {
            max_references: 32,
            max_bytes: 10_000_000,
            max_sensitivity: "sensitive".into(),
            max_combinations: 4096,
        },
        unchecked: records.iter().map(Unchecked::from).collect(),
    };
    let resolution = resolver::resolve(&request);
    if options.json {
        let sorted = crate::backlog::decisions_cli::sort_keys(&resolution.to_json());
        println!("{}", python_json(&sorted));
    } else {
        match &resolution {
            Resolution::Blocked(blocked) => {
                println!(
                    "blocked role={}/{} reason={} source={}",
                    role.function_id,
                    role.id,
                    blocked.reason,
                    blocked.source_id.as_deref().unwrap_or("<unavailable>")
                );
                if let Some(detail) = blocked.detail.as_deref().filter(|d| !d.is_empty()) {
                    println!("  detail: {detail}");
                }
            }
            Resolution::Resolved(value) => {
                let text = |v: &Value, key: &str| v[key].as_str().unwrap_or_default().to_string();
                println!(
                    "resolved role={}/{} manifest={} context={}",
                    role.function_id,
                    role.id,
                    text(value, "manifest_digest"),
                    text(&value["context_bundle"], "digest")
                );
                for source in value["source_chain"].as_array().into_iter().flatten() {
                    println!(
                        "  {} {} {} status={}",
                        text(source, "layer"),
                        text(source, "source_id"),
                        text(source, "disposition"),
                        text(source, "status")
                    );
                }
            }
        }
    }
    match resolution {
        Resolution::Blocked(_) => 1,
        Resolution::Resolved(_) => 0,
    }
}

/// Emit one honest `ContextReference` per configured artifact, stamped
/// with `--snapshot` or the catalog's own revision.
fn context(mut options: GroupOptions, tail: &[String]) -> i32 {
    let command = format!("{GROUP} context");
    let mut snapshot: Option<String> = None;
    let mut extras: Vec<&str> = Vec::new();
    let mut options_done = false;
    let mut i = 0;
    while i < tail.len() {
        let arg = tail[i].as_str();
        i += 1;
        if options_done || !arg.starts_with('-') || arg == "-" {
            extras.push(arg);
            continue;
        }
        let (flag, inline) = match arg.split_once('=') {
            Some((flag, value)) if flag.starts_with("--") => (flag, Some(value.to_string())),
            _ => (arg, None),
        };
        match flag {
            "--" => options_done = true,
            "--json" | "-J" => options.json = true,
            "--help" => {
                print!("{CONTEXT_HELP}");
                return 0;
            }
            "--snapshot" => {
                let value = match inline {
                    Some(value) => value,
                    None => match tail.get(i) {
                        Some(value) => {
                            i += 1;
                            value.clone()
                        }
                        None => {
                            return usage_error(
                                &command,
                                "[OPTIONS]",
                                "Option '--snapshot' requires an argument.",
                            )
                        }
                    },
                };
                snapshot = Some(value);
            }
            _ => return usage_error(&command, "[OPTIONS]", &format!("No such option: {arg}")),
        }
    }
    if !extras.is_empty() {
        let plural = if extras.len() > 1 { "s" } else { "" };
        return usage_error(
            &command,
            "[OPTIONS]",
            &format!(
                "Got unexpected extra argument{plural} ({})",
                extras.join(" ")
            ),
        );
    }
    let root = context::repo_root();
    let artifacts = match context::configured_artifacts(&root) {
        Ok(artifacts) => artifacts,
        Err(message) => {
            eprintln!("{command}: invalid config: {message}");
            return 1;
        }
    };
    let revision = snapshot
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| context::catalog_revision(&artifacts));
    let references = context::build_catalog(&artifacts, &revision, &root);
    if options.json {
        let rows: Vec<Value> = references
            .iter()
            .map(models::ContextReference::to_json)
            .collect();
        let sorted = crate::backlog::decisions_cli::sort_keys(&Value::Array(rows));
        println!("{}", python_json(&sorted));
        return 0;
    }
    if references.is_empty() {
        println!(
            "no artifacts configured ([context.artifacts] in .fno/config.toml); revision={revision}"
        );
        return 0;
    }
    for reference in &references {
        let state = if reference.readable {
            let digest = reference
                .content_digest
                .as_deref()
                .map_or("(none)", |d| &d[..12]);
            format!("ok digest={digest}")
        } else {
            let reason = reference.unavailable_reason.as_deref().unwrap_or_default();
            format!("unreadable: {reason}")
        };
        println!(
            "{} sensitivity={} bytes={} {state} revision={revision}",
            reference.identifier, reference.sensitivity, reference.byte_size
        );
    }
    0
}
