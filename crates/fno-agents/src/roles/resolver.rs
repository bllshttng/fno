//! Business-role resolution, ported from `fno.roles.resolver.resolve_role`:
//! pure and deterministic over one immutable snapshot. It reads nothing,
//! writes nothing, grants nothing and never falls back. Each refusal names
//! the source, and the field or reference, that blocked it.
//!
//! The layered definitions overlay lowest layer first, and a higher layer
//! may only narrow: the same role, mission and topology, no wider authority,
//! no lower approval floor, a subset of the deliverables, delegations and
//! capabilities, and context selectors each matched to a distinct, at
//! least as strict, lower selector. Then every required capability needs
//! a fact for this work order at this revision, and every context selector
//! needs an eligible reference. The bundle is the cheapest combination
//! inside the bounds, and its digest pins it.
//!
//! The `spawn-overlay` kind `roles-resolve` serves the Python package:
//! `fno.roles.resolver.resolve_role` sends its arguments dumped as JSON and
//! validates the answer back into its models.

use super::models::{
    self, approval_rank, authority_rank, canonical_digest, sensitivity_rank, CapabilityFact,
    ContextBundleBounds, ContextReference, ContextSelector, DefinitionSource, Instant, Manifest,
    RoleLayer, RoleRef, WorkOrderRef,
};
use super::registry::Discovered;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};

/// `RoleResolutionBlocked`.
#[derive(Clone, Debug)]
pub struct Blocked {
    pub role: RoleRef,
    pub reason: &'static str,
    pub source_layer: Option<RoleLayer>,
    pub source_id: Option<String>,
    pub reference: Option<String>,
    pub detail: Option<String>,
}

impl Blocked {
    pub fn to_json(&self) -> Value {
        json!({
            "role": self.role.to_json(),
            "reason": self.reason,
            "source_layer": self.source_layer.map(RoleLayer::as_str),
            "source_id": self.source_id,
            "reference": self.reference,
            "detail": self.detail,
        })
    }
}

/// A resolution: the dumped `ResolvedRole`, or the refusal.
pub enum Resolution {
    Resolved(Value),
    Blocked(Blocked),
}

impl Resolution {
    pub fn to_json(&self) -> Value {
        match self {
            Resolution::Resolved(value) => value.clone(),
            Resolution::Blocked(blocked) => blocked.to_json(),
        }
    }
}

/// A discovery record the resolver checks before any definition: a source
/// that never validated blocks the role it may have named.
#[derive(Clone, Debug)]
pub struct Unchecked {
    pub layer: RoleLayer,
    pub source_id: String,
    pub status: String,
    pub role: Option<RoleRef>,
    pub has_definition: bool,
    pub error: Option<String>,
}

impl From<&Discovered> for Unchecked {
    fn from(record: &Discovered) -> Self {
        Unchecked {
            layer: record.layer,
            source_id: record.source_id.clone(),
            status: record.status.to_string(),
            role: record.role.clone(),
            has_definition: record.definition.is_some(),
            error: record.error.clone(),
        }
    }
}

/// Everything one resolution reads.
pub struct Request {
    pub role: RoleRef,
    pub definitions: Vec<DefinitionSource>,
    pub capability_facts: Vec<CapabilityFact>,
    pub context_catalog: Vec<ContextReference>,
    pub work_order: WorkOrderRef,
    pub clock: Instant,
    pub snapshot_revision: String,
    pub bounds: ContextBundleBounds,
    pub unchecked: Vec<Unchecked>,
}

fn blocked(
    role: &RoleRef,
    reason: &'static str,
    source: Option<&DefinitionSource>,
    reference: Option<String>,
    detail: Option<String>,
) -> Blocked {
    Blocked {
        role: role.clone(),
        reason,
        source_layer: source.map(|s| s.layer),
        source_id: source.map(|s| s.source_id.clone()),
        reference,
        detail,
    }
}

fn subset(items: &[String], of: &[String]) -> bool {
    items.iter().all(|item| of.contains(item))
}

fn selector_field(selector: &ContextSelector, field: Option<&str>) -> String {
    let reference = format!("context_selectors[{}]", selector.reference());
    match field {
        Some(field) => format!("{reference}.{field}"),
        None => reference,
    }
}

