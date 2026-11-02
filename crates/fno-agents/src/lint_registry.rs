//! The `fno doctor lint registry` and `field-coverage` checks, read through
//! the fno-agents binary's registry store (`status --lint-registry`). The
//! Python legs died with `cli/src/fno/lint_cli.py`; the liveness legs and
//! the transcript resolution reuse the crate's own readers (footprint_cause,
//! session_truth), so no second resolver exists. Recorded departure on
//! field-coverage --live: the projected-row key set is the schema's
//! required + derived keys, where the Python leg read its own
//! serialize_entry projection.

use crate::paths::AgentsHome;
use crate::session_truth::{resolve_session, session_transcript, Stores};
use serde_json::{json, Value};
use std::path::Path;

/// `status --lint-registry [--json]` / `--lint-field-coverage [--live] [--json]`
pub fn run(op: &str, rest: &[String], home: &AgentsHome) -> i32 {
    let json = rest.iter().any(|a| a == "--json" || a == "-J");
    match op {
        "--lint-registry" => run_registry(rest, home, json),
        "--lint-field-coverage" => {
            let live = rest.iter().any(|a| a == "--live");
            run_field_coverage(rest, home, json, live)
        }
        _ => {
            eprintln!("lint-registry: unknown op {op}");
            2
        }
    }
}

/// The registry check: three legs per row, three buckets, exit 1 when a row
/// has no handle recorded or nothing resolves. Read-only.
fn run_registry(rest: &[String], home: &AgentsHome, json: bool) -> i32 {
    let _ = rest;
    let path = home.registry_json();
    let entries = crate::state::load_registry(&path).unwrap_or_default();
    let stores = Stores::ambient();
    let mut no_handle: Vec<String> = Vec::new();
    let mut unresolvable: Vec<(String, Vec<&str>)> = Vec::new();
    let mut ok_count = 0usize;
    for e in &entries {
        let mut legs_recorded: Vec<&str> = Vec::new();
        let mut legs_resolved: Vec<&str> = Vec::new();
        if let Some(pid) = e.pid {
            if let Some(start) = e.pid_start_time {
                legs_recorded.push("pid");
                if crate::footprint_cause::pid_alive(pid, Some(start)) == Some(true) {
                    legs_resolved.push("pid");
                }
            }
        }
        if let Some(log) = &e.log_path {
            if !log.is_empty() {
                legs_recorded.push("log_path");
                if Path::new(log).exists() {
                    legs_resolved.push("log_path");
                }
            }
        }
        let harness = e.harness.as_deref().unwrap_or("");
        let sid = e.harness_session_id.as_deref().unwrap_or("");
        if !harness.is_empty() && !sid.is_empty() {
            legs_recorded.push("harness");
            let resolved = resolve_session(None, sid, &stores)
                .and_then(|s| session_transcript(&s, &stores))
                .is_some();
            if resolved {
                legs_resolved.push("harness");
            }
        }
        if legs_recorded.is_empty() {
            no_handle.push(e.name.clone());
        } else if legs_resolved.is_empty() {
            unresolvable.push((e.name.clone(), legs_recorded.clone()));
        } else {
            ok_count += 1;
        }
    }
    let total = entries.len();
    if json {
        let no_handle_json: Vec<Value> =
            no_handle.iter().map(|name| json!({"name": name})).collect();
        let unresolvable_json: Vec<Value> = unresolvable
            .iter()
            .map(|(name, legs)| json!({"name": name, "legs_recorded": legs}))
            .collect();
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "total": total,
                "no_handle_recorded": no_handle_json,
                "recorded_but_unresolvable": unresolvable_json,
                "ok": ok_count,
            }))
            .unwrap_or_else(|_| "{}".to_string())
        );
    } else {
        for name in &no_handle {
            println!("no handle recorded: {name}");
        }
        for (name, legs) in &unresolvable {
            println!(
                "recorded but unresolvable: {name} (recorded: {})",
                legs.join(", ")
            );
        }
        println!(
            "fno doctor lint registry: {} no handle recorded, {} recorded but unresolvable, {} ok, {} total",
            no_handle.len(),
            unresolvable.len(),
            ok_count,
            total
        );
    }
    if no_handle.is_empty() && unresolvable.is_empty() {
        0
    } else {
        1
    }
}

