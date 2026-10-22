"""`fno backlog unarchive` - the inverse of the archive sweep.

Under the single store, archive residents are rows carrying ``archived_at``
in the same graph.db the working graph lives in. Unarchive clears the stamp
and returns the row to the working population; the round-trip with `archive`
proves neither command can drop a node from both populations.
"""
from __future__ import annotations
from tests.fixtures.graph_seed import seed_graph
from tests.goldens._door import native_backlog

import json
from pathlib import Path

import pytest

from fno.graph.store import read_graph_strict


@pytest.fixture
def store(tmp_path, monkeypatch) -> Path:
    working = tmp_path / "graph.json"
    working.write_text('{"entries": []}\n')
    import fno.graph._constants as gc
    import fno.graph.store as gs

    monkeypatch.setattr(gc, "GRAPH_JSON", working)
    monkeypatch.setattr(gc, "GRAPH_MD", tmp_path / "graph.md")
    monkeypatch.setattr(gs, "GRAPH_JSON", working)
    # The command's archive read resolves through paths, not the constant.
    from fno import paths

    monkeypatch.setattr(paths, "graph_json", lambda: working)
    monkeypatch.delenv("CLAUDECODE_SESSION_ID", raising=False)
    return working


def _node(nid: str, **over) -> dict:
    base = {
        "id": nid,
        "title": f"node {nid}",
        "slug": nid,
        "type": "feature",
        "status": "done",
        "completed_at": "2026-01-01T00:00:00+00:00",
        "domain": "code",
        "priority": "p2",
        "created_at": "2025-12-01T00:00:00+00:00",
    }
    base.update(over)
    return base


def _seed(graph: Path, entries: list[dict]) -> None:
    seed_graph(graph, json.dumps({"entries": entries}) + "\n")


def _rows(graph: Path) -> list[dict]:
    return read_graph_strict(graph)


def _live_ids(graph: Path) -> set[str]:
    return {e["id"] for e in _rows(graph) if not e.get("archived_at")}


def _archived_ids(graph: Path) -> set[str]:
    # The default read hides archive residents; the archive read names them.
    from fno.graph.store import read_archive_entries

    return {e["id"] for e in read_archive_entries(graph)}


def test_a_reminted_node_comes_back_by_its_previous_id(store):
    """A dedupe remint keeps the old id as previous_id; `get` resolves it that
    way, so `unarchive` - the remedy the dedupe command names - must too."""
    _seed(
        store,
        [_node("ab-99999999", previous_id="ab-22222222", archived_at="2026-02-01T00:00:00Z")],
    )
    res = native_backlog("backlog", "unarchive", "ab-22222222")
    assert res.exit_code == 0, res.output
    assert "ab-99999999" in _live_ids(store)
    assert "ab-99999999" not in _archived_ids(store)


def test_a_node_in_neither_population_is_an_error(store):
    res = native_backlog("backlog", "unarchive", "ab-99999999")
    assert res.exit_code == 1
    assert "neither" in res.output


def test_archive_then_unarchive_round_trips(store):
    """The pair, end to end: no window where the node is in neither population."""
    old = _node("ab-22222222", completed_at="2025-01-01T00:00:00+00:00")
    _seed(store, [old])
    res = native_backlog("backlog", "archive", "--apply")
    assert res.exit_code == 0, res.output
    assert _live_ids(store) == set()
    assert _archived_ids(store) == {"ab-22222222"}

    res = native_backlog("backlog", "unarchive", "ab-22222222")
    assert res.exit_code == 0, res.output
    assert _live_ids(store) == {"ab-22222222"}
    assert _archived_ids(store) == set()
