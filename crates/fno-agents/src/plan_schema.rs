//! The single-doc plan frontmatter schema: the Rust port of the deleted
//! `cli/src/fno/plan/schema.py` pydantic authority. One hidden binary-direct
//! door (`plan-schema`, matched with `matches!` in the client so the parity
//! guard and menu caps never see it) answers three ops over one stdin JSON
//! payload: the whole-frontmatter model, the two date gates, and the two
//! block shapes validate-plan.sh validates. Every refusal text is the string
//! the Python leg printed, carried verbatim.

use serde_json::{json, Map, Value};
use std::io::Read;

/// The monotonic axis plus the off-axis terminals (`plan/_status.py`).
const STATUS_VOCABULARY: [&str; 7] = [
    "idea",
    "design",
    "ready",
    "in_progress",
    "in_review",
    "done",
    "superseded",
];

/// Retired spellings, accepted on read and never written.
const STATUS_ALIASES: [(&str, &str); 3] = [
    ("shipped", "in_review"),
    ("archived", "superseded"),
    ("stub", "idea"),
];

/// The rungs the waves gate applies at; `idea` and `design` carry no strategy
/// by design.
const WAVES_GATED_STATUSES: [&str; 3] = ["ready", "in_progress", "in_review"];

/// Date the Execution Strategy requirement became required for executable
/// plans (plan/schema.py WAVES_REQUIRED_AFTER). A literal, never computed.
const WAVES_REQUIRED_AFTER: (i32, u32, u32) = (2026, 9, 2);

/// `[a-z][a-z0-9]{0,7}-?[0-9a-f]{4,8}` fullmatch: the config-free liberal
/// node-id shape (graph/_constants.py NODE_ID_BODY).
fn is_wellformed_node_id(v: &str) -> bool {
    let re = regex::Regex::new(r"^[a-z][a-z0-9]{0,7}-?[0-9a-f]{4,8}$").unwrap();
    re.is_match(v)
}

/// The consolidation-entry fallback shape: the same body case-insensitively,
/// excluding an all-hex git-hash literal (the regex's `(?![0-9a-f]+$)` guard).
fn is_legacy_id_literal(v: &str) -> bool {
    let re = regex::Regex::new(r"^[a-z][a-z0-9]{0,7}-?[0-9a-f]{4,8}$").unwrap();
    let low = v.to_lowercase();
    re.is_match(&low) && !low.bytes().all(|b| b.is_ascii_hexdigit())
}

/// `^d-[0-9a-f]{4,32}$`, case-insensitive (plan/_decision_shape.py).
fn looks_like_decision_id(token: &str) -> bool {
    let re = regex::Regex::new(r"(?i)^d-[0-9a-f]{4,32}$").unwrap();
    re.is_match(token.trim())
}

/// One collected violation in the shape plan/cli.py renders:
/// `{field, message, got}` (`got` omitted when the field was absent, the
/// same rule the old error renderer applied to `missing`).
fn violation(field: &str, message: impl Into<String>, got: Option<&Value>) -> Value {
    let mut out = json!({"field": field, "message": message.into()});
    if let Some(g) = got {
        out["got"] = g.clone();
    }
    out
}

/// Fold a raw status value the way the model's before-validator did: an
/// unquoted YAML bool becomes its lowercase word, then a retired spelling
/// resolves to its survivor. Alias matching is exact, never case-folded.
fn coerce_status(v: &Value) -> Value {
    match v {
        Value::Bool(b) => Value::String(b.to_string()),
        Value::String(s) => {
            for (alias, canonical) in STATUS_ALIASES {
                if s == alias {
                    return Value::String(canonical.to_string());
                }
            }
            v.clone()
        }
        other => other.clone(),
    }
}