/// The field-coverage check. Source half: the AgentEntry dataclass's declared
/// fields against the schema's accounting blocks. Live half (--live): the
/// persisted rows' zero-population fields over the same schema, projected
/// keys = required + derived (the recorded departure).
fn run_field_coverage(rest: &[String], home: &AgentsHome, json: bool, live: bool) -> i32 {
    let _ = rest;
    let root = match repo_root_for_lint() {
        Some(r) => r,
        None => {
            eprintln!("field-coverage: no repo root resolved");
            return 2;
        }
    };
    let source = source_field_coverage(&root);
    let exit_source = match source.status.as_str() {
        "unmeasured" => 2,
        "uncovered" => 1,
        _ => 0,
    };
    if !live {
        return emit_source(&source, json);
    }
    if exit_source == 2 {
        return emit_source(&source, json);
    }
    let schema = read_schema(&root);
    let contract = population_contract(schema.as_ref());
    let live_result = live_field_coverage(home, &schema, &contract);
    let exit_live = match live_result.status.as_str() {
        "unmeasured" => 2,
        "findings" => 1,
        _ => 0,
    };
    emit_both(&source, &live_result, json)
}

fn source_field_coverage(root: &Path) -> SourceReport {
    let registry_py = root.join("cli/src/fno/agents/registry.py");
    let schema_path = root.join("schemas/agents-list-row.json");
    let (Ok(body), Ok(schema_text)) = (
        std::fs::read_to_string(&registry_py),
        std::fs::read_to_string(&schema_path),
    ) else {
        return SourceReport {
            status: "unmeasured".into(),
            detail: format!("unreadable: {}", registry_py.display()),
            declared: Vec::new(),
            required: Vec::new(),
            known_gaps: Vec::new(),
            uncovered: Vec::new(),
        };
    };
    let schema: Value = serde_json::from_str(&schema_text).unwrap_or(Value::Null);
    let declared = declared_entry_fields(&body);
    let field = |key: &str| schema_keys(&schema, key);
    let required = field("required");
    let mut accounted: Vec<String> = Vec::new();
    for key in [
        "required",
        "projection_omissions",
        "removed",
        "rust_only",
        "python_only",
        "storage_only",
    ] {
        for name in field(key) {
            if !accounted.contains(&name) {
                accounted.push(name.clone());
            }
        }
    }
    let gaps = schema.get("known_gaps");
    let known_gaps: Vec<String> = match gaps {
        Some(Value::Object(map)) => {
            let mut names: Vec<String> = Vec::new();
            for name in map.keys() {
                if is_valid_gap(name, map.get(name)) {
                    names.push(name.clone());
                }
            }
            names
        }
        _ => Vec::new(),
    };
    for gap in &known_gaps {
        if !accounted.contains(gap) {
            accounted.push(gap.clone());
        }
    }
    if required.len() < 40 || declared.len() < 40 {
        return SourceReport {
            status: "unmeasured".into(),
            detail: format!(
                "positive-control floor failed: {} required keys and {} declared fields",
                required.len(),
                declared.len()
            ),
            declared,
            required,
            known_gaps,
            uncovered: Vec::new(),
        };
    }
    let uncovered: Vec<String> = declared
        .iter()
        .filter(|d| !accounted.contains(d))
        .cloned()
        .collect();
    SourceReport {
        status: if uncovered.is_empty() {
            "ok".into()
        } else {
            "uncovered".into()
        },
        detail: String::new(),
        declared,
        required,
        known_gaps,
        uncovered,
    }
}

struct SourceReport {
    status: String,
    detail: String,
    declared: Vec<String>,
    required: Vec<String>,
    known_gaps: Vec<String>,
    uncovered: Vec<String>,
}

