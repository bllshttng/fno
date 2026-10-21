"""Terminal parents must not strand live children (x-a31a).

Three surfaces, one invariant: a live node under a terminal parent is
stranded (visibly owned, never dispatchable), so (1) closing over live
children is refused or re-parents them, (2) the starvation receipt names
terminal-parent stranding on every `next`, and (3) reconcile heals what
predates the guard and reports the count.
"""
from __future__ import annotations
from tests.fixtures.graph_seed import seed_graph

import json
from pathlib import Path

import pytest
from typer.testing import CliRunner

from fno.cli import app
from fno.graph.strand import (
    _is_live,
    _reparent_live_children,
)
from tests.conftest import run_native_create
from tests.goldens._door import door_graph

runner = CliRunner()

DONE = "2026-09-01T00:00:00+00:00"


def _node(node_id: str, **overrides) -> dict:
    base = {
        "id": node_id,
        "title": f"node {node_id}",
        "slug": node_id,
        "priority": "p2",
        "type": "feature",
        "project": "fno",
        "cwd": None,
        "parent": None,
        "blocked_by": [],
        "status": "ready",
        "completed_at": None,
        "plan_path": None,
        "created_at": "2026-07-01T00:00:00+00:00",
    }
    base.update(overrides)
    return base


def _by_id(entries: list[dict]) -> dict:
    return {e["id"]: e for e in entries}


def test_is_live_reads_the_terminal_fields():
    assert _is_live(_node("x-a"))
    assert not _is_live(_node("x-a", completed_at=DONE))
    assert not _is_live(_node("x-a", deferred_at=DONE))
    assert not _is_live(
        _node("x-a", superseded_by="x-b", supersession={"verified_at": DONE})
    )
    # superseded_by with no verifiable supersession record reads dead.
    assert not _is_live(_node("x-a", superseded_by="x-b", supersession=None))
    # A superseded node whose supersession is recorded but UNVERIFIED is live.
    assert _is_live(_node("x-a", superseded_by="x-b", supersession={"verified_at": None}))


def test_reparent_never_returns_the_dead_node_itself():
    """A parent cycle back into the dead node yields None, not the dead id."""
    entries = [
        _node("x-loop", parent="x-kid", completed_at=DONE, status="done"),
        _node("x-kid", parent="x-loop", completed_at=DONE, status="done"),
        _node("x-livekid", parent="x-loop"),
    ]
    moved = _reparent_live_children(entries, "x-loop")
    assert moved == [("x-livekid", None)]
    assert _by_id(entries)["x-livekid"]["parent"] is None


# ---------------------------------------------------------------------------
# CLI scaffolding (the test_contain.py pattern)
# ---------------------------------------------------------------------------


@pytest.fixture
def tmp_graph(tmp_path, monkeypatch) -> Path:
    """Fresh empty graph.json routed to tmp_path."""
    g = tmp_path / "graph.json"
    import fno.graph._constants as gc
    import fno.graph.store as gs

    monkeypatch.setattr(gc, "GRAPH_JSON", g)
    monkeypatch.setattr(gc, "GRAPH_MD", tmp_path / "graph.md")
    monkeypatch.setattr(gc, "GRAPH_ARCHIVE_JSON", tmp_path / "graph-archive.json")
    monkeypatch.setattr(gs, "GRAPH_JSON", g)
    monkeypatch.setattr("fno.paths.graph_json", lambda: g)
    return g


def _read_entries(g: Path) -> list[dict]:
    from fno.graph.store import read_graph_strict

    return read_graph_strict(g)


def _by_id_file(g: Path) -> dict:
    return {e["id"]: e for e in _read_entries(g)}


def _seed_idea(g: Path, title: str, *extra: str) -> str:
    # The idea leaf answers natively; drive the dev binary over the same
    # store the fixture seeded.
    import os as _os
    import subprocess as _sp

    from fno.rust_binary import find_dev_binary, resolve_binary

    binary = find_dev_binary() or resolve_binary()
    if binary is None:
        pytest.skip("no fno-agents dev build (cargo build -p fno-agents)")
    before = {e.get("id") for e in _read_entries(g)}
    proc = _sp.run(
        [str(binary), "backlog", "idea", title, "--difficulty", "medium", "--separate", *extra],
        capture_output=True,
        text=True,
        timeout=60,
        env={
            **_os.environ,
            "HOME": str(g.parent),
            "FNO_STATE_DIR": str(g.parent),
            "FNO_TRACKER_BACKEND": "graph",
        },
        cwd=str(g.parent),
    )
    assert proc.returncode == 0, proc.stderr
    new = [e["id"] for e in _read_entries(g) if e.get("id") not in before]
    assert len(new) == 1, f"expected one new node, saw {new}"
    return new[0]


