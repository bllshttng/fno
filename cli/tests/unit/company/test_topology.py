from __future__ import annotations


from fno.company.topology import (
    Topology,
    TopologyRefusal,
    validate_manifest_topology,
)
from fno.roles.models import RoleLayer


def test_each_legal_literal_resolves() -> None:
    for literal in ("direct", "loop", "squad", "pipeline"):
        resolved = validate_manifest_topology(literal)
        assert isinstance(resolved, Topology)
        assert resolved.value == literal


def test_ac4_err_unknown_literal_refuses_with_value_and_source_layer() -> None:
    refusal = validate_manifest_topology("star", source_layer=RoleLayer.PLAN)
    assert isinstance(refusal, TopologyRefusal)
    assert refusal.value == "star"
    assert refusal.source_layer is RoleLayer.PLAN
    assert "direct" in refusal.recovery
