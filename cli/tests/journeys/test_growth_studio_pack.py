from __future__ import annotations

import hashlib
import json
import subprocess
from datetime import UTC, datetime
from pathlib import Path

import pytest

from fno.rust_binary import find_dev_binary

# Activation, role discovery and role resolution run in fno-agents; this
# journey speaks to the spawn-overlay kinds the verbs answer through.
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


def _spawn_overlay(payload: dict) -> dict:
    """One native spawn-overlay round-trip: JSON payload in, answer out."""
    proc = subprocess.run(
        [str(find_dev_binary()), "spawn-overlay"],
        input=json.dumps(payload),
        capture_output=True,
        text=True,
    )
    assert proc.returncode == 0, proc.stdout + proc.stderr
    return json.loads(proc.stdout)


def _discover_rows(root: Path) -> list[dict]:
    """The full discovery rows (raw_definition, definition included) the
    retired Python `discover_role_definitions` door parsed."""
    return _spawn_overlay({"kind": "roles-discover", "root": str(root)})["rows"]


def _pack_asset(identifier: str, sensitivity: str, revision: str) -> dict:
    asset = PACK_ASSETS / f"{identifier}.md"
    data = asset.read_bytes()
    digest = hashlib.sha256(data).hexdigest()
    return {
        "kind": "artifact", "identifier": identifier, "provenance": str(asset),
        "work_order_scope": None, "content_digest": digest, "content_revision": digest,
        "snapshot_revision": revision, "fresh_until": None,
        "sensitivity": sensitivity, "byte_size": len(data),
        "readable": True, "unavailable_reason": None,
    }


def _resolve_role(root: Path, role_id: str, function_id: str, *, extra_definitions=()):
    rows = _discover_rows(root)
    role_ref = next(r["role"] for r in rows if r["role"] is not None and r["role"]["id"] == role_id)
    definitions = [
        r["definition"] for r in rows if r["definition"] is not None and r["role"] == role_ref
    ]
    revision = definitions[0]["snapshot_revision"]
    work_order = {"node_id": "wo-1", "attempt_id": "attempt-1",
                  "principal_id": None, "role_id": role_id}
    manifest = definitions[0]["manifest"]
    # The roles declare real required_capabilities now, so an empty fact list
    # would block with missing_capability. Mint work-order-scoped capability
    # facts for every declared capability - the scoped-fact form the parity
    # goldens exercise, not a weakening of the roles.
    facts = [
        {
            "capability": capability, "available": True, "source_id": "test-fixture",
            "snapshot_revision": revision, "work_order_scope": work_order,
        }
        for capability in manifest["required_capabilities"]
    ]
    # Supply a context catalog satisfying every declared artifact selector. The
    # content here is fixture-only (the pack's own asset files); the
    # project-supplied product-truth provenance is exercised separately in
    # the roles_context goldens and test_growth_studio_faucet.py.
    catalog = [
        _pack_asset(selector["identifier"], selector["max_sensitivity"], revision)
        for selector in manifest["context_selectors"]
        if selector["kind"] == "artifact" and selector["identifier"]
    ]
    return _spawn_overlay(
        {
            "kind": "roles-resolve",
            "role": role_ref,
            "definitions": (*definitions, *extra_definitions),
            "capability_facts": facts,
            "context_catalog": catalog,
            "work_order": work_order,
            "clock": datetime.now(UTC).isoformat(),
            "snapshot_revision": revision,
            "bundle_bounds": {
                "max_references": 32,
                "max_bytes": 10_000_000,
                "max_sensitivity": "sensitive",
                "max_combinations": 4096,
            },
            "unchecked_definitions": [
                {
                    "layer": r["layer"],
                    "source": r["source"],
                    "status": r["status"],
                    "role": r["role"],
                    "has_definition": r["definition"] is not None,
                    "error": r["error"],
                }
                for r in rows
            ],
        }
    )


def test_pack_activates_resolves_and_deactivates_end_to_end(tmp_path):
    root = tmp_path / "roles"
    root.mkdir()
    outcome = _plugins(root, "activate", str(PACK))
    assert outcome["already_active"] is False
    assert {path.split("/")[-1] for path in outcome["written_paths"]} == {
        f"{role}.json" for role in ROLE_IDS
    }

    rows = _discover_rows(root)
    found = {r["role"]["id"] for r in rows if r["role"] is not None}
    assert set(ROLE_IDS).issubset(found)
    assert all(r["layer"] == "plugin" for r in rows if r["role"] is not None)

    for role_id in ROLE_IDS:
        result = _resolve_role(root, role_id, "growth-studio")
        assert "reason" not in result, (role_id, result.get("reason"))
        plugin_sources = [s for s in result["source_chain"] if s["layer"] == "plugin"]
        assert plugin_sources and plugin_sources[-1]["disposition"] == "contributing"

    outcome = _plugins(root, "deactivate", "growth-studio")
    assert {path.split("/")[-1] for path in outcome["removed"]} == {f"{role}.json" for role in ROLE_IDS}
    after = _discover_rows(root)
    assert not any(r["role"] is not None and r["role"]["id"] in ROLE_IDS for r in after)


# The declared-publication-effect classification moved to Rust
# (effect_gate.rs, tested there); this file no longer asserts the table.


def test_overlay_lowering_approval_floor_is_refused_as_authority_expansion(tmp_path):
    root = tmp_path / "roles"
    root.mkdir()
    _plugins(root, "activate", str(PACK))

    rows = _discover_rows(root)
    plugin_def = next(
        r["definition"]
        for r in rows
        if r["definition"] is not None and r["role"]["id"] == "marketing"
    )
    # A project-layer overlay that LOWERS the approval floor (founder -> none)
    # is an authority expansion and must be refused.
    widened = {
        **plugin_def,
        "layer": "project",
        "source_id": "project/marketing.json",
        "manifest": {**plugin_def["manifest"], "approval_floor": "none"},
    }
    blocked = _resolve_role(root, "marketing", "growth-studio", extra_definitions=(widened,))
    assert "reason" in blocked
    assert blocked["reason"] == "authority_expansion"
    assert "approval_floor" in (blocked.get("reference") or "")

    # An overlay that RAISES the floor (already founder) or tightens the ceiling
    # succeeds: the ladder narrows, it does not widen.
    # (marketing is already founder; verify a tightened authority ceiling accepts.)
    tightened = {
        **plugin_def,
        "layer": "project",
        "source_id": "project/marketing-tight.json",
        "manifest": {**plugin_def["manifest"], "approval_floor": "founder"},
    }
    resolved = _resolve_role(root, "marketing", "growth-studio", extra_definitions=(tightened,))
    assert "reason" not in resolved
