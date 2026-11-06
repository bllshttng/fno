"""Retention classification at the Python write boundary.

The schema declares a retention class per event type; the store now owns the
classification (node x-add3 routed journals; x-0915 moved the boundary into
events.db). An ephemeral row lands in the store carrying `ephemeral`, and no
sibling journal is ever created: the class is metadata, not a file route.
"""
from __future__ import annotations

import json
from pathlib import Path

from fno.paths import EPHEMERAL_EVENTS_SUFFIX as EPHEMERAL_SUFFIX
from fno.events.store_client import build_event as _build, append_event
from fno.events.store_client import read_committed_lines, store_db_path


def _rows(path: Path) -> list[dict]:
    return [json.loads(line) for line in read_committed_lines(path) if line]


def _classes(path: Path) -> dict[str, str]:
    import sqlite3

    db = store_db_path(path)
    conn = sqlite3.connect(f"file:{db}?mode=ro", uri=True)
    try:
        return {
            row[0]: row[1]
            for row in conn.execute("SELECT type, retention_class FROM events")
        }
    finally:
        conn.close()


def test_ephemeral_event_lands_in_the_store_as_ephemeral(tmp_path: Path) -> None:
    main = tmp_path / "events.jsonl"
    event = _build(
        "mux_pane_counters",
        "test",
        {"session": "s1", "panes": []},
    )
    append_event(event, events_path=main)

    rows = _rows(main)
    assert [r["type"] for r in rows] == ["mux_pane_counters"], (
        "the gauge is stored, not dropped"
    )
    assert _classes(main)["mux_pane_counters"] == "ephemeral"
    # The class is store metadata now: the sibling journal is never created.
    assert not (tmp_path / ("events.jsonl" + EPHEMERAL_SUFFIX)).exists()