/// Every candidate selector must match a distinct prior selector of the
/// same kind that it does not loosen: a bipartite match, so two candidate
/// selectors cannot both narrow one prior selector.
fn context_overlay_violation(previous: &Manifest, candidate: &Manifest) -> Option<String> {
    let prior = &previous.context_selectors;
    let mut options: Vec<Vec<usize>> = Vec::new();
    for selector in &candidate.context_selectors {
        let same_kind: Vec<usize> = (0..prior.len())
            .filter(|&i| prior[i].kind == selector.kind)
            .collect();
        if same_kind.is_empty() {
            return Some(selector_field(selector, None));
        }
        let identifier: Vec<usize> = same_kind
            .into_iter()
            .filter(|&i| {
                prior[i].identifier.is_none() || prior[i].identifier == selector.identifier
            })
            .collect();
        if identifier.is_empty() {
            return Some(selector_field(selector, Some("identifier")));
        }
        let freshness: Vec<usize> = identifier
            .into_iter()
            .filter(|&i| !prior[i].requires_freshness || selector.requires_freshness)
            .collect();
        if freshness.is_empty() {
            return Some(selector_field(selector, Some("requires_freshness")));
        }
        let sensitivity: Vec<usize> = freshness
            .into_iter()
            .filter(|&i| {
                sensitivity_rank(&selector.max_sensitivity)
                    <= sensitivity_rank(&prior[i].max_sensitivity)
            })
            .collect();
        if sensitivity.is_empty() {
            return Some(selector_field(selector, Some("max_sensitivity")));
        }
        options.push(sensitivity);
    }

    fn assign(
        candidate: usize,
        seen: &mut HashSet<usize>,
        options: &[Vec<usize>],
        matched: &mut HashMap<usize, usize>,
    ) -> bool {
        for &prior in &options[candidate] {
            if !seen.insert(prior) {
                continue;
            }
            let occupant = matched.get(&prior).copied();
            if occupant.is_none_or(|o| assign(o, seen, options, matched)) {
                matched.insert(prior, candidate);
                return true;
            }
        }
        false
    }

    let mut matched = HashMap::new();
    for (index, selector) in candidate.context_selectors.iter().enumerate() {
        if !assign(index, &mut HashSet::new(), &options, &mut matched) {
            return Some(selector_field(selector, None));
        }
    }
    None
}

/// The first rule a higher layer breaks, as `(reason, field)`.
pub(crate) fn overlay_violation(
    previous: &Manifest,
    candidate: &Manifest,
) -> Option<(&'static str, String)> {
    let overlay = |field: &str| Some(("invalid_overlay", field.to_string()));
    let delegation = |m: &Manifest| -> Vec<String> {
        m.delegation_targets
            .iter()
            .map(|t| format!("{}\u{0}{}", t.function_id, t.id))
            .collect()
    };
    if candidate.role != previous.role {
        return overlay("role");
    }
    if candidate.function_id != previous.function_id {
        return overlay("function");
    }
    if authority_rank(&candidate.authority_ceiling) > authority_rank(&previous.authority_ceiling) {
        return Some(("authority_expansion", "authority_ceiling".into()));
    }
    if approval_rank(&candidate.approval_floor) < approval_rank(&previous.approval_floor) {
        return Some(("authority_expansion", "approval_floor".into()));
    }
    if !subset(&candidate.deliverable_kinds, &previous.deliverable_kinds) {
        return overlay("deliverable_kinds");
    }
    if !subset(&delegation(candidate), &delegation(previous)) {
        return overlay("delegation_targets");
    }
    if !subset(
        &candidate.required_capabilities,
        &previous.required_capabilities,
    ) {
        return overlay("required_capabilities");
    }
    let (was, now) = (&previous.review_policy, &candidate.review_policy);
    if was.required && !now.required {
        return overlay("review_policy.required");
    }
    if now.minimum_reviewers < was.minimum_reviewers {
        return overlay("review_policy.minimum_reviewers");
    }
    if !subset(&was.required_review_kinds, &now.required_review_kinds) {
        return overlay("review_policy.required_review_kinds");
    }
    let (was, now) = (&previous.delivery_policy, &candidate.delivery_policy);
    if !subset(&was.required_evidence, &now.required_evidence) {
        return overlay("delivery_policy.required_evidence");
    }
    if was.require_all_deliverables && !now.require_all_deliverables {
        return overlay("delivery_policy.require_all_deliverables");
    }
    if let Some(field) = context_overlay_violation(previous, candidate) {
        return Some(("invalid_overlay", field));
    }
    if candidate.mission != previous.mission {
        return overlay("mission");
    }
    if candidate.default_topology != previous.default_topology {
        return overlay("default_topology");
    }
    if let Some(hint) = &previous.routing_hint {
        let next = candidate.routing_hint.as_ref();
        if hint.provider.is_some() && next.is_none_or(|n| n.provider.is_none()) {
            return overlay("routing_hint.provider");
        }
        if hint.model.is_some() && next.is_none_or(|n| n.model.is_none()) {
            return overlay("routing_hint.model");
        }
    }
    None
}