/// The shared date-keyed difficulty refusal. `undatable_refuses` splits the
/// scopes: the minting lanes have no fallback dater and refuse an undatable
/// plan, while the authoring scope (validate-plan.sh) dates those plans
/// itself and defers.
pub(crate) fn difficulty_gate_error(
    fm: &Map<String, Value>,
    undatable_refuses: bool,
) -> Option<String> {
    if fm.is_empty() {
        return None;
    }
    let (y, m, d) = match gate_created(fm) {
        Ok(ymd) => ymd,
        Err(raw) => {
            if !undatable_refuses {
                return None;
            }
            return Some(match raw {
                None => "cannot read created: absent from frontmatter; the difficulty \
                         gate needs a date. Set created: <YYYY-MM-DD> and, for \
                         post-gate plans, difficulty: low, medium, high."
                    .to_string(),
                Some(raw) => format!(
                    "cannot read created: {}; the difficulty gate needs a date. \
                     Set created: <YYYY-MM-DD> and, for post-gate plans, \
                     difficulty: low, medium, high.",
                    super::backlog::intake::yaml_display(&raw)
                ),
            });
        }
    };
    if (y, m, d) <= (2026, 8, 26) {
        return None;
    }
    match fm.get("difficulty").and_then(Value::as_str) {
        Some(b) if ["low", "medium", "high"].contains(&b.trim().to_lowercase().as_str()) => None,
        _ => Some(format!(
            "difficulty is required for plans created after \
             2026-08-26: set it to one of low, medium, high (an intrinsic \
             property of the work - the expected time, edge cases, unknowns, \
             and what a senior tech lead makes of the problem; not a model \
             or capacity hint)"
        )),
    }
}

/// The gate date read shared by both date gates: `Ok` a real calendar date,
/// `Err` the raw undatable value. Impossible dates (2026-02-31: a day the
/// calendar never had) route to the undatable branch beside absent and
/// unparseable ones, so a minting lane refuses them and an authoring scope
/// defers.
fn gate_created(fm: &Map<String, Value>) -> Result<(i32, u32, u32), Option<Value>> {
    let raw = fm.get("created");
    let Some((y, m, d)) = super::backlog::intake::parse_created(raw) else {
        return Err(raw.cloned());
    };
    if !super::backlog::intake::valid_ymd(y, m, d) {
        return Err(raw.cloned());
    }
    Ok((y, m, d))
}

/// Whether `created` dates the plan past the difficulty gate. None: empty or
/// undatable, which the lenient scopes treat as no band requirement.
pub(crate) fn created_after_gate(fm: &Map<String, Value>) -> Option<bool> {
    if fm.is_empty() {
        return None;
    }
    gate_created(fm)
        .ok()
        .map(|(y, m, d)| (y, m, d) > (2026, 8, 26))
}

/// The full `created` timestamp: a real calendar date, and any time tail a
/// real time. The deleted pydantic field was `datetime`, so a garbage tail
/// ("2026-08-20Tnot-a-time") refused there and refuses here; `iso_created`
/// alone accepts any suffix after the T.
fn valid_created_string(s: &str) -> bool {
    use chrono::{DateTime, NaiveDateTime};
    if !s.is_ascii() || s.len() < 10 {
        return false;
    }
    let date_ok = super::backlog::intake::valid_ymd(
        s[..4].parse().unwrap_or(0),
        s[5..7].parse().unwrap_or(0),
        s[8..10].parse().unwrap_or(0),
    );
    if !date_ok {
        return false;
    }
    if s.len() == 10 {
        return true;
    }
    const NAIVE: [&str; 6] = [
        "%Y-%m-%dT%H:%M",
        "%Y-%m-%dT%H:%M:%S",
        "%Y-%m-%dT%H:%M:%S%.f",
        "%Y-%m-%d %H:%M",
        "%Y-%m-%d %H:%M:%S",
        "%Y-%m-%d %H:%M:%S%.f",
    ];
    NAIVE
        .iter()
        .any(|f| NaiveDateTime::parse_from_str(s, f).is_ok())
        || NAIVE
            .iter()
            .any(|f| DateTime::parse_from_str(s, &format!("{f}%#z")).is_ok())
}

