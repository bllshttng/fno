//! The block-module validators: `fno/config/_attention.py`, `_active_backlog.py`,
//! `routing_blocks.py`, `_evals.py`, `_lead.py`, `_dispatch_commands.py`,
//! `_sweeps.py`, `split_block.py`, `build_block.py`, `status_sinks.py` and
//! `spawn_blocks.py`.

use super::{py_repr, Ctx, Fail, Registry};
use serde_json::{Map, Value};
use std::sync::OnceLock;

const LEAD_CHECKIN_TEXT: &str = concat!(
    "lead check-in. Run fno agents org checkin: it gathers the check-in ",
    "readings, prints them, diffs the last beat, and journals lead_checkin. ",
    "Then act on the printout per the lead skill. When nothing changed and ",
    "coverage is full, print 'no change' and stop. This beat is a heartbeat. ",
);

const STATUS_SINK_TYPES: [&str; 3] = ["json-webhook", "text-webhook", "backlog-progress"];

// `events/schema.yaml` `protocol_family.envelope.allowed` minus `data`.
const MATCH_KEYS: [&str; 14] = [
    "ts", "v", "type", "source", "from", "model", "host", "project", "node", "task", "run",
    "parent", "outcome", "harness",
];

pub(super) fn register(reg: &mut Registry) {
    reg.model_before("AttentionBlock", ignore_retired_rows);

    reg.before("DispatchBlock", &["auto_merge"], coerce_auto_merge);
    reg.before("DispatchBlock", &["on_exhaustion"], coerce_on_exhaustion);
    reg.before(
        "DispatchBlock",
        &["cutover_low_after_minutes"],
        coerce_cutover_low,
    );
    reg.before(
        "DispatchBlock",
        &["blueprint_floor"],
        coerce_blueprint_floor,
    );

    reg.before("LeadBlock", &["checkin_interval"], coerce_checkin_interval);
    reg.before(
        "LeadBlock",
        &["implementation_guard"],
        coerce_implementation_guard,
    );
    reg.before("LeadBlock", &["write_roots"], coerce_write_roots);
    reg.before("LeadBlock", &["checkin_text"], coerce_term_text);

    reg.before("ActiveBacklogConfig", &["enabled"], coerce_backlog_enabled);
    reg.before("ActiveBacklogConfig", &["interval"], coerce_interval);
    reg.before(
        "ActiveBacklogConfig",
        &["failure_limit"],
        coerce_failure_limit,
    );
    reg.before(
        "ActiveBacklogConfig",
        &["max_concurrent"],
        coerce_max_concurrent,
    );

    reg.before("EvalsBlock", &["enabled"], coerce_evals_enabled);
    reg.model_before("EvalsBlock", sanitize_days);

    reg.after("StatusSinkConfig", &["name"], safe_name);
    reg.after("StatusSinkConfig", &["type"], known_type);
    reg.after("StatusSinkConfig", &["match"], match_keys_in_whitelist);
    reg.model_after("StatusSinkConfig", url_xor_url_env);

    reg.before("RoutingBlock", &["objective"], coerce_objective);

    reg.before("ReapBlock", &["roster_scope"], coerce_roster_scope);
    reg.before("StateReapBlock", &["enabled"], coerce_state_reap_enabled);
    reg.before(
        "StateReapBlock",
        &[
            "locks_retain_days",
            "expired_claims_retain_days",
            "pr_status_cache_retain_days",
        ],
        coerce_days,
    );
    // SweepKeys is AgentsBlock's base class; its validators bind there.
    reg.before(
        "AgentsBlock",
        &[
            "single_flight_ttl_seconds",
            "single_flight_join_budget_seconds",
            "orphan_reap_after_seconds",
        ],
        coerce_seconds,
    );

    reg.before("BuildBlock", &["agent_freeze"], reject_non_bool);
    reg.before("SplitBlock", &["opens"], coerce_opens);

    // SpawnProfileBlock subclasses SpawnDefaultsBlock and inherits its validator.
    reg.before("SpawnDefaultsBlock", &["harness"], keep_dict_values);
    reg.before("SpawnProfileBlock", &["harness"], keep_dict_values);
    reg.before("SpawnProfileBlock", &["by_difficulty"], keep_dict_values);
}

