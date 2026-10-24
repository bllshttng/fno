//! The pack manifest: frozen, non-granting declarations, checked over raw
//! YAML-loaded JSON the way the pydantic models checked it. Every model
//! refuses unknown fields and every `NonEmptyStr` trims.
//!
//! Two declaration families are declarations, never facts. `permissions` is
//! the pack's maximum expected effect ceiling, read for review; activation
//! grants none of it. Adapter `conformance` mirrors `AdapterCapability`
//! field for field; the registry records which pack digest declared it, so a
//! false declaration stays attributable. A packaged role is a full role
//! manifest, checked by the role schema itself, so the digest the resolver
//! computes over it is the same one discovery computes.

use super::version::Version;
use crate::roles::models::{
    self, check_manifest, lax_bool, object, only_fields, optional_str, required_str,
    role_ref, Manifest, Obj, RoleRef, CONTEXT_KINDS,
};
use serde_json::{json, Value};

pub const EVIDENCE_RESULTS: [&str; 4] = ["passed", "failed", "blocked", "unknown"];

#[derive(Clone, Debug)]
pub struct Range {
    pub minimum: String,
    pub maximum: Option<String>,
}

#[derive(Clone, Debug)]
pub struct Dependency {
    pub pack_id: String,
    pub range: Range,
}

/// A skill or workflow: an id and a pack-relative source.
#[derive(Clone, Debug)]
pub struct Sourced {
    pub id: String,
    pub source: String,
}

/// A subagent bundled at build time. Its frontmatter is copied verbatim, so
/// verify binds the copied `tools` list to the declared role.
#[derive(Clone, Debug)]
pub struct Agent {
    pub id: String,
    pub source: String,
    pub role: Option<RoleRef>,
}

#[derive(Clone, Debug)]
pub struct Conformance {
    pub adapter_id: String,
    pub adapter_version: String,
    pub remote_idempotency: bool,
    pub reconciliation: bool,
}

#[derive(Clone, Debug)]
pub struct Adapter {
    pub id: String,
    pub destination: String,
    pub conformance: Conformance,
}

#[derive(Clone, Debug)]
pub struct Evaluator {
    pub id: String,
    pub command: String,
    pub required: bool,
}

#[derive(Clone, Debug)]
pub struct Asset {
    pub id: String,
    pub source: String,
    pub kind: String,
}

#[derive(Clone, Debug)]
pub struct Effect {
    pub effect_class: String,
    pub destination: String,
}

impl Effect {
    pub fn to_json(&self) -> Value {
        json!({"effect_class": self.effect_class, "destination": self.destination})
    }
}

#[derive(Clone, Debug)]
pub struct Scenario {
    pub id: String,
    pub command: String,
    pub recorded_result: String,
}

#[derive(Clone, Debug)]
pub struct PackManifest {
    pub id: String,
    pub version: String,
    pub footnote_compat: Range,
    pub roles: Vec<Manifest>,
    pub skills: Vec<Sourced>,
    pub agents: Vec<Agent>,
    pub workflows: Vec<Sourced>,
    pub adapters: Vec<Adapter>,
    pub evaluators: Vec<Evaluator>,
    pub assets: Vec<Asset>,
    pub permissions: Vec<Effect>,
    pub scenarios: Vec<Scenario>,
    pub depends_on: Vec<Dependency>,
}

fn pep440(value: &str, label: &str, at: &str) -> Result<(), String> {
    Version::parse(value).map(|_| ()).map_err(|e| {
        format!(
            "{at}: {label} {} is not a valid (PEP 440) version: {e}",
            py(value)
        )
    })
}

fn py(text: &str) -> String {
    crate::client_commands::py_repr_str(text)
}

fn range(value: &Value, at: &str) -> Result<Range, String> {
    let obj = object(value, at)?;
    only_fields(obj, &["minimum", "maximum"], at)?;
    let minimum = required_str(obj, "minimum", at)?;
    let maximum = optional_str(obj, "maximum", at)?;
    pep440(&minimum, "minimum", at)?;
    if let Some(maximum) = &maximum {
        pep440(maximum, "maximum", at)?;
        if Version::parse(maximum)? < Version::parse(&minimum)? {
            return Err(format!(
                "{at}: maximum {} is lower than minimum {}",
                py(maximum),
                py(&minimum)
            ));
        }
    }
    Ok(Range { minimum, maximum })
}

fn nested<'a>(obj: &'a Obj, key: &str, at: &str) -> Result<&'a Value, String> {
    obj.get(key)
        .ok_or_else(|| format!("{at}.{key}: field required"))
}

/// A `tuple[X, ...] = ()` field: absent is empty, a list checks each item.
fn items<T>(
    obj: &Obj,
    key: &str,
    at: &str,
    check: impl Fn(&Value, &str) -> Result<T, String>,
) -> Result<Vec<T>, String> {
    match obj.get(key) {
        None => Ok(Vec::new()),
        Some(Value::Array(list)) => list
            .iter()
            .enumerate()
            .map(|(i, item)| check(item, &format!("{at}.{key}.{i}")))
            .collect(),
        Some(_) => Err(format!("{at}.{key}: input should be a valid tuple")),
    }
}

