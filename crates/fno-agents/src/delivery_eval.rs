//! The `fno do delivery evaluate` command, native.
//!
//! One owner for plan-activated generic delivery evaluation: the plan
//! frontmatter read, the coherent event-journal snapshot, the producer-event
//! selection walk, the approval and effect adapters, and the requirement
//! evaluator. Python owned those legs before the port; the command's bytes are
//! frozen against goldens captured from the Python leg before its deletion
//! (`crates/fno-agents/tests/delivery_evaluate_parity.rs`).
//!
//! Recorded divergence: the Python leg wrapped model-level plan validation
//! failures in pydantic's rendered error, whose body embeds a versioned URL
//! and a truncated dict repr. The native command emits the same refusal sentence
//! bare inside the same undeterminable response, so every consumer verdict is
//! unchanged while the message text stays stable across pydantic releases.
//! Unreadable-file and journal diagnostics keep the exact Python spelling;
//! they carry no pydantic wrapper.

use chrono::{DateTime, FixedOffset};
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::path::Path;

const RESPONSE_VERSION: &str = "delivery-evaluate-response.v1";
const EVALUATOR_VERSION: &str = "delivery-evaluator.v1";
const FACT_VERSION: &str = "delivery-evidence-fact.v1";

const SUBJECT_KINDS: &[&str] = &[
    "artifact",
    "review",
    "approval",
    "probe",
    "acknowledgment",
    "deliverable",
    "effect",
    "observation",
];
const EVIDENCE_RESULTS: &[&str] = &["passed", "failed", "blocked", "unknown"];
const EFFECT_STATES: &[&str] = &[
    "prepared",
    "executing",
    "acknowledged",
    "failed",
    "blocked",
    "unknown",
];
const DIFFICULTY_REQUIRED_AFTER: (i32, u32, u32) = (2026, 8, 26);

fn utc_now() -> DateTime<FixedOffset> {
    chrono::Utc::now().with_timezone(&chrono::FixedOffset::east_opt(0).expect("zero offset"))
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

/// The `delivery-evaluate` transport command: `--plan-path` (required),
/// `--events` (default `.fno/events.jsonl`), `--json` / `-J`. Always exits 0
/// for an answered evaluation; the response status carries the verdict.
pub fn run(rest: &[String]) -> i32 {
    let mut plan_path: Option<String> = None;
    let mut events_path: Option<String> = None;
    let mut json_output = false;
    let mut it = rest.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--plan-path" => plan_path = it.next().cloned(),
            "--events" => events_path = it.next().cloned(),
            "--json" | "-J" => json_output = true,
            other => {
                eprintln!("fno-agents: delivery-evaluate: unknown argument: {other}");
                return 2;
            }
        }
    }
    let Some(plan_path) = plan_path else {
        eprintln!("error: --plan-path is required");
        return 2;
    };
    let events_path = events_path.unwrap_or_else(|| ".fno/events.jsonl".to_string());
    let response = evaluate_plan_delivery(Path::new(&plan_path), Path::new(&events_path));
    if json_output {
        println!("{}", write_compact_ensure_ascii(&response));
    } else {
        print_human(&response);
    }
    0
}

fn print_human(response: &Value) {
    let status = str_field(response, "status");
    match response.get("verdict") {
        None | Some(Value::Null) => println!("delivery: {status}"),
        Some(verdict) => {
            println!("delivery: {}", str_field(verdict, "aggregate"));
            for row in seq_field(verdict, "requirements") {
                if str_field(row, "result") == "passed" {
                    continue;
                }
                let items = seq_field(row, "diagnostics");
                let detail = if items.is_empty() {
                    "no diagnostic".to_string()
                } else {
                    items
                        .iter()
                        .map(|d| d.as_str().unwrap_or_default())
                        .collect::<Vec<_>>()
                        .join("; ")
                };
                println!(
                    "- {}: {} - {}",
                    str_field(row, "evidence_id"),
                    str_field(row, "result"),
                    detail
                );
            }
        }
    }
    for diagnostic in seq_field(response, "diagnostics") {
        println!("- {}", diagnostic.as_str().unwrap_or_default());
    }
}

fn str_field<'a>(value: &'a Value, key: &str) -> &'a str {
    value.get(key).and_then(Value::as_str).unwrap_or_default()
}

fn seq_field<'a>(value: &'a Value, key: &str) -> &'a Vec<Value> {
    static EMPTY: Vec<Value> = Vec::new();
    value.get(key).and_then(Value::as_array).unwrap_or(&EMPTY)
}

// ---------------------------------------------------------------------------
// Python-shaped JSON emission
// ---------------------------------------------------------------------------

/// Serialize `value` exactly as Python's `json.dumps(value,
/// separators=(",", ":"), ensure_ascii=True)` would: compact, insertion
/// order, `\uXXXX` escapes for every non-ASCII code point (surrogate pairs
/// beyond the BMP), and the five short control escapes where Python uses
/// them.
fn write_compact_ensure_ascii(value: &Value) -> String {
    let mut out = String::new();
    write_compact_value(value, &mut out);
    out
}

fn write_compact_value(value: &Value, out: &mut String) {
    match value {
        Value::Null => out.push_str("null"),
        Value::Bool(true) => out.push_str("true"),
        Value::Bool(false) => out.push_str("false"),
        Value::Number(n) => out.push_str(&n.to_string()),
        Value::String(s) => write_json_string(s, out),
        Value::Array(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_compact_value(item, out);
            }
            out.push(']');
        }
        Value::Object(map) => {
            out.push('{');
            for (i, (k, v)) in map.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_json_string(k, out);
                out.push(':');
                write_compact_value(v, out);
            }
            out.push('}');
        }
    }
}

/// Canonical form: `json.dumps(value, sort_keys=True, separators=(",", ":"))`
/// with the same ensure_ascii escaping. Keys sort by code point at every
/// level.
fn write_canonical_ensure_ascii(value: &Value, out: &mut String) {
    match value {
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            out.push('{');
            for (i, key) in keys.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_json_string(key, out);
                out.push(':');
                write_canonical_ensure_ascii(&map[*key], out);
            }
            out.push('}');
        }
        Value::Array(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_canonical_ensure_ascii(item, out);
            }
            out.push(']');
        }
        other => write_compact_value(other, out),
    }
}

fn write_json_string(s: &str, out: &mut String) {
    out.push('"');
    for ch in s.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\u{08}' => out.push_str("\\b"),
            '\u{0c}' => out.push_str("\\f"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c if (c as u32) < 0x7f => out.push(c),
            c => {
                let code = c as u32;
                if code <= 0xffff {
                    out.push_str(&format!("\\u{code:04x}"));
                } else {
                    let code = code - 0x1_0000;
                    let high = 0xd800 + (code >> 10);
                    let low = 0xdc00 + (code & 0x3ff);
                    out.push_str(&format!("\\u{high:04x}\\u{low:04x}"));
                }
            }
        }
    }
    out.push('"');
}

/// Render a raw JSON field the way an Python f-string would (`str(value)`):
/// absent reads "None", booleans read "True"/"False". Only scalar fields
/// reach an f-string in the diagnostics below.
fn py_str_raw(value: Option<&Value>) -> String {
    match value {
        None | Some(Value::Null) => "None".to_string(),
        Some(Value::Bool(true)) => "True".to_string(),
        Some(Value::Bool(false)) => "False".to_string(),
        Some(Value::Number(n)) => n.to_string(),
        Some(Value::String(s)) => s.clone(),
        Some(other) => other.to_string(),
    }
}

fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    let mut hex = String::with_capacity(64);
    for byte in hasher.finalize() {
        hex.push_str(&format!("{byte:02x}"));
    }
    hex
}

// ---------------------------------------------------------------------------
// Datetimes
// ---------------------------------------------------------------------------

fn parse_aware(value: &str) -> Option<DateTime<FixedOffset>> {
    if let Ok(parsed) = DateTime::parse_from_rfc3339(value) {
        return Some(parsed);
    }
    // pydantic's time parser accepts a space separator where RFC3339 does not.
    if let Some(position) = value.find(' ') {
        let mut spaced = value.to_string();
        spaced.replace_range(position..position + 1, "T");
        return DateTime::parse_from_rfc3339(&spaced).ok();
    }
    None
}

/// The selection walk's timestamp read: an event ts that fails to parse as an
/// aware datetime reads as absent, exactly like `fromisoformat` answering a
/// naive datetime.
fn event_datetime(event: &Value) -> Option<DateTime<FixedOffset>> {
    let text = event.get("ts").and_then(Value::as_str)?;
    parse_aware(&text.replace('Z', "+00:00"))
}

/// Python `datetime.isoformat()`: microseconds only when nonzero, offset as
/// `+HH:MM` (UTC spells `+00:00`, never `Z`).
fn py_isoformat(t: DateTime<FixedOffset>) -> String {
    let mut out = t.format("%Y-%m-%dT%H:%M:%S").to_string();
    let micros = t.timestamp_subsec_micros();
    if micros != 0 {
        out.push_str(&format!(".{micros:06}"));
    }
    out.push_str(&t.format("%:z").to_string());
    out
}

// ---------------------------------------------------------------------------
// Response shapes
// ---------------------------------------------------------------------------

fn response_value(
    status: &str,
    fact_revision: Option<String>,
    evidence_revision: Option<String>,
    verdict: Option<Value>,
    diagnostics: Vec<String>,
) -> Value {
    json!({
        "version": RESPONSE_VERSION,
        "status": status,
        "fact_revision": fact_revision,
        "evidence_revision": evidence_revision,
        "verdict": verdict,
        "diagnostics": diagnostics,
    })
}

fn undeterminable(diagnostic: String) -> Value {
    response_value("undeterminable", None, None, None, vec![diagnostic])
}

fn inactive_declaration() -> Value {
    response_value(
        "inactive",
        None,
        None,
        None,
        vec!["generic delivery requires a valid company work declaration".to_string()],
    )
}

// ---------------------------------------------------------------------------
// Datatypes
// ---------------------------------------------------------------------------

