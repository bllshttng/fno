"""Selection-time exclusion of open-batch members.

A node committed to an open batch has its atomic commits on a shared batch
branch and ships as part of the batch PR, not its own. `fno backlog next` /
`ready` (and therefore the auto-continue daemon + advance, which shell them)
must NOT re-select it - else a second worker rebuilds work already on the
branch. The mark is the graph `batch` field, set by `/target batched` via
`fno backlog update batch=<id>` and cleared (`batch=null`) on abandon so a
requeued member resurfaces.

Mirrors test_open_pr_selection.py: a new `_is_batched_member()` predicate applied
at both selection sites, plus the `fno backlog update batch=<id>` marker.

Filter: `uv run pytest cli/tests/graph/test_batched_member_selection.py -q`
"""
from __future__ import annotations

import json
from datetime import datetime, timedelta, timezone
from pathlib import Path

import pytest
from tests.fixtures.graph_seed import seed_graph

from fno.graph.cli import _is_batched_member
from tests.goldens._door import door_graph


# Recent so the G1 stale-ready guard never quarantines these selection
# fixtures (a hardcoded past date rots past the threshold as wall-clock moves).
_RECENT_CREATED = (datetime.now(timezone.utc) - timedelta(days=1)).isoformat()


def _node(node_id: str, **overrides) -> dict:
    base = {
        "id": node_id,
        "title": f"node {node_id}",
        "plan_path": f"plans/{node_id}.md",
        "priority": "p2",
        "rank": None,
        "type": "feature",
        "parent": None,
        "project": "fno",
        "domain": "code",
        "blocked_by": [],
        "session_id": None,
        "claimed_at": None,
        "completed_at": None,
        "pr_number": None,
        "merge_status": None,
        "batch": None,
        "status": "ready",
        "created_at": _RECENT_CREATED,
    }
    base.update(overrides)
    return base


@pytest.fixture
def graph_file(tmp_path, monkeypatch):
    path = tmp_path / "db" / "graph.json"

    def write(entries):
        seed_graph(path, entries)
        return path

    # The selection decision is served by the keeper now: the graph is pinned
    # through FNO_CONFIG (the client seam), not through a monkeypatched
    # `_graph_path`, and claims resolve under a redirected root.
    config = tmp_path / "config.toml"
    config.write_text(
        f'state_dir = "{tmp_path}"\n', encoding="utf-8"
    )
    (tmp_path / "claims-root/.fno/claims").mkdir(parents=True, exist_ok=True)
    monkeypatch.setenv("FNO_CONFIG", str(config))
    monkeypatch.setenv("FNO_CLAIMS_ROOT", str(tmp_path / "claims-root"))
    monkeypatch.setattr("fno.graph.cli._graph_path", lambda: path)
    monkeypatch.setattr(
        "fno.graph.cli._live_claimed_node_ids", lambda **_kwargs: set()
    )
    return write


# ---------------------------------------------------------------------------
# Predicate unit coverage
# ---------------------------------------------------------------------------


def test_predicate_empty_string_is_ready():
    """An empty-string batch id is a cleared mark (never a real batch id)."""
    assert _is_batched_member({"batch": ""}) is False


# ---------------------------------------------------------------------------
# `fno backlog ready` selector
# ---------------------------------------------------------------------------


def test_ready_omits_batched_member_lists_sibling(graph_file):
    """`ready` hides the batched node, lists the unbatched one."""
    graph = graph_file([
        _node("ab-aaaa1111", batch="batch-ab12cd34"),
        _node("ab-bbbb2222"),
    ])
    code, out, err = door_graph(graph, "ready", "--all")
    assert code == 0, err
    listed_ids = {e["id"] for e in json.loads(out)}
    assert "ab-aaaa1111" not in listed_ids
    assert "ab-bbbb2222" in listed_ids


# ---------------------------------------------------------------------------
# `fno backlog update batch=<id>` marker (mark + clear-on-abandon)
# ---------------------------------------------------------------------------


def _native_update(graph: Path, *args: str):
    """The update leaf answers natively; drive the dev binary over the same
    store the fixture seeded (FNO_CONFIG's state_dir)."""
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
            "HOME": str(graph.parent),
            "FNO_STATE_DIR": str(graph.parent),
            "FNO_TRACKER_BACKEND": "graph",
        },
        cwd=str(graph.parent),
    )
    return proc.returncode, proc.stdout + proc.stderr


def test_update_batch_marks_node(graph_file):
    """`update --batch <id>` sets node.batch so selection then excludes it."""
    graph = graph_file([_node("ab-cccc3333")])
    code, out = _native_update(graph, "ab-cccc3333", "--batch", "batch-ffff9999")
    assert code == 0, out
    # The mark now hides it from `next`.
    dcode, dout, derr = door_graph(graph, "next", "--all")
    assert dcode == 0, derr
    assert dout.strip() == "null"


def test_update_batch_null_clears_mark(graph_file):
    """`update --batch null` requeues a member (resurfaces in selection)."""
    graph = graph_file([_node("ab-cccc3333", batch="batch-ffff9999")])
    code, out = _native_update(graph, "ab-cccc3333", "--batch", "null")
    assert code == 0, out
    dcode, dout, derr = door_graph(graph, "next", "--all")
    assert dcode == 0, derr
    picked = json.loads(dout)
    assert picked is not None
    assert picked["id"] == "ab-cccc3333"
