"""Per-reader-class sentinel tests for consumers routed through the seam.

The migration standard (plan task 2.1): a consumer that reads ONLY
seam-owned fields stops touching the graph file. Each test here gives the
tracker side and the sidecar side DIFFERENT sentinel values than the graph
file carries, so a pass is positive evidence the reader went through the
seam - an assertion that ``read_graph`` text disappeared is not evidence
(plan Risk 3 / the lead's census requirement).
"""

from __future__ import annotations
from tests.fixtures.graph_seed import seed_graph

import json
from pathlib import Path

import pytest


@pytest.fixture
def external_store(tmp_path, monkeypatch):
    """External mode with per-id sidecar files; returns the sidecar dir."""
    monkeypatch.setenv("FNO_TRACKER_BACKEND", "github")
    sidecars = tmp_path / "sidecars"
    sidecars.mkdir()
    import fno.tracker.sidecar as sidecar_store

    monkeypatch.setattr(sidecar_store, "sidecar_path", lambda i: sidecars / f"{i}.json")
    return sidecars


@pytest.fixture
def contradictory_graph(tmp_path, monkeypatch):
    """A graph file whose every value CONTRADICTS the sidecar sentinels."""
    g = tmp_path / "graph.json"
    seed_graph(
        g,
        json.dumps(
            {
                "entries": [
                    {
                        "id": "n-000001",
                        "title": "n-000001",
                        "pr_number": 999,
                        "pr_url": "https://graph/999",
                        "cwd": "/graph-cwd",
                        "size": "GRAPH-SIZE",
                        "source_cwd": "/graph-src",
                        "slug": "graph-slug-1",
                        "type": "epic",
                        "priority": "p2",
                        "status": "idea",
                        "project": "graph-proj",
                        "mission_active": True,
                        "session_id": "sess-graph",
                    },
                    {
                        "id": "n-000002",
                        "pr_number": 999,
                        "pr_url": "https://graph/999b",
                        "cwd": "/graph-cwd",
                        "size": None,
                        "type": "feature",
                        "priority": "p2",
                        "status": "idea",
                        "slug": "graph-slug-2",
                        "title": "n-000002",
                        "mission_active": False,
                    },
                ]
            }
        ),
    )
    # Patch the resolver, not _constants.GRAPH_JSON: monkeypatch teardown
    # concretizes that lazy attr, and a frozen value would poison every later
    # paths.graph_json redirect in the same process (sidecar._graph_store_path
    # reads the resolver directly for exactly this reason).
    monkeypatch.setattr("fno.paths.graph_json", lambda: g)
    return g


def _write_sidecar(sidecars: Path, id: str, **fields) -> None:
    payload = {"id": id}
    payload.update(fields)
    (sidecars / f"{id}.json").write_text(json.dumps(payload), encoding="utf-8")


def test_pr_scan_class_reads_sidecar_not_graph(external_store, contradictory_graph):
    """Reader class: scan-by-PR (mail job addressing, pr merge node resolve)."""
    _write_sidecar(external_store, "n-000001", pr_number=7, pr_url="https://ext/7", cwd="/ext-cwd")
    _write_sidecar(
        external_store, "n-000002", additional_prs=[{"number": 7, "url": "https://ext/7b"}]
    )
    _write_sidecar(external_store, "N-3", pr_number=None)

    from fno.mail.job_address import _node_ids_for_pr

    ids = _node_ids_for_pr(7)
    # Both PR-bearing sidecars found (primary + additional); the graph's 999s
    # are invisible, and a PR the store does not carry resolves to nothing.
    assert set(ids) == {"n-000001", "n-000002"}
    assert _node_ids_for_pr(999) == []


def test_metadata_class_guards_external(external_store, contradictory_graph):
    """Reader class: footnote-minted metadata (slug / type / project /
    mission_active / session pins) raises under an external backend and every
    caller keeps its missing-data path - never a stale graph row."""
    from fno.tracker import metadata

    from fno.agents.role import _graph_entry

    def _active_missions():
        # The fail-safe shape every mission reader keeps: a guarded read that
        # raised under the external backend reads as no missions.
        try:
            rows = metadata.read_entries("active_backlog")
        except Exception:  # noqa: BLE001 - the guard IS the behavior under test
            return []
        return [e for e in rows if isinstance(e, dict) and e.get("mission_active") is True]

    assert _active_missions() == []
    assert _graph_entry("n-000001") is None


def test_graph_mode_scans_project_from_the_store(tmp_path, monkeypatch):
    """Graph-mode parity for the scan class: the sidecar projection reads the
    footnote-owned fields out of the graph entry, so the same callers keep
    finding what they found before the migration."""
    monkeypatch.delenv("FNO_TRACKER_BACKEND", raising=False)
    g = tmp_path / "graph.json"
    seed_graph(
        g,
        json.dumps(
            {
                "entries": [
                    {
                        "id": "ab-00000001",
                        "slug": "ab-00000001",
                        "title": "ab-00000001",
                        "type": "feature",
                        "priority": "p2",
                        "status": "idea",
                        "pr_number": 42,
                        "cwd": "/repo",
                        "size": "L",
                    },
                    {
                        "id": "ab-00000002",
                        "slug": "ab-00000002",
                        "title": "ab-00000002",
                        "type": "feature",
                        "priority": "p2",
                        "status": "idea",
                    },
                ]
            }
        ),
    )
    monkeypatch.setattr("fno.paths.graph_json", lambda: g)

    from fno.mail.job_address import _node_ids_for_pr
    from fno.tracker import sidecar as sidecar_store

    assert _node_ids_for_pr(42) == ["ab-00000001"]
    all_loaded = sidecar_store.load_all()
    assert all_loaded["ab-00000001"].cwd == "/repo"
    # The size pin is footnote-minted metadata, NOT a sidecar field: the
    # projection must not carry it even though the entry does.
    assert "size" not in type(all_loaded["ab-00000001"]).model_fields
    assert all_loaded["ab-00000002"].pr_number is None