#[derive(Clone)]
struct EvidenceValue {
    id: String,
    work_order_id: String,
    attempt_id: String,
    subject_kind: String,
    subject_id: String,
    result: String,
}

#[derive(Clone)]
struct Fact {
    evidence: EvidenceValue,
    producer: String,
    observed_at: DateTime<FixedOffset>,
    source_revision: String,
    fresh_until: DateTime<FixedOffset>,
    fact_revision: String,
}

struct Rejection {
    evidence_id: String,
    diagnostic: String,
    fact_revision: String,
}

enum FactInput {
    Fact(Fact),
    Rejection(Rejection),
}

struct CompanyWork {
    work_order: Option<(String, String)>,
    deliverables: Vec<DeliverableDecl>,
    effects: Vec<EffectDecl>,
    evidence: Vec<EvidenceDecl>,
}

struct DeliverableDecl {
    id: String,
    work_order_id: String,
    attempt_id: String,
    required_evidence_ids: Vec<String>,
    effect_id: Option<String>,
}

struct EffectDecl {
    id: String,
    work_order_id: String,
    attempt_id: String,
    effect_class: String,
    destination: String,
    idempotency_key: Option<String>,
    approval_id: Option<String>,
}

struct EvidenceDecl {
    id: String,
    work_order_id: String,
    attempt_id: String,
    subject_kind: String,
    subject_id: String,
}

fn nonempty_stripped(value: Option<&Value>) -> Option<String> {
    let text = value?.as_str()?;
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return None;
    }
    Some(trimmed.to_string())
}

/// A nonempty RAW string, whitespace preserved: the selection walk keys
/// approvals and effects by the journal's own spelling, where the adapters
/// strip.
fn raw_nonempty(value: Option<&Value>) -> Option<String> {
    let text = value?.as_str()?;
    if text.trim().is_empty() {
        return None;
    }
    Some(text.to_string())
}

fn seq_or_empty(value: Option<&Value>) -> &[Value] {
    static EMPTY: Vec<Value> = Vec::new();
    match value {
        None | Some(Value::Null) => &EMPTY,
        Some(Value::Array(items)) => items,
        Some(_) => &EMPTY,
    }
}

// ---------------------------------------------------------------------------
// Plan frontmatter
// ---------------------------------------------------------------------------

fn python_io_error(error: &std::io::Error, path: &Path) -> String {
    let spelled = match error.raw_os_error() {
        Some(2) => Some(("No such file or directory", 2)),
        Some(13) => Some(("Permission denied", 13)),
        Some(20) => Some(("Not a directory", 20)),
        Some(21) => Some(("Is a directory", 21)),
        _ => None,
    };
    match spelled {
        Some((text, errno)) => format!("[Errno {errno}] {text}: '{}'", path.display()),
        None => error.to_string(),
    }
}

/// Read and validate the plan the way the Python leg did: frontmatter split,
/// YAML mapping, required identity fields, the date-keyed difficulty gate,
/// and the company work declaration with its cross-references. Errors answer
/// the Python refusal sentence; the pydantic wrapping is the one recorded
/// divergence.
fn load_company_work(plan_path: &Path) -> Result<(bool, CompanyWork), String> {
    let text = match std::fs::read_to_string(plan_path) {
        Ok(text) => text,
        Err(error) => {
            return Err(format!(
                "plan is unreadable or invalid: {}",
                python_io_error(&error, plan_path)
            ))
        }
    };
    let mapping = split_frontmatter(&text)?;
    validate_frontmatter(&mapping)?;
    let active = mapping.get("completion").and_then(Value::as_str) == Some("delivery");
    let work = match mapping.get("company_work") {
        None | Some(Value::Null) => CompanyWork {
            work_order: None,
            deliverables: Vec::new(),
            effects: Vec::new(),
            evidence: Vec::new(),
        },
        Some(company) => parse_company_work(company)?,
    };
    if let Some((order_node, _)) = &work.work_order {
        if plan_node_value(&mapping)? != *order_node {
            return Err(
                "plan is unreadable or invalid: company_work work_order node_id must match plan node"
                    .to_string(),
            );
        }
    }
    Ok((active, work))
}

/// The collapsed plan identity the Python model compared against: `node`
/// verbatim, else `claims` verbatim (its before-validator), else refusal.
fn plan_node_value(mapping: &Map<String, Value>) -> Result<String, String> {
    match mapping.get("node") {
        Some(Value::String(text)) => Ok(text.clone()),
        Some(_) => {
            Err("plan is unreadable or invalid: plan frontmatter node must be a string".to_string())
        }
        None => match mapping.get("claims") {
            Some(Value::String(text)) => Ok(text.clone()),
            _ => Err(
                "plan is unreadable or invalid: plan frontmatter node must be a string".to_string(),
            ),
        },
    }
}

/// The plan-doc frontmatter split: `---\n ... \n---` fence; an absent fence
/// reads as no frontmatter, which fails required-field validation.
fn split_frontmatter(text: &str) -> Result<Map<String, Value>, String> {
    if !text.starts_with("---") {
        return Err(required_field_error());
    }
    let Some(first_newline) = text.find('\n') else {
        return Err("plan is unreadable or invalid: substring not found".to_string());
    };
    let rest = &text[first_newline + 1..];
    let Some(close_idx) = rest.find("\n---") else {
        return Err(required_field_error());
    };
    let parsed: serde_yaml_ng::Value = match serde_yaml_ng::from_str(&rest[..close_idx]) {
        Ok(parsed) => parsed,
        Err(error) => return Err(format!("plan is unreadable or invalid: {error}")),
    };
    if parsed.is_null() {
        return Err(required_field_error());
    }
    let value = yaml_to_json(&parsed)?;
    match value {
        Value::Object(map) => Ok(map),
        _ => Err("plan is unreadable or invalid: Frontmatter must be a YAML mapping".to_string()),
    }
}

fn required_field_error() -> String {
    "plan is unreadable or invalid: plan frontmatter is missing its required fields".to_string()
}

fn yaml_to_json(value: &serde_yaml_ng::Value) -> Result<Value, String> {
    // serde_yaml_ng maps one-to-one onto JSON scalars for the shapes plan
    // frontmatter carries; a non-string-key mapping refuses rather than
    // guessing.
    Ok(match value {
        serde_yaml_ng::Value::Null => Value::Null,
        serde_yaml_ng::Value::Bool(b) => Value::Bool(*b),
        serde_yaml_ng::Value::Number(n) => serde_json::to_value(n).map_err(|e| e.to_string())?,
        serde_yaml_ng::Value::String(s) => Value::String(s.clone()),
        serde_yaml_ng::Value::Sequence(items) => {
            let mut out = Vec::with_capacity(items.len());
            for item in items {
                out.push(yaml_to_json(item)?);
            }
            Value::Array(out)
        }
        serde_yaml_ng::Value::Mapping(map) => {
            let mut out = Map::new();
            for (key, item) in map {
                let key = match key {
                    serde_yaml_ng::Value::String(s) => s.clone(),
                    other => {
                        return Err(format!(
                            "plan is unreadable or invalid: non-string frontmatter key: {other:?}"
                        ))
                    }
                };
                out.insert(key, yaml_to_json(item)?);
            }
            Value::Object(out)
        }
        serde_yaml_ng::Value::Tagged(tagged) => yaml_to_json(&tagged.value)?,
    })
}

fn validate_frontmatter(mapping: &Map<String, Value>) -> Result<(), String> {
    // node: filled from an identical `claims` spelling when absent, the same
    // collapse the Python model ran before validation.
    let node_present = matches!(mapping.get("node"), Some(Value::String(_)));
    if !node_present {
        let claims_ok = match mapping.get("claims") {
            Some(Value::String(text)) => !text.is_empty(),
            Some(Value::Array(items)) => !items.is_empty(),
            Some(Value::Object(map)) => !map.is_empty(),
            _ => false,
        };
        if !claims_ok {
            return Err(required_field_error());
        }
    }
    let status = match mapping.get("status") {
        Some(Value::Bool(true)) => "true".to_string(),
        Some(Value::Bool(false)) => "false".to_string(),
        Some(Value::String(text)) => match text.as_str() {
            "shipped" => "in_review".to_string(),
            "archived" => "superseded".to_string(),
            "stub" => "idea".to_string(),
            other => other.to_string(),
        },
        _ => {
            return Err(
                "plan is unreadable or invalid: status must be a known plan status word"
                    .to_string(),
            )
        }
    };
    if !crate::plan_doc::status::known_statuses().contains(&status.as_str()) {
        return Err(format!(
            "plan is unreadable or invalid: status must be one of the plan statuses, got {status}"
        ));
    }
    let created = match mapping.get("created") {
        Some(Value::Bool(_)) | Some(Value::Number(_)) => return Err(
            "plan is unreadable or invalid: created must be a date (YYYY-MM-DD), not a bare number"
                .to_string(),
        ),
        Some(Value::String(text)) => text.trim().to_string(),
        _ => {
            return Err(
                "plan is unreadable or invalid: created must be a date (YYYY-MM-DD)".to_string(),
            )
        }
    };
    let created_date = created_date(&created)?.ok_or_else(|| {
        "plan is unreadable or invalid: created must be a date (YYYY-MM-DD)".to_string()
    })?;
    let gate = chrono::NaiveDate::from_ymd_opt(
        DIFFICULTY_REQUIRED_AFTER.0,
        DIFFICULTY_REQUIRED_AFTER.1,
        DIFFICULTY_REQUIRED_AFTER.2,
    )
    .expect("constant date");
    if created_date > gate {
        let band_ok = match mapping.get("difficulty") {
            Some(Value::String(band)) => {
                matches!(
                    band.trim().to_lowercase().as_str(),
                    "low" | "medium" | "high"
                )
            }
            _ => false,
        };
        if !band_ok {
            return Err(
                "difficulty is required for plans created after 2026-08-26: set it to one of low, medium, high (an intrinsic property of the work - the expected time, edge cases, unknowns, and what a senior tech lead makes of the problem; not a model or capacity hint)".to_string(),
            );
        }
    }
    Ok(())
}

