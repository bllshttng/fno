"""Door tests for `fno backlog roadmap`: the public filter and its refusals."""
from __future__ import annotations
from tests.fixtures.graph_seed import seed_graph
from tests.goldens._door import native_backlog

import json
from pathlib import Path

import pytest


@pytest.fixture
def tmp_graph(tmp_path, monkeypatch) -> Path:
    g = tmp_path / "graph.json"
    import fno.graph._constants as gc
    import fno.graph.store as gs
    monkeypatch.setattr(gc, "GRAPH_JSON", g)
    monkeypatch.setattr(gc, "GRAPH_MD", tmp_path / "graph.md")
    monkeypatch.setattr(gs, "GRAPH_JSON", g)
    # Seam readers (guarded metadata/display reads) resolve paths.graph_json
    # at call time; pin the resolver to the same hermetic file.
    monkeypatch.setattr("fno.paths.graph_json", lambda: g)
    return g


def _seed(g: Path, entries: list[dict]) -> None:
    seed_graph(g, json.dumps({"entries": entries}, indent=2) + "\n")


# -- ready: slug leads -------------------------------------------------------


def test_roadmap_only_public_no_leaks(tmp_graph):
    _seed(tmp_graph, [
        {"id": "ab-11111111", "title": "Public feature", "slug": "pub", "status": "ready",
         "priority": "p1", "size": "M", "project": "fno", "public": True,
         "plan_path": "internal/fno/plans/secret.md", "cwd": "/private/x"},
        {"id": "ab-22222222", "title": "Private thing", "slug": "priv", "status": "ready",
         "priority": "p2", "project": "fno"},  # absent public flag -> included
        {"id": "ab-22222223", "title": "Explicitly private", "slug": "private", "status": "ready",
         "priority": "p2", "project": "fno", "public": False},
        {"id": "ab-33333333", "title": "Other project pub", "slug": "op", "status": "ready",
         "priority": "p1", "project": "other", "public": True},  # wrong project -> excluded
    ])
    result = native_backlog("backlog", "roadmap", "--project", "fno")
    assert result.exit_code == 0, result.output
    out = result.stdout
    assert "Public feature" in out
    assert "Private thing" in out
    assert "Explicitly private" not in out
    assert "Other project pub" not in out
    # No internal fields leak.
    assert "ab-11111111" not in out
    assert "secret.md" not in out
    assert "/private/x" not in out
    # Grouped under the Now column (p1).
    assert "## Now" in out


def test_roadmap_refuses_stale_local_graph_under_external_tracker(tmp_graph):
    _seed(tmp_graph, [
        {"id": "ab-local001", "title": "STALE-LOCAL-MARKER", "status": "ready",
         "priority": "p1", "project": "fno"},
    ])

    result = native_backlog(
        "backlog", "roadmap", "--project", "fno",
        env_override={"FNO_TRACKER_BACKEND": "github"},
    )

    assert result.exit_code == 2
    assert "external tracker" in result.output
    assert "STALE-LOCAL-MARKER" not in result.output


def test_roadmap_refuses_corrupt_live_graph_even_with_healthy_archive(tmp_path, monkeypatch):
    graph = tmp_path / "graph.json"
    graph.with_suffix(".db").write_text("{broken", encoding="utf-8")
    monkeypatch.setattr("fno.paths.graph_json", lambda: graph)
    (tmp_path / "graph-archive.json").write_text(
        json.dumps({"entries": [
            {"id": "ab-archive1", "title": "ARCHIVE-ONLY-SUCCESS-MARKER",
             "status": "done", "priority": "p2", "project": "fno"},
        ]}),
        encoding="utf-8",
    )

    result = native_backlog("backlog", "roadmap", "--project", "fno")

    assert result.exit_code != 0
    assert "canonical graph read failed" in result.output
    assert "ARCHIVE-ONLY-SUCCESS-MARKER" not in result.output
