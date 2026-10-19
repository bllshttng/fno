"""AC5-HP (x-baef): the one-shot model_tier -> difficulty graph migration.

Dry-run by default, --apply to write, idempotent on the second run. Same-band
both-spellings pairs drain (the retired --model-tier handler wrote both keys
with equal bands, so those are the machine-created shape); a DIVERGENT pair
or a garbage band with no canonical field is refused, never guessed at.
"""
from __future__ import annotations
from tests.fixtures.graph_seed import seed_graph
from tests.goldens._door import native_backlog

import json
from pathlib import Path

import pytest

from fno.graph.store import read_graph_strict


@pytest.fixture
def tmp_graph(tmp_path, monkeypatch) -> Path:
    """A fresh empty graph.json; same patch surface as the integration fixture."""
    g = tmp_path / "graph.json"
    seed_graph(g, '{"entries": []}\n')
    import fno.graph._constants as gc
    import fno.graph.store as gs
    monkeypatch.setattr(gc, "GRAPH_JSON", g)
    monkeypatch.setattr(gc, "GRAPH_MD", tmp_path / "graph.md")
    monkeypatch.setattr(gc, "GRAPH_HTML", tmp_path / "graph.html")
    monkeypatch.setattr(gc, "GRAPH_ARCHIVE_JSON", tmp_path / "graph-archive.json")
    monkeypatch.setattr(gs, "GRAPH_JSON", g)
    monkeypatch.setattr("fno.paths.graph_json", lambda: g)
    return g


def _seed(g: Path, n: int) -> list[str]:
    entries = [
        {"id": f"x-{i:04x}", "title": f"legacy {i}", "model_tier": "high" if i % 2 else "medium"}
        for i in range(n)
    ]
    seed_graph(g, json.dumps({"entries": entries}))
    return [e["id"] for e in entries]


def test_migrate_difficulty_dry_run_apply_then_empty(tmp_graph):
    ids = _seed(tmp_graph, 21)

    r = native_backlog("backlog", "migrate-difficulty")
    assert r.exit_code == 0, r.output
    receipt = json.loads(r.output)
    assert sorted(receipt["candidates"]) == sorted(ids)
    assert receipt["candidate_count"] == 21
    assert receipt["apply"] is False
    for e in read_graph_strict(tmp_graph):
        assert "difficulty" not in e, "dry-run must not write"

    r2 = native_backlog("backlog", "migrate-difficulty", "--apply")
    assert r2.exit_code == 0, r2.output
    entries = {e["id"]: e for e in read_graph_strict(tmp_graph)}
    for i, nid in enumerate(ids):
        band = "high" if i % 2 else "medium"
        row = entries[nid]
        assert row["difficulty"] == band
        assert "model_tier" not in row
        hist = row["difficulty_history"]
        assert hist[-1]["value"] == band
        assert hist[-1]["source"] == "migration"
        assert hist[-1].get("ts")

    r3 = native_backlog("backlog", "migrate-difficulty")
    assert r3.exit_code == 0, r3.output
    receipt3 = json.loads(r3.output)
    assert receipt3["candidate_count"] == 0
    assert receipt3["candidates"] == []


def test_migrate_difficulty_refuses_divergent_pairs(tmp_graph):
    seed_graph(tmp_graph, json.dumps({"entries": [
        {"id": "x-0001", "model_tier": "high", "difficulty": "low"},
    ]}))
    for argv in (
        ["backlog", "migrate-difficulty"],
        ["backlog", "migrate-difficulty", "--apply"],
    ):
        r = native_backlog(*argv)
        assert r.exit_code == 2, r.output
        assert "x-0001" in r.output
    row = read_graph_strict(tmp_graph)[0]
    assert row["model_tier"] == "high" and row["difficulty"] == "low"