/// The gate's date read: `YYYY-MM-DD` prefix, optionally with a timestamp
/// after `T` or a space.
fn created_date(created: &str) -> Result<Option<chrono::NaiveDate>, String> {
    let refused =
        || Err("plan is unreadable or invalid: created must be a date (YYYY-MM-DD)".to_string());
    let mut parts = created.split('-');
    let year: i32 = match parts.next().and_then(|p| p.parse().ok()) {
        Some(year) => year,
        None => return refused().map(|_: String| None),
    };
    let month: u32 = match parts.next().and_then(|p| p.parse().ok()) {
        Some(month) => month,
        None => return refused().map(|_: String| None),
    };
    let rest = match parts.next() {
        Some(rest) => rest,
        None => return refused().map(|_: String| None),
    };
    let (day_text, tail) = match rest.len() {
        0 => return refused().map(|_: String| None),
        _ => match rest.find(|c: char| !c.is_ascii_digit()) {
            Some(position) => (&rest[..position], &rest[position..]),
            None => (rest, ""),
        },
    };
    let day: u32 = match day_text.parse().ok() {
        Some(day) => day,
        None => return refused().map(|_: String| None),
    };
    if !tail.is_empty() && !tail.starts_with('T') && !tail.starts_with(' ') {
        return Ok(None);
    }
    Ok(chrono::NaiveDate::from_ymd_opt(year, month, day))
}

fn parse_company_work(value: &Value) -> Result<CompanyWork, String> {
    let invalid = || {
        "plan is unreadable or invalid: company_work must be a valid company work declaration"
            .to_string()
    };
    let map = value.as_object().ok_or_else(invalid)?;
    let work_order = match map.get("work_order") {
        None | Some(Value::Null) => None,
        Some(order) => {
            let order = order.as_object().ok_or_else(invalid)?;
            Some((
                nonempty_stripped(order.get("node_id")).ok_or_else(invalid)?,
                nonempty_stripped(order.get("attempt_id")).ok_or_else(invalid)?,
            ))
        }
    };
    let mut deliverables = Vec::new();
    for item in seq_or_empty(map.get("deliverables")) {
        let item = item.as_object().ok_or_else(invalid)?;
        // kind is a required contract field; validated, never read here.
        nonempty_stripped(item.get("kind")).ok_or_else(invalid)?;
        let effect_id = match item.get("effect_id") {
            Some(Value::Null) | None => None,
            other => Some(nonempty_stripped(other).ok_or_else(invalid)?),
        };
        deliverables.push(DeliverableDecl {
            id: nonempty_stripped(item.get("id")).ok_or_else(invalid)?,
            work_order_id: nonempty_stripped(item.get("work_order_id")).ok_or_else(invalid)?,
            attempt_id: nonempty_stripped(item.get("attempt_id")).ok_or_else(invalid)?,
            required_evidence_ids: seq_or_empty(item.get("required_evidence_ids"))
                .iter()
                .filter_map(|required| nonempty_stripped(Some(required)))
                .collect(),
            effect_id,
        });
    }
    let mut effects = Vec::new();
    for item in seq_or_empty(map.get("effects")) {
        let item = item.as_object().ok_or_else(invalid)?;
        effects.push(EffectDecl {
            id: nonempty_stripped(item.get("id")).ok_or_else(invalid)?,
            work_order_id: nonempty_stripped(item.get("work_order_id")).ok_or_else(invalid)?,
            attempt_id: nonempty_stripped(item.get("attempt_id")).ok_or_else(invalid)?,
            effect_class: nonempty_stripped(item.get("effect_class")).ok_or_else(invalid)?,
            destination: nonempty_stripped(item.get("destination")).ok_or_else(invalid)?,
            idempotency_key: nonempty_stripped(item.get("idempotency_key")),
            approval_id: nonempty_stripped(item.get("approval_id")),
        });
    }
    let mut evidence = Vec::new();
    for item in seq_or_empty(map.get("evidence")) {
        let item = item.as_object().ok_or_else(invalid)?;
        let subject_kind = nonempty_stripped(item.get("subject_kind")).ok_or_else(invalid)?;
        if !SUBJECT_KINDS.contains(&subject_kind.as_str()) {
            return Err(invalid());
        }
        evidence.push(EvidenceDecl {
            id: nonempty_stripped(item.get("id")).ok_or_else(invalid)?,
            work_order_id: nonempty_stripped(item.get("work_order_id")).ok_or_else(invalid)?,
            attempt_id: nonempty_stripped(item.get("attempt_id")).ok_or_else(invalid)?,
            subject_kind,
            subject_id: nonempty_stripped(item.get("subject_id")).ok_or_else(invalid)?,
        });
    }
    let work = CompanyWork {
        work_order,
        deliverables,
        effects,
        evidence,
    };
    validate_company_work(&work)?;
    Ok(work)
}

/// One duplicate check for the cross-reference validators. A plain fn, not a
/// closure: the push ties the id's lifetime to the vec's element lifetime,
/// which needs a named parameter, and a closure's inferred type cannot name
/// one.
fn check_duplicate<'a>(existing: &mut Vec<&'a str>, id: &'a str, kind: &str) -> Result<(), String> {
    if existing.contains(&id) {
        return Err(format!("duplicate {kind} id {id}"));
    }
    existing.push(id);
    Ok(())
}

/// The company work contract's cross-reference validators. A declaration that
/// breaks one is refused, never evaluated.
fn validate_company_work(work: &CompanyWork) -> Result<(), String> {
    let mut deliverable_ids: Vec<&str> = Vec::new();
    for deliverable in &work.deliverables {
        check_duplicate(&mut deliverable_ids, &deliverable.id, "deliverable")?;
    }
    let mut effect_ids: Vec<&str> = Vec::new();
    for effect in &work.effects {
        check_duplicate(&mut effect_ids, &effect.id, "effect")?;
    }
    let mut evidence_ids: Vec<&str> = Vec::new();
    for item in &work.evidence {
        check_duplicate(&mut evidence_ids, &item.id, "evidence")?;
    }
    let Some((node_id, attempt_id)) = &work.work_order else {
        if !work.deliverables.is_empty() || !work.effects.is_empty() || !work.evidence.is_empty() {
            return Err(
                "delivery, effect, evidence, and observation refs require a work_order".to_string(),
            );
        }
        return Ok(());
    };
    for deliverable in &work.deliverables {
        validate_attempt(
            "deliverable",
            &deliverable.id,
            (
                deliverable.work_order_id.as_str(),
                deliverable.attempt_id.as_str(),
            ),
            node_id,
            attempt_id,
        )?;
        let missing: Vec<&String> = deliverable
            .required_evidence_ids
            .iter()
            .filter(|required| !evidence_ids.contains(&required.as_str()))
            .collect();
        if !missing.is_empty() {
            let mut sorted = missing;
            sorted.sort();
            let listed = sorted
                .iter()
                .map(|item| format!("'{}'", item))
                .collect::<Vec<_>>()
                .join(", ");
            return Err(format!(
                "deliverable {} references undeclared evidence [{}]",
                deliverable.id, listed
            ));
        }
        if let Some(effect_id) = &deliverable.effect_id {
            if !effect_ids.contains(&effect_id.as_str()) {
                return Err(format!(
                    "deliverable {} references undeclared effect {}",
                    deliverable.id, effect_id
                ));
            }
        }
    }
    for effect in &work.effects {
        validate_attempt(
            "effect",
            &effect.id,
            (&effect.work_order_id.as_str(), &effect.attempt_id.as_str()),
            node_id,
            attempt_id,
        )?;
    }
    for item in &work.evidence {
        validate_attempt(
            "evidence",
            &item.id,
            (&item.work_order_id.as_str(), &item.attempt_id.as_str()),
            node_id,
            attempt_id,
        )?;
        let declared: Option<Vec<&str>> = match item.subject_kind.as_str() {
            "deliverable" => Some(deliverable_ids.clone()),
            "effect" => Some(effect_ids.clone()),
            _ => None,
        };
        if let Some(subjects) = declared {
            if !subjects.contains(&item.subject_id.as_str()) {
                return Err(format!(
                    "evidence {} references undeclared {} {}",
                    item.id, item.subject_kind, item.subject_id
                ));
            }
        }
    }
    Ok(())
}

