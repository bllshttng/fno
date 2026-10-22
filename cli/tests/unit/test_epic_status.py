"""Tests for `fno backlog epic status` - the native children-rollup door.

The command answers from the binary, so these exec it over a fixture store the
way the other native-door suites do (HOME/FNO_STATE_DIR pinned at the seed's
parent); they skip where this checkout has no dev build.
"""
from __future__ import annotations
from tests._native_door import run_native
from tests.fixtures.graph_seed import seed_graph

import json
from pathlib import Path

import pytest


def _seed(tmp_path: Path, entries: list[dict]) -> None:
    complete = []
    for e in entries:
        row = {"type": "feature", "priority": "p2", "status": "idea", **e}
        row.setdefault("title", e.get("id", "node"))
        row.setdefault("slug", e.get("id", "node"))
        complete.append(row)
    seed_graph(tmp_path / "graph.json", json.dumps({"entries": complete}) + "\n")


@pytest.fixture
def epic_fixture(tmp_path, monkeypatch, _plan_hermetic_events_journal) -> Path:
    # Journal isolation is twofold: the conftest pin sets FNO_EVENTS_PATH to
    # this test's tmp journal, and the binary's own reads sit under the tmp
    # HOME this fixture exports (the global mirror is HOME/.fno/events.jsonl,
    # absent here; child journals live under the seeded cwd).
    monkeypatch.setenv("HOME", str(tmp_path))
    monkeypatch.setenv("FNO_STATE_DIR", str(tmp_path))
    journal = tmp_path / "wt" / ".fno"
    journal.mkdir(parents=True)
    (journal / "events.jsonl").write_text(
        json.dumps({
            "ts": "2026-10-01T00:00:00Z",
            "type": "advance_dispatched",
            "data": {"node_id": "ab-ch1d0001", "agent_name": "archer"},
        })
        + "\n",
        encoding="utf-8",
    )
    _seed(
        tmp_path,
        [
            {
                "id": "ab-ep1c0001",
                "type": "epic",
                "title": "The mission",
                "status": "idea",
                "slug": "mission",
                "created_at": "2026-09-01T00:00:00Z",
            },
            {
                "id": "ab-ch1d0001",
                "parent": "ab-ep1c0001",
                "title": "Child one",
                "status": "ready",
                "slug": "child-one",
                "cwd": str(tmp_path / "wt"),
                "created_at": "2026-09-02T00:00:00Z",
            },
            {
                "id": "ab-d0ne0002",
                "parent": "ab-ep1c0001",
                "title": "Child two",
                "status": "done",
                "slug": "child-two",
                "pr_number": 99,
                "merge_status": "merged",
                "completed_at": "2026-09-03T00:00:00Z",
                "created_at": "2026-09-02T00:00:00Z",
            },
        ],
    )
    return tmp_path


def test_json_output(epic_fixture: Path):
    code, out, err = run_native("backlog", "epic", "status", "ab-ep1c0001", "--json")
    assert code == 0, out + err
    payload = json.loads(out)
    assert payload["epic"] == "ab-ep1c0001"
    assert payload["children_total"] == 2
    assert payload["children_done"] == 1
    children = {c["id"]: c for c in payload["children"]}
    assert children["ab-ch1d0001"]["receipt"] == "dispatched archer"
    assert children["ab-d0ne0002"]["merge_unconfirmed"] is False
    assert "scope_growth" in payload


def test_refuses_a_leaf_by_name(epic_fixture: Path):
    code, out, err = run_native("backlog", "epic", "status", "ab-ch1d0001")
    assert code == 1
    combined = out + err
    assert "is a leaf, not a container" in combined