fn sourced(value: &Value, at: &str) -> Result<Sourced, String> {
    let obj = object(value, at)?;
    only_fields(obj, &["id", "source"], at)?;
    Ok(Sourced {
        id: required_str(obj, "id", at)?,
        source: required_str(obj, "source", at)?,
    })
}

fn agent(value: &Value, at: &str) -> Result<Agent, String> {
    let obj = object(value, at)?;
    only_fields(obj, &["id", "source", "role"], at)?;
    let role = match obj.get("role") {
        None | Some(Value::Null) => None,
        Some(role) => Some(role_ref(role, &format!("{at}.role"))?),
    };
    Ok(Agent {
        id: required_str(obj, "id", at)?,
        source: required_str(obj, "source", at)?,
        role,
    })
}

fn adapter(value: &Value, at: &str) -> Result<Adapter, String> {
    let obj = object(value, at)?;
    only_fields(obj, &["id", "destination", "conformance"], at)?;
    let conformance_at = format!("{at}.conformance");
    let c = object(nested(obj, "conformance", at)?, &conformance_at)?;
    only_fields(
        c,
        &[
            "adapter_id",
            "adapter_version",
            "remote_idempotency",
            "reconciliation",
        ],
        &conformance_at,
    )?;
    Ok(Adapter {
        id: required_str(obj, "id", at)?,
        destination: required_str(obj, "destination", at)?,
        conformance: Conformance {
            adapter_id: required_str(c, "adapter_id", &conformance_at)?,
            adapter_version: required_str(c, "adapter_version", &conformance_at)?,
            remote_idempotency: lax_bool(c, "remote_idempotency", false, &conformance_at)?,
            reconciliation: lax_bool(c, "reconciliation", false, &conformance_at)?,
        },
    })
}

fn evaluator(value: &Value, at: &str) -> Result<Evaluator, String> {
    let obj = object(value, at)?;
    only_fields(obj, &["id", "command", "required"], at)?;
    Ok(Evaluator {
        id: required_str(obj, "id", at)?,
        command: required_str(obj, "command", at)?,
        required: lax_bool(obj, "required", true, at)?,
    })
}

fn asset(value: &Value, at: &str) -> Result<Asset, String> {
    let obj = object(value, at)?;
    only_fields(obj, &["id", "source", "kind"], at)?;
    Ok(Asset {
        id: required_str(obj, "id", at)?,
        source: required_str(obj, "source", at)?,
        kind: models::choice(obj, "kind", &CONTEXT_KINDS, Some("artifact"), at)?,
    })
}

pub(crate) fn effect(value: &Value, at: &str) -> Result<Effect, String> {
    let obj = object(value, at)?;
    only_fields(obj, &["effect_class", "destination"], at)?;
    Ok(Effect {
        effect_class: required_str(obj, "effect_class", at)?,
        destination: required_str(obj, "destination", at)?,
    })
}

fn scenario(value: &Value, at: &str) -> Result<Scenario, String> {
    let obj = object(value, at)?;
    only_fields(obj, &["id", "command", "recorded_result"], at)?;
    Ok(Scenario {
        id: required_str(obj, "id", at)?,
        command: required_str(obj, "command", at)?,
        recorded_result: models::choice(obj, "recorded_result", &EVIDENCE_RESULTS, None, at)?,
    })
}

fn dependency(value: &Value, at: &str) -> Result<Dependency, String> {
    let obj = object(value, at)?;
    only_fields(obj, &["pack_id", "version_range"], at)?;
    Ok(Dependency {
        pack_id: required_str(obj, "pack_id", at)?,
        range: range(
            nested(obj, "version_range", at)?,
            &format!("{at}.version_range"),
        )?,
    })
}

fn unique(field: &str, keys: &[String]) -> Result<(), String> {
    let mut seen = std::collections::HashSet::new();
    for key in keys {
        if !seen.insert(key) {
            return Err(format!("duplicate {field} declaration {}", py(key)));
        }
    }
    Ok(())
}