fn validate_attempt(
    kind: &str,
    id: &str,
    attempt: (&str, &str),
    node_id: &str,
    attempt_id: &str,
) -> Result<(), String> {
    let (work_order_id, item_attempt) = attempt;
    if work_order_id != node_id {
        return Err(format!(
            "{kind} {id} work_order_id does not match work_order {node_id}"
        ));
    }
    if item_attempt != attempt_id {
        return Err(format!(
            "{kind} {id} attempt_id does not match work_order attempt_id"
        ));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Journal snapshot
// ---------------------------------------------------------------------------

struct Snapshot {
    raw: Vec<u8>,
    events: Vec<Value>,
}

enum JournalRead {
    Unreadable(String),
    Conflict(Vec<String>),
}

fn stat_revision(m: &std::fs::Metadata) -> String {
    use std::os::unix::fs::MetadataExt;
    format!(
        "stat:{}:{}:{}:{}",
        m.dev(),
        m.ino(),
        m.len(),
        m.mtime_nsec()
    )
}

fn stat_identity(m: &std::fs::Metadata) -> (u64, u64, u64, i64) {
    use std::os::unix::fs::MetadataExt;
    (m.dev(), m.ino(), m.len(), m.mtime_nsec())
}

fn journal_unreadable(error: &std::io::Error, path: &Path) -> JournalRead {
    JournalRead::Unreadable(format!(
        "event journal is unreadable or malformed: {}",
        python_io_error(error, path)
    ))
}

/// One coherent snapshot: committed store rows when a store sits beside the
/// journal, raw bytes otherwise. The store commit is the write boundary, so a
/// post-cutover journal may have no bytes at all.
fn read_snapshot(events_path: &Path) -> Result<Snapshot, JournalRead> {
    if crate::event_store::store_path(events_path).exists() {
        let _ = crate::event_store::import_all(events_path);
        let rows = crate::event_store::query_events(
            events_path,
            &crate::event_store::EventQuery {
                include_rejected: false,
                ..Default::default()
            },
        )
        .map_err(|_| {
            JournalRead::Unreadable(format!(
                "event journal is unreadable or malformed: event store beside {} is unreadable",
                events_path.display()
            ))
        })?;
        let mut raw = Vec::new();
        for row in &rows {
            raw.extend_from_slice(row.line.as_bytes());
            raw.push(b'\n');
        }
        let events = parse_events(&raw).map_err(JournalRead::Unreadable)?;
        return Ok(Snapshot { raw, events });
    }
    let raw = read_coherent_bytes(events_path)?;
    let events = parse_events(&raw).map_err(JournalRead::Unreadable)?;
    Ok(Snapshot { raw, events })
}

/// The stat-identity coherent read: identity checked before, after, and
/// against a fresh path stat; any change answers a revision conflict.
fn read_coherent_bytes(path: &Path) -> Result<Vec<u8>, JournalRead> {
    use std::io::Read;
    let mut file = std::fs::File::open(path).map_err(|error| journal_unreadable(&error, path))?;
    let before = file
        .metadata()
        .map_err(|error| journal_unreadable(&error, path))?;
    let mut raw = Vec::new();
    file.read_to_end(&mut raw).map_err(|error| {
        JournalRead::Unreadable(format!("event journal is unreadable or malformed: {error}"))
    })?;
    let after = file
        .metadata()
        .map_err(|error| journal_unreadable(&error, path))?;
    let current = path
        .metadata()
        .map_err(|error| journal_unreadable(&error, path))?;
    if stat_identity(&before) != stat_identity(&after)
        || stat_identity(&after) != stat_identity(&current)
    {
        return Err(JournalRead::Conflict(vec![
            stat_revision(&before),
            stat_revision(&after),
            stat_revision(&current),
        ]));
    }
    if raw.len() as u64 != after.len() {
        return Err(JournalRead::Unreadable(
            "event journal is unreadable or malformed: event journal read was partial".to_string(),
        ));
    }
    Ok(raw)
}

fn parse_events(raw: &[u8]) -> Result<Vec<Value>, String> {
    let text = std::str::from_utf8(raw).map_err(|_| {
        "event journal is unreadable or malformed: journal is not UTF-8".to_string()
    })?;
    let mut events = Vec::new();
    for (index, line) in text.lines().enumerate() {
        let line_number = index + 1;
        if line.trim().is_empty() {
            continue;
        }
        let parsed: Value = match serde_json::from_str(line) {
            Ok(parsed) => parsed,
            Err(_) => {
                return Err(format!(
                    "event journal is unreadable or malformed: malformed JSON at line {line_number}"
                ))
            }
        };
        if !parsed.is_object() {
            return Err(format!(
                "event journal is unreadable or malformed: event at line {line_number} is not an object"
            ));
        }
        events.push(parsed);
    }
    Ok(events)
}

// ---------------------------------------------------------------------------
// Producer-event selection
// ---------------------------------------------------------------------------

struct Latest {
    event: Value,
    timestamp: Option<DateTime<FixedOffset>>,
    latest_valid: Option<DateTime<FixedOffset>>,
}

fn select_latest(selected: &mut Option<Latest>, event: Value) {
    let timestamp = event_datetime(&event);
    match selected {
        None => {
            *selected = Some(Latest {
                timestamp,
                latest_valid: timestamp,
                event,
            });
        }
        Some(current) => match timestamp {
            None => {
                current.timestamp = None;
                current.event = event;
            }
            Some(ts) => match current.timestamp {
                None => {
                    let newer = match current.latest_valid {
                        None => true,
                        Some(latest_valid) => ts > latest_valid,
                    };
                    if newer {
                        *selected = Some(Latest {
                            timestamp: Some(ts),
                            latest_valid: Some(ts),
                            event,
                        });
                    }
                }
                Some(current_ts) => {
                    if ts >= current_ts {
                        *selected = Some(Latest {
                            timestamp: Some(ts),
                            latest_valid: Some(ts),
                            event,
                        });
                    }
                }
            },
        },
    }
}

fn immutable_payload_key(data: &Value) -> String {
    let mut stripped = data.clone();
    if let Some(map) = stripped.as_object_mut() {
        map.remove("event_id");
    }
    let mut out = String::new();
    write_canonical_ensure_ascii(&stripped, &mut out);
    out
}

/// One producer read kept for evaluation: an event still in play, or a
/// synthesized rejection.
enum SelectedEvent {
    Json(Value),
    Rejection(Rejection),
}

fn required_evidence_ids(work: &CompanyWork) -> BTreeSet<String> {
    work.deliverables
        .iter()
        .flat_map(|deliverable| deliverable.required_evidence_ids.iter().cloned())
        .collect()
}

/// Select current producer state without reconstructing its authority. The
/// walk order and every insertion order below are load-bearing: they decide
/// the diagnostic order in the verdict rows.
fn current_evidence_events(
    events: &[Value],
    work: &CompanyWork,
    fact_revision: &str,
) -> Result<Vec<(SelectedEvent, Option<Value>)>, String> {
    let Some((node_id, _)) = &work.work_order else {
        return Ok(Vec::new());
    };
    let mut observed: Vec<(Vec<String>, ObservedState)> = Vec::new();
    let mut requests: Vec<(String, Vec<(String, Option<Latest>)>)> = Vec::new();
    let mut approvals: Vec<(String, Vec<(String, Option<Latest>)>)> = Vec::new();
    let mut unkeyed_approvals: Vec<(String, Option<Latest>)> = Vec::new();
    let mut effects: Vec<(String, Option<Latest>)> = Vec::new();
    let mut producer_event_ids: Vec<(String, (String, String, Value))> = Vec::new();

    for event in events {
        let event_type = event.get("type").and_then(Value::as_str);
        let data = event.get("data");
        if event_type == Some("delivery_evidence_observed") {
            let evidence_map = data
                .filter(|data| data.is_object())
                .and_then(|data| data.get("evidence"))
                .filter(|evidence| evidence.is_object());
            let evidence_id = evidence_map
                .and_then(|evidence| evidence.get("id"))
                .and_then(Value::as_str);
            let (Some(evidence_map), Some(evidence_id)) = (evidence_map, evidence_id) else {
                return Err("malformed delivery_evidence_observed event".to_string());
            };
            let identity: Vec<String> = [
                evidence_map.get("work_order_id"),
                evidence_map.get("attempt_id"),
                Some(&json!(evidence_id)),
                evidence_map.get("subject_kind"),
                evidence_map.get("subject_id"),
                data.and_then(|data| data.get("producer")),
            ]
            .iter()
            .map(|value| py_str_raw(*value))
            .collect();
            let observed_at = observed_envelope_observed_at(event);
            let position = match observed
                .iter()
                .position(|(existing, _)| *existing == identity)
            {
                Some(position) => position,
                None => {
                    observed.push((identity, ObservedState::default()));
                    observed.len() - 1
                }
            };
            observed[position].1.select(observed_at, event);
            continue;
        }
        if !matches!(data, Some(Value::Object(_))) {
            if matches!(
                event_type,
                Some("approval_requested" | "approval_decided" | "effect_state_changed")
            ) {
                return Err(format!(
                    "malformed {} event: data is not an object",
                    event_type.unwrap_or_default()
                ));
            }
            continue;
        }
        let data_map = data.expect("checked above");
        if matches!(
            event_type,
            Some("approval_requested" | "approval_decided" | "effect_state_changed")
        ) {
            let event_work_order = data_map.get("work_order_id").and_then(Value::as_str);
            if let Some(event_work_order) = event_work_order {
                if !event_work_order.is_empty() && event_work_order != node_id {
                    continue;
                }
            }
            if let Some(event_id) = data_map.get("event_id").and_then(Value::as_str) {
                if !event_id.is_empty() {
                    let identity = (
                        event_type.unwrap_or_default().to_string(),
                        py_str_raw(event.get("source")),
                        data_map.clone(),
                    );
                    let prior = producer_event_ids
                        .iter()
                        .find(|(existing, _)| existing == event_id)
                        .map(|(_, value)| value.clone());
                    match prior {
                        Some(prior_identity) if prior_identity != identity => {
                            return Err(format!("conflicting producer event_id {event_id}"));
                        }
                        Some(_) => continue,
                        None => producer_event_ids.push((event_id.to_string(), identity)),
                    }
                }
            }
        }
        match event_type {
            Some("approval_requested") | Some("approval_decided") => {
                let digest = data_map.get("request_digest").and_then(Value::as_str);
                if let Some(digest) = digest.filter(|d| !d.is_empty()) {
                    let key = immutable_payload_key(data_map);
                    let bucket = if event_type == Some("approval_requested") {
                        &mut requests
                    } else {
                        &mut approvals
                    };
                    bucket_entry(bucket, digest, &key, event);
                } else if let Some(effect_id) = raw_nonempty(data_map.get("effect_id")) {
                    upsert_latest(&mut unkeyed_approvals, &effect_id, event.clone());
                }
            }
            Some("effect_state_changed") => {
                if let Some(key) = raw_nonempty(data_map.get("idempotency_key")) {
                    upsert_latest(&mut effects, &format!("key:{key}"), event.clone());
                } else if let Some(effect_id) = raw_nonempty(data_map.get("effect_id")) {
                    upsert_latest(&mut effects, &format!("effect:{effect_id}"), event.clone());
                }
            }
            _ => {}
        }
    }

    let mut selected: Vec<(SelectedEvent, Option<Value>)> = Vec::new();
    for (_, state) in &observed {
        for event in &state.events {
            let evidence = event
                .get("data")
                .and_then(|data| data.get("evidence"))
                .filter(|evidence| evidence.is_object());
            let explicit_other_work = evidence
                .and_then(|evidence| evidence.get("work_order_id"))
                .and_then(Value::as_str)
                .map(|work_order_id| !work_order_id.is_empty() && work_order_id != node_id)
                .unwrap_or(false);
            if explicit_other_work {
                continue;
            }
            selected.push((SelectedEvent::Json(event.clone()), None));
        }
    }
    let approvals_for = |digest: &str| {
        approvals
            .iter()
            .find(|(existing, _)| existing == digest)
            .map(|(_, bucket)| bucket)
    };
    for (digest, current_requests) in &requests {
        let emit = |bucket: &[(String, Option<Latest>)],
                    out: &mut Vec<(SelectedEvent, Option<Value>)>| {
            let mut rows: Vec<&(String, Option<Latest>)> = bucket.iter().collect();
            rows.sort_by(|a, b| a.0.cmp(&b.0));
            for (_, latest) in rows {
                if let Some(latest) = latest {
                    out.push((SelectedEvent::Json(latest.event.clone()), None));
                }
            }
        };
        match approvals_for(digest) {
            Some(decisions) => emit(decisions, &mut selected),
            None => emit(current_requests, &mut selected),
        }
    }
    for (digest, decisions) in &approvals {
        if requests.iter().any(|(existing, _)| existing == digest) {
            continue;
        }
        let mut rows: Vec<&(String, Option<Latest>)> = decisions.iter().collect();
        rows.sort_by(|a, b| a.0.cmp(&b.0));
        for (_, latest) in rows {
            if let Some(latest) = latest {
                selected.push((SelectedEvent::Json(latest.event.clone()), None));
            }
        }
    }
    for (digest, decisions) in &approvals {
        if decisions.len() > 1 {
            for rejection in approval_conflict_rejections(work, digest, fact_revision, "decisions")
            {
                selected.push((SelectedEvent::Rejection(rejection), None));
            }
        }
    }
    for (digest, current_requests) in &requests {
        if current_requests.len() > 1 {
            for rejection in approval_conflict_rejections(work, digest, fact_revision, "requests") {
                selected.push((SelectedEvent::Rejection(rejection), None));
            }
        }
    }
    for (_, latest) in &unkeyed_approvals {
        if let Some(latest) = latest {
            selected.push((SelectedEvent::Json(latest.event.clone()), None));
        }
    }
    for (_, latest) in &effects {
        let Some(latest) = latest else { continue };
        let event = &latest.event;
        let digest = event
            .get("data")
            .and_then(|data| data.get("request_digest"))
            .and_then(Value::as_str)
            .map(str::to_string);
        let approval_request = digest.and_then(|digest| {
            let bucket = requests
                .iter()
                .find(|(existing, _)| *existing == digest)
                .map(|(_, bucket)| bucket);
            match bucket {
                Some(bucket) if bucket.len() == 1 => bucket[0].1.as_ref().map(|l| l.event.clone()),
                _ => None,
            }
        });
        selected.push((SelectedEvent::Json(event.clone()), approval_request));
    }
    Ok(selected)
}

#[derive(Default)]
struct ObservedState {
    timestamp: Option<DateTime<FixedOffset>>,
    latest_valid: Option<DateTime<FixedOffset>>,
    events: Vec<Value>,
}

impl ObservedState {
    /// The observed-identity fold: the latest observation wins, ties at one
    /// timestamp all stay, a timestamp-less row replaces but keeps the best
    /// valid clock seen.
    fn select(&mut self, observed_at: Option<DateTime<FixedOffset>>, event: &Value) {
        let observed_at = match observed_at {
            None => {
                // A timestamp-less row replaces the slot but keeps the best
                // valid clock seen, exactly like the Python fold.
                self.timestamp = None;
                self.events = vec![event.clone()];
                return;
            }
            Some(observed_at) => observed_at,
        };
        match self.timestamp {
            None => {
                let newer = match self.latest_valid {
                    None => true,
                    Some(latest_valid) => observed_at > latest_valid,
                };
                if newer {
                    *self = ObservedState {
                        timestamp: Some(observed_at),
                        latest_valid: Some(observed_at),
                        events: vec![event.clone()],
                    };
                }
            }
            Some(current_ts) => {
                if observed_at > current_ts {
                    *self = ObservedState {
                        timestamp: Some(observed_at),
                        latest_valid: Some(observed_at),
                        events: vec![event.clone()],
                    };
                } else if observed_at == current_ts {
                    self.events.push(event.clone());
                }
            }
        }
    }
}

fn bucket_entry(
    bucket: &mut Vec<(String, Vec<(String, Option<Latest>)>)>,
    digest: &str,
    key: &str,
    event: &Value,
) {
    let position = match bucket.iter().position(|(existing, _)| existing == digest) {
        Some(position) => position,
        None => {
            bucket.push((digest.to_string(), Vec::new()));
            bucket.len() - 1
        }
    };
    let entries = &mut bucket[position].1;
    if let Some(slot) = entries.iter_mut().find(|(existing, _)| existing == key) {
        let mut latest = slot.1.take();
        select_latest(&mut latest, event.clone());
        slot.1 = latest;
        return;
    }
    let mut latest = None;
    select_latest(&mut latest, event.clone());
    entries.push((key.to_string(), latest));
}

fn upsert_latest(map: &mut Vec<(String, Option<Latest>)>, key: &str, event: Value) {
    if let Some(slot) = map.iter_mut().find(|(existing, _)| existing == key) {
        let mut latest = slot.1.take();
        select_latest(&mut latest, event);
        slot.1 = latest;
        return;
    }
    let mut latest = None;
    select_latest(&mut latest, event);
    map.push((key.to_string(), latest));
}

fn approval_conflict_rejections(
    work: &CompanyWork,
    request_digest: &str,
    fact_revision: &str,
    lifecycle: &str,
) -> Vec<Rejection> {
    let required = required_evidence_ids(work);
    let producer_event = if lifecycle == "requests" {
        "approval_requested"
    } else {
        "approval_decided"
    };
    let producer = format!("event:approvals:{producer_event}");
    work.evidence
        .iter()
        .filter(|evidence| {
            required.contains(&evidence.id)
                && evidence.subject_kind == "approval"
                && evidence.subject_id == request_digest
        })
        .map(|evidence| Rejection {
            evidence_id: evidence.id.clone(),
            diagnostic: format!(
                "requirement {} from {producer} rejected binding: conflicting immutable approval {lifecycle} for {request_digest}",
                evidence.id
            ),
            fact_revision: fact_revision.to_string(),
        })
        .collect()
}

/// The strict observed-event envelope read: its parsed `observed_at` drives
/// selection order; a malformed envelope falls back to the raw event ts,
/// exactly like the Python fold.
fn observed_envelope_observed_at(event: &Value) -> Option<DateTime<FixedOffset>> {
    match observed_event_fact(event) {
        Some(fact) => Some(fact.observed_at),
        None => event_datetime(event),
    }
}

// ---------------------------------------------------------------------------
// Fact contracts
// ---------------------------------------------------------------------------

/// Strict `DeliveryEvidenceFact` validation, pydantic shape included: exact
/// key set, stripped nonempty strings, enum words, aware datetimes,
/// freshness window. Answers the parsed fact.
fn parse_fact_value(data: &Value) -> Option<Fact> {
    let map = data.as_object()?;
    if map.len() != 8 {
        return None;
    }
    let evidence = map.get("evidence")?.as_object()?;
    if evidence.len() != 6 {
        return None;
    }
    if map.get("version")?.as_str()? != FACT_VERSION {
        return None;
    }
    let evidence_id = nonempty_stripped(evidence.get("id"))?;
    let work_order_id = nonempty_stripped(evidence.get("work_order_id"))?;
    let attempt_id = nonempty_stripped(evidence.get("attempt_id"))?;
    let subject_kind = nonempty_stripped(evidence.get("subject_kind"))?;
    let subject_id = nonempty_stripped(evidence.get("subject_id"))?;
    let result = nonempty_stripped(evidence.get("result"))?;
    if !SUBJECT_KINDS.contains(&subject_kind.as_str())
        || !EVIDENCE_RESULTS.contains(&result.as_str())
    {
        return None;
    }
    let observed_at = parse_aware(map.get("observed_at")?.as_str()?)?;
    let fresh_until = parse_aware(map.get("fresh_until")?.as_str()?)?;
    if fresh_until < observed_at {
        return None;
    }
    Some(Fact {
        evidence: EvidenceValue {
            id: evidence_id,
            work_order_id,
            attempt_id,
            subject_kind,
            subject_id,
            result,
        },
        producer: nonempty_stripped(map.get("producer"))?,
        observed_at,
        source_revision: nonempty_stripped(map.get("source_revision"))?,
        fresh_until,
        fact_revision: nonempty_stripped(map.get("fact_revision"))?,
    })
}

/// Strict `DeliveryEvidenceObservedEvent` validation: exact envelope keys,
/// literal type, nonempty source, aware ts equal to the fact's observed_at.
fn observed_event_fact(event: &Value) -> Option<Fact> {
    let map = event.as_object()?;
    if map.len() != 4
        || ["ts", "type", "source", "data"]
            .iter()
            .any(|key| !map.contains_key(*key))
    {
        return None;
    }
    if map.get("type")?.as_str()? != "delivery_evidence_observed" {
        return None;
    }
    nonempty_stripped(map.get("source"))?;
    let ts = parse_aware(map.get("ts").and_then(Value::as_str)?)?;
    let fact = parse_fact_value(map.get("data")?)?;
    if ts != fact.observed_at {
        return None;
    }
    Some(fact)
}

// ---------------------------------------------------------------------------
// Adapters
// ---------------------------------------------------------------------------

/// Strict `_ProducerEvent` validation: exact envelope keys, the three
/// producer literals, `source` exactly `approvals`, any data mapping.
fn producer_event_parts(event: &Value) -> Option<(String, DateTime<FixedOffset>, Value)> {
    let map = event.as_object()?;
    if map.len() != 4
        || ["ts", "type", "source", "data"]
            .iter()
            .any(|key| !map.contains_key(*key))
    {
        return None;
    }
    let event_type = map.get("type")?.as_str()?.to_string();
    if !matches!(
        event_type.as_str(),
        "approval_requested" | "approval_decided" | "effect_state_changed"
    ) {
        return None;
    }
    if map.get("source").and_then(Value::as_str) != Some("approvals") {
        return None;
    }
    if !map.get("data").map(Value::is_object).unwrap_or(false) {
        return None;
    }
    let ts = parse_aware(map.get("ts").and_then(Value::as_str)?)?;
    Some((event_type, ts, map.get("data").expect("checked").clone()))
}

struct EffectBinding {
    id: String,
    effect_class: String,
    destination: String,
    idempotency_key: Option<String>,
    approval_id: Option<String>,
}

fn producer_binding(work: &CompanyWork, data: &Value) -> Option<EffectBinding> {
    let work_order_id = nonempty_stripped(data.get("work_order_id"))?;
    let attempt_id = nonempty_stripped(data.get("attempt_id"))?;
    let effect_id = nonempty_stripped(data.get("effect_id"))?;
    let (node_id, attempt) = work.work_order.as_ref()?;
    if &work_order_id != node_id || &attempt_id != attempt {
        return None;
    }
    let matches: Vec<&EffectDecl> = work
        .effects
        .iter()
        .filter(|effect| effect.id == effect_id)
        .collect();
    let effect = if matches.len() == 1 {
        matches[0]
    } else {
        return None;
    };
    Some(EffectBinding {
        id: effect.id.clone(),
        effect_class: effect.effect_class.clone(),
        destination: effect.destination.clone(),
        idempotency_key: effect.idempotency_key.clone(),
        approval_id: effect.approval_id.clone(),
    })
}

struct EvidenceSlot {
    id: String,
    work_order_id: String,
    attempt_id: String,
    subject_kind: String,
    subject_id: String,
}

fn match_slot(
    work: &CompanyWork,
    subject_kind: &str,
    subject_id: &str,
    evidence_id: Option<&str>,
) -> Option<EvidenceSlot> {
    let required = required_evidence_ids(work);
    let matches: Vec<&EvidenceDecl> = work
        .evidence
        .iter()
        .filter(|evidence| {
            required.contains(&evidence.id)
                && evidence_id.map(|id| evidence.id == id).unwrap_or(true)
                && evidence.subject_kind == subject_kind
                && evidence.subject_id == subject_id
        })
        .collect();
    let declared = if matches.len() == 1 {
        matches[0]
    } else {
        return None;
    };
    Some(EvidenceSlot {
        id: declared.id.clone(),
        work_order_id: declared.work_order_id.clone(),
        attempt_id: declared.attempt_id.clone(),
        subject_kind: declared.subject_kind.clone(),
        subject_id: declared.subject_id.clone(),
    })
}

fn has_nonempty(data: &Value, keys: &[&str]) -> bool {
    keys.iter()
        .all(|key| nonempty_stripped(data.get(*key)).is_some())
}

/// Normalize one authoritative event into its exact declared evidence slots.
/// A pure projection: unreadable, unknown, mismatched, or ambiguously bound
/// inputs answer no fact, which leaves coverage unknown.
fn adapt_delivery_event(
    work: &CompanyWork,
    event: &Value,
    fresh_until: DateTime<FixedOffset>,
    fact_revision: &str,
    approval_request_event: Option<&Value>,
) -> Vec<Fact> {
    let Some((event_type, ts, data)) = producer_event_parts(event) else {
        return Vec::new();
    };
    let Some(binding) = producer_binding(work, &data) else {
        return Vec::new();
    };
    if event_type == "approval_requested" || event_type == "approval_decided" {
        let Some(request_digest) = nonempty_stripped(data.get("request_digest")) else {
            return Vec::new();
        };
        if let Some(approval_id) = &binding.approval_id {
            if *approval_id != request_digest {
                return Vec::new();
            }
        }
        let Some(declared) = match_slot(work, "approval", &request_digest, None) else {
            return Vec::new();
        };
        let (result, source_revision) = if event_type == "approval_requested" {
            if !has_nonempty(
                &data,
                &["effect_class", "destination", "action_digest", "expires_at"],
            ) {
                return Vec::new();
            }
            if raw_string(&data, "effect_class").as_deref() != Some(binding.effect_class.as_str())
                || raw_string(&data, "destination").as_deref() != Some(binding.destination.as_str())
            {
                return Vec::new();
            }
            ("unknown", format!("request:{request_digest}:requested"))
        } else {
            if nonempty_stripped(data.get("deciding_principal_id")).is_none() {
                return Vec::new();
            }
            let decision = match nonempty_stripped(data.get("decision")).as_deref() {
                Some("approved") => ("passed", "approved"),
                Some("declined") => ("blocked", "declined"),
                _ => return Vec::new(),
            };
            (
                decision.0,
                format!("request:{}:decision:{}", request_digest, decision.1),
            )
        };
        return vec![event_fact(
            &declared,
            result,
            &format!("event:approvals:{event_type}"),
            ts,
            &source_revision,
            fresh_until,
            fact_revision,
        )];
    }
    // effect_state_changed
    if !has_nonempty(&data, &["idempotency_key", "state", "request_digest"]) {
        return Vec::new();
    }
    if let Some(idempotency_key) = &binding.idempotency_key {
        if raw_string(&data, "idempotency_key").as_deref() != Some(idempotency_key.as_str()) {
            return Vec::new();
        }
    }
    if let Some(approval_id) = &binding.approval_id {
        if raw_string(&data, "request_digest").as_deref() != Some(approval_id.as_str()) {
            return Vec::new();
        }
    }
    let Some(state) = nonempty_stripped(data.get("state")) else {
        return Vec::new();
    };
    if !EFFECT_STATES.contains(&state.as_str()) {
        return Vec::new();
    }
    match nonempty_stripped(data.get("previous_state")) {
        None => {
            if state != "prepared" {
                return Vec::new();
            }
        }
        Some(previous) => {
            if !EFFECT_STATES.contains(&previous.as_str()) {
                return Vec::new();
            }
        }
    }
    let Some(request_event) = approval_request_event else {
        return Vec::new();
    };
    let Some((request_type, _request_ts, request_data)) = producer_event_parts(request_event)
    else {
        return Vec::new();
    };
    if request_type != "approval_requested" {
        return Vec::new();
    }
    for key in [
        "request_digest",
        "work_order_id",
        "attempt_id",
        "effect_id",
        "effect_class",
        "destination",
        "action_digest",
    ] {
        if nonempty_stripped(request_data.get(key)).is_none() {
            return Vec::new();
        }
    }
    for key in ["request_digest", "work_order_id", "attempt_id", "effect_id"] {
        if request_data.get(key) != data.get(key) {
            return Vec::new();
        }
    }
    if raw_string(&request_data, "effect_class").as_deref() != Some(binding.effect_class.as_str())
        || raw_string(&request_data, "destination").as_deref() != Some(binding.destination.as_str())
    {
        return Vec::new();
    }
    let Some(idempotency_key) = nonempty_stripped(data.get("idempotency_key")) else {
        return Vec::new();
    };
    let result = match state.as_str() {
        "acknowledged" => "passed",
        "failed" => "failed",
        "blocked" => "blocked",
        _ => "unknown",
    };
    let source_revision = format!("effect:{idempotency_key}:state:{state}");
    let mut kinds = vec!["effect"];
    if state == "acknowledged" {
        kinds.push("acknowledgment");
    }
    let mut facts = Vec::new();
    for kind in kinds {
        if let Some(declared) = match_slot(work, kind, &binding.id, None) {
            facts.push(event_fact(
                &declared,
                result,
                "event:approvals:effect_state_changed",
                ts,
                &source_revision,
                fresh_until,
                fact_revision,
            ));
        }
    }
    facts
}

fn raw_string(data: &Value, key: &str) -> Option<String> {
    data.get(key).and_then(Value::as_str).map(str::to_string)
}

#[allow(clippy::too_many_arguments)]
fn event_fact(
    declared: &EvidenceSlot,
    result: &str,
    producer: &str,
    observed_at: DateTime<FixedOffset>,
    source_revision: &str,
    fresh_until: DateTime<FixedOffset>,
    fact_revision: &str,
) -> Fact {
    Fact {
        evidence: EvidenceValue {
            id: declared.id.clone(),
            work_order_id: declared.work_order_id.clone(),
            attempt_id: declared.attempt_id.clone(),
            subject_kind: declared.subject_kind.clone(),
            subject_id: declared.subject_id.clone(),
            result: result.to_string(),
        },
        producer: producer.to_string(),
        observed_at,
        source_revision: source_revision.to_string(),
        fresh_until,
        fact_revision: fact_revision.to_string(),
    }
}

/// Retain a rejected producer read when its requirement is identifiable.
fn reject_delivery_event(work: &CompanyWork, event: &Value, fact_revision: &str) -> Vec<Rejection> {
    let map = match event.as_object() {
        Some(map) => map,
        None => return Vec::new(),
    };
    let event_type = match map.get("type").and_then(Value::as_str) {
        Some(event_type) => event_type.to_string(),
        None => return Vec::new(),
    };
    let data = match map.get("data") {
        Some(data @ Value::Object(_)) => data,
        _ => return Vec::new(),
    };
    let producer = rejected_producer(&event_type, data);
    let diagnostics = rejected_binding_diagnostics(work, &event_type, event, data);
    if diagnostics.is_empty() {
        return Vec::new();
    }
    let evidence_ids = rejected_evidence_ids(work, &event_type, data);
    evidence_ids
        .into_iter()
        .map(|evidence_id| Rejection {
            diagnostic: format!(
                "requirement {evidence_id} from {producer} rejected binding: {}",
                diagnostics.join(", ")
            ),
            evidence_id,
            fact_revision: fact_revision.to_string(),
        })
        .collect()
}

fn rejected_producer(event_type: &str, data: &Value) -> String {
    if event_type == "delivery_evidence_observed" {
        return nonempty_stripped(data.get("producer"))
            .unwrap_or_else(|| "unknown producer".to_string());
    }
    let source = if matches!(
        event_type,
        "approval_requested" | "approval_decided" | "effect_state_changed"
    ) {
        "approvals"
    } else {
        "unknown"
    };
    format!("event:{source}:{event_type}")
}

fn rejected_evidence_ids(work: &CompanyWork, event_type: &str, data: &Value) -> Vec<String> {
    let required = required_evidence_ids(work);
    if event_type == "delivery_evidence_observed" {
        let evidence_id = data
            .get("evidence")
            .filter(|evidence| evidence.is_object())
            .and_then(|evidence| evidence.get("id"))
            .and_then(Value::as_str);
        return match evidence_id {
            Some(id) if required.contains(id) => vec![id.to_string()],
            _ => Vec::new(),
        };
    }
    let (subject_ids, kinds): (Vec<String>, Vec<&str>) = match event_type {
        "approval_requested" | "approval_decided" => {
            let mut subject_ids = Vec::new();
            if let Some(digest) = nonempty_stripped(data.get("request_digest")) {
                subject_ids.push(digest);
            }
            if let Some(binding) = producer_binding(work, data) {
                if let Some(approval_id) = binding.approval_id {
                    subject_ids.push(approval_id);
                }
            }
            (subject_ids, vec!["approval"])
        }
        "effect_state_changed" => {
            let subject_ids = match nonempty_stripped(data.get("effect_id")) {
                Some(effect_id) => vec![effect_id],
                None => Vec::new(),
            };
            (subject_ids, vec!["effect", "acknowledgment"])
        }
        _ => return Vec::new(),
    };
    if subject_ids.is_empty() {
        return Vec::new();
    }
    work.evidence
        .iter()
        .filter(|evidence| {
            required.contains(&evidence.id)
                && kinds.contains(&evidence.subject_kind.as_str())
                && subject_ids.contains(&evidence.subject_id)
        })
        .map(|evidence| evidence.id.clone())
        .collect()
}

fn rejected_binding_diagnostics(
    work: &CompanyWork,
    event_type: &str,
    event: &Value,
    data: &Value,
) -> Vec<String> {
    let mut diagnostics: Vec<String> = Vec::new();
    if event_type == "delivery_evidence_observed" {
        if observed_event_fact(event).is_none() {
            diagnostics.push("malformed delivery_evidence_observed event".to_string());
        }
        if raw_string(event, "source").as_deref() != Some("target") {
            diagnostics.push(format!("source {}", py_str_raw(event.get("source"))));
        }
        return diagnostics;
    }
    if raw_string(event, "source").as_deref() != Some("approvals") {
        diagnostics.push(format!("source {}", py_str_raw(event.get("source"))));
    }
    if let Some((node_id, attempt_id)) = &work.work_order {
        if data.get("work_order_id") != Some(&json!(node_id)) {
            diagnostics.push(format!(
                "work order {}",
                py_str_raw(data.get("work_order_id"))
            ));
        }
        if data.get("attempt_id") != Some(&json!(attempt_id)) {
            diagnostics.push(format!("attempt {}", py_str_raw(data.get("attempt_id"))));
        }
    }
    let required: &[&str] = match event_type {
        "approval_requested" => &[
            "request_digest",
            "effect_id",
            "effect_class",
            "destination",
            "action_digest",
            "expires_at",
        ],
        "approval_decided" => &[
            "request_digest",
            "decision",
            "deciding_principal_id",
            "effect_id",
        ],
        "effect_state_changed" => &[
            "idempotency_key",
            "state",
            "previous_state",
            "request_digest",
            "effect_id",
        ],
        _ => &[],
    };
    let missing: Vec<&str> = required
        .iter()
        .copied()
        .filter(|key| nonempty_stripped(data.get(*key)).is_none())
        .collect();
    if !missing.is_empty() {
        diagnostics.push(format!("missing {}", missing.join(", ")));
    }
    if diagnostics.is_empty() {
        diagnostics.push("producer event did not match its declared slot".to_string());
    }
    diagnostics
}

// ---------------------------------------------------------------------------
// Evaluator
// ---------------------------------------------------------------------------

fn evaluate_delivery(
    work: &CompanyWork,
    facts: Vec<FactInput>,
    evaluated_at: DateTime<FixedOffset>,
) -> Value {
    let required = required_evidence_ids(work);
    let mut valid_by_id: Vec<(String, Vec<Fact>)> = Vec::new();
    let mut rejected_by_id: Vec<(String, Vec<String>)> = Vec::new();
    let mut global_diagnostics: Vec<String> = Vec::new();
    let mut revisions: BTreeSet<String> = BTreeSet::new();

    for input in facts {
        match input {
            FactInput::Rejection(rejection) => {
                if required.contains(&rejection.evidence_id) {
                    bucket_strings(&mut rejected_by_id, &rejection.evidence_id)
                        .push(rejection.diagnostic);
                    revisions.insert(rejection.fact_revision);
                }
            }
            FactInput::Fact(fact) => {
                if fact.evidence.subject_kind == "observation" {
                    continue;
                }
                if !required.contains(&fact.evidence.id) {
                    continue;
                }
                revisions.insert(fact.fact_revision.clone());
                bucket_facts(&mut valid_by_id, &fact.evidence.id).push(fact);
            }
        }
    }

    let work_order = work.work_order.as_ref().expect("validated declaration");
    let no_facts: Vec<Fact> = Vec::new();
    if revisions.len() > 1 {
        let joined = revisions.iter().cloned().collect::<Vec<_>>().join(", ");
        let diagnostic = format!("mixed fact revisions: {joined}");
        let rows: Vec<Value> = work
            .deliverables
            .iter()
            .flat_map(|deliverable| {
                deliverable.required_evidence_ids.iter().map(|evidence_id| {
                    let declared = work
                        .evidence
                        .iter()
                        .find(|evidence| &evidence.id == evidence_id)
                        .expect("validated declaration");
                    let rejected = rejected_by_id
                        .iter()
                        .find(|(existing, _)| existing == evidence_id)
                        .map(|(_, items)| {
                            let mut items = items.clone();
                            items.push(diagnostic.clone());
                            items
                        })
                        .unwrap_or_else(|| vec![diagnostic.clone()]);
                    requirement_row(
                        &deliverable.id,
                        &declared.id,
                        &declared.subject_kind,
                        &declared.subject_id,
                        "unknown",
                        valid_by_id
                            .iter()
                            .find(|(existing, _)| existing == evidence_id)
                            .map(|(_, facts)| facts)
                            .unwrap_or(&no_facts),
                        rejected,
                    )
                })
            })
            .collect();
        return verdict_value(&work_order.0, &work_order.1, "unknown", None, rows, {
            global_diagnostics.push(diagnostic);
            global_diagnostics
        });
    }

    let mut rows: Vec<Value> = Vec::new();
    for deliverable in &work.deliverables {
        for evidence_id in &deliverable.required_evidence_ids {
            let declared = work
                .evidence
                .iter()
                .find(|evidence| &evidence.id == evidence_id)
                .expect("validated declaration");
            let rejected = rejected_by_id
                .iter()
                .find(|(existing, _)| existing == evidence_id)
                .map(|(_, items)| items.clone())
                .unwrap_or_default();
            let facts = valid_by_id
                .iter()
                .find(|(existing, _)| existing == evidence_id)
                .map(|(_, facts)| facts)
                .unwrap_or(&no_facts);
            rows.push(evaluate_requirement(
                &deliverable.id,
                declared,
                facts,
                rejected,
                evaluated_at,
            ));
        }
    }
    if !global_diagnostics.is_empty() {
        let diagnostic = global_diagnostics.join("; ");
        rows = rows
            .into_iter()
            .map(|row| {
                let mut updated = row.clone();
                if let Some(map) = updated.as_object_mut() {
                    map.insert("result".to_string(), Value::String("unknown".to_string()));
                    let mut items: Vec<Value> = map
                        .get("diagnostics")
                        .and_then(Value::as_array)
                        .cloned()
                        .unwrap_or_default();
                    items.push(Value::String(diagnostic.clone()));
                    map.insert("diagnostics".to_string(), Value::Array(items));
                }
                updated
            })
            .collect();
    }
    let aggregate = aggregate_of(&rows);
    let (aggregate, diagnostics) = if rows.is_empty() {
        ("unknown", {
            global_diagnostics.push("no declared delivery evidence requirements".to_string());
            global_diagnostics
        })
    } else {
        (aggregate, global_diagnostics)
    };
    verdict_value(
        &work_order.0,
        &work_order.1,
        aggregate,
        if revisions.len() == 1 {
            revisions.iter().next().cloned()
        } else {
            None
        },
        rows,
        diagnostics,
    )
}

fn bucket_strings<'a>(map: &'a mut Vec<(String, Vec<String>)>, key: &str) -> &'a mut Vec<String> {
    if let Some(position) = map.iter().position(|(existing, _)| existing == key) {
        return &mut map[position].1;
    }
    map.push((key.to_string(), Vec::new()));
    let last = map.len() - 1;
    &mut map[last].1
}

