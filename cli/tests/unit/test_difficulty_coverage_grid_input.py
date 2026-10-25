"""Acceptance tests for difficulty on every graph-node birth path."""
from __future__ import annotations

import pytest

from fno.graph.store import read_graph_strict
from tests.conftest import run_native_create
from tests.fixtures.graph_seed import seed_graph


@pytest.fixture
def tmp_graph(tmp_path, monkeypatch):
    graph = tmp_path / "graph.json"
    seed_graph(graph, '{"entries": []}\n')
    import fno.graph._constants as constants
    import fno.graph.store as store

    monkeypatch.setattr(constants, "GRAPH_JSON", graph)
    monkeypatch.setattr(constants, "GRAPH_MD", tmp_path / "graph.md")
    monkeypatch.setattr(constants, "GRAPH_HTML", tmp_path / "graph.html")
    monkeypatch.setattr(constants, "GRAPH_ARCHIVE_JSON", tmp_path / "graph-archive.json")
    monkeypatch.setattr(store, "GRAPH_JSON", graph)
    monkeypatch.setattr("fno.paths.graph_json", lambda: graph)
    monkeypatch.delenv("FNO_TRACKER_BACKEND", raising=False)
    return graph


def test_backlog_add_requires_difficulty_noninteractive(tmp_graph):
    result = run_native_create(
        tmp_graph, "add", "missing difficulty", auto_difficulty=False
    )

    assert result.exit_code == 2
    assert "non-interactive filing requires --difficulty" in (result.output + result.stderr)
    assert read_graph_strict(tmp_graph) == []


def test_backlog_new_writes_difficulty(tmp_graph):
    result = run_native_create(tmp_graph, "new", "mail-born work", "--difficulty", "high")

    assert result.exit_code == 0, result.stderr
    row = read_graph_strict(tmp_graph)[0]
    assert row["difficulty"] == "high"
    assert row["difficulty_history"][-1]["source"] == "filed"


def test_no_difficulty_dispatch_receipt_is_named():
    from fno.route_resolve import resolve_dispatch_model

    model, source, chain = resolve_dispatch_model()

    assert model is None
    assert source == "provider-default(no-difficulty)"
    assert chain == ["provider-default(no-difficulty)"]
