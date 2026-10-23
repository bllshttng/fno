"""Pure deterministic resolution for immutable role and context snapshots."""

from __future__ import annotations

from datetime import datetime
from typing import Sequence

from fno.company.contracts import RoleRef, WorkOrderRef
from fno.roles.models import (
    CapabilityFact,
    ContextBundleBounds,
    ContextReference,
    ResolvedRole,
    RoleDefinitionSource,
    RoleResolution,
    RoleResolutionBlocked,
)
from fno.roles.registry import DiscoveredRoleDefinition
from fno.rust_binary import command_call


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
    answer = command_call("spawn-overlay", {
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