fn bucket_facts<'a>(map: &'a mut Vec<(String, Vec<Fact>)>, key: &str) -> &'a mut Vec<Fact> {
    if let Some(position) = map.iter().position(|(existing, _)| existing == key) {
        return &mut map[position].1;
    }
    map.push((key.to_string(), Vec::new()));
    let last = map.len() - 1;
    &mut map[last].1
}

fn evaluate_requirement(
    deliverable_id: &str,
    declared: &EvidenceDecl,
    facts: &[Fact],
    rejected: Vec<String>,
    evaluated_at: DateTime<FixedOffset>,
) -> Value {
    let mut diagnostics = rejected;
    let mut candidates: Vec<Fact> = Vec::new();
    for fact in facts {
        let evidence = &fact.evidence;
        let mut binding_errors: Vec<String> = Vec::new();
        if evidence.work_order_id != declared.work_order_id {
            binding_errors.push(format!("work order {}", evidence.work_order_id));
        }
        if evidence.attempt_id != declared.attempt_id {
            binding_errors.push(format!("attempt {}", evidence.attempt_id));
        }
        if evidence.subject_kind != declared.subject_kind {
            binding_errors.push(format!("subject kind {}", evidence.subject_kind));
        }
        if evidence.subject_id != declared.subject_id {
            binding_errors.push(format!("subject {}", evidence.subject_id));
        }
        if fact.observed_at > evaluated_at {
            binding_errors.push(format!(
                "future observation {}",
                py_isoformat(fact.observed_at)
            ));
        }
        if fact.fresh_until < evaluated_at {
            binding_errors.push(format!("stale after {}", py_isoformat(fact.fresh_until)));
        }
        if binding_errors.is_empty() {
            candidates.push(fact.clone());
        } else {
            diagnostics.push(format!(
                "requirement {} from {} rejected binding: {}",
                declared.id,
                fact.producer,
                binding_errors.join(", ")
            ));
        }
    }
    let row = |result: &str, facts: &[Fact], diagnostics: Vec<String>| {
        requirement_row(
            deliverable_id,
            &declared.id,
            &declared.subject_kind,
            &declared.subject_id,
            result,
            facts,
            diagnostics,
        )
    };
    if !diagnostics.is_empty() {
        return row("unknown", &candidates, diagnostics);
    }
    if candidates.is_empty() {
        let diagnostic = if declared.subject_kind == "observation" {
            format!(
                "requirement {}: observation subjects cannot satisfy delivery",
                declared.id
            )
        } else {
            format!("requirement {}: missing runtime fact", declared.id)
        };
        return row("unknown", &[], vec![diagnostic]);
    }
    let results: BTreeSet<&str> = candidates
        .iter()
        .map(|fact| fact.evidence.result.as_str())
        .collect();
    let bindings: BTreeSet<String> = candidates
        .iter()
        .map(|fact| {
            format!(
                "{}\u{0}{}\u{0}{}\u{0}{}",
                fact.evidence.work_order_id,
                fact.evidence.attempt_id,
                fact.evidence.subject_kind,
                fact.evidence.subject_id
            )
        })
        .collect();
    if results.len() > 1 || bindings.len() > 1 {
        let sources: BTreeSet<&str> = candidates
            .iter()
            .map(|fact| fact.producer.as_str())
            .collect();
        let diagnostic = format!(
            "requirement {}: conflicting duplicate facts from {}",
            declared.id,
            sources.into_iter().collect::<Vec<_>>().join(", ")
        );
        return row("unknown", &candidates, vec![diagnostic]);
    }
    let result = results.into_iter().next().expect("nonempty");
    let mut explicit = Vec::new();
    if result == "unknown" {
        explicit.push(format!(
            "requirement {} explicitly reported unknown",
            declared.id
        ));
    }
    row(result, &candidates, explicit)
}

