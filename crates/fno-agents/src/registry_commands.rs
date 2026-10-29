//! `fno agents whoami` and `fno agents registry-repair`: two registry-backed
//! commands, native since their Python fronts were retired. Both are read leaves
//! over the registry the daemon reads; neither starts or asks the daemon.
//! The router rewrites each into an argument of the `status` action
//! (`status --whoami`, `status --registry-repair`), because the binary's
//! action list is shrink-only.

use crate::paths::AgentsHome;
use crate::spawn_context::{resolve_self_identity, OwnedDisposition};
use serde_json::{Map, Value};
use std::io::IsTerminal;
use std::path::{Path, PathBuf};

/// "Ran fine, but you are not a registered mesh agent": distinct from the
/// usage error (2) and a generic failure (1) so a caller can branch on it.
const EXIT_NOT_REGISTERED: i32 = 3;

fn nonempty(value: Option<String>) -> Option<String> {
    value
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

fn str_of(row: &Value, key: &str) -> Option<String> {
    row.get(key)
        .and_then(Value::as_str)
        .filter(|v| !v.is_empty())
        .map(str::to_string)
}

/// `FNO_AGENT_HARNESS`, else the pre-cutover `FNO_AGENT_PROVIDER` with its
/// one rename line on stderr.
fn harness_from_env() -> Option<String> {
    if let Some(v) = nonempty(std::env::var("FNO_AGENT_HARNESS").ok()) {
        return Some(v);
    }
    let legacy = nonempty(std::env::var("FNO_AGENT_PROVIDER").ok())?;
    eprintln!(
        "FNO_AGENT_PROVIDER is the pre-cutover name for FNO_AGENT_HARNESS; \
this worker predates the rename. New spawns set FNO_AGENT_HARNESS."
    );
    Some(legacy)
}

/// The row a session id names. A known harness scopes the match through the
/// one matcher `fno whoami` uses; an unknown harness keeps the claude-shaped
/// scan, with the short-id prefix pass still claude-only.
fn find_row<'a>(rows: &'a [Value], session: &str, harness: Option<&str>) -> Option<&'a Value> {
    if let Some(h) = harness {
        return crate::whoami::find_by_session(rows, session, h);
    }
    let exact = rows.iter().find(|row| {
        ["harness_session_id", "cc_session_id"]
            .iter()
            .any(|key| row.get(*key).and_then(Value::as_str) == Some(session))
    });
    if exact.is_some() {
        return exact;
    }
    let norm = session.replace('-', "").to_ascii_lowercase();
    rows.iter().find(|row| {
        let sid = row
            .get("short_id")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .replace('-', "")
            .to_ascii_lowercase();
        row.get("harness").and_then(Value::as_str) == Some("claude")
            && !sid.is_empty()
            && norm.starts_with(&sid)
    })
}

/// The resume-target id: claude's transport key when the row carries one,
/// else the canonical `harness_session_id`.
fn row_session_id(row: &Value) -> Option<String> {
    if row.get("harness").and_then(Value::as_str) == Some("claude") {
        if let Some(short) = str_of(row, "short_id") {
            return Some(short);
        }
    }
    str_of(row, "harness_session_id")
}

/// The one role rendering: `L{level} {scope} (by {grantor})`.
pub(crate) fn role_text(row: &Value) -> Option<String> {
    let level = row.get("role_level").and_then(Value::as_u64)?;
    let scope = str_of(row, "role_scope").unwrap_or_else(|| "?".to_string());
    let grantor = str_of(row, "role_grantor").unwrap_or_else(|| "human".to_string());
    Some(format!("L{level} {scope} (by {grantor})"))
}

/// claude's roster state on the output vocabulary; an unmapped value passes.
fn live_status_word(state: &str) -> String {
    match state.trim().to_ascii_lowercase().as_str() {
        "working" | "busy" | "live" | "spawning" | "restarting" => "Working".into(),
        "blocked" | "needs input" => "Needs input".into(),
        "idle" | "ready" => "Idle".into(),
        "done" | "failed" | "stopped" => "Done".into(),
        _ => state.to_string(),
    }
}

