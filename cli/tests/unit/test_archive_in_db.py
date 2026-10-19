"""Archive residency lives in the same store as the working graph: default
reads filter archived rows and unarchive clears the stamp. The Python seam
runs against a real keeper on a temp graph; the operator's live graph and
config are never touched."""

from __future__ import annotations

import sqlite3
from pathlib import Path

import pytest

from tests.fixtures.graph_seed import seed_graph

FULL = {
    "type": "feature",
    "status": "idea",
    "priority": "p2",
    "domain": "code",
    "created_at": "2026-09-11T00:00:00+00:00",
}


def _row(node_id: str, **overrides):
    return {
        **FULL,
        "id": node_id,
        "slug": node_id,
        "title": node_id,
        "tags": [],
        **overrides,
    }


def _archived_at(graph: Path, node_id: str):
    with sqlite3.connect(graph.with_suffix(".db")) as connection:
        row = connection.execute(
            "SELECT archived_at FROM nodes WHERE id = ?", (node_id,)
        ).fetchone()
    return None if row is None else row[0]


def _seed(graph: Path, *rows) -> None:
    seed_graph(graph, rows)


@pytest.fixture
def world(tmp_path, monkeypatch):
    """A temp machine: graph + state root. Skips where no keeper binary
    can spawn."""
    graph = tmp_path / "graph.json"
    _seed(
        graph,
        _row(
            "x-old",
            status="done",
            completed_at="2026-06-01T00:00:00Z",
            title="old done",
        ),
        _row("x-live", title="open work"),
    )
    monkeypatch.setattr("fno.paths.graph_json", lambda: graph)
    monkeypatch.setattr("fno.paths.state_dir", lambda: tmp_path)
    return {"graph": graph, "tmp": tmp_path}


def _sweep(world) -> None:
    from tests.goldens._door import door_graph

    code, out, err = door_graph(
        world["graph"], "archive", "--apply", "--older-than-days", "30"
    )
    assert code == 0, out + err


def test_default_reads_drop_archived_include_archived_keeps_them(world):
    from fno.graph.api import wire_rows

    _sweep(world)
    live = {r["id"] for r in wire_rows(path=world["graph"])}
    everything = {r["id"] for r in wire_rows(path=world["graph"], include_archived=True)}
    assert live == {"x-live"}
    assert everything == {"x-live", "x-old"}


def test_unarchive_clears_the_stamp(world):
    from fno.graph.api import unarchive_node, wire_rows

    _sweep(world)
    payload = unarchive_node("x-old", path=world["graph"])
    assert payload.success
    assert _archived_at(world["graph"], "x-old") is None, "the stamp is gone, not blanked"
    assert "x-old" in {r["id"] for r in wire_rows(path=world["graph"])}