fn requirement_row(
    deliverable_id: &str,
    evidence_id: &str,
    subject_kind: &str,
    subject_id: &str,
    result: &str,
    facts: &[Fact],
    diagnostics: Vec<String>,
) -> Value {
    let producers: BTreeSet<&str> = facts.iter().map(|fact| fact.producer.as_str()).collect();
    let revisions: BTreeSet<&str> = facts
        .iter()
        .map(|fact| fact.source_revision.as_str())
        .collect();
    json!({
        "deliverable_id": deliverable_id,
        "evidence_id": evidence_id,
        "subject_kind": subject_kind,
        "subject_id": subject_id,
        "result": result,
        "producers": producers.into_iter().collect::<Vec<_>>(),
        "source_revisions": revisions.into_iter().collect::<Vec<_>>(),
        "diagnostics": diagnostics,
    })
}

fn aggregate_of(rows: &[Value]) -> &'static str {
    let results: Vec<&str> = rows
        .iter()
        .map(|row| {
            row.get("result")
                .and_then(Value::as_str)
                .unwrap_or("unknown")
        })
        .collect();
    if !results.is_empty() && results.iter().all(|result| *result == "passed") {
        return "passed";
    }
    if results.contains(&"failed") {
        return "failed";
    }
    if results.contains(&"blocked") {
        return "blocked";
    }
    "unknown"
}