/// The first `<key>: <value>` line's value, with one matched quote pair
/// stripped.
pub(crate) fn scan_field(text: &str, key: &str) -> Option<String> {
    let prefix = format!("{key}:");
    for line in text.lines() {
        let Some(rest) = line.strip_prefix(&prefix) else {
            continue;
        };
        let Some(value) = rest.split_whitespace().next() else {
            continue;
        };
        let bytes = value.as_bytes();
        let quoted = bytes.len() >= 2
            && bytes[0] == bytes[bytes.len() - 1]
            && (bytes[0] == b'"' || bytes[0] == b'\'');
        return Some(if quoted {
            value[1..value.len() - 1].to_string()
        } else {
            value.to_string()
        });
    }
    None
}

/// The node this worker holds, as `node:<id>`, only when the cwd manifest's
/// claude session id proves it is this process's.
fn find_held_node(session: Option<&str>) -> Option<String> {
    let session = session?;
    let text = std::fs::read_to_string(Path::new(".fno").join("target-state.md")).ok()?;
    let owner = scan_field(&text, "claude_session_id")
        .or_else(|| scan_field(&text, "claude_transcript_id"))?;
    if owner != session {
        return None;
    }
    scan_field(&text, "graph_node_id")
        .filter(|v| !v.eq_ignore_ascii_case("null"))
        .map(|v| format!("node:{v}"))
}

fn opt(value: &Option<String>) -> Value {
    value.clone().map(Value::String).unwrap_or(Value::Null)
}

pub fn run_whoami(args: &[String], home: &AgentsHome) -> i32 {
    let mut json_out = false;
    for a in args {
        match a.as_str() {
            "--json" | "-J" => json_out = true,
            "-h" | "--help" => {
                println!("usage: fno agents whoami [--json]");
                return 0;
            }
            other => {
                eprintln!("whoami: unknown argument {other:?}");
                return 2;
            }
        }
    }
    let mut warnings: Vec<String> = Vec::new();
    let rows =
        match crate::client_commands::load_registry_entries(&AgentsHome::shared_registry_json()) {
            Ok(rows) => rows,
            Err(e) => {
                warnings.push(format!("registry unreadable, enrichment skipped: {e}"));
                Vec::new()
            }
        };
    let get = |name: &str| std::env::var(name).ok();
    let ident = resolve_self_identity(&get, None, None, home);
    let agent_self = nonempty(std::env::var("FNO_AGENT_SELF").ok());
    if agent_self.is_none() && matches!(ident.disposition, OwnedDisposition::Ambiguous) {
        eprintln!("error: {}", crate::register::ambiguity_message(&ident));
        return 4;
    }
    let session_uuid = nonempty(ident.session_id.clone());
    // The harness scopes a match only beside a resolved session id: a
    // launcher stamp alone must not scope an env-tier lookup.
    let harness = session_uuid
        .as_ref()
        .map(|_| nonempty(ident.harness.clone()).unwrap_or_else(|| "claude".to_string()));

    let env_provider = harness_from_env();
    let env_session = nonempty(std::env::var("FNO_AGENT_SESSION").ok());
    let mut found: Option<(&str, &Value)> = None;
    for (via, token) in [("env", &env_session), ("session-fallback", &session_uuid)] {
        if let Some(token) = token {
            if let Some(row) = find_row(&rows, token, harness.as_deref()) {
                found = Some((via, row));
                break;
            }
        }
    }

    let mut fields: Map<String, Value> = Map::new();
    let (registered, human) = match found {
        None => {
            for key in [
                "live_status",
                "name",
                "node",
                "provider",
                "registered",
                "resolved_via",
                "role",
                "session",
                "short_id",
                "status",
            ] {
                fields.insert(key.to_string(), Value::Null);
            }
            fields.insert("registered".into(), Value::Bool(false));
            (false, String::new())
        }
        Some((via, row)) => {
            let name = str_of(row, "name");
            let provider = env_provider.clone().or_else(|| str_of(row, "harness"));
            let session = env_session.clone().or_else(|| row_session_id(row));
            let short_id = str_of(row, "short_id");
            let status = str_of(row, "status");
            let mut live_status = None;
            if provider.as_deref() == Some("claude") {
                if let Some(short) = &short_id {
                    let snapshot = crate::claude_roster::read_all_agents();
                    let text = snapshot.warning_text();
                    if !text.is_empty() {
                        warnings.push(text);
                    }
                    live_status = snapshot
                        .find(short)
                        .and_then(|r| r.state.as_deref())
                        .map(live_status_word);
                }
            }
            let node = find_held_node(session_uuid.as_deref());
            let role = role_text(row);
            let mut human = format!("name:        {}", name.clone().unwrap_or_default());
            for (label, value) in [
                ("provider:    ", &provider),
                ("session:     ", &session),
                ("short_id:    ", &short_id),
                ("status:      ", &status),
                ("live_status: ", &live_status),
                ("node:        ", &node),
                ("role:       ", &role),
            ] {
                if let Some(v) = value {
                    human.push_str(&format!("\n{label}{v}"));
                }
            }
            // Sorted keys: the JSON render matches the Python json.dumps
            // sort_keys output whatever serde_json's map ordering is.
            fields.insert("live_status".into(), opt(&live_status));
            fields.insert("name".into(), opt(&name));
            fields.insert("node".into(), opt(&node));
            fields.insert("provider".into(), opt(&provider));
            fields.insert("registered".into(), Value::Bool(true));
            fields.insert("resolved_via".into(), Value::String(via.to_string()));
            fields.insert("role".into(), opt(&role));
            fields.insert("session".into(), opt(&session));
            fields.insert("short_id".into(), opt(&short_id));
            fields.insert("status".into(), opt(&status));
            (true, human)
        }
    };
    for warn in &warnings {
        eprintln!("WARN: {warn}");
    }
    if json_out || !std::io::stdout().is_terminal() {
        println!(
            "{}",
            serde_json::to_string_pretty(&Value::Object(fields)).unwrap_or_default()
        );
    } else if registered {
        println!("{human}");
    } else {
        eprintln!("not a registered mesh agent (human / top-level session)");
    }
    if registered {
        0
    } else {
        EXIT_NOT_REGISTERED
    }
}