/// `class AgentEntry` block's annotated names, scanned off the lines.
fn declared_entry_fields(body: &str) -> Vec<String> {
    let mut fields: Vec<String> = Vec::new();
    let mut inside = false;
    for line in body.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("class AgentEntry") {
            inside = true;
            continue;
        }
        if !inside {
            continue;
        }
        if trimmed.starts_with("class ") && !trimmed.starts_with("class AgentEntry") {
            break;
        }
        let plain = line.trim_end();
        if plain.starts_with("    ") && !plain.starts_with("        ") {
            let rest = plain.trim_start();
            if let Some(colon) = rest.find(':') {
                let name = rest[..colon].trim();
                let is_name = !name.is_empty()
                    && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
                    && name
                        .chars()
                        .next()
                        .map(|c| c.is_ascii_lowercase() || c == '_')
                        .unwrap_or(false);
                let is_annot =
                    rest[colon + 1..].contains('=') || rest[colon + 1..].trim_end().len() > 0;
                if is_name && is_annot && !rest[colon + 1..].trim().is_empty() {
                    if !fields.iter().any(|f| f == name) {
                        fields.push(name.to_string());
                    }
                }
            }
        }
    }
    fields
}

/// The string names under one schema block: a list, or an object's `keys`.
fn schema_keys(schema: &Value, key: &str) -> Vec<String> {
    let block = schema.get(key);
    match block {
        Some(Value::Array(items)) => items
            .iter()
            .filter_map(|v| v.as_str().map(str::to_string))
            .collect(),
        Some(Value::Object(map)) => match map.get("keys") {
            Some(Value::Array(items)) => items
                .iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect(),
            _ => map
                .keys()
                .filter(|k| !k.starts_with('$'))
                .cloned()
                .collect(),
        },
        _ => Vec::new(),
    }
}

/// A known_gaps entry is coverage when its detail carries exactly one
/// wellformed owner id and no newline.
fn is_valid_gap(name: &str, detail: Option<&Value>) -> bool {
    let _ = name;
    let Some(Value::String(detail)) = detail else {
        return false;
    };
    if detail.contains('\n') {
        return false;
    }
    let owner = regex::Regex::new(r"\b[a-z][a-z0-9]*-[0-9a-f]+\b").unwrap();
    owner.find(detail).is_some()
}

struct LiveReport {
    status: String,
    detail: String,
    anchors: Value,
    contract_errors: Vec<String>,
    persisted_total: usize,
    persisted_dead: Vec<String>,
    projected_total: usize,
    projected_dead: Vec<String>,
}

fn read_schema(root: &Path) -> Option<Value> {
    let text = std::fs::read_to_string(root.join("schemas/agents-list-row.json")).ok()?;
    serde_json::from_str(&text).ok()
}

/// The population_contract block as (name, mode, surface) triples plus the
/// per-entry errors.
fn population_contract(schema: Option<&Value>) -> (Vec<(String, String, String)>, Vec<String>) {
    let mut entries = Vec::new();
    let mut errors: Vec<String> = Vec::new();
    let Some(Value::Object(block)) = schema.map(|s| s.get("population_contract")).flatten() else {
        return (entries, errors);
    };
    for (name, entry) in block {
        if name.starts_with('$') {
            continue;
        }
        let mode = entry.get("mode").and_then(Value::as_str).unwrap_or("");
        let surface = entry.get("surface").and_then(Value::as_str).unwrap_or("");
        if mode != "conditional" && mode != "transient" {
            errors.push(format!("{name}: mode must be conditional or transient"));
            continue;
        }
        if surface != "persisted" && surface != "projected" && surface != "persisted_and_projected"
        {
            errors.push(format!(
                "{name}: surface must be persisted, projected, or persisted_and_projected"
            ));
            continue;
        }
        entries.push((name.clone(), mode.to_string(), surface.to_string()));
    }
    (entries, errors)
}

