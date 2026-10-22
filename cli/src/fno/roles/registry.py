"""Ordered discovery records for role definitions."""

from __future__ import annotations

from collections import defaultdict
from dataclasses import dataclass
import os
from pathlib import Path
from typing import Any, Sequence

from pydantic import Field

from fno.company.contracts import RoleRef
from fno.paths import resolve_repo_root
from fno.roles.models import (
    DefinitionStatus,
    RoleDefinitionSource,
    RoleLayer,
    _RoleModel,
)
from fno.rust_binary import verb_call

_LAYER_ORDER = {layer: index for index, layer in enumerate(RoleLayer)}


class RegistryError(ValueError):
    """The registry cannot choose between equal-precedence definitions."""


class DiscoveryInputError(ValueError):
    """An explicit discovery source has malformed syntax."""


def default_role_root() -> Path:
    """Resolve the conventional role root from project identity, not cwd."""
    configured = os.environ.get("FNO_ROLES_ROOT")
    if configured:
        return Path(configured).expanduser()
    try:
        project_root = resolve_repo_root()
    except RuntimeError:
        project_root = Path.cwd()
    return project_root / ".fno" / "roles"


@dataclass(frozen=True)
class DiscoveredRoleDefinition:
    """One source-attributed discovery result, including invalid documents."""

    layer: RoleLayer
    source_id: str
    status: DefinitionStatus
    role: RoleRef | None
    raw_definition: Any
    definition: RoleDefinitionSource | None
    error: str | None = None


class RoleRegistry(_RoleModel):
    records: tuple[RoleDefinitionSource, ...] = Field(default_factory=tuple)

    def definitions_for(self, role: RoleRef) -> tuple[RoleDefinitionSource, ...]:
        matches = tuple(record for record in self.records if record.role == role)
        by_layer: dict[RoleLayer, list[RoleDefinitionSource]] = defaultdict(list)
        for record in matches:
            by_layer[record.layer].append(record)
        for layer in RoleLayer:
            records = by_layer[layer]
            if len(records) > 1:
                source_ids = ", ".join(sorted(record.source_id for record in records))
                raise RegistryError(
                    f"ambiguous {layer.value} definitions for "
                    f"{role.function_id}/{role.id}: {source_ids}"
                )
        return ordered_definitions(matches)


def ordered_definitions(
    definitions: Sequence[RoleDefinitionSource],
) -> tuple[RoleDefinitionSource, ...]:
    """Return the fixed precedence order independent of discovery order."""
    return tuple(
        sorted(
            definitions,
            key=lambda record: (_LAYER_ORDER[record.layer], record.source_id),
        )
    )


def discover_role_definitions(
    *,
    root: Path | None = None,
    source_specs: Sequence[str] = (),
) -> tuple[DiscoveredRoleDefinition, ...]:
    """Read sorted JSON definitions from the layered root; fno-agents walks it."""
    payload = {"kind": "roles-discover", "root": None if root is None else str(root)}
    answer = verb_call("spawn-overlay", {**payload, "sources": list(source_specs)})
    if "input_error" in answer:
        raise DiscoveryInputError(answer["input_error"])
    return tuple(
        DiscoveredRoleDefinition(
            layer=RoleLayer(row["layer"]),
            source_id=row["source"],
            status=DefinitionStatus(row["status"]),
            role=None if row["role"] is None else RoleRef.model_validate(row["role"]),
            raw_definition=row["raw_definition"],
            definition=(
                None
                if row["definition"] is None
                else RoleDefinitionSource.model_validate(row["definition"])
            ),
            error=row["error"],
        )
        for row in answer["rows"]
    )