// -------------------------------------------------------------------------
// registry-repair
// -------------------------------------------------------------------------

/// Python's `repr` of a scalar, for the refusal texts.
fn py_repr(value: Option<&Value>) -> String {
    match value {
        None | Some(Value::Null) => "None".into(),
        Some(Value::Bool(b)) => if *b { "True" } else { "False" }.into(),
        Some(Value::String(s)) => crate::client_commands::py_repr_str(s),
        Some(other) => other.to_string(),
    }
}

fn py_type_name(value: Option<&Value>) -> &'static str {
    match value {
        None | Some(Value::Null) => "NoneType",
        Some(Value::Bool(_)) => "bool",
        Some(Value::Number(n)) if n.is_f64() => "float",
        Some(Value::Number(_)) => "int",
        Some(Value::String(_)) => "str",
        Some(Value::Array(_)) => "list",
        Some(Value::Object(_)) => "dict",
    }
}

/// None and an empty string, list or object read as "the newer schema added
/// this field and nothing ever wrote it". Everything else is data, 0 and
/// false included.
fn is_empty_value(value: &Value) -> bool {
    match value {
        Value::Null => true,
        Value::String(s) => s.is_empty(),
        Value::Array(a) => a.is_empty(),
        Value::Object(o) => o.is_empty(),
        _ => false,
    }
}

/// The keys of `row` this fno's registry row does not model: whatever lands
/// in the row's catch-all on a read.
fn unknown_keys(row: &Value) -> Result<Vec<String>, String> {
    let entry: crate::state::RegistryEntry =
        serde_json::from_value(row.clone()).map_err(|e| e.to_string())?;
    let order: Vec<String> = row
        .as_object()
        .map(|o| o.keys().cloned().collect())
        .unwrap_or_default();
    Ok(order
        .into_iter()
        .filter(|k| entry.extra.contains_key(k))
        .collect())
}

