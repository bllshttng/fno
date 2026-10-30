from __future__ import annotations
from tests.fixtures.graph_seed import seed_graph

import json
from pathlib import Path

import pytest
from typer.testing import CliRunner

from fno.company.contracts import CompanyWorkRefs, FunctionRef, RoleRef, WorkOrderRef
from fno.plan.cli import plan_app
from fno.graph.store import read_graph_strict


runner = CliRunner()


def _refs() -> CompanyWorkRefs:
    return CompanyWorkRefs(
        function=FunctionRef(id="operations"),
        role=RoleRef(id="operations-lead", function_id="operations"),
        work_order=WorkOrderRef(
            node_id="x-e9a3",
            attempt_id="attempt-1",
            role_id="operations-lead",
        ),
    )


@pytest.mark.parametrize(("work_order_node", "exit_code"), [("x-other", 1)])
def test_plan_validate_checks_company_work_from_markdown(
    tmp_path: Path, work_order_node: str, exit_code: int
) -> None:
    plan = tmp_path / "company-plan.md"
    plan.write_text(
        "---\n"
        "node: x-e9a3\n"
        "status: ready\n"
        "created: 2026-08-02\n"
        "company_work:\n"
        "  work_order:\n"
        f"    node_id: {work_order_node}\n"
        "    attempt_id: attempt-1\n"
        "---\n"
        "# Company plan\n"
    )

    result = runner.invoke(plan_app, ["validate", str(plan)])

    assert result.exit_code == exit_code
    if exit_code == 1:
        assert "must match plan node" in result.output


def _outer_identity_mismatch() -> dict:
    company_work = _refs().model_dump(mode="json")
    company_work["work_order"]["node_id"] = "x-other"
    return company_work


def _contradictory_backlinks() -> dict:
    return {
        "work_order": {"node_id": "x-owner", "attempt_id": "attempt-1"},
        "deliverables": [
            {
                "id": "d1",
                "kind": "artifact",
                "work_order_id": "x-owner",
                "attempt_id": "attempt-1",
                "effect_id": "e1",
            },
            {
                "id": "d2",
                "kind": "artifact",
                "work_order_id": "x-owner",
                "attempt_id": "attempt-1",
            },
        ],
        "effects": [
            {
                "id": "e1",
                "work_order_id": "x-owner",
                "attempt_id": "attempt-1",
                "deliverable_id": "d2",
                "effect_class": "external-communication",
                "destination": "helpdesk://ticket/42",
            }
        ],
    }


@pytest.mark.parametrize(
    ("company_work", "error"),
    [
        (_outer_identity_mismatch(), "must match graph entry id"),
        (_contradictory_backlinks(), "conflicting deliverable"),
    ],
)
def test_graph_store_rejects_invalid_company_work_without_writing(
    tmp_path, company_work: dict, error: str
) -> None:
    from fno.graph.store import commit_rows_via_store

    graph = tmp_path / "graph.json"
    seed_graph(graph, [{"id": "x-owner"}])
    from fno.graph.store import read_graph_strict, store_export_status

    original = (read_graph_strict(graph), store_export_status(graph))

    def attach_invalid_company_work(entries: list[dict]) -> list[dict]:
        entries[0]["company_work"] = company_work
        return entries

    with pytest.raises(ValueError, match=error):
        commit_rows_via_store(graph, attach_invalid_company_work)

    assert (read_graph_strict(graph), store_export_status(graph)) == original


def test_graph_store_persists_valid_company_work_and_unknown_fields(tmp_path: Path) -> None:
    from fno.graph.store import commit_rows_via_store

    graph = tmp_path / "graph.json"
    seed_graph(graph, json.dumps(
            {
                "entries": [
                    {
                        "id": "x-e9a3",
                        "company_work": _refs().model_dump(mode="json"),
                        "future_graph_field": {"kept": True},
                    }
                ]
            }
        )
        + "\n")

    commit_rows_via_store(graph, lambda entries: entries)

    saved = read_graph_strict(graph)[0]
    assert saved["company_work"]["work_order"]["node_id"] == "x-e9a3"
    assert saved["future_graph_field"] == {"kept": True}