/// The date-keyed Execution Strategy refusal, or None. Same shape as the
/// difficulty gate, including the `undatable_refuses` split. A plan below
/// `ready` is exempt whatever its date.
pub(crate) fn waves_gate_error(
    fm: &Map<String, Value>,
    has_execution_strategy: bool,
    undatable_refuses: bool,
) -> Option<String> {
    let raw_status = fm.get("status");
    let status = raw_status.map(coerce_status);
    let gated = status
        .as_ref()
        .and_then(Value::as_str)
        .map(|s| WAVES_GATED_STATUSES.contains(&s.trim().to_lowercase().as_str()))
        .unwrap_or(false);
    if !gated || has_execution_strategy {
        return None;
    }
    let (y, m, d) = match gate_created(fm) {
        Ok(ymd) => ymd,
        Err(raw) => {
            if !undatable_refuses {
                return None;
            }
            return Some(match raw {
                None => "cannot read created: absent from frontmatter; the Execution \
                         Strategy gate needs a date. Set created: <YYYY-MM-DD> and, \
                         for post-gate plans, an ## Execution Strategy section."
                    .to_string(),
                Some(raw) => format!(
                    "cannot read created: {}; the Execution Strategy gate needs a \
                     date. Set created: <YYYY-MM-DD> and, for post-gate plans, an \
                     ## Execution Strategy section.",
                    super::backlog::intake::yaml_display(&raw)
                ),
            });
        }
    };
    if (y, m, d) <= WAVES_REQUIRED_AFTER {
        return None;
    }
    Some(
        "## Execution Strategy is required for plans created after \
         2026-09-02: a plan with no waves block measures width 0, so \
         `fno backlog join` cannot tell a genuinely single-task plan from \
         one that never declared its parallelism. One numbered change \
         becomes one task, its **Files:** line becomes the task surface, \
         and the tasks go in one wave with mode: parallel"
            .to_string(),
    )
}

/// One consolidation row: `{id, reason}` for the candidate lists,
/// `{decision_id, reason}` for `decisions_acknowledged`. The id must be a
/// node id or a legacy literal; a decision id must carry the d-hex shape;
/// the reason must be a non-empty string.
fn validate_consolidation_entry(
    entries: &Value,
    field: &str,
    decision_rows: bool,
    errors: &mut Vec<Value>,
) {
    let Some(rows) = entries.as_array() else {
        errors.push(violation(
            field,
            "Input should be a valid list",
            Some(entries),
        ));
        return;
    };
    for (index, row) in rows.iter().enumerate() {
        let item_field = format!("{field}.{index}");
        let Some(map) = row.as_object() else {
            errors.push(violation(
                &item_field,
                "Input should be a valid dictionary",
                Some(row),
            ));
            continue;
        };
        let (key_field, detail) = if decision_rows {
            (
                "decision_id",
                "Value error, is not a decision id (expected d-<hex>, e.g. d-a4b6e1c8)",
            )
        } else {
            (
                "id",
                "Value error, is not a node id (expected <prefix>-<hex>, e.g. x-aaaa)",
            )
        };
        match map.get(key_field).and_then(Value::as_str) {
            Some(id) => {
                let ok = if decision_rows {
                    looks_like_decision_id(id)
                } else {
                    is_wellformed_node_id(id) || is_legacy_id_literal(id)
                };
                if !ok {
                    errors.push(violation(
                        &format!("{item_field}.{key_field}"),
                        detail,
                        Some(&Value::String(id.to_string())),
                    ));
                }
            }
            None => errors.push(violation(
                &format!("{item_field}.{key_field}"),
                "Field required",
                None,
            )),
        }
        match map.get("reason").and_then(Value::as_str) {
            Some(r) if !r.is_empty() => {}
            Some(r) => errors.push(violation(
                &format!("{item_field}.reason"),
                "String should have at least 1 character",
                Some(&Value::String(r.to_string())),
            )),
            None => errors.push(violation(
                &format!("{item_field}.reason"),
                "Field required",
                None,
            )),
        }
    }
}

