"""Ordered discovery records for role definitions."""

from __future__ import annotations

from dataclasses import dataclass
import os
from pathlib import Path
from typing import Any, Sequence

from fno.company.contracts import RoleRef
from fno.paths import resolve_repo_root
from fno.roles.models import (
    DefinitionStatus,
    RoleDefinitionSource,
    RoleLayer,
)
from fno.rust_binary import command_call

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


def discover_role_definitions(
    *,
    root: Path | None = None,
    source_specs: Sequence[str] = (),
) -> tuple[DiscoveredRoleDefinition, ...]:
    """Read sorted JSON definitions from the layered root; fno-agents walks it."""
    payload = {"kind": "roles-discover", "root": None if root is None else str(root)}
    answer = command_call("spawn-overlay", {**payload, "sources": list(source_specs)})
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