def _seed_stranded_family(g: Path) -> tuple[str, str, list[str]]:
    """grand(live) -> parent(live) -> two kids; returns (grand, parent, kids)."""
    grand = _seed_idea(g, "strand grand epic")
    parent = _seed_idea(g, "strand parent epic", "--parent", grand)
    kids = [_seed_idea(g, f"strand kid {i}", "--parent", parent) for i in range(2)]
    return grand, parent, kids


# ---------------------------------------------------------------------------
# Change 2: the close-time guard
# ---------------------------------------------------------------------------


def test_canonical_done_refuses_over_live_children(tmp_graph):
    _grand, parent, kids = _seed_stranded_family(tmp_graph)
    # The canonical close is native; the refusal (and the kid names) ride
    # stderr through the door.
    r = run_native_create(tmp_graph, "done", parent)
    assert r.exit_code == 1, r.output + r.stderr
    for kid in kids:
        assert kid in r.stderr
    rows = _by_id_file(tmp_graph)
    assert not rows[parent].get("completed_at")  # nothing closed
    for kid in kids:
        assert rows[kid]["parent"] == parent  # nobody moved


def test_canonical_done_force_reparents_to_nearest_live_ancestor(tmp_graph):
    grand, parent, kids = _seed_stranded_family(tmp_graph)
    # The update leaf answers natively; the completion-note setup drives the
    # dev binary over the same store the fixture seeded.
    import os as _os
    import subprocess as _sp

    from fno.rust_binary import find_dev_binary, resolve_binary

    binary = find_dev_binary() or resolve_binary()
    if binary is None:
        pytest.skip("no fno-agents dev build (cargo build -p fno-agents)")
    note = _sp.run(
        [str(binary), "backlog", "update", parent,
         "completion_note+=deliberate close"],
        capture_output=True,
        text=True,
        timeout=60,
        env={
            "PATH": _os.environ["PATH"],
            "HOME": str(tmp_graph.parent),
            "FNO_STATE_DIR": str(tmp_graph.parent),
            "FNO_TRACKER_BACKEND": "graph",
        },
        cwd=str(tmp_graph.parent),
    )
    assert note.returncode == 0, note.stderr
    # The canonical close is native; the force + reparent receipt ride the
    # door over the same store.
    r = run_native_create(tmp_graph, "done", parent, "--force", "--reason", "deliberate close")
    assert r.exit_code == 0, r.output + r.stderr
    rows = _by_id_file(tmp_graph)
    assert rows[parent]["completed_at"]
    for kid in kids:
        assert rows[kid]["parent"] == grand
        assert f"{kid} -> {grand}" in r.output


def test_rich_done_auto_reparents_with_a_receipt(tmp_graph):
    grand, parent, kids = _seed_stranded_family(tmp_graph)
    r = runner.invoke(
        app,
        ["backlog", "done", parent, "--note", "rich close"],
        catch_exceptions=False,
    )
    assert r.exit_code == 0, r.output
    rows = _by_id_file(tmp_graph)
    assert rows[parent]["completed_at"]
    for kid in kids:
        assert rows[kid]["parent"] == grand
    assert "re-parented 2 stranded child(ren)" in r.output




# ---------------------------------------------------------------------------
# Change 3: `next` keeps its stdout contract and still names the stranded
# ---------------------------------------------------------------------------


def test_next_winner_stdout_stays_clean_stderr_names_the_stranded(
    tmp_graph, tmp_path
):
    plan = tmp_path / "winner.md"
    plan.write_text("---\nstatus: ready\nnode: x-winner\n---\n\n## Execution Strategy\n")
    entries = [
        _node("x-winner", plan_path=str(plan)),
        _node("x-donep", completed_at=DONE, status="done"),
        _node("x-kid", parent="x-donep", status="idea"),
    ]
    seed_graph(tmp_graph, json.dumps({"entries": entries}) + "\n")

    code, out, err = door_graph(tmp_graph, "next", "--all")
    assert code == 0, err
    # stdout is EXACTLY the winner JSON - the contract `_next_node` parses.
    picked = json.loads(out)
    assert picked["id"] == "x-winner"
    # stderr carries the capped strand receipt and the heal command.
    assert "stranded x-kid: dead-ancestor" in err
    assert "1 node(s) stranded under terminal parents" in err
    assert "fno backlog reconcile" in err