fn capability_fact_applies(
    fact: &CapabilityFact,
    work_order: &WorkOrderRef,
    role: &RoleRef,
) -> bool {
    match &fact.work_order_scope {
        None => work_order.principal_id.is_none(),
        Some(scope) => {
            scope.node_id == work_order.node_id
                && scope.attempt_id == work_order.attempt_id
                && scope.principal_id == work_order.principal_id
                && scope.role_id.as_deref() == Some(role.id.as_str())
        }
    }
}

type CandidateKey<'a> = (u64, &'a str, &'a str, &'a str, Option<&'a str>);

fn candidate_key(item: &ContextReference) -> CandidateKey<'_> {
    (
        item.byte_size,
        &item.kind,
        &item.identifier,
        &item.provenance,
        item.content_digest.as_deref(),
    )
}

type IdentityKey = (String, String, String, Option<String>);

fn identity(item: &ContextReference) -> IdentityKey {
    (
        item.kind.clone(),
        item.identifier.clone(),
        item.provenance.clone(),
        item.content_digest.clone(),
    )
}

/// The context checks in the order a refusal names them.
const CONTEXT_CHECKS: [&str; 5] = [
    "mixed_revision",
    "unreadable_context",
    "context_out_of_scope",
    "stale_context",
    "sensitivity_refused",
];

struct ContextScope<'a> {
    role: &'a RoleRef,
    source: &'a DefinitionSource,
    work_order: &'a WorkOrderRef,
    clock: &'a Instant,
    snapshot_revision: &'a str,
    bounds: &'a ContextBundleBounds,
}