fn live_field_coverage(
    home: &AgentsHome,
    schema: &Option<Value>,
    contract: &(Vec<(String, String, String)>, Vec<String>),
) -> LiveReport {
    let rows = crate::state::load_registry(&home.registry_json()).unwrap_or_default();
    if rows.is_empty() {
        return LiveReport {
            status: "unmeasured".into(),
            detail: "zero persisted rows".into(),
            anchors: Value::Null,
            contract_errors: Vec::new(),
            persisted_total: 0,
            persisted_dead: Vec::new(),
            projected_total: 0,
            projected_dead: Vec::new(),
        };
    }
    let total = rows.len();
    let anchor_populated = |name: &str| -> usize {
        rows.iter()
            .filter(|r| match name {
                "name" => !r.name.is_empty(),
                "created_at" => !r.created_at.is_empty(),
                "harness" => r.harness.as_deref().unwrap_or("").len() > 0,
                "status" => true,
                _ => false,
            })
            .count()
    };
    let mut anchors = serde_json::Map::new();
    let mut failed: Vec<String> = Vec::new();
    for name in ["name", "created_at", "harness", "status"] {
        let set = anchor_populated(name);
        anchors.insert(name.to_string(), json!({"set": set, "total": total}));
        if set != total {
            failed.push(name.to_string());
        }
    }
    if !failed.is_empty() {
        return LiveReport {
            status: "unmeasured".into(),
            detail: format!("anchor fields unset: {}", failed.join(", ")),
            anchors: Value::Object(anchors),
            contract_errors: contract.1.clone(),
            persisted_total: total,
            persisted_dead: Vec::new(),
            projected_total: total,
            projected_dead: Vec::new(),
        };
    }
    let persisted_names: Vec<String> = declared_row_names();
    let projected_keys: Vec<String> = projected_row_keys(schema);
    let persisted_dead: Vec<String> = persisted_names
        .into_iter()
        .filter(|name| dead_field(&rows, name))
        .collect();
    let projected_dead: Vec<String> = projected_keys
        .into_iter()
        .filter(|name| dead_field(&rows, name))
        .collect();
    let findings =
        !persisted_dead.is_empty() || !projected_dead.is_empty() || !contract.1.is_empty();
    LiveReport {
        status: if findings {
            "findings".into()
        } else {
            "ok".into()
        },
        detail: String::new(),
        anchors: Value::Object(anchors),
        contract_errors: contract.1.clone(),
        persisted_total: total,
        persisted_dead,
        projected_total: total,
        projected_dead,
    }
}

/// The persisted field names, from the RegistryEntry serde shape: read one
/// live row's keys and fall back to the schema's stored block. The Python
/// leg read its dataclass `fields()`; the row keys are the same surface.
fn declared_row_names() -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    let home = AgentsHome::from_env();
    let text = std::fs::read_to_string(home.registry_json()).unwrap_or_default();
    if let Ok(Value::Array(rows)) = serde_json::from_str::<Value>(&text) {
        if let Some(first) = rows.first() {
            if let Value::Object(map) = first {
                for key in map.keys() {
                    names.push(key.clone());
                }
            }
        }
    }
    names
}

/// The projected keys: the schema's required + derived + rust_only +
/// python_only names (the recorded departure from serialize_entry).
fn projected_row_keys(schema: &Option<Value>) -> Vec<String> {
    let mut keys: Vec<String> = Vec::new();
    let Some(schema) = schema else {
        return keys;
    };
    for block in ["required", "derived", "rust_only", "python_only"] {
        for name in schema_keys(schema, block) {
            if !keys.contains(&name) {
                keys.push(name);
            }
        }
    }
    keys
}

