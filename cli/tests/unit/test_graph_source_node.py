"""CLI-level tests for the explicit ``--source-node`` origin flag (x-d157).

The unit-level precedence chain lives in ``test_graph_provenance.py``; this file
covers the verb surface, where the fail-closed contract actually bites: an
assertion that does not resolve must refuse the command rather than file a node
that looks organically captured.
"""
from __future__ import annotations
from tests.fixtures.graph_seed import seed_graph

import json
from pathlib import Path

import pytest

from fno.graph.store import read_graph_strict
from tests.conftest import run_native_create


@pytest.fixture
def tmp_graph(tmp_path, monkeypatch) -> Path:
    g = tmp_path / "graph.json"
    seed_graph(g, json.dumps(
            {
                "entries": [
                    {
                        "id": "x-aaaa",
                        "title": "The origin node",
                        "_status": "ready",
                        "domain": "code",
                        "project": "fno",
                        "slug": "the-origin-node",
                    }
                ]
            },
            indent=2,
        )
        + "\n")
    import fno.graph._constants as gc
    import fno.graph.store as gs

    monkeypatch.setattr(gc, "GRAPH_JSON", g)
    monkeypatch.setattr(gc, "GRAPH_MD", tmp_path / "graph.md")
    monkeypatch.setattr(gs, "GRAPH_JSON", g)
    # Ambient capture must not colour these assertions: the flag is the subject.
    for var in ("FNO_NODE", "CLAUDE_CODE_SESSION_ID", "CODEX_THREAD_ID",
                "CODEX_SESSION_ID", "GEMINI_SESSION_ID"):
        monkeypatch.delenv(var, raising=False)
    return g


def _entries(g: Path) -> list[dict]:
    return read_graph_strict(g)


def _by_id(g: Path, node_id: str) -> dict:
    return next(e for e in _entries(g) if e["id"] == node_id)


def test_ac1_hp_explicit_source_node_stamps_the_origin(tmp_graph):
    """AC1-HP: --source-node on a filing verb stamps exactly that origin."""
    result = run_native_create(tmp_graph, "idea", "follow-up", "--source-node", "x-aaaa", "--difficulty", "low")
    assert result.exit_code == 0, result.output
    new_id = json.loads(result.stdout)["id"]
    assert _by_id(tmp_graph, new_id)["source_node_id"] == "x-aaaa"


def test_ac1_err_unresolvable_source_node_refuses_and_writes_nothing(tmp_graph):
    """AC1-ERR: fail closed - non-zero, the token named, and no node created."""
    before = len(_entries(tmp_graph))
    result = run_native_create(tmp_graph, "idea", "follow-up", "--source-node", "x-zzzz", "--difficulty", "low")
    assert result.exit_code != 0
    assert "x-zzzz" in (result.output + result.stderr)
    assert len(_entries(tmp_graph)) == before


def test_ac3_edge_stale_env_origin_degrades_through_the_real_filing_path(
    tmp_graph, monkeypatch
):
    """AC3-EDGE: FNO_NODE naming a node absent from the graph stamps null.

    Exercised end-to-end rather than against _session_provenance directly,
    because the live-snapshot validation only has a snapshot to check against
    inside the locked mutator.
    """
    monkeypatch.setenv("CLAUDE_CODE_SESSION_ID", "sess-stale")
    monkeypatch.setenv("FNO_NODE", "x-deleted")

    result = run_native_create(tmp_graph, "idea", "orphaned follow-up", "--difficulty", "low")
    assert result.exit_code == 0, result.output
    node = _by_id(tmp_graph, json.loads(result.stdout)["id"])
    assert node["source_node_id"] is None
    assert "x-deleted" not in json.dumps(node)