impl ContextScope<'_> {
    fn ceiling(&self, selector: &ContextSelector) -> usize {
        sensitivity_rank(&selector.max_sensitivity)
            .min(sensitivity_rank(&self.bounds.max_sensitivity))
    }

    fn fails(&self, check: &str, selector: &ContextSelector, item: &ContextReference) -> bool {
        match check {
            "mixed_revision" => item.snapshot_revision != self.snapshot_revision,
            "unreadable_context" => !item.readable,
            "context_out_of_scope" => {
                !WorkOrderRef::scope_matches(item.work_order_scope.as_ref(), self.work_order)
            }
            "stale_context" => {
                (selector.requires_freshness && item.fresh_until.is_none())
                    || item.fresh_until.is_some_and(|f| f < *self.clock)
            }
            _ => sensitivity_rank(&item.sensitivity) > self.ceiling(selector),
        }
    }

    fn blocked_for(
        &self,
        selector: &ContextSelector,
        item: &ContextReference,
        reason: &'static str,
    ) -> Blocked {
        let detail = match reason {
            "unreadable_context" => item.unavailable_reason.clone(),
            "stale_context" if selector.requires_freshness && item.fresh_until.is_none() => {
                Some("freshness metadata required".into())
            }
            _ => None,
        };
        Blocked {
            role: self.role.clone(),
            reason,
            source_layer: Some(self.source.layer),
            source_id: Some(item.provenance.clone()),
            reference: Some(format!("{}:{}", item.kind, item.identifier)),
            detail,
        }
    }

    /// The refusal for a selector: first a reason every candidate shares,
    /// else the first reason of the first failing candidate.
    fn failure(
        &self,
        selector: &ContextSelector,
        candidates: &[&ContextReference],
    ) -> Option<Blocked> {
        if candidates.is_empty() {
            return Some(blocked(
                self.role,
                "missing_context",
                Some(self.source),
                Some(selector.reference()),
                None,
            ));
        }
        let mut ordered = candidates.to_vec();
        ordered.sort_by(|a, b| candidate_key(a).cmp(&candidate_key(b)));
        for reason in CONTEXT_CHECKS {
            if ordered
                .iter()
                .all(|item| self.fails(reason, selector, item))
            {
                return Some(self.blocked_for(selector, ordered[0], reason));
            }
        }
        for item in &ordered {
            for reason in CONTEXT_CHECKS {
                if self.fails(reason, selector, item) {
                    return Some(self.blocked_for(selector, item, reason));
                }
            }
        }
        None
    }

    fn eligible<'c>(
        &self,
        selector: &ContextSelector,
        candidates: &[&'c ContextReference],
    ) -> Vec<&'c ContextReference> {
        let mut eligible: Vec<&ContextReference> = candidates
            .iter()
            .copied()
            .filter(|item| {
                item.snapshot_revision == self.snapshot_revision
                    && item.readable
                    && WorkOrderRef::scope_matches(item.work_order_scope.as_ref(), self.work_order)
                    && (!selector.requires_freshness || item.fresh_until.is_some())
                    && item.fresh_until.is_none_or(|f| f >= *self.clock)
                    && sensitivity_rank(&item.sensitivity) <= self.ceiling(selector)
            })
            .collect();
        eligible.sort_by(|a, b| candidate_key(a).cmp(&candidate_key(b)));
        eligible
    }

    /// The cheapest combination inside the bounds, by total bytes, then
    /// reference count, then the stable reference list.
    fn select_bundle(
        &self,
        manifest: &Manifest,
        catalog: &[ContextReference],
    ) -> Result<Value, Blocked> {
        let mut sets: Vec<Vec<&ContextReference>> = Vec::new();
        for selector in &manifest.context_selectors {
            let candidates: Vec<&ContextReference> = catalog
                .iter()
                .filter(|item| {
                    item.kind == selector.kind
                        && selector
                            .identifier
                            .as_ref()
                            .is_none_or(|id| *id == item.identifier)
                })
                .collect();
            let failure = self.failure(selector, &candidates);
            let eligible = self.eligible(selector, &candidates);
            if eligible.is_empty() {
                return Err(failure.unwrap_or_else(|| {
                    blocked(
                        self.role,
                        "missing_context",
                        Some(self.source),
                        Some(selector.reference()),
                        None,
                    )
                }));
            }
            sets.push(eligible);
        }
        let count = sets
            .iter()
            .fold(1u128, |acc, set| acc.saturating_mul(set.len() as u128));
        if count > u128::from(self.bounds.max_combinations) {
            return Err(blocked(
                self.role,
                "over_budget",
                Some(self.source),
                Some("context_combinations".into()),
                Some(format!(
                    "{count} candidate combinations exceed limit {}",
                    self.bounds.max_combinations
                )),
            ));
        }
        type Rank = (u64, usize, Vec<IdentityKey>);
        let mut best: Option<(Rank, Vec<&ContextReference>)> = None;
        // itertools.product order: the last selector's choice moves fastest.
        let mut index = vec![0usize; sets.len()];
        loop {
            let mut unique: Vec<(IdentityKey, &ContextReference)> = Vec::new();
            for (set, &i) in sets.iter().zip(&index) {
                let item = set[i];
                let key = identity(item);
                match unique.iter_mut().find(|(k, _)| *k == key) {
                    Some(slot) => slot.1 = item,
                    None => unique.push((key, item)),
                }
            }
            unique.sort_by(|a, b| a.0.cmp(&b.0));
            let total: u64 = unique.iter().map(|(_, item)| item.byte_size).sum();
            if unique.len() as u64 <= self.bounds.max_references && total <= self.bounds.max_bytes {
                let rank: Rank = (
                    total,
                    unique.len(),
                    unique.iter().map(|(k, _)| k.clone()).collect(),
                );
                if best.as_ref().is_none_or(|(b, _)| rank < *b) {
                    best = Some((rank, unique.iter().map(|(_, item)| *item).collect()));
                }
            }
            let mut position = sets.len();
            let exhausted = loop {
                if position == 0 {
                    break true;
                }
                position -= 1;
                index[position] += 1;
                if index[position] < sets[position].len() {
                    break false;
                }
                index[position] = 0;
            };
            if exhausted {
                break;
            }
        }
        let Some(((total, _, _), references)) = best else {
            return Err(blocked(
                self.role,
                "over_budget",
                Some(self.source),
                Some("context_bundle".into()),
                None,
            ));
        };
        let references: Vec<Value> = references.iter().map(|r| r.to_json()).collect();
        let work_order = self.work_order.to_json();
        let digest = canonical_digest(
            "context-bundle",
            &json!({
                "work_order": work_order,
                "snapshot_revision": self.snapshot_revision,
                "references": references,
                "total_bytes": total,
            }),
        );
        Ok(json!({
            "work_order": work_order,
            "snapshot_revision": self.snapshot_revision,
            "references": references,
            "total_bytes": total,
            "digest": digest,
        }))
    }
}

