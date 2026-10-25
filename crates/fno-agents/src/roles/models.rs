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

use serde_json::{json, Map, Value};

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
pub(crate) const CONTEXT_KINDS: [&str; 6] = [
    "plan",
    "brief",
    "graph-node",
    "artifact",
    "source",
    "config",
];
pub(crate) const SENSITIVITY: [&str; 4] = ["public", "internal", "sensitive", "restricted"];
const LAYERS: [&str; 5] = ["built-in", "plugin", "company", "project", "plan"];

/// A `RoleRef`: `(id, function_id)`, both trimmed.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct RoleRef {
    pub id: String,
    pub function_id: String,
}

impl RoleRef {
    pub fn to_json(&self) -> Value {
        json!({"id": self.id, "function_id": self.function_id})
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct RoutingHint {
    pub provider: Option<String>,
    pub model: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ReviewPolicy {
    pub required: bool,
    pub minimum_reviewers: u64,
    pub required_review_kinds: Vec<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct DeliveryPolicy {
    pub required_evidence: Vec<String>,
    pub require_all_deliverables: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ContextSelector {
    pub kind: String,
    pub identifier: Option<String>,
    pub max_sensitivity: String,
    pub requires_freshness: bool,
}

impl ContextSelector {
    /// `kind:identifier`, or `kind:*` for any identifier.
    pub fn reference(&self) -> String {
        format!(
            "{}:{}",
            self.kind,
            self.identifier.as_deref().unwrap_or("*")
        )
    }
}

/// A checked `RoleManifest`, every default filled and every lax input
/// coerced, as pydantic held it.
#[derive(Clone, Debug, PartialEq)]
pub struct Manifest {
    pub role: RoleRef,
    pub function_id: String,
    pub mission: String,
    pub deliverable_kinds: Vec<String>,
    pub delegation_targets: Vec<RoleRef>,
    pub required_capabilities: Vec<String>,
    pub authority_ceiling: String,
    pub context_selectors: Vec<ContextSelector>,
    pub review_policy: ReviewPolicy,
    pub delivery_policy: DeliveryPolicy,
    pub default_topology: String,
    pub approval_floor: String,
    pub routing_hint: Option<RoutingHint>,
}

impl RoutingHint {
    pub fn to_json(&self) -> Value {
        json!({"provider": self.provider, "model": self.model})
    }
}

impl ReviewPolicy {
    pub fn to_json(&self) -> Value {
        json!({
            "required": self.required,
            "minimum_reviewers": self.minimum_reviewers,
            "required_review_kinds": self.required_review_kinds,
        })
    }
}

impl DeliveryPolicy {
    pub fn to_json(&self) -> Value {
        json!({
            "required_evidence": self.required_evidence,
            "require_all_deliverables": self.require_all_deliverables,
        })
    }
}

impl Manifest {
    /// `model_dump(mode="json")`.
    pub fn to_json(&self) -> Value {
        let selectors: Vec<Value> = self
            .context_selectors
            .iter()
            .map(|s| {
                json!({
                    "kind": s.kind,
                    "identifier": s.identifier,
                    "max_sensitivity": s.max_sensitivity,
                    "requires_freshness": s.requires_freshness,
                })
            })
            .collect();
        json!({
            "role": self.role.to_json(),
            "function": {"id": self.function_id},
            "mission": self.mission,
            "deliverable_kinds": self.deliverable_kinds,
            "delegation_targets": self.delegation_targets.iter().map(RoleRef::to_json).collect::<Vec<_>>(),
            "required_capabilities": self.required_capabilities,
            "authority_ceiling": self.authority_ceiling,
            "context_selectors": selectors,
            "review_policy": self.review_policy.to_json(),
            "delivery_policy": self.delivery_policy.to_json(),
            "default_topology": self.default_topology,
            "approval_floor": self.approval_floor,
            "routing_hint": self.routing_hint.as_ref().map_or(Value::Null, RoutingHint::to_json),
        })
    }
}

/// A schema-valid `RoleDefinitionSource`.
#[derive(Clone, Debug)]
pub struct DefinitionSource {
    pub layer: RoleLayer,
    pub source_id: String,
    pub snapshot_revision: String,
    pub role: RoleRef,
    pub manifest: Option<Manifest>,
    pub status: &'static str,
    pub error: Option<String>,
}

impl DefinitionSource {
    pub fn to_json(&self) -> Value {
        json!({
            "layer": self.layer.as_str(),
            "source_id": self.source_id,
            "snapshot_revision": self.snapshot_revision,
            "role": self.role.to_json(),
            "manifest": self.manifest.as_ref().map_or(Value::Null, Manifest::to_json),
            "status": self.status,
            "error": self.error,
        })
    }
}

pub(crate) type Obj = Map<String, Value>;

pub(crate) fn object<'a>(value: &'a Value, at: &str) -> Result<&'a Obj, String> {
    value
        .as_object()
        .ok_or_else(|| format!("{at}: input should be an object"))
}

pub(crate) fn only_fields(obj: &Obj, allowed: &[&str], at: &str) -> Result<(), String> {
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

pub(crate) fn non_empty(value: &Value, at: &str) -> Result<String, String> {
    let text = value
        .as_str()
        .ok_or_else(|| format!("{at}: input should be a valid string"))?
        .trim();
    if text.is_empty() {
        return Err(format!("{at}: string should have at least 1 character"));
    }
    Ok(text.to_string())
}

pub(crate) fn required_str(obj: &Obj, key: &str, at: &str) -> Result<String, String> {
    match present(obj, key) {
        Some(value) => non_empty(value, &format!("{at}.{key}")),
        None => Err(format!("{at}.{key}: field required")),
    }
}

pub(crate) fn optional_str(obj: &Obj, key: &str, at: &str) -> Result<Option<String>, String> {
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

pub(crate) fn choice(
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
pub(crate) fn lax_bool(obj: &Obj, key: &str, default: bool, at: &str) -> Result<bool, String> {
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

/// Pydantic's int-from-string grammar, as measured: trimmed, an optional
/// `+`, digits with single `_` separators, and an optional `.0...` tail.
/// It refuses `1e3`, `1.`, `.0`, `-0`, `inf` and `0x1`.
fn int_text(text: &str) -> Option<f64> {
    let text = text.trim();
    let text = text.strip_prefix('+').unwrap_or(text);
    let (whole, fraction) = match text.split_once('.') {
        Some((whole, fraction)) => (whole, Some(fraction)),
        None => (text, None),
    };
    if fraction.is_some_and(|f| f.is_empty() || f.bytes().any(|b| b != b'0')) {
        return None;
    }
    let digits_ok = whole.split('_').all(|run| !run.is_empty())
        && whole.bytes().all(|b| b.is_ascii_digit() || b == b'_');
    if !digits_ok {
        return None;
    }
    whole.replace('_', "").parse::<f64>().ok()
}

/// Pydantic's lax int with `ge=0`: an int, a bool, a float with no
/// fractional part inside the i64 range, or an [`int_text`] string.
fn lax_count(obj: &Obj, key: &str, at: &str) -> Result<f64, String> {
    let at = format!("{at}.{key}");
    let bad = || format!("{at}: input should be a valid integer");
    let number = match present(obj, key) {
        None => return Ok(0.0),
        Some(Value::Bool(b)) => f64::from(u8::from(*b)),
        Some(Value::Number(n)) if n.is_i64() || n.is_u64() => n.as_f64().ok_or_else(bad)?,
        Some(Value::Number(n)) => match n.as_f64() {
            Some(f) if f.is_finite() && f.fract() == 0.0 && f.abs() < 9.2e18 => f,
            _ => return Err(bad()),
        },
        Some(Value::String(text)) => int_text(text).ok_or_else(bad)?,
        Some(_) => return Err(bad()),
    };
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

fn check_routing_hint(value: &Value, at: &str) -> Result<RoutingHint, String> {
    let obj = object(value, at)?;
    only_fields(obj, &["provider", "model"], at)?;
    let provider = optional_str(obj, "provider", at)?;
    let model = optional_str(obj, "model", at)?;
    if provider.is_none() && model.is_none() {
        return Err(format!("{at}: routing_hint must name a provider or model"));
    }
    Ok(RoutingHint { provider, model })
}

fn check_review_policy(value: &Value, at: &str) -> Result<ReviewPolicy, String> {
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
    Ok(ReviewPolicy {
        required,
        minimum_reviewers: minimum as u64,
        required_review_kinds: kinds,
    })
}

fn check_delivery_policy(value: &Value, at: &str) -> Result<DeliveryPolicy, String> {
    let obj = object(value, at)?;
    only_fields(obj, &["required_evidence", "require_all_deliverables"], at)?;
    Ok(DeliveryPolicy {
        required_evidence: str_tuple(obj, "required_evidence", false, at)?,
        require_all_deliverables: lax_bool(obj, "require_all_deliverables", true, at)?,
    })
}

fn check_context_selector(value: &Value, at: &str) -> Result<ContextSelector, String> {
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
    Ok(ContextSelector {
        kind: choice(obj, "kind", &CONTEXT_KINDS, None, at)?,
        identifier: optional_str(obj, "identifier", at)?,
        max_sensitivity: choice(obj, "max_sensitivity", &SENSITIVITY, Some("internal"), at)?,
        requires_freshness: lax_bool(obj, "requires_freshness", false, at)?,
    })
}

fn unique(items: &[String], at: &str, field: &str) -> Result<(), String> {
    let mut seen = std::collections::HashSet::new();
    if items.iter().all(|item| seen.insert(item)) {
        Ok(())
    } else {
        Err(format!("{at}: {field} must be unique"))
    }
}

/// A `RoleManifest`.
pub(crate) fn check_manifest(value: &Value, at: &str) -> Result<Manifest, String> {
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
    let mission = required_str(obj, "mission", at)?;
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
    let authority_ceiling = choice(obj, "authority_ceiling", &AUTHORITY, None, at)?;
    let context_selectors = match present(obj, "context_selectors") {
        None => Vec::new(),
        Some(Value::Array(items)) => items
            .iter()
            .enumerate()
            .map(|(i, item)| check_context_selector(item, &format!("{at}.context_selectors.{i}")))
            .collect::<Result<Vec<_>, _>>()?,
        Some(_) => {
            return Err(format!(
                "{at}.context_selectors: input should be a valid tuple"
            ))
        }
    };
    let review_policy = check_review_policy(
        nested(obj, "review_policy", at)?,
        &format!("{at}.review_policy"),
    )?;
    let delivery_policy = check_delivery_policy(
        nested(obj, "delivery_policy", at)?,
        &format!("{at}.delivery_policy"),
    )?;
    let default_topology = required_str(obj, "default_topology", at)?;
    let approval_floor = choice(obj, "approval_floor", &APPROVAL, Some("none"), at)?;
    let routing_hint = match present(obj, "routing_hint") {
        None | Some(Value::Null) => None,
        Some(hint) => Some(check_routing_hint(hint, &format!("{at}.routing_hint"))?),
    };
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
    Ok(Manifest {
        role,
        function_id,
        mission,
        deliverable_kinds: deliverables,
        delegation_targets: delegation,
        required_capabilities: capabilities,
        authority_ceiling,
        context_selectors,
        review_policy,
        delivery_policy,
        default_topology,
        approval_floor,
        routing_hint,
    })
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
    let snapshot_revision = required_str(obj, "snapshot_revision", at)?;
    let role = role_ref(nested(obj, "role", at)?, &format!("{at}.role"))?;
    let manifest = match present(obj, "manifest") {
        None | Some(Value::Null) => None,
        Some(manifest) => Some(check_manifest(manifest, &format!("{at}.manifest"))?),
    };
    let status = choice(obj, "status", &STATUSES, Some("valid"), at)?;
    let error = optional_str(obj, "error", at)?;
    if status == "valid" {
        match &manifest {
            None => return Err(format!("{at}: valid definition requires a manifest")),
            Some(_) if error.is_some() => {
                return Err(format!("{at}: valid definition cannot carry an error"))
            }
            Some(found) if found.role != role => {
                return Err(format!("{at}: definition role must match manifest role"))
            }
            Some(_) => {}
        }
    } else {
        if manifest.is_some() {
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
        snapshot_revision,
        role,
        manifest,
        status: STATUSES
            .into_iter()
            .find(|s| *s == status)
            .expect("choice() admits only status names"),
        error,
    })
}

// -------------------------------------------------------------------------
// The resolver's inputs: `WorkOrderRef`, `CapabilityFact`,
// `ContextReference` and `ContextBundleBounds`.
// -------------------------------------------------------------------------

pub type Instant = chrono::DateTime<chrono::FixedOffset>;

/// Pydantic's datetime input: an ISO string with `T` or a space, minutes
/// with optional seconds and fraction, and a `Z` or `+HH:MM` offset; or a
/// number of Unix seconds, read as UTC. A naive time is refused.
pub fn parse_instant(value: &Value, at: &str) -> Result<Instant, String> {
    let utc = chrono::FixedOffset::east_opt(0).expect("zero offset");
    match value {
        Value::Number(n) => {
            let seconds = n
                .as_f64()
                .ok_or_else(|| format!("{at}: input should be a valid datetime"))?;
            let micros = (seconds * 1_000_000.0).round() as i64;
            chrono::DateTime::from_timestamp_micros(micros)
                .map(|t| t.with_timezone(&utc))
                .ok_or_else(|| format!("{at}: input should be a valid datetime"))
        }
        Value::String(text) => {
            let text = text.replacen(' ', "T", 1);
            let (body, offset) = match text.strip_suffix(['Z', 'z']) {
                Some(body) => (body.to_string(), "+00:00".to_string()),
                None => match text.len().checked_sub(6) {
                    // An ASCII sign byte is always a char boundary.
                    Some(cut)
                        if matches!(text.as_bytes()[cut], b'+' | b'-')
                            && text.as_bytes()[cut + 3] == b':' =>
                    {
                        (text[..cut].to_string(), text[cut..].to_string())
                    }
                    _ => return Err(format!("{at}: datetime must be timezone-aware")),
                },
            };
            let body = if body.len() == 16 {
                format!("{body}:00")
            } else {
                body
            };
            chrono::DateTime::parse_from_rfc3339(&format!("{body}{offset}"))
                .map_err(|_| format!("{at}: input should be a valid datetime"))
        }
        _ => Err(format!("{at}: input should be a valid datetime")),
    }
}

/// Pydantic's JSON datetime: microseconds only when nonzero, `Z` for a
/// zero offset.
pub fn format_instant(instant: &Instant) -> String {
    let mut out = instant.format("%Y-%m-%dT%H:%M:%S").to_string();
    let micros = instant.timestamp_subsec_micros();
    if micros != 0 {
        out.push_str(&format!(".{micros:06}"));
    }
    if instant.offset().local_minus_utc() == 0 {
        out.push('Z');
    } else {
        out.push_str(&instant.format("%:z").to_string());
    }
    out
}

fn required_bool(obj: &Obj, key: &str, at: &str) -> Result<bool, String> {
    if present(obj, key).is_none() {
        return Err(format!("{at}.{key}: field required"));
    }
    lax_bool(obj, key, false, at)
}

/// A lax int at or above `floor`, with a default when absent.
fn count_at_least(
    obj: &Obj,
    key: &str,
    floor: u64,
    default: Option<u64>,
    at: &str,
) -> Result<u64, String> {
    if present(obj, key).is_none() {
        return default.ok_or_else(|| format!("{at}.{key}: field required"));
    }
    let number = lax_count(obj, key, at)?;
    if number < floor as f64 {
        return Err(format!(
            "{at}.{key}: input should be greater than or equal to {floor}"
        ));
    }
    Ok(number as u64)
}

#[derive(Clone, Debug, PartialEq)]
pub struct WorkOrderRef {
    pub node_id: String,
    pub attempt_id: String,
    pub principal_id: Option<String>,
    pub role_id: Option<String>,
}

impl WorkOrderRef {
    pub fn to_json(&self) -> Value {
        json!({
            "node_id": self.node_id,
            "attempt_id": self.attempt_id,
            "principal_id": self.principal_id,
            "role_id": self.role_id,
        })
    }

    /// `work_order_scope_matches`: an absent scope matches every order.
    pub fn scope_matches(scope: Option<&WorkOrderRef>, work_order: &WorkOrderRef) -> bool {
        let Some(scope) = scope else {
            return true;
        };
        scope.node_id == work_order.node_id
            && scope.attempt_id == work_order.attempt_id
            && scope
                .role_id
                .as_ref()
                .is_none_or(|r| Some(r) == work_order.role_id.as_ref())
            && scope
                .principal_id
                .as_ref()
                .is_none_or(|p| Some(p) == work_order.principal_id.as_ref())
    }
}

pub fn work_order_ref(value: &Value, at: &str) -> Result<WorkOrderRef, String> {
    let obj = object(value, at)?;
    only_fields(
        obj,
        &["node_id", "attempt_id", "principal_id", "role_id"],
        at,
    )?;
    Ok(WorkOrderRef {
        node_id: required_str(obj, "node_id", at)?,
        attempt_id: required_str(obj, "attempt_id", at)?,
        principal_id: optional_str(obj, "principal_id", at)?,
        role_id: optional_str(obj, "role_id", at)?,
    })
}

fn optional_work_order(obj: &Obj, key: &str, at: &str) -> Result<Option<WorkOrderRef>, String> {
    match present(obj, key) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => work_order_ref(value, &format!("{at}.{key}")).map(Some),
    }
}

#[derive(Clone, Debug)]
pub struct CapabilityFact {
    pub capability: String,
    pub available: bool,
    pub source_id: String,
    pub snapshot_revision: String,
    pub work_order_scope: Option<WorkOrderRef>,
}

pub fn capability_fact(value: &Value, at: &str) -> Result<CapabilityFact, String> {
    let obj = object(value, at)?;
    only_fields(
        obj,
        &[
            "capability",
            "available",
            "source_id",
            "snapshot_revision",
            "work_order_scope",
        ],
        at,
    )?;
    Ok(CapabilityFact {
        capability: required_str(obj, "capability", at)?,
        available: required_bool(obj, "available", at)?,
        source_id: required_str(obj, "source_id", at)?,
        snapshot_revision: required_str(obj, "snapshot_revision", at)?,
        work_order_scope: optional_work_order(obj, "work_order_scope", at)?,
    })
}

#[derive(Clone, Debug)]
pub struct ContextReference {
    pub kind: String,
    pub identifier: String,
    pub provenance: String,
    pub work_order_scope: Option<WorkOrderRef>,
    pub content_digest: Option<String>,
    pub content_revision: Option<String>,
    pub snapshot_revision: String,
    pub fresh_until: Option<Instant>,
    pub sensitivity: String,
    pub byte_size: u64,
    pub readable: bool,
    pub unavailable_reason: Option<String>,
}

impl ContextReference {
    pub fn to_json(&self) -> Value {
        json!({
            "kind": self.kind,
            "identifier": self.identifier,
            "provenance": self.provenance,
            "work_order_scope": self.work_order_scope.as_ref().map_or(Value::Null, WorkOrderRef::to_json),
            "content_digest": self.content_digest,
            "content_revision": self.content_revision,
            "snapshot_revision": self.snapshot_revision,
            "fresh_until": self.fresh_until.as_ref().map(format_instant),
            "sensitivity": self.sensitivity,
            "byte_size": self.byte_size,
            "readable": self.readable,
            "unavailable_reason": self.unavailable_reason,
        })
    }
}

pub fn context_reference(value: &Value, at: &str) -> Result<ContextReference, String> {
    let obj = object(value, at)?;
    only_fields(
        obj,
        &[
            "kind",
            "identifier",
            "provenance",
            "work_order_scope",
            "content_digest",
            "content_revision",
            "snapshot_revision",
            "fresh_until",
            "sensitivity",
            "byte_size",
            "readable",
            "unavailable_reason",
        ],
        at,
    )?;
    let content_digest = match present(obj, "content_digest") {
        None | Some(Value::Null) => None,
        Some(Value::String(text))
            if text.len() == 64
                && text
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)) =>
        {
            Some(text.clone())
        }
        Some(_) => {
            return Err(format!(
                "{at}.content_digest: string should match pattern '^[0-9a-f]{{64}}$'"
            ))
        }
    };
    let fresh_until = match present(obj, "fresh_until") {
        None | Some(Value::Null) => None,
        Some(value) => Some(parse_instant(value, &format!("{at}.fresh_until"))?),
    };
    let reference = ContextReference {
        kind: choice(obj, "kind", &CONTEXT_KINDS, None, at)?,
        identifier: required_str(obj, "identifier", at)?,
        provenance: required_str(obj, "provenance", at)?,
        work_order_scope: optional_work_order(obj, "work_order_scope", at)?,
        content_digest,
        content_revision: optional_str(obj, "content_revision", at)?,
        snapshot_revision: required_str(obj, "snapshot_revision", at)?,
        fresh_until,
        sensitivity: choice(obj, "sensitivity", &SENSITIVITY, None, at)?,
        byte_size: count_at_least(obj, "byte_size", 0, None, at)?,
        readable: required_bool(obj, "readable", at)?,
        unavailable_reason: optional_str(obj, "unavailable_reason", at)?,
    };
    let claims_content = reference.content_digest.is_some() || reference.content_revision.is_some();
    if reference.readable && reference.unavailable_reason.is_some() {
        return Err(format!(
            "{at}: readable context cannot have an unavailable_reason"
        ));
    }
    if !reference.readable && reference.unavailable_reason.is_none() {
        return Err(format!(
            "{at}: unreadable context requires an unavailable_reason"
        ));
    }
    if reference.readable
        && (reference.content_digest.is_none() || reference.content_revision.is_none())
    {
        return Err(format!(
            "{at}: readable context requires content_digest and content_revision"
        ));
    }
    if !reference.readable && claims_content {
        return Err(format!(
            "{at}: unreadable context cannot carry a content_digest or content_revision"
        ));
    }
    Ok(reference)
}

#[derive(Clone, Debug)]
pub struct ContextBundleBounds {
    pub max_references: u64,
    pub max_bytes: u64,
    pub max_sensitivity: String,
    pub max_combinations: u64,
}

pub fn bundle_bounds(value: &Value, at: &str) -> Result<ContextBundleBounds, String> {
    let obj = object(value, at)?;
    only_fields(
        obj,
        &[
            "max_references",
            "max_bytes",
            "max_sensitivity",
            "max_combinations",
        ],
        at,
    )?;
    Ok(ContextBundleBounds {
        max_references: count_at_least(obj, "max_references", 1, None, at)?,
        max_bytes: count_at_least(obj, "max_bytes", 1, None, at)?,
        max_sensitivity: choice(obj, "max_sensitivity", &SENSITIVITY, Some("sensitive"), at)?,
        max_combinations: count_at_least(obj, "max_combinations", 1, Some(4096), at)?,
    })
}

/// `canonical_digest`: SHA-256 over `fno/<domain>/v1\0` and the value as
/// `json.dumps(sort_keys=True, separators=(",", ":"), ensure_ascii=False)`.
pub fn canonical_digest(domain: &str, value: &Value) -> String {
    use sha2::{Digest, Sha256};
    let sorted = crate::backlog::decisions_cli::sort_keys(value);
    let canonical = serde_json::to_string(&sorted).expect("a Value always serializes");
    let digest = Sha256::digest(format!("fno/{domain}/v1\u{0}{canonical}").as_bytes());
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

fn rank_in(scale: &[&str], word: &str) -> usize {
    scale
        .iter()
        .position(|w| *w == word)
        .expect("choice() admits only scale words")
}

pub fn sensitivity_rank(word: &str) -> usize {
    rank_in(&SENSITIVITY, word)
}

pub fn authority_rank(word: &str) -> usize {
    rank_in(&AUTHORITY, word)
}

pub fn approval_rank(word: &str) -> usize {
    rank_in(&APPROVAL, word)
}
