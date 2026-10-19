"""`fno backlog update --status/--set`: the patch door, end to end (x-665f).

Every test drives the real CLI, which forwards to the native backlog-update
action, which writes through the real store - never a stubbed receipt
asserted against its own renderer. The lifecycle verbs are transports over
the same door, so the undefer/unsupersede contracts live here too.

Filter: ``fno doctor test cli/tests/unit/test_backlog_update_status.py``
"""
from __future__ import annotations

from pathlib import Path

import pytest

from fno.graph.store import commit_rows_via_store, read_graph_strict
from tests.conftest import run_native_create

pytestmark = pytest.mark.usefixtures("native_backlog_door")


def _node(node_id: str, **overrides) -> dict:
    base = {
        "id": node_id,
        "slug": f"slug-{node_id}",
        "title": "sample node",
        "project": "fno",
        "type": "feature",
        "parent": None,
        "priority": "p2",
        "status": "idea",
        "blocked_by": [],
        "completed_at": None,
        "deferred_at": None,
        "pr_number": None,
        "pr_url": None,
        "children": [],
    }
    base.update(overrides)
    return base


@pytest.fixture()
def tmp_graph(tmp_path, monkeypatch):
    from fno.graph import cli as graph_cli

    g = tmp_path / "graph.json"
    commit_rows_via_store(g, lambda entries: entries)
    monkeypatch.setattr(graph_cli, "_graph_path", lambda: g)
    return g


def _seed(g: Path, *nodes: dict) -> None:
    commit_rows_via_store(g, lambda entries: entries + list(nodes))


def _entry(g: Path, node_id: str) -> dict:
    return next(e for e in read_graph_strict(g) if e["id"] == node_id)


# ---------------------------------------------------------------------------
# --status: the readback contract
# ---------------------------------------------------------------------------


def test_undefer_a_still_superseded_node_refuses_naming_the_door(tmp_graph):
    """AC5-HP: the false-receipt shape. A node carrying both facts cannot be
    undeferred into a lie; the refusal names the route that revives it."""
    _seed(
        tmp_graph,
        _node(
            "x-3873",
            status="superseded",
            superseded_by="x-aaaa",
            deferred_at="2026-01-01T00:00:00+00:00",
            deferred_reason="stale",
        ),
        _node("x-aaaa", supersedes=["x-3873"], status="ready"),
    )

    r = run_native_create(tmp_graph, "undefer", "x-3873")

    assert r.exit_code == 2, r.output + r.stderr
    assert "fno backlog update x-3873 --status idea" in (r.output + r.stderr)
    assert "Undeferred" not in (r.output + r.stderr)
    node = _entry(tmp_graph, "x-3873")
    assert node["deferred_at"] == "2026-01-01T00:00:00+00:00"
    assert node["status"] == "superseded"


def test_unsupersede_lands_on_the_surviving_park(tmp_graph):
    """AC6-HP: leaving superseded keeps the deferral; the node reads deferred."""
    _seed(
        tmp_graph,
        _node(
            "x-0005",
            status="superseded",
            superseded_by="x-0004",
            supersession={"cause": "moved on"},
            deferred_at="2026-01-01T00:00:00+00:00",
            deferred_reason="waiting",
        ),
        _node("x-0004", supersedes=["x-0005"], status="ready"),
    )

    r = run_native_create(tmp_graph, "unsupersede", "x-0005")

    assert r.exit_code == 0, r.output + r.stderr
    assert "Unsuperseded x-0005" in (r.output + r.stderr)
    assert "x-0004" in (r.output + r.stderr)
    node = _entry(tmp_graph, "x-0005")
    assert node["superseded_by"] is None
    assert node["deferred_at"] == "2026-01-01T00:00:00+00:00"
    assert node["status"] == "deferred"
    assert _entry(tmp_graph, "x-0004")["supersedes"] == []


def test_defer_and_retract_land_the_same_rows_through_the_door(tmp_graph):
    """AC7-HP: a locked node defers cleanly (lock facts clear); retract is
    defer with the kind forced; both routes share the door's validator set."""
    _seed(tmp_graph, _node("x-0003", status="ready", locked_by="sess-1", locked_at="2026-01-01T00:00:00+00:00"))

    r = run_native_create(tmp_graph, "defer", "x-0003", "--reason", "waiting on x")
    assert r.exit_code == 0, r.output + r.stderr
    node = _entry(tmp_graph, "x-0003")
    assert node["locked_by"] is None
    assert node["locked_at"] is None
    assert node["deferred_reason"] == "waiting on x"
    assert node["status"] == "deferred"

    r = run_native_create(tmp_graph, "defer", "x-0003", "--reason", "again")
    assert r.exit_code == 0, r.output + r.stderr
    assert _entry(tmp_graph, "x-0003")["deferred_reason"] == "again"

    r = run_native_create(tmp_graph, "retract", "x-0003", "filed on a false premise")
    assert r.exit_code == 0, r.output + r.stderr
    node = _entry(tmp_graph, "x-0003")
    assert node["deferred_kind"] == "retracted"
    assert node["status"] == "deferred"
