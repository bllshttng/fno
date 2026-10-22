"""Pure deterministic resolution for immutable role and context snapshots."""

from __future__ import annotations

from datetime import datetime
from typing import Sequence

from fno.company.contracts import RoleRef, WorkOrderRef
from fno.roles.models import (
    ApprovalFloor,
    AuthorityCeiling,
    CapabilityFact,
    ContextBundleBounds,
    ContextReference,
    ContextSelector,
    DefinitionStatus,
    ManifestRoutingResolution,
    ResolvedRole,
    RoleDefinitionSource,
    RoleManifest,
    RoleResolution,
    RoleResolutionBlocked,
    RoleResolutionReason,
    Sensitivity,
    canonical_digest as _digest,
)
from fno.roles.registry import DiscoveredRoleDefinition, RegistryError, RoleRegistry
from fno.rust_binary import verb_call

_AUTHORITY_RANK = {
    AuthorityCeiling.ADVISORY: 0,
    AuthorityCeiling.INTERNAL: 1,
    AuthorityCeiling.EXTERNAL: 2,
}
_APPROVAL_RANK = {
    ApprovalFloor.NONE: 0,
    ApprovalFloor.PRINCIPAL: 1,
    ApprovalFloor.FOUNDER: 2,
}
_SENSITIVITY_RANK = {
    Sensitivity.PUBLIC: 0,
    Sensitivity.INTERNAL: 1,
    Sensitivity.SENSITIVE: 2,
    Sensitivity.RESTRICTED: 3,
}


def _delegation_keys(manifest: RoleManifest) -> set[tuple[str, str]]:
    return {(target.function_id, target.id) for target in manifest.delegation_targets}


def _selector_field(selector: ContextSelector, field: str | None = None) -> str:
    reference = f"context_selectors[{selector.reference}]"
    return f"{reference}.{field}" if field is not None else reference


def _context_overlay_violation(
    previous: RoleManifest,
    candidate: RoleManifest,
) -> str | None:
    options: list[tuple[int, ...]] = []
    for selector in candidate.context_selectors:
        same_kind = tuple(
            index
            for index, prior in enumerate(previous.context_selectors)
            if prior.kind is selector.kind
        )
        if not same_kind:
            return _selector_field(selector)
        compatible_identifier = tuple(
            index
            for index in same_kind
            if previous.context_selectors[index].identifier is None
            or previous.context_selectors[index].identifier == selector.identifier
        )
        if not compatible_identifier:
            return _selector_field(selector, "identifier")
        compatible_freshness = tuple(
            index
            for index in compatible_identifier
            if not previous.context_selectors[index].requires_freshness
            or selector.requires_freshness
        )
        if not compatible_freshness:
            return _selector_field(selector, "requires_freshness")
        compatible_sensitivity = tuple(
            index
            for index in compatible_freshness
            if _SENSITIVITY_RANK[selector.max_sensitivity]
            <= _SENSITIVITY_RANK[previous.context_selectors[index].max_sensitivity]
        )
        if not compatible_sensitivity:
            return _selector_field(selector, "max_sensitivity")
        options.append(compatible_sensitivity)

    matched_prior: dict[int, int] = {}

    def assign(candidate_index: int, seen: set[int]) -> bool:
        for prior_index in options[candidate_index]:
            if prior_index in seen:
                continue
            seen.add(prior_index)
            occupant = matched_prior.get(prior_index)
            if occupant is None or assign(occupant, seen):
                matched_prior[prior_index] = candidate_index
                return True
        return False

    for candidate_index, selector in enumerate(candidate.context_selectors):
        if not assign(candidate_index, set()):
            return _selector_field(selector)
    return None


