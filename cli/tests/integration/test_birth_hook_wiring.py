"""Integration: the v2 A1 birth paths route through on_node_born.

The add/idea hook wiring is native (the golden replays pin its offer, skip
ladder and events) and the retro path is native (crates/fno-agents/src/retro/land.rs). This covers
the remaining birth paths prove they invoke the shared hook.

Decompose is NOT one of them. It used to route unflagged children here,
which spawned a /think on every autonomous decompose rather than offering one;
`needs_think` is now the sole consent for a decompose-time design pass, and the
inverse is pinned below.
"""
from __future__ import annotations
from tests.fixtures.graph_seed import seed_graph

import json
from pathlib import Path

import pytest
from typer.testing import CliRunner

import fno.graph._constants as gc
import fno.graph.store as gs
from fno.cli import app

runner = CliRunner()


def _invoke(*args, input=None):
    argv = list(args)
    if "backlog" in argv:
        index = argv.index("backlog")
        if argv[index : index + 2] == ["backlog", "add"] and "--difficulty" not in argv:
            argv.extend(["--difficulty", "medium"])
    return runner.invoke(app, argv, input=input, catch_exceptions=False)


def _route_graph(g: Path, tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setattr(gc, "GRAPH_JSON", g)
    monkeypatch.setattr(gc, "GRAPH_MD", tmp_path / "graph.md")
    monkeypatch.setattr(gc, "GRAPH_HTML", tmp_path / "graph.html")
    monkeypatch.setattr(gs, "GRAPH_JSON", g)
    # Seam readers resolve fno.paths.graph_json at call time; pin the
    # resolver to the same hermetic file (module-attr pins do not reach it).
    monkeypatch.setattr("fno.paths.graph_json", lambda: g)


# -- decompose child mint: deliberately NOT a birth-hook path --

_EPIC = {
    "id": "ab-epic0001", "parent": None, "title": "Epic", "type": "feature",
    "project": "fno", "cwd": "/tmp/proj", "priority": "p1", "domain": "code",
    "blocked_by": [], "plan_path": "internal/fno/plans/big.md#anchor",
    "created_at": "2026-01-01T00:00:00+00:00",
}
_GROUPS = json.dumps([
    {"slug": "1", "title": "G1", "waves": "1", "blocked_by_groups": []},
    {"slug": "2", "title": "G2", "waves": "2", "blocked_by_groups": ["1"]},
])


def test_decompose_never_fires_birth_hook(tmp_path, monkeypatch):
    """An unflagged decompose child reaches no spawn seam, on any pass.

    The epic doc is a group child's design authority (the blueprint session
    inline-fills each scaffold), so a decompose-born child needs no /think of
    its own unless the operator flags the group `needs_think`. The command is
    native now: the receipt's `fanout` array is the spawn evidence, read at
    the door with the fixture root pinned (HOME) so no subprocess write
    escapes to operator state.
    """
    import os as _os
    import subprocess as _sp

    from fno.rust_binary import find_dev_binary

    binary = find_dev_binary()
    if binary is None:
        pytest.skip("no fno-agents dev build (the smoke shard deletes it on purpose)")
    home = tmp_path / "home"
    (home / ".fno" / "db").mkdir(parents=True)
    g = home / ".fno" / "db" / "graph.json"
    seed_graph(g, json.dumps({"entries": [{**_EPIC, "cwd": str(tmp_path)}]}) + "\n")
    env = {**_os.environ, "HOME": str(home), "FNO_TRACKER_BACKEND": "graph"}

    def _door(*args):
        return _sp.run(
            [str(binary), "backlog", *args],
            capture_output=True, text=True, env=env, cwd=tmp_path, timeout=60,
        )

    # First pass: 2 children created, still no spawns.
    r = _door("decompose", "ab-epic0001", "--groups", _GROUPS, "--json")
    assert r.returncode == 0, r.stdout + r.stderr
    payload = json.loads(r.stdout)
    assert [row["action"] for row in payload["groups"]] == ["created", "created"]
    assert payload["fanout"] == []

    # Identical re-run stays idempotent and equally silent.
    r2 = _door("decompose", "ab-epic0001", "--groups", _GROUPS, "--json")
    assert r2.returncode == 0, r2.stdout + r2.stderr
    payload2 = json.loads(r2.stdout)
    assert [row["action"] for row in payload2["groups"]] == ["updated", "updated"]
    assert payload2["fanout"] == []