fn verdict_value(
    work_order_node_id: &str,
    attempt_id: &str,
    aggregate: &str,
    fact_revision: Option<String>,
    requirements: Vec<Value>,
    diagnostics: Vec<String>,
) -> Value {
    let required_requirements: Vec<Value> = requirements
        .iter()
        .map(|row| {
            json!({
                "deliverable_id": str_field(row, "deliverable_id"),
                "evidence_id": str_field(row, "evidence_id"),
            })
        })
        .collect();
    json!({
        "evaluator_version": EVALUATOR_VERSION,
        "session_id": None::<String>,
        "work_order_node_id": work_order_node_id,
        "attempt_id": attempt_id,
        "aggregate": aggregate,
        "fact_revision": fact_revision,
        "required_requirements": required_requirements,
        "requirements": requirements,
        "diagnostics": diagnostics,
    })
}

fn evidence_revision(verdict: &Value) -> String {
    let mut canonical = verdict.clone();
    if let Some(map) = canonical.as_object_mut() {
        map.remove("fact_revision");
        map.remove("session_id");
    }
    let mut out = String::new();
    write_canonical_ensure_ascii(&canonical, &mut out);
    format!("sha256:{}", sha256_hex(out.as_bytes()))
}

// ---------------------------------------------------------------------------
// The command body
// ---------------------------------------------------------------------------

