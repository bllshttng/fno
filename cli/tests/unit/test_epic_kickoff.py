"""Tests for epic auto-close on the cascade and sweep close paths."""
from __future__ import annotations


def test_cascade_close_clears_mission_active(monkeypatch):
    """_cascade_close_parents clears mission_active when the epic auto-closes."""
    from fno.graph.cli import _cascade_close_parents

    entries = [
        {"id": "x-EPIC", "project": "fno", "mission_active": True},
        {"id": "x-only", "parent": "x-EPIC", "project": "web",
         "completed_at": "2026-07-18T00:00:00Z"},
    ]
    closed = _cascade_close_parents(entries, "x-only")
    assert "x-EPIC" in closed
    epic = next(e for e in entries if e["id"] == "x-EPIC")
    assert "mission_active" not in epic


def test_auto_close_holds_on_both_close_paths():
    """A plan_path+no-pr container is NOT closed on EITHER close path.

    The old UNVERIFIED completion_note flag was the mitigation: the close
    still fired and the note marked the deliverables unverified. The
    own-plan-work hold replaces it: the plan-having parent stays open on
    both the cascade (child-close walk) and the sweep (reconcile
    self-heal), and both paths auto-fire on a fresh session, so pinning
    both is still load-bearing.
    """
    from fno.graph.cli import _cascade_close_parents, _sweep_close_done_epics

    def entries():
        return [
            {"id": "x-EPIC", "project": "fno", "plan_path": "/p/plan.md"},
            {"id": "x-only", "parent": "x-EPIC", "project": "fno",
             "completed_at": "2026-08-06T00:00:00Z"},
        ]

    # cascade path: the walk that fires when a child lands
    cas = entries()
    assert _cascade_close_parents(cas, "x-only") == []
    epic = next(e for e in cas if e["id"] == "x-EPIC")
    assert epic.get("completed_at") is None
    assert "completion_note" not in epic

    # sweep path: the reconcile self-heal that re-closes on SessionStart
    swe = entries()
    assert _sweep_close_done_epics(swe) == []
    epic = next(e for e in swe if e["id"] == "x-EPIC")
    assert epic.get("completed_at") is None
    assert "completion_note" not in epic