// ---------------------------------------------------------------------------
// Python builtins the validators lean on
// ---------------------------------------------------------------------------

/// `str.strip()`: Python's whitespace set adds the four separators 0x1c-0x1f.
fn py_strip(text: &str) -> &str {
    text.trim_matches(|c: char| c.is_whitespace() || ('\x1c'..='\x1f').contains(&c))
}

/// `isinstance(v, int) and not isinstance(v, bool)`: the int itself.
fn as_int(v: &Value) -> Option<i128> {
    match v {
        Value::Number(n) if n.is_i64() => n.as_i64().map(i128::from),
        Value::Number(n) if n.is_u64() => n.as_u64().map(i128::from),
        _ => None,
    }
}

fn as_float(v: &Value) -> Option<f64> {
    match v {
        Value::Number(n) if n.is_f64() => n.as_f64(),
        _ => None,
    }
}

/// `int(text)` on a str: surrounding whitespace, one sign, digits with
/// single underscores between them. `None` is the ValueError.
fn py_int_str(text: &str) -> Option<i128> {
    let body = py_strip(text);
    let (negative, digits) = match body.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, body.strip_prefix('+').unwrap_or(body)),
    };
    if digits.is_empty()
        || digits.starts_with('_')
        || digits.ends_with('_')
        || digits.contains("__")
    {
        return None;
    }
    let clean: String = digits.chars().filter(|c| *c != '_').collect();
    if !clean.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let magnitude: i128 = clean.parse().ok()?;
    Some(if negative { -magnitude } else { magnitude })
}

/// `int(v)` for any value: `None` is the TypeError or ValueError.
fn py_int(v: &Value) -> Option<i128> {
    match v {
        Value::Bool(b) => Some(i128::from(*b)),
        Value::String(s) => py_int_str(s),
        Value::Number(_) => as_int(v).or_else(|| {
            as_float(v)
                .filter(|f| f.is_finite())
                .map(|f| f.trunc() as i128)
        }),
        _ => None,
    }
}

fn int_value(n: i128) -> Value {
    match i64::try_from(n) {
        Ok(small) => Value::from(small),
        Err(_) => match u64::try_from(n) {
            Ok(big) => Value::from(big),
            Err(_) => Value::from(n as f64),
        },
    }
}

/// `_coerce_affirmative(v, False)` in `fno/config/__init__.py`.
fn coerce_affirmative(v: &Value) -> bool {
    match v {
        Value::Bool(b) => *b,
        Value::Number(_) => as_int(v) == Some(1),
        Value::String(s) => matches!(
            py_strip(s).to_lowercase().as_str(),
            "1" | "true" | "yes" | "on"
        ),
        _ => false,
    }
}

fn is_one_of(v: &Value, choices: &[&str]) -> bool {
    matches!(v, Value::String(s) if choices.contains(&s.as_str()))
}

// ---------------------------------------------------------------------------
// _attention.py
// ---------------------------------------------------------------------------

fn ignore_retired_rows(_ctx: &mut Ctx, v: Value) -> Result<Value, Fail> {
    Ok(if v.is_array() {
        Value::Object(Map::new())
    } else {
        v
    })
}

// ---------------------------------------------------------------------------
// _dispatch_commands.py
// ---------------------------------------------------------------------------

fn coerce_auto_merge(_ctx: &mut Ctx, v: Value) -> Result<Value, Fail> {
    Ok(if v.is_boolean() {
        v
    } else {
        Value::Bool(false)
    })
}