def _overlay_violation(
    previous: RoleManifest,
    candidate: RoleManifest,
) -> tuple[RoleResolutionReason, str] | None:
    if candidate.role != previous.role:
        return RoleResolutionReason.INVALID_OVERLAY, "role"
    if candidate.function != previous.function:
        return RoleResolutionReason.INVALID_OVERLAY, "function"
    if _AUTHORITY_RANK[candidate.authority_ceiling] > _AUTHORITY_RANK[previous.authority_ceiling]:
        return RoleResolutionReason.AUTHORITY_EXPANSION, "authority_ceiling"
    if _APPROVAL_RANK[candidate.approval_floor] < _APPROVAL_RANK[previous.approval_floor]:
        return RoleResolutionReason.AUTHORITY_EXPANSION, "approval_floor"
    if not set(candidate.deliverable_kinds).issubset(previous.deliverable_kinds):
        return RoleResolutionReason.INVALID_OVERLAY, "deliverable_kinds"
    if not _delegation_keys(candidate).issubset(_delegation_keys(previous)):
        return RoleResolutionReason.INVALID_OVERLAY, "delegation_targets"
    if not set(candidate.required_capabilities).issubset(previous.required_capabilities):
        return RoleResolutionReason.INVALID_OVERLAY, "required_capabilities"
    if previous.review_policy.required and not candidate.review_policy.required:
        return RoleResolutionReason.INVALID_OVERLAY, "review_policy.required"
    if candidate.review_policy.minimum_reviewers < previous.review_policy.minimum_reviewers:
        return RoleResolutionReason.INVALID_OVERLAY, "review_policy.minimum_reviewers"
    if not set(previous.review_policy.required_review_kinds).issubset(
        candidate.review_policy.required_review_kinds
    ):
        return RoleResolutionReason.INVALID_OVERLAY, "review_policy.required_review_kinds"
    if not set(previous.delivery_policy.required_evidence).issubset(
        candidate.delivery_policy.required_evidence
    ):
        return RoleResolutionReason.INVALID_OVERLAY, "delivery_policy.required_evidence"
    if (
        previous.delivery_policy.require_all_deliverables
        and not candidate.delivery_policy.require_all_deliverables
    ):
        return RoleResolutionReason.INVALID_OVERLAY, "delivery_policy.require_all_deliverables"
    context_violation = _context_overlay_violation(previous, candidate)
    if context_violation is not None:
        return RoleResolutionReason.INVALID_OVERLAY, context_violation
    if candidate.mission != previous.mission:
        return RoleResolutionReason.INVALID_OVERLAY, "mission"
    if candidate.default_topology != previous.default_topology:
        return RoleResolutionReason.INVALID_OVERLAY, "default_topology"
    if previous.routing_hint is not None:
        for field in ("provider", "model"):
            if getattr(previous.routing_hint, field) is not None and (
                candidate.routing_hint is None or getattr(candidate.routing_hint, field) is None
            ):
                return RoleResolutionReason.INVALID_OVERLAY, f"routing_hint.{field}"
    return None


def _blocked(
    role: RoleRef,
    reason: RoleResolutionReason,
    *,
    source: RoleDefinitionSource | None = None,
    source_id: str | None = None,
    reference: str | None = None,
    detail: str | None = None,
) -> RoleResolutionBlocked:
    return RoleResolutionBlocked(
        role=role,
        reason=reason,
        source_layer=source.layer if source is not None else None,
        source_id=source.source_id if source is not None else source_id,
        reference=reference,
        detail=detail,
    )


def resolve_role(
    *,
    role: RoleRef,
    definitions: Sequence[RoleDefinitionSource],
    capability_facts: Sequence[CapabilityFact],
    context_catalog: Sequence[ContextReference],
    work_order: WorkOrderRef,
    clock: datetime,
    snapshot_revision: str,
    bundle_bounds: ContextBundleBounds,
    unchecked_definitions: Sequence[DiscoveredRoleDefinition] = (),
) -> RoleResolution:
    """Resolve one role without reads, writes, grants, or implicit fallback; fno-agents resolves."""
    if clock.tzinfo is None:
        raise ValueError("clock must be timezone-aware")
    unchecked = [
        {"layer": r.layer.value, "source": r.source_id, "status": r.status.value,
         "role": r.role and r.role.model_dump(mode="json"),
         "has_definition": r.definition is not None, "error": r.error}
        for r in unchecked_definitions
    ]
    answer = verb_call("spawn-overlay", {
        "kind": "roles-resolve",
        "role": role.model_dump(mode="json"),
        "definitions": [d.model_dump(mode="json") for d in definitions],
        "capability_facts": [f.model_dump(mode="json") for f in capability_facts],
        "context_catalog": [c.model_dump(mode="json") for c in context_catalog],
        "work_order": work_order.model_dump(mode="json"),
        "clock": clock.isoformat(),
        "snapshot_revision": snapshot_revision,
        "bundle_bounds": bundle_bounds.model_dump(mode="json"),
        "unchecked_definitions": unchecked,
    })
    model = RoleResolutionBlocked if "reason" in answer else ResolvedRole
    return model.model_validate(answer)


