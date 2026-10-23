from __future__ import annotations
from tests.fixtures.graph_seed import seed_graph

import json
import subprocess
from pathlib import Path
from typing import Any

import pytest
from typer.testing import CliRunner

from fno.company.contracts import FunctionRef, RoleRef
from fno.roles import (
    AuthorityCeiling,
    DefinitionStatus,
    DeliveryPolicy,
    ReviewPolicy,
    RoleDefinitionSource,
    RoleLayer,
    RoleManifest,
)

# Role discovery runs in fno-agents (spawn-overlay kind roles-discover).
pytestmark = pytest.mark.dev_build


runner = CliRunner()


def _invoke(*args: str):
    from fno.cli import app

    return runner.invoke(app, list(args))


def _manifest(role_id: str, function_id: str) -> RoleManifest:
    role = RoleRef(id=role_id, function_id=function_id)
    return RoleManifest(
        role=role,
        function=FunctionRef(id=function_id),
        mission="Produce one bounded artifact.",
        deliverable_kinds=("brief",),
        authority_ceiling=AuthorityCeiling.INTERNAL,
        review_policy=ReviewPolicy(required=True, minimum_reviewers=1),
        delivery_policy=DeliveryPolicy(required_evidence=("artifact-exists",)),
        default_topology="direct",
    )


def _write_source(
    root: Path,
    *,
    layer: RoleLayer,
    name: str,
    role_id: str,
    function_id: str,
    status: DefinitionStatus = DefinitionStatus.VALID,
    error: str | None = None,
) -> Path:
    path = root / layer.value / f"{name}.json"
    path.parent.mkdir(parents=True, exist_ok=True)
    role = RoleRef(id=role_id, function_id=function_id)
    source = RoleDefinitionSource(
        layer=layer,
        source_id=f"{layer.value}/{name}.json",
        snapshot_revision="snapshot-1",
        role=role,
        manifest=_manifest(role_id, function_id) if status is DefinitionStatus.VALID else None,
        status=status,
        error=error,
    )
    path.write_text(json.dumps(source.model_dump(mode="json")), encoding="utf-8")
    return path


def _snapshot_tree(root: Path) -> dict[str, bytes | None]:
    return {
        str(path.relative_to(root)): path.read_bytes() if path.is_file() else None
        for path in sorted(root.rglob("*"))
    }


def _assert_pretty_sorted_json(output: str) -> Any:
    parsed = json.loads(output)
    assert output == json.dumps(parsed, indent=2, sort_keys=True) + "\n"
    return parsed