/// The step 2d consolidation block's shape, and the plan-level dispatch hold.
/// `kind` is "consolidation" or "dispatch_hold"; unknown kinds refuse.
pub(crate) fn validate_block(kind: &str, block: &Value) -> Vec<Value> {
    let mut errors = Vec::new();
    let Some(map) = block.as_object() else {
        errors.push(violation(
            "block",
            "Input should be a valid dictionary",
            Some(block),
        ));
        return errors;
    };
    match kind {
        "consolidation" => {
            match map.get("outcome").and_then(Value::as_str) {
                Some(o) if ["absorb", "append", "proceed_alone"].contains(&o) => {}
                Some(o) => errors.push(violation(
                    "consolidation.outcome",
                    "Input should be 'absorb', 'append' or 'proceed_alone'",
                    Some(&Value::String(o.to_string())),
                )),
                None => errors.push(violation("consolidation.outcome", "Field required", None)),
            }
            for key in [
                "absorbed",
                "appended_to",
                "proceed_alone_against",
                "rejected",
                "decisions_acknowledged",
            ] {
                if let Some(entries) = map.get(key) {
                    let field = format!("consolidation.{key}");
                    validate_consolidation_entry(
                        entries,
                        &field,
                        key == "decisions_acknowledged",
                        &mut errors,
                    );
                }
            }
            let outcome = map
                .get("outcome")
                .and_then(Value::as_str)
                .unwrap_or_default();
            for (key, plural) in [("absorb", "absorbed"), ("append", "appended_to")] {
                if outcome == key {
                    let empty = map
                        .get(plural)
                        .and_then(Value::as_array)
                        .map(|a| a.is_empty())
                        .unwrap_or(true);
                    if empty {
                        errors.push(violation(
                            "consolidation",
                            format!(
                                "Value error, outcome {key} requires at least one {plural} entry"
                            ),
                            None,
                        ));
                    }
                }
            }
            if let Some(reversal) = map.get("reversal") {
                let ok = reversal.is_string()
                    || reversal
                        .as_array()
                        .map(|a| a.iter().all(Value::is_string))
                        .unwrap_or(false);
                if !ok {
                    errors.push(violation(
                        "consolidation.reversal",
                        "Input should be a valid string or a valid list of strings",
                        Some(reversal),
                    ));
                }
            }
        }
        "dispatch_hold" => {
            for key in ["reason", "release_when", "set_by"] {
                match map.get(key) {
                    Some(Value::String(s)) if !s.trim().is_empty() => {}
                    Some(Value::String(s)) => errors.push(violation(
                        &format!("dispatch_hold.{key}"),
                        "Value error, must not be blank",
                        Some(&Value::String(s.clone())),
                    )),
                    Some(other) => errors.push(violation(
                        &format!("dispatch_hold.{key}"),
                        "Input should be a valid string",
                        Some(other),
                    )),
                    None => errors.push(violation(
                        &format!("dispatch_hold.{key}"),
                        "Field required",
                        None,
                    )),
                }
            }
            match map.get("review_on") {
                Some(Value::String(s)) => {
                    if super::backlog::intake::parse_created(Some(&Value::String(s.clone())))
                        .is_none()
                    {
                        errors.push(violation(
                            "dispatch_hold.review_on",
                            "Input should be a valid date",
                            Some(&Value::String(s.clone())),
                        ));
                    }
                }
                Some(other) => errors.push(violation(
                    "dispatch_hold.review_on",
                    "Input should be a valid date",
                    Some(other),
                )),
                None => errors.push(violation("dispatch_hold.review_on", "Field required", None)),
            }
        }
        other => errors.push(violation(
            "block.kind",
            &format!("unknown block kind {other:?}: consolidation or dispatch_hold"),
            None,
        )),
    }
    errors
}