/// Evaluate an activated plan from one immutable journal byte snapshot.
pub fn evaluate_plan_delivery(plan_path: &Path, events_path: &Path) -> Value {
    let evaluated_at = utc_now();
    let (active, work) = match load_company_work(plan_path) {
        Ok(pair) => pair,
        Err(message) => return undeterminable(message),
    };
    if !active {
        return response_value(
            "inactive",
            None,
            None,
            None,
            vec!["generic delivery completion is not activated".to_string()],
        );
    }
    if work.work_order.is_none()
        || work.deliverables.is_empty()
        || required_evidence_ids(&work).is_empty()
    {
        return inactive_declaration();
    }
    let snapshot = match read_snapshot(events_path) {
        Ok(snapshot) => snapshot,
        Err(JournalRead::Conflict(revisions)) => {
            return conflict_response(&work, &revisions, evaluated_at)
        }
        Err(JournalRead::Unreadable(message)) => return undeterminable(message),
    };
    let fact_revision = format!("sha256:{}", sha256_hex(&snapshot.raw));
    let selected = match current_evidence_events(&snapshot.events, &work, &fact_revision) {
        Ok(selected) => selected,
        Err(message) => return undeterminable(message),
    };
    let mut inputs: Vec<FactInput> = Vec::new();
    for (event, request_event) in selected {
        match event {
            SelectedEvent::Rejection(rejection) => inputs.push(FactInput::Rejection(rejection)),
            SelectedEvent::Json(event) => {
                if event.get("type").and_then(Value::as_str) == Some("delivery_evidence_observed") {
                    let parsed = observed_event_fact(&event);
                    let source_is_target =
                        raw_string(&event, "source").as_deref() == Some("target");
                    match (parsed, source_is_target) {
                        (Some(mut fact), true) => {
                            fact.fact_revision = fact_revision.clone();
                            inputs.push(FactInput::Fact(fact));
                        }
                        // A valid read from a non-target source rejects its
                        // requirement; an empty rejection simply drops it.
                        (Some(_), false) => {
                            let rejected = reject_delivery_event(&work, &event, &fact_revision);
                            inputs.extend(rejected.into_iter().map(FactInput::Rejection));
                        }
                        _ => {
                            let rejected = reject_delivery_event(&work, &event, &fact_revision);
                            if rejected.is_empty() {
                                return undeterminable(
                                    "malformed delivery_evidence_observed event".to_string(),
                                );
                            }
                            inputs.extend(rejected.into_iter().map(FactInput::Rejection));
                        }
                    }
                    continue;
                }
                let facts = adapt_delivery_event(
                    &work,
                    &event,
                    evaluated_at,
                    &fact_revision,
                    request_event.as_ref(),
                );
                if !facts.is_empty() {
                    inputs.extend(facts.into_iter().map(FactInput::Fact));
                    continue;
                }
                let rejected = reject_delivery_event(&work, &event, &fact_revision);
                inputs.extend(rejected.into_iter().map(FactInput::Rejection));
            }
        }
    }
    let mut verdict = evaluate_delivery(&work, inputs, evaluated_at);
    if verdict
        .get("fact_revision")
        .map(Value::is_null)
        .unwrap_or(false)
    {
        if let Some(map) = verdict.as_object_mut() {
            map.insert(
                "fact_revision".to_string(),
                Value::String(fact_revision.clone()),
            );
        }
    }
    let revision = evidence_revision(&verdict);
    let diagnostics: Vec<String> = seq_field(&verdict, "diagnostics")
        .iter()
        .map(|item| item.as_str().unwrap_or_default().to_string())
        .collect();
    response_value(
        "evaluated",
        Some(fact_revision),
        Some(revision),
        Some(verdict),
        diagnostics,
    )
}

fn conflict_response(
    work: &CompanyWork,
    revisions: &[String],
    evaluated_at: DateTime<FixedOffset>,
) -> Value {
    let fact_revision = format!("conflict:{}", revisions.join(":"));
    let diagnostic = format!(
        "event journal changed during read; conflicting revisions {}",
        revisions.join(", ")
    );
    let inputs: Vec<FactInput> = work
        .deliverables
        .iter()
        .flat_map(|deliverable| deliverable.required_evidence_ids.iter())
        .map(|evidence_id| {
            FactInput::Rejection(Rejection {
                diagnostic: format!("requirement {evidence_id}: {diagnostic}"),
                evidence_id: evidence_id.clone(),
                fact_revision: fact_revision.clone(),
            })
        })
        .collect();
    let mut verdict = evaluate_delivery(work, inputs, evaluated_at);
    if let Some(map) = verdict.as_object_mut() {
        map.insert(
            "diagnostics".to_string(),
            Value::Array(vec![Value::String(diagnostic.clone())]),
        );
    }
    let revision = evidence_revision(&verdict);
    response_value(
        "evaluated",
        Some(fact_revision),
        Some(revision),
        Some(verdict),
        vec![diagnostic],
    )
}