fn coerce_on_exhaustion(_ctx: &mut Ctx, v: Value) -> Result<Value, Fail> {
    Ok(if is_one_of(&v, &["defer", "failover"]) {
        v
    } else {
        Value::from("defer")
    })
}

fn coerce_cutover_low(_ctx: &mut Ctx, v: Value) -> Result<Value, Fail> {
    Ok(match as_int(&v) {
        Some(n) if n >= 0 => v,
        _ => Value::from(0),
    })
}

fn coerce_blueprint_floor(_ctx: &mut Ctx, v: Value) -> Result<Value, Fail> {
    if let Value::String(s) = &v {
        let lowered = py_strip(s).to_lowercase();
        if matches!(lowered.as_str(), "high" | "medium" | "low" | "never") {
            return Ok(Value::from(lowered));
        }
    }
    Ok(Value::from("high"))
}

// ---------------------------------------------------------------------------
// _lead.py
// ---------------------------------------------------------------------------

fn coerce_checkin_interval(_ctx: &mut Ctx, v: Value) -> Result<Value, Fail> {
    static SHAPE: OnceLock<regex::Regex> = OnceLock::new();
    let shape =
        SHAPE.get_or_init(|| regex::Regex::new(r"^\d+[smhd]?$").expect("interval regex compiles"));
    if let Value::String(s) = &v {
        let stripped = py_strip(s);
        if shape.is_match(stripped) {
            return Ok(Value::from(stripped));
        }
    }
    Ok(Value::from("55m"))
}

fn coerce_implementation_guard(_ctx: &mut Ctx, v: Value) -> Result<Value, Fail> {
    if let Value::String(s) = &v {
        let stripped = py_strip(s);
        if matches!(stripped, "refuse" | "warn" | "off") {
            return Ok(Value::from(stripped));
        }
    }
    Ok(Value::from("refuse"))
}

fn coerce_write_roots(_ctx: &mut Ctx, v: Value) -> Result<Value, Fail> {
    let items = match v {
        Value::String(s) => vec![Value::String(s)],
        Value::Array(items) => items,
        _ => return Ok(Value::Array(Vec::new())),
    };
    let roots: Vec<Value> = items
        .iter()
        .filter_map(Value::as_str)
        .map(py_strip)
        .filter(|s| !s.is_empty())
        .map(Value::from)
        .collect();
    Ok(Value::Array(roots))
}

fn coerce_term_text(_ctx: &mut Ctx, v: Value) -> Result<Value, Fail> {
    let kept = matches!(&v, Value::String(s) if !py_strip(s).is_empty());
    Ok(if kept {
        v
    } else {
        Value::from(LEAD_CHECKIN_TEXT)
    })
}

// ---------------------------------------------------------------------------
// _active_backlog.py
// ---------------------------------------------------------------------------

fn coerce_backlog_enabled(ctx: &mut Ctx, v: Value) -> Result<Value, Fail> {
    if v.is_boolean() {
        return Ok(v);
    }
    match &v {
        Value::Object(map) => {
            let out: Map<String, Value> = map
                .iter()
                .map(|(k, val)| (k.clone(), Value::Bool(coerce_affirmative(val))))
                .collect();
            Ok(Value::Object(out))
        }
        _ => {
            if v.is_string() {
                ctx.warn(format!(
                    "config active_backlog.enabled is the quoted string {}; write a bare true/false so a readout cannot mislead",
                    py_repr(&v)
                ));
            }
            Ok(Value::Bool(coerce_affirmative(&v)))
        }
    }
}

fn coerce_interval(_ctx: &mut Ctx, v: Value) -> Result<Value, Fail> {
    if v.is_string() {
        return Ok(v);
    }
    if let Some(n) = as_int(&v) {
        return Ok(Value::from(format!("{n}s")));
    }
    // `int(float)` truncates exactly; `{:.0}` prints the whole f64 digit for digit.
    match as_float(&v) {
        Some(f) if f.is_finite() => Ok(Value::from(format!("{:.0}s", f.trunc() + 0.0))),
        _ => Ok(Value::from("5m")),
    }
}

