//! The role-definition schema, checked over raw JSON. A port of the
//! pydantic contracts in `fno.roles.models` and `fno.company.contracts`
//! as far as discovery reads them: every model is frozen and refuses
//! unknown fields, every `NonEmptyStr` trims and must stay non-empty, and
//! the scalar fields accept pydantic's lax-mode inputs (a bool from `1`,
//! `"yes"` or `1.0`, an int from `"2"` or `2.0`), so a document valid
//! under the Python schema stays valid here.
//!
//! The check answers valid or invalid and a reason. The reason wording is
//! this module's own: pydantic's error text is not a contract.

use serde_json::{Map, Value};

/// The fixed precedence order. Discovery walks one directory per layer
/// and sorts every record by this index first.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum RoleLayer {
    BuiltIn,
    Plugin,
    Company,
    Project,
    Plan,
}

impl RoleLayer {
    pub const ALL: [RoleLayer; 5] = [
        RoleLayer::BuiltIn,
        RoleLayer::Plugin,
        RoleLayer::Company,
        RoleLayer::Project,
        RoleLayer::Plan,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            RoleLayer::BuiltIn => "built-in",
            RoleLayer::Plugin => "plugin",
            RoleLayer::Company => "company",
            RoleLayer::Project => "project",
            RoleLayer::Plan => "plan",
        }
    }

    pub fn parse(text: &str) -> Option<RoleLayer> {
        RoleLayer::ALL.into_iter().find(|l| l.as_str() == text)
    }
}

const STATUSES: [&str; 3] = ["valid", "invalid", "unreadable"];
const AUTHORITY: [&str; 3] = ["advisory", "internal", "external"];
const APPROVAL: [&str; 3] = ["none", "principal", "founder"];
const CONTEXT_KINDS: [&str; 6] = [
    "plan",
    "brief",
    "graph-node",
    "artifact",
    "source",
    "config",
];
const SENSITIVITY: [&str; 4] = ["public", "internal", "sensitive", "restricted"];
const LAYERS: [&str; 5] = ["built-in", "plugin", "company", "project", "plan"];

/// A `RoleRef`: `(id, function_id)`, both trimmed.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct RoleRef {
    pub id: String,
    pub function_id: String,
}

/// The fields of a schema-valid `RoleDefinitionSource` that discovery
/// reports. The document itself travels on as raw JSON.
#[derive(Clone, Debug)]
pub struct DefinitionSource {
    pub layer: RoleLayer,
    pub source_id: String,
    pub role: RoleRef,
    pub status: &'static str,
    pub error: Option<String>,
}

type Obj = Map<String, Value>;

fn object<'a>(value: &'a Value, at: &str) -> Result<&'a Obj, String> {
    value
        .as_object()
        .ok_or_else(|| format!("{at}: input should be an object"))
}

fn only_fields(obj: &Obj, allowed: &[&str], at: &str) -> Result<(), String> {
    match obj.keys().find(|k| !allowed.contains(&k.as_str())) {
        Some(extra) => Err(format!("{at}.{extra}: extra inputs are not permitted")),
        None => Ok(()),
    }
}

/// A present, non-null field. Pydantic treats an explicit null on a
/// non-optional field as a type error, not as the default.
fn present<'a>(obj: &'a Obj, key: &str) -> Option<&'a Value> {
    obj.get(key)
}

fn non_empty(value: &Value, at: &str) -> Result<String, String> {
    let text = value
        .as_str()
        .ok_or_else(|| format!("{at}: input should be a valid string"))?
        .trim();
    if text.is_empty() {
        return Err(format!("{at}: string should have at least 1 character"));
    }
    Ok(text.to_string())
}

fn required_str(obj: &Obj, key: &str, at: &str) -> Result<String, String> {
    match present(obj, key) {
        Some(value) => non_empty(value, &format!("{at}.{key}")),
        None => Err(format!("{at}.{key}: field required")),
    }
}

fn optional_str(obj: &Obj, key: &str, at: &str) -> Result<Option<String>, String> {
    match present(obj, key) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => non_empty(value, &format!("{at}.{key}")).map(Some),
    }
}

fn str_tuple(obj: &Obj, key: &str, required: bool, at: &str) -> Result<Vec<String>, String> {
    let at = format!("{at}.{key}");
    match present(obj, key) {
        None if required => Err(format!("{at}: field required")),
        None => Ok(Vec::new()),
        Some(Value::Array(items)) => items
            .iter()
            .enumerate()
            .map(|(i, item)| non_empty(item, &format!("{at}.{i}")))
            .collect(),
        Some(_) => Err(format!("{at}: input should be a valid tuple")),
    }
}

fn choice(
    obj: &Obj,
    key: &str,
    allowed: &[&str],
    default: Option<&'static str>,
    at: &str,
) -> Result<String, String> {
    let at = format!("{at}.{key}");
    match present(obj, key) {
        None => default
            .map(str::to_string)
            .ok_or_else(|| format!("{at}: field required")),
        Some(Value::String(text)) if allowed.contains(&text.as_str()) => Ok(text.clone()),
        Some(_) => Err(format!(
            "{at}: input should be one of {}",
            allowed.join(", ")
        )),
    }
}

