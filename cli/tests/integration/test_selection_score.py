"""The selection key reads votes, below the decision terms (AC3)."""
from __future__ import annotations

from datetime import datetime, timedelta, timezone

from fno.graph._intake import make_selection_sort_key


def _recent(days_ago: int = 1) -> str:
    return (datetime.now(timezone.utc) - timedelta(days=days_ago)).isoformat()


def _node(node_id: str, priority: str = "p2", votes: int = 0, **over) -> dict:
    entry = {
        "id": node_id,
        "title": node_id,
        "status": "ready",
        "priority": priority,
        "project": "fno",
        "created_at": _recent(),
        "sessions": [{"session_id": "s1"}],
    }
    if votes:
        entry["encounters"] = [
            {"voter_key": f"{node_id}-voter-{i}", "voter_kind": "agent"}
            for i in range(votes)
        ]
    entry.update(over)
    return entry


def _order(entries: list[dict]) -> list[str]:
    return [e["id"] for e in sorted(entries, key=make_selection_sort_key(entries))]


def test_ac3_hp_encountered_row_leads_its_priority_band():
    quiet = _node("x-aaa1")
    loud = _node("x-bbb2", votes=3)

    assert _order([quiet, loud]) == ["x-bbb2", "x-aaa1"]


def test_ac3_hp_priority_gates_the_band():
    """A p1 nobody voted on still sorts ahead of a much-voted p2."""
    p1 = _node("x-ccc3", priority="p1")
    loud_p2 = _node("x-bbb2", priority="p2", votes=5)
    quiet_p2 = _node("x-aaa1", priority="p2")

    assert _order([quiet_p2, loud_p2, p1]) == ["x-ccc3", "x-bbb2", "x-aaa1"]