/// A field is dead when no row populates it (non-null, non-empty string,
/// non-empty list, non-empty object).
fn dead_field(rows: &[crate::state::RegistryEntry], name: &str) -> bool {
    let mut populated = 0usize;
    for row in rows {
        let value = serde_json::to_value(row).unwrap_or(Value::Null);
        let field = value.get(name);
        let live = match field {
            None | Some(Value::Null) => false,
            Some(Value::String(s)) => !s.is_empty(),
            Some(Value::Array(a)) => !a.is_empty(),
            Some(Value::Object(o)) => !o.is_empty(),
            Some(Value::Bool(_)) | Some(Value::Number(_)) => true,
        };
        if live {
            populated += 1;
        }
    }
    populated == 0
}

/// The source-only print (no --live): JSON or the human lines.
fn emit_source(source: &SourceReport, json: bool) -> i32 {
    let exit_code = match source.status.as_str() {
        "unmeasured" => 2,
        "uncovered" => 1,
        _ => 0,
    };
    if json {
        let payload = json!({
            "status": source.status,
            "source": {
                "status": source.status,
                "detail": source.detail,
                "declared_count": source.declared.len(),
                "required_count": source.required.len(),
                "accounted_count": source.declared.len() - source.uncovered.len(),
                "known_gaps": source.known_gaps,
                "uncovered_fields": source.uncovered,
            },
        });
        println!(
            "{}",
            serde_json::to_string_pretty(&payload).unwrap_or_else(|_| "{}".into())
        );
        return exit_code;
    }
    if source.status == "unmeasured" {
        println!(
            "fno doctor lint field-coverage: UNMEASURED: {}",
            source.detail
        );
    } else {
        println!(
            "fno doctor lint field-coverage: {} declared fields accounted for",
            source.declared.len() - source.uncovered.len()
        );
        for gap in &source.known_gaps {
            println!("known gap: {gap}");
        }
        for field in &source.uncovered {
            println!(
                "unaccounted field: {field}; add it to required and project it, \
                 add it to storage_only, or add it to known_gaps with an owning node"
            );
        }
    }
    exit_code
}

/// The source + live print. JSON folds both payloads; human prints the
/// source lines then the live readings.
fn emit_both(source: &SourceReport, live: &LiveReport, json: bool) -> i32 {
    let exit_live = match live.status.as_str() {
        "unmeasured" => 2,
        "findings" => 1,
        _ => 0,
    };
    if json {
        let payload = json!({
            "status": live.status,
            "source": {
                "status": source.status,
                "declared_count": source.declared.len(),
                "required_count": source.required.len(),
                "accounted_count": source.declared.len() - source.uncovered.len(),
                "known_gaps": source.known_gaps,
                "uncovered_fields": source.uncovered,
            },
            "detail": live.detail,
            "anchors": live.anchors,
            "contract_errors": live.contract_errors,
            "persisted": {
                "total": live.persisted_total,
                "dead_fields": live.persisted_dead,
            },
            "projected": {
                "total": live.projected_total,
                "dead_fields": live.projected_dead,
            },
        });
        println!(
            "{}",
            serde_json::to_string_pretty(&payload).unwrap_or_else(|_| "{}".into())
        );
    } else {
        let _ = emit_source(source, false);
        if live.status == "unmeasured" {
            println!("live field coverage: UNMEASURED: {}", live.detail);
        } else {
            for error in &live.contract_errors {
                println!("population contract error: {error}");
            }
            println!("persisted dead fields: {}", live.persisted_dead.join(", "));
            println!("projected dead fields: {}", live.projected_dead.join(", "));
        }
    }
    exit_live
}

/// The repo root for the source scan: FNO_REPO_ROOT, then git rev-parse
/// from the current dir, then the cwd.
fn repo_root_for_lint() -> Option<std::path::PathBuf> {
    if let Some(pin) = std::env::var_os("FNO_REPO_ROOT").filter(|v| !v.is_empty()) {
        return Some(std::path::PathBuf::from(pin));
    }
    let cwd = std::env::current_dir().ok()?;
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(&cwd)
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .ok()?;
    if out.status.success() {
        let top = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if !top.is_empty() {
            return Some(std::path::PathBuf::from(top));
        }
    }
    Some(cwd)
}
