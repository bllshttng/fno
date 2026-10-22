//! The business-role lookup model routing reads, ported from the Python
//! `fno.roles.resolver.resolve_manifest_routing` and the role-root
//! tri-state of `fno.agents.model_routing._default_business_lookup`.
//!
//! [`business_role`] answers one role name with a routing projection, or a
//! refusal. `not_found` means no business role by that name: the caller
//! keeps its legacy routing. Any other refusal fails closed, so no worker
//! launches on a role whose manifests do not validate. It checks the
//! overlay chain as resolution does, but reads no capability or context:
//! routing needs only the provider and model the chain settles on.
//!
//! The `spawn-overlay` kind `business-role` serves the Python routing
//! lanes; a Rust spawn arm can call [`business_role`] in process.

use super::models::{self, canonical_digest, DefinitionSource, RoleLayer, RoleRef, RoutingHint};
use super::registry::{self, Discovered};
use super::resolver::{overlay_violation, Blocked};
use crate::client_verbs::py_repr_str;
use serde_json::{json, Value};
use std::path::Path;

/// `ManifestRoutingResolution`: the validated chain's routing projection.
pub struct Routing {
    pub role: RoleRef,
    pub source_digest: String,
    pub routing_projection: Option<RoutingHint>,
}

impl Routing {
    pub fn to_json(&self) -> Value {
        json!({
            "role": self.role.to_json(),
            "source_digest": self.source_digest,
            "routing_projection": self
                .routing_projection
                .as_ref()
                .map_or(Value::Null, RoutingHint::to_json),
        })
    }
}

fn refuse(
    role: &RoleRef,
    reason: &'static str,
    source: Option<(RoleLayer, &str)>,
    reference: &str,
    detail: Option<String>,
) -> Blocked {
    Blocked {
        role: role.clone(),
        reason,
        source_layer: source.map(|(layer, _)| layer),
        source_id: source.map(|(_, id)| id.to_string()),
        reference: Some(reference.to_string()),
        detail,
    }
}

/// Look up `role_id` under the role root (`FNO_ROLES_ROOT`, else the
/// project's `.fno/roles`). A missing default root is `not_found`; a
/// configured root that is missing, unreadable or not a directory blocks.
pub(crate) fn business_role(role_id: &str) -> Result<Routing, Blocked> {
    let configured = std::env::var_os("FNO_ROLES_ROOT").is_some_and(|v| !v.is_empty());
    let root = registry::default_role_root();
    let unavailable = RoleRef {
        id: role_id.to_string(),
        function_id: "unavailable".into(),
    };
    let root_text = root.to_string_lossy().into_owned();
    let root_block = |detail: String| Blocked {
        source_id: Some(root_text.clone()),
        ..refuse(
            &unavailable,
            "invalid_manifest",
            None,
            role_id,
            Some(detail),
        )
    };
    match std::fs::metadata(&root) {
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            return Err(if configured {
                root_block("configured role root does not exist".into())
            } else {
                refuse(&unavailable, "not_found", None, role_id, None)
            });
        }
        Err(err) => return Err(root_block(registry::os_error_text(&err, &root))),
        Ok(meta) if !meta.is_dir() => {
            return Err(root_block("role root exists but is not a directory".into()))
        }
        Ok(_) => {}
    }
    let records = registry::discover(Some(Path::new(&root)), &[])
        .map_err(|registry::DiscoveryInputError(message)| root_block(message))?;
    manifest_routing(role_id, &records)
}

