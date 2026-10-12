"""Tests for the epic advance / converge verb (x-9608 K1).

Covers the mission fan-out (advance_epic), the shared converge core reuse, the
mission-activation graph field, per-project + overall caps, per-child isolation,
idempotence (not TTL-dependent), and cascade-close deactivation.

Claim + graph isolation mirrors test_advance: claims route under a tmp
FNO_CLAIMS_ROOT/FNO_REPO_ROOT, the graph is a tmp graph.json with fno.paths.graph_json
patched to it, and _spawn_worker / _ready_leaf_children are patched at the module.

BLIND SPOT, known and accepted here: patching _ready_leaf_children bypasses
the real `fno backlog ready` projection, and the injected dicts below are
richer than that surface ever was - which is exactly how the dispatch_verb
loss (x-0961) stayed invisible to this suite. The projection is now covered
end-to-end by test_dispatch_verb_projection.py, which runs the real
selection subprocess; keep dispatch-projection regressions THERE.
"""
from __future__ import annotations
from tests.fixtures.graph_seed import seed_graph

import json
from pathlib import Path

import pytest



@pytest.fixture
def iso(tmp_path: Path, monkeypatch: pytest.MonkeyPatch):
    monkeypatch.setenv("FNO_CLAIMS_ROOT", str(tmp_path))
    monkeypatch.setenv("FNO_REPO_ROOT", str(tmp_path))
    monkeypatch.setenv("FNO_AUTO_CONTINUE", "1")  # armed by default
    return tmp_path / ".fno" / "events.jsonl"


def _events(p: Path) -> list[dict]:
    from tests._event_rows import event_rows

    return event_rows(Path(p))


def _write_graph(tmp_path: Path, entries: list[dict], monkeypatch) -> Path:
    """Seed the graph the way the store writes it: every row carries the
    fields the typed api requires (the store stamps them on every write)."""
    complete = []
    for e in entries:
        row = {"type": "feature", "priority": "p2", "status": "idea", **e}
        row.setdefault("title", e.get("id", "node"))
        row.setdefault("slug", e.get("id", "node"))
        complete.append(row)
    g = tmp_path / "graph.json"
    seed_graph(g, json.dumps({"entries": complete}) + "\n")
    monkeypatch.setattr("fno.paths.graph_json", lambda: g)
    return g


def _epic_graph(tmp_path, monkeypatch, *, children_done=False):
    """Epic x-EPIC with two leaf children in two projects (web, etl)."""
    done = {"completed_at": "2026-07-18T00:00:00Z"} if children_done else {}
    entries = [
        {"id": "x-EPIC", "title": "mission", "type": "epic", "project": "fno"},
        {"id": "x-web", "title": "web child", "parent": "x-EPIC", "project": "web",
         "slug": "web-child", "status": "ready", **done},
        {"id": "x-etl", "title": "etl child", "parent": "x-EPIC", "project": "etl",
         "slug": "etl-child", "status": "ready", **done},
    ]
    _write_graph(tmp_path, entries, monkeypatch)
    return entries


def _patch_map(monkeypatch, mapping: dict[str, str]):
    """project name -> work-map root; unmapped -> None."""
    monkeypatch.setattr(
        "fno.graph._intake.project_root_from_settings",
        lambda project: mapping.get(project),
    )




def _read_epic(epic_id="x-EPIC"):
    """Read the epic node via the (test-patched) graph_json, resolved at call time."""
    import fno.paths as _p
    from fno.graph.store import read_graph
    from fno.graph._intake import _find_node

    return _find_node(read_graph(_p.graph_json()), epic_id)




def _ready(*ids_projects):
    """Build a _ready_leaf_children return list from (id, project) pairs."""
    return [
        {"id": nid, "slug": nid, "title": nid, "project": proj}
        for nid, proj in ids_projects
    ]


# ---------------------------------------------------------------------------
# Refusals
# ---------------------------------------------------------------------------












# ---------------------------------------------------------------------------
# Happy path (AC1-HP)
# ---------------------------------------------------------------------------








# ---------------------------------------------------------------------------
# Per-child isolation (AC1-ERR / AC2-ERR)
# ---------------------------------------------------------------------------












# ---------------------------------------------------------------------------
# Idempotence (AC1-EDGE / AC2-FR)
# ---------------------------------------------------------------------------






# ---------------------------------------------------------------------------
# All-done epic (AC2-EDGE)
# ---------------------------------------------------------------------------








# ---------------------------------------------------------------------------
# Stop
# ---------------------------------------------------------------------------




# ---------------------------------------------------------------------------
# Caps
# ---------------------------------------------------------------------------










# ---------------------------------------------------------------------------
# Cascade-close deactivation
# ---------------------------------------------------------------------------


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


def test_auto_close_short_note_without_unverified_marker():
    """A container with no plan_path, or one that shipped its own PR, still
    closes and keeps the short note. The own-plan-work hold is gated on
    plan_path AND no PR ref, so the PR-shipped shape is not held; this
    pins that predicate."""
    from fno.graph.cli import _cascade_close_parents

    def note_for(parent_extra):
        entries = [
            {"id": "x-EPIC", "project": "fno", **parent_extra},
            {"id": "x-only", "parent": "x-EPIC", "project": "fno",
             "completed_at": "2026-08-06T00:00:00Z"},
        ]
        _cascade_close_parents(entries, "x-only")
        return next(e for e in entries if e["id"] == "x-EPIC")["completion_note"]

    short = "auto-closed: all children complete"
    # no plan_path -> short note
    assert note_for({}) == short
    # plan_path but it shipped its own PR -> deliverables verified -> short note
    assert note_for({"plan_path": "/p/plan.md", "pr_number": 605}) == short


# ---------------------------------------------------------------------------
# CLI wiring
# ---------------------------------------------------------------------------


def test_cli_epic_and_closed_mutually_exclusive(iso, tmp_path, monkeypatch):
    import subprocess
    from fno.rust_binary import resolve_binary

    r = subprocess.run([str(resolve_binary()), "backlog", "advance", "--epic", "x-EPIC", "--closed", "x-1"], capture_output=True, text=True)
    assert r.returncode == 2
    assert "mutually exclusive" in r.stderr


def test_cli_stop_requires_epic(iso, tmp_path, monkeypatch):
    import subprocess
    from fno.rust_binary import resolve_binary

    r = subprocess.run([str(resolve_binary()), "backlog", "advance", "--stop"], capture_output=True, text=True)
    assert r.returncode == 2
    assert "require --epic" in r.stderr


# ---------------------------------------------------------------------------
# Ordered child selection: the drain takes children in shared-selection order
# ---------------------------------------------------------------------------






# ---------------------------------------------------------------------------
# Continuation mode (x-a4dc K2 daemon drain): never reactivate; retire inactive
# ---------------------------------------------------------------------------
