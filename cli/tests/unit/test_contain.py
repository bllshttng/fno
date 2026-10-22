"""Tests for `fno backlog contain` and the containment contract it stamps.

Covers:
- the contain command: a done-owner refusal, target refusals (live claim / open
  PR) with batch atomicity
- ``_container_ids`` treating an all-contained parent as a delivery unit
- the named-dispatch redirect (``_redirect_if_contained``)
- the merge cascade (``_cascade_close_contained``)
- a release dropping the owner's PR link
"""
from __future__ import annotations
from tests.fixtures.graph_seed import seed_graph

from pathlib import Path

import pytest
import typer

from fno.graph.store import commit_rows_via_store, read_graph_strict
from tests.conftest import run_native_create


@pytest.fixture
def tmp_graph(tmp_path, monkeypatch) -> Path:
    """Fresh empty graph.json routed to tmp_path (the test_defer.py pattern)."""
    g = tmp_path / "graph.json"
    seed_graph(g, '{"entries": []}\n')
    import fno.graph._constants as gc
    import fno.graph.store as gs

    monkeypatch.setattr(gc, "GRAPH_JSON", g)
    monkeypatch.setattr(gc, "GRAPH_MD", tmp_path / "graph.md")
    monkeypatch.setattr(gc, "GRAPH_ARCHIVE_JSON", tmp_path / "graph-archive.json")
    monkeypatch.setattr(gs, "GRAPH_JSON", g)
    monkeypatch.setattr("fno.paths.graph_json", lambda: g)
    return g


def _native_update(g: Path, *args: str):
    """The update leaf answers natively; drive the dev binary over the same
    store the fixture seeded (in-process monkeypatches cannot reach a
    subprocess)."""
    import os as _os
    import subprocess as _sp

    from fno.rust_binary import find_dev_binary, resolve_binary

    binary = find_dev_binary() or resolve_binary()
    if binary is None:
        pytest.skip("no fno-agents dev build (cargo build -p fno-agents)")
    proc = _sp.run(
        [str(binary), "backlog", "update", *args],
        capture_output=True,
        text=True,
        timeout=60,
        env={
            "PATH": _os.environ["PATH"],
            "HOME": str(g.parent),
            "FNO_STATE_DIR": str(g.parent),
            "FNO_TRACKER_BACKEND": "graph",
        },
        cwd=str(g.parent),
    )
    return proc.returncode, proc.stdout + proc.stderr


def _read_entries(g: Path) -> list[dict]:
    return read_graph_strict(g)


def _native_contain(g: Path, *args: str):
    """The contain leaf answers natively; drive the dev binary over the same
    store the fixture seeded (in-process monkeypatches cannot reach a
    subprocess). Returns (code, combined output)."""
    import os as _os
    import subprocess as _sp

    from fno.rust_binary import find_dev_binary, resolve_binary

    binary = find_dev_binary() or resolve_binary()
    if binary is None:
        pytest.skip("no fno-agents dev build (cargo build -p fno-agents)")
    env = {
        "PATH": _os.environ["PATH"],
        "HOME": str(g.parent),
        "FNO_STATE_DIR": str(g.parent),
        "FNO_TRACKER_BACKEND": "graph",
    }
    if _os.environ.get("FNO_CLAIMS_ROOT"):
        env["FNO_CLAIMS_ROOT"] = _os.environ["FNO_CLAIMS_ROOT"]
    proc = _sp.run(
        [str(binary), "backlog", "contain", *args],
        capture_output=True,
        text=True,
        timeout=60,
        env=env,
        cwd=str(g.parent),
    )
    return proc.returncode, proc.stdout + proc.stderr


def _mark_done(g: Path, node_id: str) -> None:
    """Arrange a done node straight through the store. The close itself is
    native; this file's subject is what contain does with a done node."""
    from fno.graph.store import commit_rows_via_store

    def stamp(rows):
        for e in rows:
            if e["id"] == node_id:
                e["status"] = "done"
                e["completed_at"] = "2026-04-01T00:00:00+00:00"
        return rows

    commit_rows_via_store(g, stamp)


def _by_id(g: Path) -> dict:
    return {e["id"]: e for e in _read_entries(g)}


def _write_rows(g: Path, rows: dict) -> None:
    """Replace the graph's rows through the store: a json rewrite would be
    ignored, because the db outlives the file it was imported from."""

    def mutator(_entries):
        return list(rows.values())

    commit_rows_via_store(g, mutator)


def _seed_idea(g: Path, title: str, *extra: str) -> str:
    """Mint a node and return its id from the GRAPH (stdout wrapping varies).

    --separate skips the non-interactive fold offer: without it a title
    resembling an existing node prints "fold offered" and mints nothing.
    """
    before = {e.get("id") for e in _read_entries(g)}
    r = run_native_create(g, "idea", title, "--difficulty", "medium", "--separate", *extra)
    assert r.exit_code == 0, r.stderr
    new = [e["id"] for e in _read_entries(g) if e.get("id") not in before]
    assert len(new) == 1, f"expected one new node, saw {new}"
    return new[0]


def _seed_owner_with_children(g: Path, n: int = 3) -> tuple[str, list[str]]:
    owner = _seed_idea(g, "owner epic")
    kids = [_seed_idea(g, f"child {i}", "--parent", owner) for i in range(n)]
    return owner, kids