/// `resolve_manifest_routing`: validate the discovered chain for `role_id`
/// and return only its routing projection.
pub(crate) fn manifest_routing(role_id: &str, records: &[Discovered]) -> Result<Routing, Blocked> {
    let candidates: Vec<&Discovered> = records
        .iter()
        .filter(|r| r.role.as_ref().is_some_and(|role| role.id == role_id))
        .collect();
    let mut functions: Vec<&str> = candidates
        .iter()
        .filter_map(|r| r.role.as_ref())
        .map(|role| role.function_id.as_str())
        .collect();
    functions.sort_unstable();
    functions.dedup();
    let role = RoleRef {
        id: role_id.to_string(),
        function_id: match functions.as_slice() {
            [only] => (*only).to_string(),
            _ => "unavailable".into(),
        },
    };
    let error_or = |record: &Discovered, fallback: &str| {
        Some(
            record
                .error
                .clone()
                .filter(|e| !e.is_empty())
                .unwrap_or_else(|| fallback.to_string()),
        )
    };
    let at = |record: &Discovered| Some((record.layer, record.source_id.as_str()));
    if let Some(unchecked) = records
        .iter()
        .find(|r| r.role.is_none() && r.status != "valid")
    {
        return Err(refuse(
            &role,
            "invalid_manifest",
            at(unchecked),
            role_id,
            error_or(unchecked, "source identity could not be validated"),
        ));
    }
    if functions.len() > 1 {
        return Err(refuse(
            &role,
            "invalid_manifest",
            at(candidates[0]),
            role_id,
            Some(format!(
                "role {} exists in multiple functions: {}",
                py_repr_str(role_id),
                functions.join(", ")
            )),
        ));
    }
    if candidates.is_empty() {
        return Err(refuse(&role, "not_found", None, role_id, None));
    }
    let mut definitions: Vec<DefinitionSource> = Vec::new();
    for &record in &candidates {
        let parsed = record
            .definition
            .as_ref()
            .ok_or_else(|| error_or(record, "source could not be validated"))
            .and_then(|raw| models::definition_source(raw).map_err(Some));
        match parsed {
            Ok(definition) => definitions.push(definition),
            Err(detail) => {
                return Err(refuse(
                    &role,
                    "invalid_manifest",
                    at(record),
                    role_id,
                    detail,
                ))
            }
        }
    }
    definitions.sort_by(|a, b| (a.layer, &a.source_id).cmp(&(b.layer, &b.source_id)));
    for layer in RoleLayer::ALL {
        let same: Vec<&str> = definitions
            .iter()
            .filter(|d| d.layer == layer)
            .map(|d| d.source_id.as_str())
            .collect();
        if same.len() > 1 {
            return Err(refuse(
                &role,
                "invalid_overlay",
                at(candidates[0]),
                role_id,
                Some(format!(
                    "ambiguous {} definitions for {}/{}: {}",
                    layer.as_str(),
                    role.function_id,
                    role.id,
                    same.join(", ")
                )),
            ));
        }
    }
    let revision = definitions[0].snapshot_revision.clone();
    let mut manifest: Option<&models::Manifest> = None;
    for source in &definitions {
        let here = Some((source.layer, source.source_id.as_str()));
        if source.snapshot_revision != revision {
            return Err(refuse(
                &role,
                "mixed_revision",
                here,
                &source.snapshot_revision,
                None,
            ));
        }
        let next = match (&source.manifest, source.status) {
            (Some(next), "valid") => next,
            _ => {
                return Err(refuse(
                    &role,
                    "invalid_manifest",
                    here,
                    role_id,
                    source.error.clone(),
                ))
            }
        };
        if let Some(previous) = manifest {
            if let Some((reason, field)) = overlay_violation(previous, next) {
                return Err(refuse(&role, reason, here, &field, None));
            }
        }
        manifest = Some(next);
    }
    let chain: Vec<Value> = definitions.iter().map(DefinitionSource::to_json).collect();
    Ok(Routing {
        source_digest: canonical_digest("role-routing-sources", &Value::Array(chain)),
        routing_projection: manifest.and_then(|m| m.routing_hint.clone()),
        role,
    })
}

/// `spawn-overlay` kind `business-role`: `{"role": NAME}` in, the dumped
/// `ManifestRoutingResolution` or `RoleResolutionBlocked` out.
pub fn business_role_kind(payload: &Value) -> Result<Value, String> {
    let role = payload
        .get("role")
        .and_then(Value::as_str)
        .ok_or("role: input should be a valid string")?;
    Ok(match business_role(role) {
        Ok(routing) => routing.to_json(),
        Err(blocked) => blocked.to_json(),
    })
}