def test_ac_r7_ui_resolve_is_typed_inert_and_has_stable_exit_codes(
    tmp_path: Path,
    monkeypatch,
) -> None:
    roles_root = tmp_path / "roles"
    _write_source(
        roles_root,
        layer=RoleLayer.BUILT_IN,
        name="owner",
        role_id="owner",
        function_id="marketing",
    )
    _write_source(
        roles_root,
        layer=RoleLayer.PLAN,
        name="unavailable",
        role_id="unavailable",
        function_id="support",
        status=DefinitionStatus.UNREADABLE,
        error="source unavailable; definition unchecked",
    )
    monkeypatch.setenv("FNO_ROLES_ROOT", str(roles_root))
    monkeypatch.setenv("FNO_SKIP_MIGRATION", "1")

    project_root = tmp_path / "redirected-project"
    graph_root = project_root / ".fno"
    claims_root = graph_root / "claims"
    (graph_root / "nested").mkdir(parents=True)
    claims_root.mkdir()
    seed_graph(graph_root / "graph.json", b'{"entries":[]}\n')
    (graph_root / "nested" / "receipt.bin").write_bytes(b"graph-sentinel\x00")
    (claims_root / "node.lock").write_bytes(b"claim-sentinel\n")
    before_graph = _snapshot_tree(graph_root)
    before_claims = _snapshot_tree(claims_root)
    monkeypatch.setenv("FNO_REPO_ROOT", str(project_root))
    monkeypatch.setenv("FNO_CLAIMS_ROOT", str(project_root))

    dispatch_calls: list[tuple[object, ...]] = []
    real = {"Popen": subprocess.Popen, "run": subprocess.run}

    def _tripwire(name: str):
        # Discovery is a read through the fno-agents spawn-overlay door; any
        # other process is a dispatch.
        def _guard(*args: object, **kwargs: object) -> object:
            argv = args[0] if args else kwargs.get("args")
            if isinstance(argv, list) and argv[1:2] == ["spawn-overlay"]:
                return real[name](*args, **kwargs)
            dispatch_calls.append((*args, kwargs))
            raise AssertionError("roles inspection must not dispatch a process")

        return _guard

    monkeypatch.setattr(subprocess, "Popen", _tripwire("Popen"))
    monkeypatch.setattr(subprocess, "run", _tripwire("run"))

    args = ["resolve", "owner", "--work-order", "x-a8c0", "--attempt", "attempt-1"]
    first = _invoke("agents", "roles", *args, "-J")
    second = _invoke("agents", "roles", *args, "-J")
    root_json = _invoke("-J", "agents", "roles", *args)
    assert first.exit_code == second.exit_code == root_json.exit_code == 0
    assert first.output == second.output == root_json.output
    resolved = _assert_pretty_sorted_json(first.output)
    assert resolved["role"] == {"function_id": "marketing", "id": "owner"}
    assert resolved["work_order"] == {
        "attempt_id": "attempt-1",
        "node_id": "x-a8c0",
        "principal_id": None,
        "role_id": "owner",
    }
    assert len(resolved["manifest_digest"]) == 64
    assert len(resolved["context_bundle"]["digest"]) == 64

    blocked_result = _invoke(
        "agents",
        "roles",
        "resolve",
        "unavailable",
        "--work-order",
        "x-a8c0",
        "--attempt",
        "attempt-1",
        "-J",
    )
    assert blocked_result.exit_code == 1
    blocked = _assert_pretty_sorted_json(blocked_result.output)
    assert blocked == {
        "detail": "source unavailable; definition unchecked",
        "reason": "invalid_manifest",
        "reference": "unavailable",
        "role": {"function_id": "support", "id": "unavailable"},
        "source_id": "plan/unavailable.json",
        "source_layer": "plan",
    }

    not_found_result = _invoke(
        "agents",
        "roles",
        "resolve",
        "missing",
        "--work-order",
        "x-a8c0",
        "--attempt",
        "attempt-1",
        "-J",
    )
    assert not_found_result.exit_code == 1
    not_found = _assert_pretty_sorted_json(not_found_result.output)
    assert not_found["reason"] == "not_found"
    assert not_found["role"] == {"function_id": "unavailable", "id": "missing"}

    malformed = _invoke("agents", "roles", "resolve", "owner")
    assert malformed.exit_code == 2
    assert dispatch_calls == []
    assert _snapshot_tree(graph_root) == before_graph
    assert _snapshot_tree(claims_root) == before_claims


def test_resolve_fails_closed_when_an_unchecked_source_has_no_role_identity(
    tmp_path: Path,
    monkeypatch,
) -> None:
    roles_root = tmp_path / "roles"
    _write_source(
        roles_root,
        layer=RoleLayer.BUILT_IN,
        name="owner",
        role_id="owner",
        function_id="marketing",
    )
    unchecked = roles_root / RoleLayer.PROJECT.value / "broken.json"
    unchecked.parent.mkdir(parents=True)
    unchecked.write_text("{not-json", encoding="utf-8")
    source_args: tuple[str, ...] = ()
    expected_source = "project/broken.json"
    monkeypatch.setenv("FNO_ROLES_ROOT", str(roles_root))
    monkeypatch.setenv("FNO_SKIP_MIGRATION", "1")

    resolved = _invoke(
        "agents",
        "roles",
        *source_args,
        "resolve",
        "owner",
        "--work-order",
        "x-a8c0",
        "--attempt",
        "attempt-1",
        "-J",
    )

    assert resolved.exit_code == 1
    blocked = _assert_pretty_sorted_json(resolved.output)
    assert blocked["reason"] == "invalid_manifest"
    assert blocked["source_layer"] == "project"
    assert blocked["source_id"] == expected_source
    assert blocked["detail"]