fn positive_or(v: Value, default: i64) -> Value {
    if let Some(n) = as_int(&v) {
        return if n >= 1 { v } else { Value::from(default) };
    }
    if let Value::String(s) = &v {
        return match py_int_str(s) {
            Some(n) if n >= 1 => int_value(n),
            _ => Value::from(default),
        };
    }
    Value::from(default)
}

fn coerce_failure_limit(_ctx: &mut Ctx, v: Value) -> Result<Value, Fail> {
    Ok(positive_or(v, 3))
}

fn coerce_max_concurrent(_ctx: &mut Ctx, v: Value) -> Result<Value, Fail> {
    Ok(positive_or(v, 1))
}

// ---------------------------------------------------------------------------
// _evals.py
// ---------------------------------------------------------------------------

/// `_coerce_bool_default_true` in `fno/config/__init__.py`.
fn coerce_evals_enabled(_ctx: &mut Ctx, v: Value) -> Result<Value, Fail> {
    let on = match &v {
        Value::Bool(b) => *b,
        Value::Number(_) if as_int(&v).is_some() => as_int(&v) == Some(1),
        Value::String(s) => matches!(
            py_strip(s).to_lowercase().as_str(),
            "1" | "true" | "yes" | "on"
        ),
        _ => true,
    };
    Ok(Value::Bool(on))
}

fn sanitize_days(ctx: &mut Ctx, v: Value) -> Result<Value, Fail> {
    let mut out = match v {
        Value::Object(map) => map,
        other => return Ok(other),
    };
    let checks: [(&str, i128); 4] = [
        ("schedule_days", 0),
        ("stale_days", 0),
        ("scratch_window_days", 0),
        ("scratch_threshold", 1),
    ];
    for (key, floor) in checks {
        let Some(raw) = out.get(key) else { continue };
        let ok = !raw.is_boolean() && py_int(raw).is_some_and(|n| n >= floor);
        if !ok {
            ctx.note(format!(
                "config.evals.{key}={} invalid; using default",
                py_repr(raw)
            ));
            out.shift_remove(key);
        }
    }
    Ok(Value::Object(out))
}

// ---------------------------------------------------------------------------
// status_sinks.py
// ---------------------------------------------------------------------------

fn safe_name(_ctx: &mut Ctx, v: Value) -> Result<Value, Fail> {
    let text = v.as_str().unwrap_or_default();
    let mut chars = text.chars();
    let ok = chars.next().is_some_and(|c| c.is_ascii_alphanumeric())
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'));
    if ok {
        return Ok(v);
    }
    Err(Fail::Value(format!(
        "status sink name {} must match [A-Za-z0-9][A-Za-z0-9._-]* (it keys the cursor filename; no path separators or leading dot)",
        py_repr(&v)
    )))
}

fn known_type(_ctx: &mut Ctx, v: Value) -> Result<Value, Fail> {
    if is_one_of(&v, &STATUS_SINK_TYPES) {
        return Ok(v);
    }
    Err(Fail::Value(format!(
        "unknown status sink type {} (allowed: {})",
        py_repr(&v),
        STATUS_SINK_TYPES.join(", ")
    )))
}

fn match_keys_in_whitelist(_ctx: &mut Ctx, v: Value) -> Result<Value, Fail> {
    let bad: Vec<Value> = v
        .as_object()
        .into_iter()
        .flatten()
        .filter(|(k, _)| !MATCH_KEYS.contains(&k.as_str()))
        .map(|(k, _)| Value::from(k.as_str()))
        .collect();
    if bad.is_empty() {
        return Ok(v);
    }
    let mut allowed = MATCH_KEYS.to_vec();
    allowed.sort_unstable();
    Err(Fail::Value(format!(
        "status sink match key(s) {} not in the protocol envelope whitelist (allowed: {})",
        py_repr(&Value::Array(bad)),
        allowed.join(", ")
    )))
}