/// The whole-frontmatter model. Field checks run in the field order the
/// pydantic model declared, then the model-level validators (difficulty
/// gate, company-work node match), collecting every violation.
pub(crate) fn validate_frontmatter(fm: &Map<String, Value>) -> Vec<Value> {
    let mut errors = Vec::new();

    // Model before-validator: claims fills an absent OR NULL node (pydantic's
    // before-validator saw null the same as absent).
    let mut working = fm.clone();
    if working.get("claims").is_some() && working.get("node").map_or(true, Value::is_null) {
        working.insert("node".into(), working["claims"].clone());
    }
    let fm = &working;

    // node: required string.
    match fm.get("node") {
        Some(Value::String(_)) => {}
        Some(other) => errors.push(violation(
            "node",
            "Input should be a valid string",
            Some(other),
        )),
        None => errors.push(violation("node", "Field required", None)),
    }

    // status: required, bool-coerced, alias-resolved, vocabulary-checked.
    match fm.get("status") {
        Some(raw) => {
            let coerced = coerce_status(raw);
            match coerced.as_str() {
                Some(s) if STATUS_VOCABULARY.contains(&s) => {}
                _ => errors.push(violation(
                    "status",
                    "Input should be 'idea', 'design', 'ready', 'in_progress', 'in_review', 'done' or 'superseded'",
                    Some(raw),
                )),
            }
        }
        None => errors.push(violation("status", "Field required", None)),
    }

    // created: required dateable; a bare number refuses by name.
    match fm.get("created") {
        Some(Value::Bool(_)) | Some(Value::Number(_)) => errors.push(violation(
            "created",
            "Value error, created must be a date (YYYY-MM-DD), not a bare number",
            Some(fm.get("created").unwrap()),
        )),
        Some(v @ Value::String(s)) => {
            if super::backlog::intake::iso_created(s.trim()) && !valid_created_string(s.trim()) {
                errors.push(violation(
                    "created",
                    "Input should be a valid date",
                    Some(v),
                ));
            }
        }
        Some(other) => errors.push(violation(
            "created",
            "Input should be a valid date",
            Some(other),
        )),
        None => errors.push(violation("created", "Field required", None)),
    }

    // Optional typed keys.
    if let Some(v) = fm.get("size") {
        if !v.is_null() && !matches!(v.as_str(), Some("S") | Some("M") | Some("L")) {
            errors.push(violation(
                "size",
                "Input should be 'S', 'M' or 'L'",
                Some(v),
            ));
        }
    }
    if let Some(v) = fm.get("difficulty") {
        if let Some(band) = v.as_str() {
            if !["low", "medium", "high"].contains(&band.trim().to_lowercase().as_str()) {
                errors.push(violation(
                    "difficulty",
                    "Value error, difficulty must be one of: low, medium, high",
                    Some(v),
                ));
            }
        } else if !v.is_null() {
            errors.push(violation(
                "difficulty",
                "Value error, difficulty must be one of: low, medium, high",
                Some(v),
            ));
        }
    }
    match fm.get("join") {
        None => {}
        Some(Value::Null) => {}
        Some(v) => match v.as_str().map(|s| s.trim().to_lowercase()) {
            Some(mode) if mode == "manual" || mode == "auto" => {}
            _ => errors.push(violation(
                "join",
                "Value error, join must be one of: manual, auto",
                Some(v),
            )),
        },
    }
    if let Some(v) = fm.get("carveouts") {
        if let Some(mode) = v.as_str() {
            let mode = mode.trim().to_lowercase();
            if mode != "allowed" && mode != "forbidden" {
                errors.push(violation(
                    "carveouts",
                    "Value error, carveouts must be one of: allowed, forbidden",
                    Some(v),
                ));
            }
        } else if !v.is_null() {
            errors.push(violation(
                "carveouts",
                "Value error, carveouts must be one of: allowed, forbidden",
                Some(v),
            ));
        }
    }
    for key in ["blocked_by", "urls", "session_ids"] {
        if let Some(v) = fm.get(key) {
            let ok = v
                .as_array()
                .map(|a| a.iter().all(Value::is_string))
                .unwrap_or(false);
            if !ok && !v.is_null() {
                errors.push(violation(
                    key,
                    "Input should be a valid list of strings",
                    Some(v),
                ));
            }
        }
    }
    for key in ["kill_criteria", "close_probes"] {
        if let Some(v) = fm.get(key) {
            let ok = v.is_string() || v.is_array() || v.is_null();
            if !ok {
                errors.push(violation(
                    key,
                    "Input should be a valid string or a valid list",
                    Some(v),
                ));
            }
        }
    }
    if let Some(v) = fm.get("acceptance_contract") {
        if !v.is_null() && v.as_str() != Some("compiled-v1") {
            errors.push(violation(
                "acceptance_contract",
                "Input should be 'compiled-v1'",
                Some(v),
            ));
        }
    }
    if let Some(v) = fm.get("completion") {
        if !v.is_null() && v.as_str() != Some("delivery") {
            errors.push(violation(
                "completion",
                "Input should be 'delivery'",
                Some(v),
            ));
        }
    }
    if let Some(v) = fm.get("expected_url_count") {
        match v {
            Value::Null => {}
            Value::Number(n) => {
                let floor = n.as_i64().unwrap_or(0);
                if floor < 1 {
                    errors.push(violation(
                        "expected_url_count",
                        "Input should be greater than or equal to 1",
                        Some(v),
                    ));
                }
            }
            other => errors.push(violation(
                "expected_url_count",
                "Input should be a valid integer",
                Some(other),
            )),
        }
    }
    if let Some(v) = fm.get("acceptance_evidence") {
        if !v.is_null() && !v.is_object() {
            errors.push(violation(
                "acceptance_evidence",
                "Input should be a valid dictionary",
                Some(v),
            ));
        }
    }
    if let Some(block) = fm.get("consolidation") {
        if !block.is_null() {
            for e in validate_block("consolidation", block) {
                errors.push(e);
            }
        }
    }
    if let Some(block) = fm.get("dispatch_hold") {
        if !block.is_null() {
            for e in validate_block("dispatch_hold", block) {
                errors.push(e);
            }
        }
    }

    // Model-level: the date-keyed difficulty gate, authoring scope. The
    // model defers an undatable plan exactly like validate-plan.sh: the
    // blueprint mint dates the doc itself, and a frontmatter still missing
    // `created` gets the created field error above, never a gate refusal.
    if let Some(gate) = difficulty_gate_error(fm, false) {
        errors.push(violation("difficulty", gate, None));
    }

    // Model-level: the company work_order must name this plan's node. The
    // deep refs validation stays with the company family; the door checks
    // the one rule schema.py owned, plus the shallow shape pydantic's typed
    // model enforced (a scalar company_work or a missing work_order is the
    // author's error to see, not a valid plan).
    if let Some(cw) = fm.get("company_work") {
        if !cw.is_null() {
            match cw.as_object() {
                None => errors.push(violation(
                    "company_work",
                    "Input should be a valid dictionary",
                    Some(cw),
                )),
                Some(cw_obj) => match cw_obj.get("work_order") {
                    None => {
                        errors.push(violation("company_work.work_order", "Field required", None))
                    }
                    Some(work_order) => {
                        if let Some(order_node) = work_order.get("node_id").and_then(Value::as_str)
                        {
                            let plan_node =
                                fm.get("node").and_then(Value::as_str).unwrap_or_default();
                            if order_node != plan_node {
                                errors.push(violation(
                                    "company_work",
                                    "Value error, company_work work_order node_id must match plan node",
                                    Some(cw),
                                ));
                            }
                        }
                    }
                },
            }
        }
    }

    errors
}