def resolve_manifest_routing(
    role_id: str,
    records: Sequence[DiscoveredRoleDefinition],
) -> ManifestRoutingResolution | RoleResolutionBlocked:
    """Validate discovered manifests and return only their routing projection."""
    candidates = tuple(
        record for record in records if record.role is not None and record.role.id == role_id
    )
    functions = sorted({record.role.function_id for record in candidates if record.role})
    role = RoleRef(
        id=role_id,
        function_id=functions[0] if len(functions) == 1 else "unavailable",
    )
    unchecked = next(
        (
            record
            for record in records
            if record.role is None and record.status is not DefinitionStatus.VALID
        ),
        None,
    )
    if unchecked is not None:
        return RoleResolutionBlocked(
            role=role,
            reason=RoleResolutionReason.INVALID_MANIFEST,
            source_layer=unchecked.layer,
            source_id=unchecked.source_id,
            reference=role_id,
            detail=unchecked.error or "source identity could not be validated",
        )
    if len(functions) > 1:
        source = candidates[0]
        return RoleResolutionBlocked(
            role=role,
            reason=RoleResolutionReason.INVALID_MANIFEST,
            source_layer=source.layer,
            source_id=source.source_id,
            reference=role_id,
            detail=(f"role {role_id!r} exists in multiple functions: " + ", ".join(functions)),
        )
    if not candidates:
        return RoleResolutionBlocked(
            role=role,
            reason=RoleResolutionReason.NOT_FOUND,
            reference=role_id,
        )
    missing_definition = next(
        (record for record in candidates if record.definition is None),
        None,
    )
    if missing_definition is not None:
        return RoleResolutionBlocked(
            role=role,
            reason=RoleResolutionReason.INVALID_MANIFEST,
            source_layer=missing_definition.layer,
            source_id=missing_definition.source_id,
            reference=role_id,
            detail=missing_definition.error or "source could not be validated",
        )
    definitions = tuple(record.definition for record in candidates if record.definition is not None)
    registry = RoleRegistry(records=definitions)
    try:
        ordered = registry.definitions_for(role)
    except RegistryError as exc:
        candidate_source = candidates[0]
        return RoleResolutionBlocked(
            role=role,
            reason=RoleResolutionReason.INVALID_OVERLAY,
            source_layer=candidate_source.layer,
            source_id=candidate_source.source_id,
            reference=role_id,
            detail=str(exc),
        )
    revision = ordered[0].snapshot_revision
    manifest: RoleManifest | None = None
    for definition_source in ordered:
        if definition_source.snapshot_revision != revision:
            return _blocked(
                role,
                RoleResolutionReason.MIXED_REVISION,
                source=definition_source,
                reference=definition_source.snapshot_revision,
            )
        if (
            definition_source.status is not DefinitionStatus.VALID
            or definition_source.manifest is None
        ):
            return _blocked(
                role,
                RoleResolutionReason.INVALID_MANIFEST,
                source=definition_source,
                reference=role_id,
                detail=definition_source.error,
            )
        if manifest is not None:
            violation = _overlay_violation(manifest, definition_source.manifest)
            if violation is not None:
                reason, reference = violation
                return _blocked(
                    role,
                    reason,
                    source=definition_source,
                    reference=reference,
                )
        manifest = definition_source.manifest
    assert manifest is not None
    return ManifestRoutingResolution(
        role=role,
        source_digest=_digest(
            "role-routing-sources",
            [source.model_dump(mode="json") for source in ordered],
        ),
        routing_projection=manifest.routing_hint,
    )