struct RepairPlan {
    on_disk: i64,
    /// (row label, the empty unknown keys this repair discards), in row order.
    dropped: Vec<(String, Vec<String>)>,
    /// Per row index: the unknown keys to strip on apply.
    strip: Vec<Vec<String>>,
}

/// Decide the repair, or refuse. Pure: reads nothing, writes nothing.
fn plan_repair(raw: &Value, shown: &str, to: i64) -> Result<RepairPlan, String> {
    let Some(doc) = raw.as_object() else {
        return Err(format!(
            "refusing to repair {shown}: it is absent, empty, or not a JSON object. A torn \
file is damage, not a version to roll back, and this command never guesses at what the rows were."
        ));
    };
    let on_disk = match doc.get("schema_version") {
        Some(Value::Number(n)) if n.is_i64() => n.as_i64().unwrap_or_default(),
        other => {
            return Err(format!(
                "refusing to repair {shown}: schema_version is {}, not an integer.",
                py_repr(other)
            ))
        }
    };
    if on_disk <= to {
        return Err(format!(
            "refusing to repair {shown}: on-disk schema_version={on_disk} is not above the \
requested --to {to}. This command only rolls a version DOWN; raising one is the deployed writer's job."
        ));
    }
    let Some(agents) = doc.get("agents").and_then(Value::as_array) else {
        return Err(format!(
            "refusing to repair {shown}: 'agents' is {}, not a list.",
            py_type_name(doc.get("agents"))
        ));
    };
    let mut dropped: Vec<(String, Vec<String>)> = Vec::new();
    let mut strip: Vec<Vec<String>> = Vec::new();
    let mut carrying: Vec<String> = Vec::new();
    for (index, row) in agents.iter().enumerate() {
        let Some(fields) = row.as_object() else {
            return Err(format!(
                "refusing to repair {shown}: row {index} is not an object."
            ));
        };
        let mut name = fields
            .get("name")
            .and_then(Value::as_str)
            .map(str::to_string)
            .unwrap_or_else(|| format!("<row {index}>"));
        // A poisoned file can carry duplicate names, and the report is keyed
        // by name: without this the preview would name one row where two drop.
        if dropped.iter().any(|(n, _)| *n == name) {
            name = format!("{name} (row {index})");
        }
        let unknown = unknown_keys(row).map_err(|e| {
            format!(
                "refusing to repair {shown}: row {index} does not read as a registry row ({e})."
            )
        })?;
        let mut empty: Vec<String> = unknown
            .iter()
            .filter(|k| fields.get(k.as_str()).is_some_and(is_empty_value))
            .cloned()
            .collect();
        let mut real: Vec<String> = unknown
            .iter()
            .filter(|k| !empty.contains(*k))
            .cloned()
            .collect();
        empty.sort();
        real.sort();
        if !real.is_empty() {
            carrying.push(format!("{name}: {}", real.join(", ")));
        }
        if !empty.is_empty() {
            dropped.push((name, empty));
        }
        strip.push(unknown);
    }
    if !carrying.is_empty() {
        return Err(format!(
            "refusing to repair {shown}: rolling schema_version={on_disk} back to {to} would \
DISCARD data these rows carry - {}. The 2026-08-28 rollback was lossless only because every row \
happened to carry the newer fields empty; this is the assertion that ends that luck. Deploy the \
newer schema instead.",
            carrying.join("; ")
        ));
    }
    Ok(RepairPlan {
        on_disk,
        dropped,
        strip,
    })
}