/// Resolve one role, or name what blocks it.
pub fn resolve(request: &Request) -> Resolution {
    match resolve_inner(request) {
        Ok(value) => Resolution::Resolved(value),
        Err(refusal) => Resolution::Blocked(refusal),
    }
}

fn resolve_inner(request: &Request) -> Result<Value, Blocked> {
    let role = &request.role;
    let revision = request.snapshot_revision.as_str();
    if request
        .work_order
        .role_id
        .as_ref()
        .is_some_and(|r| *r != role.id)
    {
        return Err(blocked(
            role,
            "invalid_manifest",
            None,
            Some("work_order.role_id".into()),
            Some("work order role does not match requested role".into()),
        ));
    }
    for record in &request.unchecked {
        if !record.has_definition
            && record.status != "valid"
            && record.role.as_ref().is_none_or(|r| r == role)
        {
            return Err(Blocked {
                role: role.clone(),
                reason: "invalid_manifest",
                source_layer: Some(record.layer),
                source_id: Some(record.source_id.clone()),
                reference: Some(role.id.clone()),
                detail: Some(
                    record
                        .error
                        .clone()
                        .unwrap_or_else(|| "source identity could not be validated".into()),
                ),
            });
        }
    }
    let mut ordered: Vec<&DefinitionSource> = request
        .definitions
        .iter()
        .filter(|d| d.role == *role)
        .collect();
    ordered.sort_by(|a, b| (a.layer, &a.source_id).cmp(&(b.layer, &b.source_id)));
    for layer in RoleLayer::ALL {
        let same: Vec<&str> = ordered
            .iter()
            .filter(|d| d.layer == layer)
            .map(|d| d.source_id.as_str())
            .collect();
        if same.len() > 1 {
            return Err(blocked(
                role,
                "invalid_overlay",
                Some(ordered[0]),
                Some(role.id.clone()),
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
    if ordered.is_empty() {
        return Err(blocked(
            role,
            "not_found",
            None,
            Some(role.id.clone()),
            None,
        ));
    }
    for source in &ordered {
        if source.snapshot_revision != revision {
            return Err(blocked(
                role,
                "mixed_revision",
                Some(*source),
                Some(source.snapshot_revision.clone()),
                None,
            ));
        }
        if source.status != "valid" {
            return Err(blocked(
                role,
                "invalid_manifest",
                Some(*source),
                Some(source.role.id.clone()),
                source.error.clone(),
            ));
        }
    }
    let valid: Vec<(&DefinitionSource, &Manifest)> = ordered
        .iter()
        .filter_map(|s| s.manifest.as_ref().map(|m| (*s, m)))
        .collect();
    let mut manifest = valid[0].1;
    for (source, next) in &valid[1..] {
        if let Some((reason, field)) = overlay_violation(manifest, next) {
            return Err(blocked(role, reason, Some(*source), Some(field), None));
        }
        manifest = *next;
    }
    let highest = valid[valid.len() - 1].0;

    let mut facts: Vec<&CapabilityFact> = request.capability_facts.iter().collect();
    facts.sort_by(|a, b| (&a.capability, &a.source_id).cmp(&(&b.capability, &b.source_id)));
    for capability in &manifest.required_capabilities {
        let matching: Vec<&CapabilityFact> = facts
            .iter()
            .copied()
            .filter(|f| {
                f.capability == *capability
                    && f.available
                    && capability_fact_applies(f, &request.work_order, role)
            })
            .collect();
        let Some(first) = matching.first() else {
            return Err(blocked(
                role,
                "missing_capability",
                Some(highest),
                Some(capability.clone()),
                None,
            ));
        };
        let fact = matching
            .iter()
            .find(|f| f.snapshot_revision == revision)
            .unwrap_or(first);
        if fact.snapshot_revision != revision {
            return Err(Blocked {
                role: role.clone(),
                reason: "mixed_revision",
                source_layer: None,
                source_id: Some(fact.source_id.clone()),
                reference: Some(capability.clone()),
                detail: None,
            });
        }
    }

    let scope = ContextScope {
        role,
        source: highest,
        work_order: &request.work_order,
        clock: &request.clock,
        snapshot_revision: revision,
        bounds: &request.bounds,
    };
    let bundle = scope.select_bundle(manifest, &request.context_catalog)?;
    let last = valid.len() - 1;
    let chain: Vec<Value> = valid
        .iter()
        .enumerate()
        .map(|(i, (source, source_manifest))| {
            json!({
                "layer": source.layer.as_str(),
                "source_id": source.source_id,
                "snapshot_revision": source.snapshot_revision,
                "status": source.status,
                "disposition": if i == last { "contributing" } else { "shadowed" },
                "manifest_digest": canonical_digest("role-source", &source_manifest.to_json()),
                "error": Value::Null,
            })
        })
        .collect();
    let manifest_json = manifest.to_json();
    Ok(json!({
        "role": role.to_json(),
        "work_order": request.work_order.to_json(),
        "snapshot_revision": revision,
        "source_chain": chain,
        "manifest_digest": canonical_digest("role-manifest", &manifest_json),
        "manifest": manifest_json,
        "context_bundle": bundle,
        "required_capabilities": manifest.required_capabilities,
        "authority_ceiling": manifest.authority_ceiling,
        "approval_floor": manifest.approval_floor,
        "review_policy": manifest.review_policy.to_json(),
        "delivery_policy": manifest.delivery_policy.to_json(),
        "routing_projection": manifest
            .routing_hint
            .as_ref()
            .map_or(Value::Null, models::RoutingHint::to_json),
    }))
}

fn list<T>(
    payload: &Value,
    key: &str,
    parse: impl Fn(&Value, &str) -> Result<T, String>,
) -> Result<Vec<T>, String> {
    match payload.get(key) {
        None | Some(Value::Null) => Ok(Vec::new()),
        Some(Value::Array(items)) => items
            .iter()
            .enumerate()
            .map(|(i, item)| parse(item, &format!("{key}.{i}")))
            .collect(),
        Some(_) => Err(format!("{key}: input should be a list")),
    }
}

fn field<'a>(payload: &'a Value, key: &str) -> Result<&'a Value, String> {
    payload
        .get(key)
        .ok_or_else(|| format!("{key}: field required"))
}

fn unchecked_row(value: &Value, at: &str) -> Result<Unchecked, String> {
    let text = |key: &str| value.get(key).and_then(Value::as_str);
    Ok(Unchecked {
        layer: text("layer")
            .and_then(RoleLayer::parse)
            .ok_or_else(|| format!("{at}.layer: input should be a role layer"))?,
        source_id: text("source")
            .ok_or_else(|| format!("{at}.source: field required"))?
            .to_string(),
        status: text("status")
            .ok_or_else(|| format!("{at}.status: field required"))?
            .to_string(),
        role: match value.get("role") {
            None | Some(Value::Null) => None,
            Some(role) => Some(models::role_ref(role, &format!("{at}.role"))?),
        },
        has_definition: value
            .get("has_definition")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        error: text("error").map(str::to_string),
    })
}

/// `spawn-overlay` kind `roles-resolve`: the dumped arguments
/// in, the dumped resolution out.
pub fn resolve_kind(payload: &Value) -> Result<Value, String> {
    let request = Request {
        role: models::role_ref(field(payload, "role")?, "role")?,
        definitions: list(payload, "definitions", |v, _| models::definition_source(v))?,
        capability_facts: list(payload, "capability_facts", models::capability_fact)?,
        context_catalog: list(payload, "context_catalog", models::context_reference)?,
        work_order: models::work_order_ref(field(payload, "work_order")?, "work_order")?,
        clock: models::parse_instant(field(payload, "clock")?, "clock")?,
        snapshot_revision: field(payload, "snapshot_revision")?
            .as_str()
            .ok_or("snapshot_revision: input should be a valid string")?
            .to_string(),
        bounds: models::bundle_bounds(field(payload, "bundle_bounds")?, "bundle_bounds")?,
        unchecked: list(payload, "unchecked_definitions", unchecked_row)?,
    };
    Ok(resolve(&request).to_json())
}
