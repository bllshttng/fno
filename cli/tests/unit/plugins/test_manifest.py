from __future__ import annotations

import copy
from pathlib import Path

import pytest
from pydantic import ValidationError

from fno.company.contracts import EvidenceResult, FunctionRef, RoleRef
from fno.roles.models import AuthorityCeiling, DeliveryPolicy, ReviewPolicy, RoleManifest
from fno.plugins import (
    AdapterConformance,
    AdapterDeclaration,
    AssetDeclaration,
    CompatibilityRange,
    EffectDeclaration,
    EvaluatorDeclaration,
    PackManifest,
    ScenarioDeclaration,
    WorkflowDeclaration,
    pack_digest,
)


def _role_manifest(role_id: str = "marketing", function_id: str = "growth-studio") -> RoleManifest:
    return RoleManifest(
        role=RoleRef(id=role_id, function_id=function_id),
        function=FunctionRef(id=function_id),
        mission="draft and ship on-brand growth content",
        deliverable_kinds=("published-post",),
        authority_ceiling=AuthorityCeiling.INTERNAL,
        review_policy=ReviewPolicy(required=True, minimum_reviewers=1),
        delivery_policy=DeliveryPolicy(required_evidence=("factual-review",)),
        default_topology="loop",
        approval_floor="founder",
    )


def _full_pack() -> PackManifest:
    return PackManifest(
        id="growth-studio",
        version="0.1.0",
        footnote_compat=CompatibilityRange(minimum="0.3.0"),
        roles=(_role_manifest(),),
        workflows=(WorkflowDeclaration(id="launch", source="workflows/launch.yaml"),),
        adapters=(
            AdapterDeclaration(
                id="social-publisher",
                destination="social-network",
                conformance=AdapterConformance(
                    adapter_id="social-publisher",
                    adapter_version="0.1.0",
                    remote_idempotency=True,
                ),
            ),
        ),
        evaluators=(EvaluatorDeclaration(id="factual-review", command="true"),),
        assets=(AssetDeclaration(id="brand-voice", source="assets/brand-voice.md"),),
        permissions=(
            EffectDeclaration(effect_class="external.publication", destination="social-network"),
        ),
        scenarios=(
            # `true` is on PATH on every POSIX host (local + CI), so the fixture
            # does not depend on fno being installed to satisfy runnability.
            ScenarioDeclaration(
                id="launch-smoke", command="true", recorded_result=EvidenceResult.PASSED
            ),
        ),
    )


def _materialize_declared_sources(pack_dir: Path, manifest: PackManifest) -> None:
    """Create the declared workflow/asset/skill sources so verification finds them."""
    for workflow in manifest.workflows:
        target = pack_dir / Path(workflow.source)
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_text("workflow", encoding="utf-8")
    for asset in manifest.assets:
        target = pack_dir / Path(asset.source)
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_text("asset", encoding="utf-8")
    for skill in manifest.skills:
        (pack_dir / Path(skill.source)).mkdir(parents=True, exist_ok=True)


def test_unknown_fields_are_forbidden():
    with pytest.raises(ValidationError):
        PackManifest(
            id="growth-studio",
            version="0.1.0",
            footnote_compat=CompatibilityRange(minimum="0.3.0"),
            assets=(AssetDeclaration(id="a", source="a.md"),),
            grants_effect=True,  # a grant would be a second authority; the schema refuses it
        )


def test_duplicate_component_id_is_refused():
    pack_dict = _full_pack().model_dump(mode="json")
    pack_dict["assets"].append(copy.deepcopy(pack_dict["assets"][0]))
    with pytest.raises(ValidationError) as info:
        PackManifest.model_validate(pack_dict)
    assert "duplicate assets declaration" in str(info.value)


def test_empty_pack_is_refused():
    with pytest.raises(ValidationError) as info:
        PackManifest(
            id="empty", version="0.1.0", footnote_compat=CompatibilityRange(minimum="0.3.0")
        )
    assert "at least one component" in str(info.value)


def test_pack_digest_is_stable_and_content_addressed():
    pack = _full_pack()
    twin = _full_pack()
    assert pack_digest(pack) == pack_digest(twin)
    mutated = pack.model_copy(update={"version": "0.2.0"})
    assert pack_digest(mutated) != pack_digest(pack)
