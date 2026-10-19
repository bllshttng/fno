"""`fno backlog album`: the memento book over graph-archive.json (x-b2bf).

The album is a read verb over the file the sweep already maintains. Cards are
done nodes sorted newest-first; a card with no gift says so rather than hiding.
"""
from __future__ import annotations
from tests.fixtures.graph_seed import seed_graph
from tests.goldens._door import native_backlog

import json
from pathlib import Path


def _route(tmp_path: Path, monkeypatch) -> Path:
    """The album reads archive residents from the store, so route the store
    itself into tmp; rows are seeded through the json import."""
    from fno import paths

    graph = tmp_path / "graph.json"
    monkeypatch.setattr(paths, "graph_json", lambda: graph)
    return graph


def _seed_archive(graph: Path, entries: list[dict]) -> None:
    """Rows the way the store writes them: the typed api drops a row the
    model cannot parse, so seeds carry the stamped fields. Every card is an
    archive resident, so each row carries archived_at."""
    complete = []
    for e in entries:
        row = {"type": "feature", "priority": "p2", "status": "idea", **e}
        row.setdefault("title", e.get("id", "node"))
        row.setdefault("slug", e.get("id", "node"))
        row.setdefault("archived_at", "2026-09-01T00:00:00Z")
        if row["status"] == "done" and not row.get("completed_at"):
            row["completed_at"] = "2026-09-01T00:00:00Z"
        complete.append(row)
    seed_graph(graph, json.dumps({"entries": complete}) + "\n")


def test_json_sorted_desc_gift_only_when_present(tmp_path, monkeypatch):
    archive = _route(tmp_path, monkeypatch)
    _seed_archive(
        archive,
        [
            {
                "id": "x-old0001",
                "title": "first ship",
                "status": "done",
                "completed_at": "2026-06-01T00:00:00Z",
                "pr_url": "https://github.com/o/r/pull/1",
                "pr_number": 1,
            },
            {
                "id": "x-new0001",
                "title": "latest ship",
                "status": "done",
                "completed_at": "2026-08-14T00:00:00Z",
            },
            {
                "id": "x-super001",
                "title": "superseded work",
                "status": "done",
                "superseded_by": "x-new0001",
                "completed_at": "2026-08-15T00:00:00Z",
            },
        ],
    )
    r = native_backlog("backlog", "album", "--json")
    assert r.exit_code == 0, r.output
    cards = json.loads(r.output)
    # Newest first, superseded excluded.
    assert [c["id"] for c in cards] == ["x-new0001", "x-old0001"]
    # pr_url present only when recorded; never a placeholder.
    assert cards[0].get("pr_url") is None
    assert cards[1]["pr_url"].endswith("/pull/1")


def test_text_names_no_gift_rather_than_hiding(tmp_path, monkeypatch):
    archive = _route(tmp_path, monkeypatch)
    _seed_archive(
        archive,
        [
            {
                "id": "x-gift001",
                "title": "gifted",
                "status": "done",
                "completed_at": "2026-08-14T00:00:00Z",
                "pr_number": 883,
            },
            {
                "id": "x-bare001",
                "title": "no gift recorded",
                "status": "done",
                "completed_at": "2026-08-13T00:00:00Z",
            },
        ],
    )
    r = native_backlog("backlog", "album")
    assert r.exit_code == 0, r.output
    assert "album: 2 shipped" in r.output
    assert "PR #883" in r.output
    assert "no gift" in r.output
    assert "no gift recorded" in r.output  # the card renders, gap visible


def test_legacy_completed_row_without_status_is_a_card(tmp_path, monkeypatch):
    # Pre-status-stamping sweep shape: completed_at is the terminal fact the
    # archive subsystem itself reads (statuses.is_terminal_entry, which the
    # album reaches through derived_status). A `ready` default must not evict
    # a shipped card.
    archive = _route(tmp_path, monkeypatch)
    _seed_archive(
        archive,
        [
            {
                "id": "x-leg0001",
                "title": "legacy ship",
                "completed_at": "2026-05-01T00:00:00Z",
            },
            {
                "id": "x-leg0002",
                "title": "superseded legacy",
                "completed_at": "2026-05-02T00:00:00Z",
                "superseded_by": "x-leg0001",
            },
        ],
    )
    r = native_backlog("backlog", "album", "--json")
    assert r.exit_code == 0, r.output
    cards = json.loads(r.output)
    assert [c["id"] for c in cards] == ["x-leg0001"]