/// The hidden `plan-schema` door: one JSON request in, one verdict out,
/// always exit 0. Ops: `validate-frontmatter`, `gates`, `validate-block`.
/// Dispatches as the `state plan-schema` action argument (state_path.rs
/// pre-table), beside the settings fact-set door.
pub fn run_plan_schema(args: &[String]) -> i32 {
    let _ = args;
    let mut input = String::new();
    let _ = std::io::stdin().read_to_string(&mut input);
    let request: Value = serde_json::from_str(input.trim()).unwrap_or(Value::Null);
    let op = request.get("op").and_then(Value::as_str).unwrap_or("");
    let verdict = match op {
        "validate-frontmatter" => {
            let empty = Map::new();
            let fm = request
                .get("frontmatter")
                .and_then(Value::as_object)
                .unwrap_or(&empty);
            let errors = validate_frontmatter(fm);
            json!({"valid": errors.is_empty(), "errors": errors})
        }
        "gates" => {
            let empty = Map::new();
            let fm = request
                .get("frontmatter")
                .and_then(Value::as_object)
                .unwrap_or(&empty);
            let undatable_refuses = request
                .get("undatable_refuses")
                .and_then(Value::as_bool)
                .unwrap_or(true);
            let has_strategy = request
                .get("has_execution_strategy")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            json!({
                "difficulty_gate": difficulty_gate_error(fm, undatable_refuses),
                "waves_gate": waves_gate_error(fm, has_strategy, undatable_refuses),
                "created_after": created_after_gate(fm),
            })
        }
        "validate-block" => {
            let kind = request.get("kind").and_then(Value::as_str).unwrap_or("");
            let null_block = Value::Null;
            let block = request.get("block").unwrap_or(&null_block);
            let errors = validate_block(kind, block);
            json!({"valid": errors.is_empty(), "errors": errors})
        }
        other => json!({
            "valid": false,
            "errors": [violation("op", format!("unknown op {other:?}"), None)],
        }),
    };
    println!("{verdict}");
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fm(pairs: &[(&str, Value)]) -> Map<String, Value> {
        let mut m = Map::new();
        for (k, v) in pairs {
            m.insert(k.to_string(), v.clone());
        }
        m
    }

    #[test]
    fn the_difficulty_gate_refuses_only_post_gate_bandless_plans() {
        let mut base = fm(&[
            ("node", json!("x-1234abcd")),
            ("status", json!("ready")),
            ("created", json!("2026-08-26")),
        ]);
        assert_eq!(difficulty_gate_error(&base, true), None);
        *base.get_mut("created").unwrap() = json!("2026-08-27");
        let err = difficulty_gate_error(&base, true).unwrap();
        assert!(err.starts_with("difficulty is required for plans created after 2026-08-26"));
        base.insert("difficulty".into(), json!("Medium"));
        assert_eq!(difficulty_gate_error(&base, true), None);
        // The authoring scope defers an undatable plan; minting refuses it.
        let mut undatable = base.clone();
        undatable.remove("created");
        assert!(difficulty_gate_error(&undatable, false).is_none());
        assert!(difficulty_gate_error(&undatable, true)
            .unwrap()
            .starts_with("cannot read created: absent"));
        // An impossible calendar date is an undatable one: the minting
        // scope refuses it instead of honoring a pre-gate reading.
        let mut impossible = base.clone();
        impossible.insert("created".into(), json!("2026-02-31"));
        assert!(difficulty_gate_error(&impossible, true)
            .unwrap()
            .starts_with("cannot read created: 2026-02-31"));
        assert_eq!(difficulty_gate_error(&impossible, false), None);
    }

    #[test]
    fn the_waves_gate_spares_pre_gate_and_below_ready_plans() {
        let post_gate = fm(&[("status", json!("ready")), ("created", json!("2026-09-03"))]);
        let err = waves_gate_error(&post_gate, false, false).unwrap();
        assert!(err.starts_with("## Execution Strategy is required"));
        // The refusal names the boundary date so an author can date their plan.
        assert!(err.contains("2026-09-02"), "{err}");
        assert_eq!(waves_gate_error(&post_gate, true, false), None);
        let design = fm(&[
            ("status", json!("design")),
            ("created", json!("2026-09-03")),
        ]);
        assert_eq!(waves_gate_error(&design, false, false), None);
        // The other pre-execution rung: a /think doc waiting on /blueprint.
        let idea = fm(&[("status", json!("idea")), ("created", json!("2026-09-03"))]);
        assert_eq!(waves_gate_error(&idea, false, false), None);
        let shipped = fm(&[
            ("status", json!("shipped")),
            ("created", json!("2026-09-03")),
        ]);
        // The alias resolves to in_review, which is gated.
        assert!(waves_gate_error(&shipped, false, false).is_some());
        // A lane with no fallback dater refuses rather than minting a
        // strategy-less plan it cannot date.
        let undatable = fm(&[("status", json!("ready"))]);
        assert!(waves_gate_error(&undatable, false, true)
            .unwrap()
            .starts_with("cannot read created"));
        // Impossible dates refuse beside absent ones in the minting scope.
        let mut impossible = post_gate.clone();
        impossible.insert("created".into(), json!("2026-02-31"));
        assert!(waves_gate_error(&impossible, false, true)
            .unwrap()
            .starts_with("cannot read created"));
    }

    #[test]
    fn the_entry_id_takes_the_legacy_dashless_literal() {
        let block = |id: &str| {
            json!({
                "outcome": "proceed_alone",
                "rejected": [{"id": id, "reason": "recorded"}],
            })
        };
        assert!(validate_block("consolidation", &block("x4d12")).is_empty());
        assert!(validate_block("consolidation", &block("a3f9c1d2")).is_empty());
        let errors = validate_block("consolidation", &block("9abc"));
        assert!(errors
            .iter()
            .any(|e| e["field"] == "consolidation.rejected.0.id"));
    }

    #[test]
    fn created_after_gate_reads_only_exact_dates() {
        assert_eq!(
            created_after_gate(&fm(&[("created", json!("2026-08-26"))])),
            Some(false)
        );
        assert_eq!(
            created_after_gate(&fm(&[("created", json!("2026-08-27"))])),
            Some(true)
        );
        assert_eq!(
            created_after_gate(&fm(&[("created", json!(20260827))])),
            None
        );
        assert_eq!(created_after_gate(&Map::new()), None);
    }

    #[test]
    fn the_consolidation_block_enforces_outcome_and_entry_shapes() {
        let block = json!({"outcome": "absorb"});
        let errors = validate_block("consolidation", &block);
        assert!(errors
            .iter()
            .any(|e| e["message"]
                == "Value error, outcome absorb requires at least one absorbed entry"));
        let block = json!({
            "outcome": "proceed_alone",
            "proceed_alone_against": [{"id": "not an id", "reason": ""}],
            "rejected": [{"id": "x-1b2c3d4e", "reason": "different scope"}],
            "decisions_acknowledged": [{"decision_id": "D-ABC123", "reason": "live"}],
            "reversal": "fno backlog rank x-1b2c3d4e --top",
        });
        let errors = validate_block("consolidation", &block);
        assert_eq!(errors.len(), 2, "{errors:?}");
        assert!(errors
            .iter()
            .any(|e| e["field"] == "consolidation.proceed_alone_against.0.id"));
        assert!(errors
            .iter()
            .any(|e| e["field"] == "consolidation.proceed_alone_against.0.reason"));
    }

    #[test]
    fn the_model_refuses_the_missing_core_and_a_bare_number_created() {
        let errors = validate_frontmatter(&Map::new());
        let fields: Vec<&str> = errors
            .iter()
            .map(|e| e["field"].as_str().unwrap())
            .collect();
        assert!(fields.contains(&"node"));
        assert!(fields.contains(&"status"));
        assert!(fields.contains(&"created"));
        let errors = validate_frontmatter(&fm(&[
            ("node", json!("x-1234abcd")),
            ("status", json!("ready")),
            ("created", json!(20260827)),
        ]));
        assert!(errors.iter().any(|e| e["message"]
            == "Value error, created must be a date (YYYY-MM-DD), not a bare number"));
        // A garbage time tail refuses: pydantic parsed the datetime, so a
        // value like this was never a valid created.
        let errors = validate_frontmatter(&fm(&[
            ("node", json!("x-1234abcd")),
            ("status", json!("ready")),
            ("created", json!("2026-08-20Tnot-a-time")),
        ]));
        assert!(errors
            .iter()
            .any(|e| e["field"] == "created" && e["message"] == "Input should be a valid date"));
    }

    #[test]
    fn the_model_fills_node_from_claims_and_coerces_status() {
        let errors = validate_frontmatter(&fm(&[
            ("claims", json!("x-1234abcd")),
            ("status", json!(true)),
            ("created", json!("2026-01-01")),
        ]));
        // status true -> "true" is not in the vocabulary: the only error.
        assert_eq!(errors.len(), 1, "{errors:?}");
        assert_eq!(errors[0]["field"], "status");
        let errors = validate_frontmatter(&fm(&[
            ("claims", json!("x-1234abcd")),
            ("status", json!("stub")),
            ("created", json!("2026-01-01")),
        ]));
        assert!(errors.is_empty(), "{errors:?}");
        // A null node is an absent one: pydantic's before-validator filled
        // it from claims, and claims-bound design docs keep that contract.
        let errors = validate_frontmatter(&fm(&[
            ("claims", json!("x-1234abcd")),
            ("node", json!(Value::Null)),
            ("status", json!("ready")),
            ("created", json!("2026-01-01")),
        ]));
        assert!(errors.is_empty(), "{errors:?}");
    }

    #[test]
    fn the_model_enforces_the_bands_joins_and_company_rule() {
        let errors = validate_frontmatter(&fm(&[
            ("node", json!("x-1234abcd")),
            ("status", json!("ready")),
            ("created", json!("2026-01-01")),
            ("join", json!("sometimes")),
            ("carveouts", json!("maybe")),
            ("expected_url_count", json!(0)),
        ]));
        assert_eq!(errors.len(), 3, "{errors:?}");
        let errors = validate_frontmatter(&fm(&[
            ("node", json!("x-1234abcd")),
            ("status", json!("ready")),
            ("created", json!("2026-01-01")),
            (
                "company_work",
                json!({"work_order": {"node_id": "x-other"}}),
            ),
        ]));
        assert!(errors
            .iter()
            .any(|e| e["message"]
                == "Value error, company_work work_order node_id must match plan node"));
        // The completion vocabulary: 'delivery' or absent, nothing else.
        let errors = validate_frontmatter(&fm(&[
            ("node", json!("x-1234abcd")),
            ("status", json!("ready")),
            ("created", json!("2026-01-01")),
            ("completion", json!("marketing")),
        ]));
        assert!(errors
            .iter()
            .any(|e| e["field"] == "completion" && e["message"] == "Input should be 'delivery'"));
        // The shallow company_work shape: a scalar or a work_order-less
        // mapping is the author's error to see, never a valid plan.
        let errors = validate_frontmatter(&fm(&[
            ("node", json!("x-1234abcd")),
            ("status", json!("ready")),
            ("created", json!("2026-01-01")),
            ("company_work", json!("solo")),
        ]));
        assert!(errors.iter().any(|e| e["field"] == "company_work"
            && e["message"] == "Input should be a valid dictionary"));
        let errors = validate_frontmatter(&fm(&[
            ("node", json!("x-1234abcd")),
            ("status", json!("ready")),
            ("created", json!("2026-01-01")),
            ("company_work", json!({"function": {"id": "operations"}})),
        ]));
        assert!(errors
            .iter()
            .any(|e| e["field"] == "company_work.work_order" && e["message"] == "Field required"));
    }
}