# ---------------------------------------------------------------------------
# The command: owner refusals
# ---------------------------------------------------------------------------


def test_contain_refuses_a_done_owner_and_stamps_nothing(tmp_graph):
    owner, kids = _seed_owner_with_children(tmp_graph, 2)
    # --force: closing over live children is refused without it (x-a31a), and
    # the forced close re-parents the kids - irrelevant here, the owner is
    # done either way and contain must still refuse. The completion note
    # satisfies the close-evidence rule; the force keeps the child gate.
    _native_update(tmp_graph, owner, "completion_note+=setup: done owner")
    _mark_done(tmp_graph, owner)
    code, out = _native_contain(tmp_graph, owner, *kids)
    assert code == 2, out
    assert "is done" in out
    rows = _by_id(tmp_graph)
    assert all(rows[k].get("contained_in") is None for k in kids)


# ---------------------------------------------------------------------------
# The command: target refusals, batch atomicity
# ---------------------------------------------------------------------------


def test_contain_refuses_a_target_with_an_open_pr_and_stamps_nothing_in_the_batch(
    tmp_graph,
):
    owner, kids = _seed_owner_with_children(tmp_graph, 2)
    rows = _by_id(tmp_graph)
    rows[kids[0]]["pr_number"] = 4242
    _write_rows(tmp_graph, rows)
    code, out = _native_contain(tmp_graph, owner, *kids)
    assert code == 2, out
    assert "own delivery unit mid-flight" in out
    fresh = _by_id(tmp_graph)
    assert all(fresh[k].get("contained_in") is None for k in kids)


def test_contain_refuses_a_target_with_a_live_claim(tmp_graph, monkeypatch):
    owner, kids = _seed_owner_with_children(tmp_graph, 1)
    import os as _os

    from fno.claims.core import acquire_claim

    claims = tmp_graph.parent / "claims"
    monkeypatch.setenv("FNO_CLAIMS_ROOT", str(claims))
    acquire_claim(f"node:{kids[0]}", "worker-7", pid=_os.getpid(), root=claims)
    code, out = _native_contain(tmp_graph, owner, kids[0])
    assert code == 2, out
    assert "being built right now by worker-7" in out
    assert _by_id(tmp_graph)[kids[0]].get("contained_in") is None


# ---------------------------------------------------------------------------
# A box is a node with a child that is not contained in it
# ---------------------------------------------------------------------------


def test_container_ids_ignores_an_all_contained_brood():
    from fno.graph.cli import _container_ids

    entries = [
        {"id": "o"},
        {"id": "c1", "parent": "o", "contained_in": "o"},
        {"id": "c2", "parent": "o", "contained_in": "o"},
    ]
    assert _container_ids(entries) == set()


# ---------------------------------------------------------------------------
# The named-dispatch redirect
# ---------------------------------------------------------------------------


def test_redirect_if_contained_exits_2_naming_the_owner(tmp_graph, capsys):
    from fno.target_cli import _redirect_if_contained

    owner, kids = _seed_owner_with_children(tmp_graph, 1)
    assert _native_contain(tmp_graph, owner, *kids)[0] == 0
    with pytest.raises(typer.Exit) as exc:
        _redirect_if_contained(_by_id(tmp_graph)[kids[0]])
    assert exc.value.exit_code == 2
    assert f"ships inside {owner}'s PR" in capsys.readouterr().err


# ---------------------------------------------------------------------------
# The merge cascade
# ---------------------------------------------------------------------------


def test_cascade_close_contained_sets_completion_with_the_shipped_inside_note():
    from fno.graph.cli import _cascade_close_contained

    entries = [
        {"id": "o", "pr_number": 77},
        {"id": "c1", "parent": "o", "contained_in": "o"},
        {"id": "c2", "parent": "o", "contained_in": "o", "completed_at": "2026-01-01"},
    ]
    closed = _cascade_close_contained(entries, "o")
    assert closed == ["c1"]
    c1 = next(e for e in entries if e["id"] == "c1")
    assert c1["completed_at"]
    assert "shipped inside o (PR #77)" in c1["completion_note"]


# ---------------------------------------------------------------------------
# Releasing containment drops the owner's PR link
# ---------------------------------------------------------------------------


def test_release_drops_owner_pr_link_and_records_released_from(tmp_graph):
    """AC1-HP: a release drops the PR the containment bind stamped on the
    child, so the owner's merge cannot close a node that no longer ships
    inside it. The owner keeps its own PR."""
    owner = _seed_idea(tmp_graph, "owner epic")
    child = _seed_idea(tmp_graph, "child")
    other = _seed_idea(tmp_graph, "other epic")
    assert _native_contain(tmp_graph, owner, child)[0] == 0

    def _stamp(entries):
        for e in entries:
            if e["id"] in (owner, child):
                e["pr_number"] = 900
                e["pr_url"] = "https://github.com/o/r/pull/900"
        return entries

    commit_rows_via_store(tmp_graph, _stamp)
    code, out = _native_update(tmp_graph, child, f"parent={other}")
    assert code == 0, out
    rows = _by_id(tmp_graph)
    c = rows[child]
    assert c.get("contained_in") is None
    assert c.get("pr_number") is None
    assert c.get("pr_url") is None
    assert c.get("merge_status") is None
    assert c.get("released_from") == owner
    assert rows[owner]["pr_number"] == 900