/// Pydantic's lax bool: a bool, 0 or 1 as an int or float, or one of its
/// fixed words in any case (never trimmed).
fn lax_bool(obj: &Obj, key: &str, default: bool, at: &str) -> Result<bool, String> {
    let at = format!("{at}.{key}");
    let bad = || format!("{at}: input should be a valid boolean");
    match present(obj, key) {
        None => Ok(default),
        Some(Value::Bool(b)) => Ok(*b),
        Some(Value::Number(n)) => match n.as_f64() {
            Some(f) if f == 0.0 => Ok(false),
            Some(f) if f == 1.0 => Ok(true),
            _ => Err(bad()),
        },
        Some(Value::String(text)) => match text.to_ascii_lowercase().as_str() {
            "0" | "off" | "f" | "false" | "n" | "no" => Ok(false),
            "1" | "on" | "t" | "true" | "y" | "yes" => Ok(true),
            _ => Err(bad()),
        },
        Some(_) => Err(bad()),
    }
}

/// Pydantic's lax int with `ge=0`: an int, a bool, a float or a trimmed
/// numeric string with no fractional part.
fn lax_count(obj: &Obj, key: &str, at: &str) -> Result<f64, String> {
    let at = format!("{at}.{key}");
    let bad = || format!("{at}: input should be a valid integer");
    let number = match present(obj, key) {
        None => return Ok(0.0),
        Some(Value::Bool(b)) => f64::from(u8::from(*b)),
        Some(Value::Number(n)) => n.as_f64().ok_or_else(bad)?,
        Some(Value::String(text)) => text.trim().parse::<f64>().map_err(|_| bad())?,
        Some(_) => return Err(bad()),
    };
    if !number.is_finite() || number.fract() != 0.0 {
        return Err(bad());
    }
    if number < 0.0 {
        return Err(format!("{at}: input should be greater than or equal to 0"));
    }
    Ok(number)
}

pub fn role_ref(value: &Value, at: &str) -> Result<RoleRef, String> {
    let obj = object(value, at)?;
    only_fields(obj, &["id", "function_id"], at)?;
    Ok(RoleRef {
        id: required_str(obj, "id", at)?,
        function_id: required_str(obj, "function_id", at)?,
    })
}

fn nested<'a>(obj: &'a Obj, key: &str, at: &str) -> Result<&'a Value, String> {
    present(obj, key).ok_or_else(|| format!("{at}.{key}: field required"))
}

fn check_routing_hint(value: &Value, at: &str) -> Result<(), String> {
    let obj = object(value, at)?;
    only_fields(obj, &["provider", "model"], at)?;
    let provider = optional_str(obj, "provider", at)?;
    let model = optional_str(obj, "model", at)?;
    if provider.is_none() && model.is_none() {
        return Err(format!("{at}: routing_hint must name a provider or model"));
    }
    Ok(())
}

fn check_review_policy(value: &Value, at: &str) -> Result<(), String> {
    let obj = object(value, at)?;
    only_fields(
        obj,
        &["required", "minimum_reviewers", "required_review_kinds"],
        at,
    )?;
    let required = lax_bool(obj, "required", false, at)?;
    let minimum = lax_count(obj, "minimum_reviewers", at)?;
    let kinds = str_tuple(obj, "required_review_kinds", false, at)?;
    if !required && (minimum != 0.0 || !kinds.is_empty()) {
        return Err(format!("{at}: review requirements require required=true"));
    }
    if required && minimum < 1.0 {
        return Err(format!("{at}: required review needs at least one reviewer"));
    }
    Ok(())
}

fn check_delivery_policy(value: &Value, at: &str) -> Result<(), String> {
    let obj = object(value, at)?;
    only_fields(obj, &["required_evidence", "require_all_deliverables"], at)?;
    str_tuple(obj, "required_evidence", false, at)?;
    lax_bool(obj, "require_all_deliverables", true, at)?;
    Ok(())
}

fn check_context_selector(value: &Value, at: &str) -> Result<(), String> {
    let obj = object(value, at)?;
    only_fields(
        obj,
        &[
            "kind",
            "identifier",
            "max_sensitivity",
            "requires_freshness",
        ],
        at,
    )?;
    choice(obj, "kind", &CONTEXT_KINDS, None, at)?;
    optional_str(obj, "identifier", at)?;
    choice(obj, "max_sensitivity", &SENSITIVITY, Some("internal"), at)?;
    lax_bool(obj, "requires_freshness", false, at)?;
    Ok(())
}

fn unique(items: &[String], at: &str, field: &str) -> Result<(), String> {
    let mut seen = std::collections::HashSet::new();
    if items.iter().all(|item| seen.insert(item)) {
        Ok(())
    } else {
        Err(format!("{at}: {field} must be unique"))
    }
}