fn url_xor_url_env(_ctx: &mut Ctx, m: &mut Map<String, Value>) -> Result<(), Fail> {
    let truthy = |key: &str| matches!(m.get(key), Some(Value::String(s)) if !s.is_empty());
    let kind = m.get("type").and_then(Value::as_str).unwrap_or_default();
    if matches!(kind, "json-webhook" | "text-webhook") && truthy("url") == truthy("url_env") {
        let name = m.get("name").cloned().unwrap_or(Value::Null);
        return Err(Fail::Value(format!(
            "status sink {} ({kind}) requires exactly one of url / url_env",
            py_repr(&name)
        )));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// routing_blocks.py
// ---------------------------------------------------------------------------

fn coerce_objective(_ctx: &mut Ctx, v: Value) -> Result<Value, Fail> {
    if is_one_of(
        &v,
        &["cheapest-that-clears", "best-available", "prefer-harness"],
    ) {
        return Ok(v);
    }
    Ok(Value::from("cheapest-that-clears"))
}

// ---------------------------------------------------------------------------
// _sweeps.py (the DEGRADED record is doctor-only and prints nothing at load)
// ---------------------------------------------------------------------------

fn coerce_roster_scope(_ctx: &mut Ctx, v: Value) -> Result<Value, Fail> {
    if let Value::String(s) = &v {
        let lowered = py_strip(s).to_lowercase();
        if matches!(lowered.as_str(), "off" | "provenanced" | "all") {
            return Ok(Value::from(lowered));
        }
    }
    Ok(Value::from("provenanced"))
}

fn coerce_state_reap_enabled(_ctx: &mut Ctx, v: Value) -> Result<Value, Fail> {
    Ok(if v.is_boolean() { v } else { Value::Bool(true) })
}

fn coerce_days(ctx: &mut Ctx, v: Value) -> Result<Value, Fail> {
    let default = match ctx.field_name.as_str() {
        "expired_claims_retain_days" => 30,
        "pr_status_cache_retain_days" => 14,
        _ => 7,
    };
    Ok(match as_int(&v) {
        Some(n) if n > 0 => v,
        _ => Value::from(default),
    })
}

fn coerce_seconds(ctx: &mut Ctx, v: Value) -> Result<Value, Fail> {
    let default = match ctx.field_name.as_str() {
        "single_flight_join_budget_seconds" => 30,
        "orphan_reap_after_seconds" => 5400,
        _ => 60,
    };
    Ok(match py_int(&v) {
        Some(n) if n > 0 => int_value(n),
        _ => Value::from(default),
    })
}

// ---------------------------------------------------------------------------
// build_block.py, split_block.py, spawn_blocks.py
// ---------------------------------------------------------------------------

fn reject_non_bool(_ctx: &mut Ctx, v: Value) -> Result<Value, Fail> {
    if v.is_boolean() {
        return Ok(v);
    }
    Err(Fail::Value(
        "build.agent_freeze is a TOML bool, not a string".to_string(),
    ))
}

fn coerce_opens(_ctx: &mut Ctx, v: Value) -> Result<Value, Fail> {
    let portal = matches!(&v, Value::String(s) if py_strip(s).to_lowercase() == "portal");
    Ok(Value::from(if portal { "portal" } else { "pane" }))
}

/// `_coerce_harness_overlays` / `_coerce_by_difficulty`: a non-mapping
/// table is empty, and a non-mapping entry drops.
fn keep_dict_values(_ctx: &mut Ctx, v: Value) -> Result<Value, Fail> {
    match v {
        Value::Object(map) => Ok(Value::Object(
            map.into_iter().filter(|(_, val)| val.is_object()).collect(),
        )),
        _ => Ok(Value::Object(Map::new())),
    }
}
