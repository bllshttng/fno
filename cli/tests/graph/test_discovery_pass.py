"""Read-only discovery pass tests.

The pass is deliberately tested through both its small Python seam and the
backlog CLI.  The graph bytes are the mutation oracle: a discovery run may
build a disposable FTS cache, but it must never rewrite graph state.
"""
from __future__ import annotations
from tests.fixtures.graph_seed import seed_graph

import json
from pathlib import Path

import pytest

from fno.graph import discovery


def _node(node_id: str, **overrides: object) -> dict:
    base = {
        "id": node_id,
        "title": f"node {node_id}",
        "slug": f"node-{node_id}",
        "project": "fno",
        "type": "feature",
        "parent": None,
        "priority": "p2",
        "status": "ready",
        "blocked_by": [],
        "completed_at": None,
        "pr_number": None,
        "pr_url": None,
        "children": [],
    }
    base.update(overrides)
    return base


@pytest.fixture()
def tmp_graph(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> Path:
    from fno.graph import _constants as constants
    from fno.graph import store

    graph = tmp_path / "graph.json"
    monkeypatch.setattr(constants, "GRAPH_JSON", graph)
    monkeypatch.setattr(constants, "GRAPH_ARCHIVE_JSON", tmp_path / "archive.json")
    monkeypatch.setattr(store, "GRAPH_JSON", graph)
    monkeypatch.setattr("fno.paths.graph_json", lambda: graph)
    monkeypatch.setattr("fno.graph._constants._graph_archive_json", lambda: tmp_path / "archive.json")
    return graph


def _seed(graph: Path, entries: list[dict]) -> None:
    seed_graph(graph, json.dumps({"entries": entries}) + "\n")


def test_candidates_union_recall_lanes(tmp_graph: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    _seed(tmp_graph, [_node("x-fts"), _node("x-related"), _node("x-both")])
    monkeypatch.setattr(discovery, "_fts_search", lambda *args, **kwargs: ["x-fts", "x-both"])
    monkeypatch.setattr(
        "fno.graph.relatedness.similar_nodes",
        lambda *args, **kwargs: [
            ("x-related", 0.7, "related terms"),
            ("x-both", 0.4, "shared terms"),
        ],
    )

    result = discovery.candidates("candidate", "details", graph_path=tmp_graph)

    assert [candidate.node_id for candidate in result] == ["x-related", "x-both", "x-fts"]
    assert result[0].lanes == frozenset({"relatedness"})
    assert result[1].lanes == frozenset({"fts", "relatedness"})
    assert result[2].lanes == frozenset({"fts"})
    assert result[2].score == 0.1


def test_candidates_degrade_when_fts_is_unavailable(
    tmp_graph: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    _seed(tmp_graph, [_node("x-related")])

    def unavailable(*args, **kwargs):
        raise discovery.SearchUnavailableError("no fts5")

    monkeypatch.setattr(discovery, "_fts_search", unavailable)
    monkeypatch.setattr(
        "fno.graph.relatedness.similar_nodes",
        lambda *args, **kwargs: [("x-related", 0.4, "shared terms")],
    )

    result = discovery.candidates("candidate", "details", graph_path=tmp_graph)

    assert result.degraded is True
    assert result.warning == "no fts5"
    assert result[0].lanes == frozenset({"relatedness"})