/// A `RoleManifest`; answers its role on success.
fn check_manifest(value: &Value, at: &str) -> Result<RoleRef, String> {
    let obj = object(value, at)?;
    only_fields(
        obj,
        &[
            "role",
            "function",
            "mission",
            "deliverable_kinds",
            "delegation_targets",
            "required_capabilities",
            "authority_ceiling",
            "context_selectors",
            "review_policy",
            "delivery_policy",
            "default_topology",
            "approval_floor",
            "routing_hint",
        ],
        at,
    )?;
    let role = role_ref(nested(obj, "role", at)?, &format!("{at}.role"))?;
    let function_at = format!("{at}.function");
    let function = object(nested(obj, "function", at)?, &function_at)?;
    only_fields(function, &["id"], &function_at)?;
    let function_id = required_str(function, "id", &function_at)?;
    required_str(obj, "mission", at)?;
    let deliverables = str_tuple(obj, "deliverable_kinds", true, at)?;
    let delegation = match present(obj, "delegation_targets") {
        None => Vec::new(),
        Some(Value::Array(items)) => items
            .iter()
            .enumerate()
            .map(|(i, item)| role_ref(item, &format!("{at}.delegation_targets.{i}")))
            .collect::<Result<Vec<_>, _>>()?,
        Some(_) => {
            return Err(format!(
                "{at}.delegation_targets: input should be a valid tuple"
            ))
        }
    };
    let capabilities = str_tuple(obj, "required_capabilities", false, at)?;
    choice(obj, "authority_ceiling", &AUTHORITY, None, at)?;
    match present(obj, "context_selectors") {
        None => {}
        Some(Value::Array(items)) => {
            for (i, item) in items.iter().enumerate() {
                check_context_selector(item, &format!("{at}.context_selectors.{i}"))?;
            }
        }
        Some(_) => {
            return Err(format!(
                "{at}.context_selectors: input should be a valid tuple"
            ))
        }
    }
    check_review_policy(
        nested(obj, "review_policy", at)?,
        &format!("{at}.review_policy"),
    )?;
    check_delivery_policy(
        nested(obj, "delivery_policy", at)?,
        &format!("{at}.delivery_policy"),
    )?;
    required_str(obj, "default_topology", at)?;
    choice(obj, "approval_floor", &APPROVAL, Some("none"), at)?;
    match present(obj, "routing_hint") {
        None | Some(Value::Null) => {}
        Some(hint) => check_routing_hint(hint, &format!("{at}.routing_hint"))?,
    }
    if role.function_id != function_id {
        return Err(format!("{at}: role function_id must match function id"));
    }
    unique(&deliverables, at, "deliverable_kinds")?;
    unique(&capabilities, at, "required_capabilities")?;
    let pairs: Vec<String> = delegation
        .iter()
        .map(|r| format!("{}\u{0}{}", r.id, r.function_id))
        .collect();
    unique(&pairs, at, "delegation_targets")?;
    Ok(role)
}

/// Validate one document as a `RoleDefinitionSource`.
pub fn definition_source(value: &Value) -> Result<DefinitionSource, String> {
    let at = "RoleDefinitionSource";
    let obj = object(value, at)?;
    only_fields(
        obj,
        &[
            "layer",
            "source_id",
            "snapshot_revision",
            "role",
            "manifest",
            "status",
            "error",
        ],
        at,
    )?;
    let layer = choice(obj, "layer", &LAYERS, None, at)?;
    let source_id = required_str(obj, "source_id", at)?;
    required_str(obj, "snapshot_revision", at)?;
    let role = role_ref(nested(obj, "role", at)?, &format!("{at}.role"))?;
    let manifest_role = match present(obj, "manifest") {
        None | Some(Value::Null) => None,
        Some(manifest) => Some(check_manifest(manifest, &format!("{at}.manifest"))?),
    };
    let status = choice(obj, "status", &STATUSES, Some("valid"), at)?;
    let error = optional_str(obj, "error", at)?;
    if status == "valid" {
        match &manifest_role {
            None => return Err(format!("{at}: valid definition requires a manifest")),
            Some(_) if error.is_some() => {
                return Err(format!("{at}: valid definition cannot carry an error"))
            }
            Some(found) if *found != role => {
                return Err(format!("{at}: definition role must match manifest role"))
            }
            Some(_) => {}
        }
    } else {
        if manifest_role.is_some() {
            return Err(format!(
                "{at}: invalid or unreadable definition cannot carry a manifest"
            ));
        }
        if error.is_none() {
            return Err(format!(
                "{at}: invalid or unreadable definition requires an error"
            ));
        }
    }
    Ok(DefinitionSource {
        layer: RoleLayer::parse(&layer).expect("choice() admits only layer names"),
        source_id,
        role,
        status: STATUSES
            .into_iter()
            .find(|s| *s == status)
            .expect("choice() admits only status names"),
        error,
    })
}
