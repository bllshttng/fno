from __future__ import annotations

import hashlib
import json
import subprocess
from datetime import UTC, datetime
from pathlib import Path

import pytest

from fno.company.contracts import WorkOrderRef
from fno.roles.models import (
    ApprovalFloor,
    CapabilityFact,
    ContextBundleBounds,
    ContextKind,
    ContextReference,
    RoleLayer,
    RoleResolutionBlocked,
    RoleResolutionReason,
)
from fno.roles.registry import discover_role_definitions
from fno.roles.resolver import resolve_role
from fno.rust_binary import find_dev_binary

# Activation and role discovery run in fno-agents.
pytestmark = pytest.mark.dev_build

REPO_ROOT = Path(__file__).resolve().parents[3]
PACK = REPO_ROOT / "plugins" / "growth-studio" / "plugin.yaml"
PACK_ASSETS = REPO_ROOT / "plugins" / "growth-studio" / "assets"
ROLE_IDS = ("marketing", "communications", "design", "social")


def _plugins(root: Path, *args: str) -> dict:
    """One native `fno config plugins` command against this role root."""
    proc = subprocess.run(
        [str(find_dev_binary()), "spawn-overlay", "plugins", "--root", str(root), *args, "--json"],
        capture_output=True,
        text=True,
    )
    assert proc.returncode == 0, proc.stdout + proc.stderr
    return json.loads(proc.stdout)


def _pack_asset(identifier: str, sensitivity, revision: str) -> ContextReference:
    asset = PACK_ASSETS / f"{identifier}.md"
    data = asset.read_bytes()
    digest = hashlib.sha256(data).hexdigest()
    return ContextReference(
        kind=ContextKind.ARTIFACT,
        identifier=identifier,
        provenance=str(asset),
        content_digest=digest,
        content_revision=digest,
        snapshot_revision=revision,
        sensitivity=sensitivity,
        byte_size=len(data),
        readable=True,
    )


def _resolve_role(root: Path, role_id: str, function_id: str, *, extra_definitions=()):
    records = discover_role_definitions(root=root)
    role_ref = next(r.role for r in records if r.role is not None and r.role.id == role_id)
    definitions = tuple(r.definition for r in records if r.definition is not None and r.role == role_ref)
    revision = definitions[0].snapshot_revision
    work_order = WorkOrderRef(node_id="wo-1", attempt_id="attempt-1", role_id=role_id)
    manifest = definitions[0].manifest
    # The roles declare real required_capabilities now, so an empty fact tuple
    # would block with MISSING_CAPABILITY. Mint work-order-scoped CapabilityFact
    # values for every declared capability - the scoped-fact form AC5/AC6
    # exercise, not a weakening of the roles.
    facts = tuple(
        CapabilityFact(
            capability=capability,
            available=True,
            source_id="test-fixture",
            snapshot_revision=revision,
            work_order_scope=work_order,
        )
        for capability in manifest.required_capabilities
    )
    # Supply a context catalog satisfying every declared artifact selector. The
    # content here is fixture-only (the pack's own asset files); AC16's
    # project-supplied product-truth provenance is exercised separately in
    # the roles_context goldens and test_growth_studio_faucet.py.
    catalog = tuple(
        _pack_asset(selector.identifier, selector.max_sensitivity, revision)
        for selector in manifest.context_selectors
        if selector.kind is ContextKind.ARTIFACT and selector.identifier
    )
    return resolve_role(
        role=role_ref,
        definitions=(*definitions, *extra_definitions),
        capability_facts=facts,
        context_catalog=catalog,
        work_order=work_order,
        clock=datetime.now(UTC),
        snapshot_revision=revision,
        bundle_bounds=ContextBundleBounds(max_references=32, max_bytes=10_000_000),
        unchecked_definitions=records,
    )


def test_pack_activates_resolves_and_deactivates_end_to_end(tmp_path):
    root = tmp_path / "roles"
    root.mkdir()
    outcome = _plugins(root, "activate", str(PACK))
    assert outcome["already_active"] is False
    assert {path.split("/")[-1] for path in outcome["written_paths"]} == {
        f"{role}.json" for role in ROLE_IDS
    }

    discovered = discover_role_definitions(root=root)
    found = {r.role.id for r in discovered if r.role is not None}
    assert set(ROLE_IDS).issubset(found)
    assert all(r.layer is RoleLayer.PLUGIN for r in discovered if r.role is not None)

    for role_id in ROLE_IDS:
        result = _resolve_role(root, role_id, "growth-studio")
        assert not isinstance(result, RoleResolutionBlocked), (role_id, result.reason)
        plugin_sources = [s for s in result.source_chain if s.layer is RoleLayer.PLUGIN]
        assert plugin_sources and plugin_sources[-1].disposition == "contributing"

    outcome = _plugins(root, "deactivate", "growth-studio")
    assert {path.split("/")[-1] for path in outcome["removed"]} == {f"{role}.json" for role in ROLE_IDS}
    after = discover_role_definitions(root=root)
    assert not any(r.role is not None and r.role.id in ROLE_IDS for r in after)


# The declared-publication-effect classification moved to Rust
# (effect_gate.rs, tested there); this file no longer asserts the table.


def test_overlay_lowering_approval_floor_is_refused_as_authority_expansion(tmp_path):
    root = tmp_path / "roles"
    root.mkdir()
    _plugins(root, "activate", str(PACK))

    records = discover_role_definitions(root=root)
    plugin_def = next(
        r.definition for r in records if r.definition is not None and r.role.id == "marketing"
    )
    # A project-layer overlay that LOWERS the approval floor (founder -> none)
    # is an authority expansion and must be refused.
    widened = plugin_def.model_copy(
        update={
            "layer": RoleLayer.PROJECT,
            "source_id": "project/marketing.json",
            "manifest": plugin_def.manifest.model_copy(update={"approval_floor": ApprovalFloor.NONE}),
        }
    )
    blocked = _resolve_role(root, "marketing", "growth-studio", extra_definitions=(widened,))
    assert isinstance(blocked, RoleResolutionBlocked)
    assert blocked.reason is RoleResolutionReason.AUTHORITY_EXPANSION
    assert "approval_floor" in (blocked.reference or "")

    # An overlay that RAISES the floor (already founder) or tightens the ceiling
    # succeeds: the ladder narrows, it does not widen.
    # (marketing is already founder; verify a tightened authority ceiling accepts.)
    tightened = plugin_def.model_copy(
        update={
            "layer": RoleLayer.PROJECT,
            "source_id": "project/marketing-tight.json",
            "manifest": plugin_def.manifest.model_copy(update={"approval_floor": ApprovalFloor.FOUNDER}),
        }
    )
    resolved = _resolve_role(root, "marketing", "growth-studio", extra_definitions=(tightened,))
    assert not isinstance(resolved, RoleResolutionBlocked)