fn repair(target: &Path, to: i64, apply: bool) -> Result<String, String> {
    let shown = target.display().to_string();
    let unreadable =
        |e: &dyn std::fmt::Display| format!("refusing to repair {shown}: could not read it ({e}).");
    let absolute = if target.is_absolute() {
        target.to_path_buf()
    } else {
        std::env::current_dir()
            .map_err(|e| unreadable(&e))?
            .join(target)
    };
    // `begin` holds the registry-wide lock for the whole read-decide-write,
    // so a live file is never repaired unlocked.
    let transaction = crate::registry_store::begin(&absolute).map_err(|e| unreadable(&e))?;
    let raw = transaction.document.clone();
    let plan = plan_repair(&raw, &shown, to)?;
    let action = if apply { "repaired" } else { "would repair" };
    let mut report = format!(
        "{action} {shown}: schema_version {} -> {to}\n",
        plan.on_disk
    );
    let mut sorted = plan.dropped.clone();
    sorted.sort_by(|a, b| a.0.cmp(&b.0));
    for (name, keys) in &sorted {
        report.push_str(&format!("  {name}: drop {} (empty)\n", keys.join(", ")));
    }
    if sorted.is_empty() {
        report.push_str("  no rows carry keys this fno does not know\n");
    }
    if !apply {
        report.push_str("  dry run; pass --apply to perform it\n");
        return Ok(report);
    }
    let stamp = chrono::Utc::now().format("%Y%m%dT%H%M%SZ");
    let file_name = absolute
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "registry.json".to_string());
    let backup_name = format!("{file_name}.bak.schema-repair-{stamp}");
    let backup = absolute.with_file_name(&backup_name);
    let pretty = serde_json::to_string_pretty(&raw)
        .map_err(|e| format!("refusing to repair {shown}: {e}"))?;
    std::fs::write(&backup, pretty).map_err(|e| format!("refusing to repair {shown}: {e}"))?;
    let rows: Vec<Value> = raw
        .get("agents")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .zip(plan.strip.iter())
        .map(|(mut row, unknown)| {
            if let Some(fields) = row.as_object_mut() {
                for key in unknown {
                    fields.remove(key);
                }
            }
            row
        })
        .collect();
    // Only the version and the rows change; every other top-level key stays.
    let mut repaired = raw.clone();
    if let Some(fields) = repaired.as_object_mut() {
        fields.insert("schema_version".to_string(), Value::from(to));
        fields.insert("agents".to_string(), Value::Array(rows));
    }
    // The stored document is AHEAD of this writer by definition, so the
    // repaired document is judged alone, as the registry-commit replace does.
    crate::state::validate_registry_document_change(&absolute, &repaired, &repaired)
        .map_err(|e| format!("refusing to repair {shown}: {e}"))?;
    transaction
        .commit(repaired)
        .map_err(|e| format!("refusing to repair {shown}: {e}"))?;
    let shown_backup = match target.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent.join(&backup_name),
        _ => PathBuf::from(&backup_name),
    };
    report.push_str(&format!("  backup: {}\n", shown_backup.display()));
    Ok(report)
}

pub fn run_registry_repair(args: &[String]) -> i32 {
    let mut to: Option<i64> = None;
    let mut path: Option<PathBuf> = None;
    let mut apply = false;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--to" => match it.next().and_then(|v| v.parse::<i64>().ok()) {
                Some(v) => to = Some(v),
                None => {
                    eprintln!("error: --to wants an integer schema version");
                    return 2;
                }
            },
            "--path" => match it.next() {
                Some(v) => path = Some(PathBuf::from(v)),
                None => {
                    eprintln!("error: --path wants a registry file");
                    return 2;
                }
            },
            "--apply" => apply = true,
            "-h" | "--help" => {
                println!("usage: fno agents registry-repair --to N [--path FILE] [--apply]");
                return 0;
            }
            other => {
                eprintln!("registry-repair: unknown argument {other:?}");
                return 2;
            }
        }
    }
    let Some(to) = to else {
        eprintln!("error: missing option '--to'");
        return 2;
    };
    let target = path.unwrap_or_else(AgentsHome::shared_registry_json);
    match repair(&target, to, apply) {
        Ok(report) => {
            print!("{report}");
            0
        }
        Err(message) => {
            eprintln!("{message}");
            1
        }
    }
}
