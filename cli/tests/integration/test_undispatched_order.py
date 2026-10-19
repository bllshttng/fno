"""The undispatched queue orders through the shared selection key (AC2)."""
from __future__ import annotations

from fno.backlog.undispatched import classify_planned_unclaimed


def _node(node_id: str, **over) -> dict:
    entry = {
        "id": node_id,
        "title": node_id,
        "status": "ready",
        "priority": "p2",
        "project": "fno",
        "plan_path": f"plans/{node_id}.md",
        "created_at": "2026-01-01T00:00:00+00:00",
    }
    entry.update(over)
    return entry


def _ids(entries: list[dict]) -> list[str]:
    receipt = classify_planned_unclaimed(entries, [])
    return [row["id"] for row in receipt["rows"]]


def test_ac2_hp_pinned_node_leads_the_undispatched_queue():
    """A pin used to be invisible here: the queue sorted by (priority, id)."""
    entries = [
        _node("x-aaa1", priority="p0"),
        _node("x-bbb2", priority="p2", rank=-9.0),
    ]

    assert _ids(entries) == ["x-bbb2", "x-aaa1"]