impl PackManifest {
    /// `PackManifest.model_validate(raw)`.
    pub fn parse(value: &Value) -> Result<PackManifest, String> {
        let at = "PackManifest";
        let obj = object(value, at)?;
        only_fields(
            obj,
            &[
                "id",
                "version",
                "footnote_compat",
                "roles",
                "skills",
                "agents",
                "workflows",
                "adapters",
                "evaluators",
                "assets",
                "permissions",
                "scenarios",
                "depends_on",
            ],
            at,
        )?;
        let id = required_str(obj, "id", at)?;
        let version = required_str(obj, "version", at)?;
        let footnote_compat = range(
            nested(obj, "footnote_compat", at)?,
            &format!("{at}.footnote_compat"),
        )?;
        let manifest = PackManifest {
            id,
            version,
            footnote_compat,
            roles: items(obj, "roles", at, check_manifest)?,
            skills: items(obj, "skills", at, sourced)?,
            agents: items(obj, "agents", at, agent)?,
            workflows: items(obj, "workflows", at, sourced)?,
            adapters: items(obj, "adapters", at, adapter)?,
            evaluators: items(obj, "evaluators", at, evaluator)?,
            assets: items(obj, "assets", at, asset)?,
            permissions: items(obj, "permissions", at, effect)?,
            scenarios: items(obj, "scenarios", at, scenario)?,
            depends_on: items(obj, "depends_on", at, dependency)?,
        };
        let ids = |list: Vec<&String>| list.into_iter().cloned().collect::<Vec<_>>();
        unique(
            "roles",
            &ids(manifest.roles.iter().map(|r| &r.role.id).collect()),
        )?;
        let pairs: Vec<String> = manifest
            .permissions
            .iter()
            .map(|p| format!("{}\u{0}{}", p.effect_class, p.destination))
            .collect();
        unique("permissions", &pairs)?;
        unique(
            "skills",
            &ids(manifest.skills.iter().map(|s| &s.id).collect()),
        )?;
        unique(
            "agents",
            &ids(manifest.agents.iter().map(|s| &s.id).collect()),
        )?;
        unique(
            "workflows",
            &ids(manifest.workflows.iter().map(|s| &s.id).collect()),
        )?;
        unique(
            "adapters",
            &ids(manifest.adapters.iter().map(|s| &s.id).collect()),
        )?;
        unique(
            "evaluators",
            &ids(manifest.evaluators.iter().map(|s| &s.id).collect()),
        )?;
        unique(
            "assets",
            &ids(manifest.assets.iter().map(|s| &s.id).collect()),
        )?;
        unique(
            "scenarios",
            &ids(manifest.scenarios.iter().map(|s| &s.id).collect()),
        )?;
        // Agents and dependencies do not count: a pack of only those is empty.
        let empty = manifest.roles.is_empty()
            && manifest.skills.is_empty()
            && manifest.workflows.is_empty()
            && manifest.adapters.is_empty()
            && manifest.evaluators.is_empty()
            && manifest.assets.is_empty()
            && manifest.permissions.is_empty()
            && manifest.scenarios.is_empty();
        if empty {
            return Err("a pack must declare at least one component".into());
        }
        pep440(&manifest.version, "version", at)?;
        Ok(manifest)
    }

    /// `model_dump(mode="json")`, the input of the pack digest.
    pub fn to_json(&self) -> Value {
        let range = |r: &Range| json!({"minimum": r.minimum, "maximum": r.maximum});
        let sourced = |s: &Sourced| json!({"id": s.id, "source": s.source});
        json!({
            "id": self.id,
            "version": self.version,
            "footnote_compat": range(&self.footnote_compat),
            "roles": self.roles.iter().map(Manifest::to_json).collect::<Vec<_>>(),
            "skills": self.skills.iter().map(sourced).collect::<Vec<_>>(),
            "agents": self.agents.iter().map(|a| json!({
                "id": a.id,
                "source": a.source,
                "role": a.role.as_ref().map_or(Value::Null, RoleRef::to_json),
            })).collect::<Vec<_>>(),
            "workflows": self.workflows.iter().map(sourced).collect::<Vec<_>>(),
            "adapters": self.adapters.iter().map(|a| json!({
                "id": a.id,
                "destination": a.destination,
                "conformance": conformance_json(&a.conformance),
            })).collect::<Vec<_>>(),
            "evaluators": self.evaluators.iter().map(|e| json!({
                "id": e.id,
                "command": e.command,
                "required": e.required,
            })).collect::<Vec<_>>(),
            "assets": self.assets.iter().map(|a| json!({
                "id": a.id,
                "source": a.source,
                "kind": a.kind,
            })).collect::<Vec<_>>(),
            "permissions": self.permissions.iter().map(Effect::to_json).collect::<Vec<_>>(),
            "scenarios": self.scenarios.iter().map(|s| json!({
                "id": s.id,
                "command": s.command,
                "recorded_result": s.recorded_result,
            })).collect::<Vec<_>>(),
            "depends_on": self.depends_on.iter().map(|d| json!({
                "pack_id": d.pack_id,
                "version_range": range(&d.range),
            })).collect::<Vec<_>>(),
        })
    }

    /// Stable sha256 over the canonical pack-manifest serialization.
    pub fn digest(&self) -> String {
        models::canonical_digest("pack-manifest", &self.to_json())
    }
}

pub fn conformance_json(c: &Conformance) -> Value {
    json!({
        "adapter_id": c.adapter_id,
        "adapter_version": c.adapter_version,
        "remote_idempotency": c.remote_idempotency,
        "reconciliation": c.reconciliation,
    })
}
