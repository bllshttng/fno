"""Selection-time exclusion of nodes that already carry an open PR.

The bug: `fno backlog next` / `ready` (and therefore `advance` auto-continue and
megawalk, which both shell `fno backlog next`) re-select a node that already has
an open, unmerged PR. A `no-merge` node keeps `status: ready` for the entire
review window (it only becomes `done` at merge), and its PID-based `node:<id>`
claim dies when the builder session exits - so the node looks like fresh ready
work and a redundant worker is dispatched.

The fix mirrors the existing `_live_claimed_node_ids()` exclusion with a new
`_has_unmerged_open_pr()` predicate applied at both selection sites.

Filter: `uv run pytest cli/tests/graph/test_open_pr_selection.py -q`
"""
from __future__ import annotations

import json
from datetime import datetime, timedelta, timezone

import pytest

from tests.fixtures.graph_seed import seed_graph
from fno.graph.cli import _has_unmerged_open_pr
from tests.goldens._door import door_graph


# Recent so the G1 stale-ready guard never quarantines these selection
# fixtures (a hardcoded past date rots past the threshold as wall-clock moves).
_RECENT_CREATED = (datetime.now(timezone.utc) - timedelta(days=1)).isoformat()


def _node(node_id: str, **overrides) -> dict:
    """A minimally-complete graph entry for the in-memory selectors.

    Defaults model a normal ready node (plan_path set, not claimed, not done,
    no PR). Override pr_number / merge_status / completed_at to model the
    in-review states.
    """
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
        "status": "ready",
        "created_at": _RECENT_CREATED,
    }
    base.update(overrides)
    return base


@pytest.fixture
def graph_file(tmp_path, monkeypatch):
    """Point the graph selectors at a temp graph.json and silence live claims.

    Returns a writer: call it with a list of entries to (re)write the graph.
    """
    path = tmp_path / "graph.json"

    def write(entries):
        seed_graph(path, entries)
        return path

    # The selection decision is served by the keeper; claims resolve under a
    # redirected root - never the operator's real claims.
    (tmp_path / "claims-root/.fno/claims").mkdir(parents=True, exist_ok=True)
    monkeypatch.setenv("FNO_CLAIMS_ROOT", str(tmp_path / "claims-root"))
    monkeypatch.setattr("fno.paths.graph_json", lambda: path)
    monkeypatch.setattr("fno.graph.cli._graph_path", lambda: path)
    monkeypatch.setattr(
        "fno.graph.cli._live_claimed_node_ids", lambda **_kwargs: set()
    )
    return write


# ---------------------------------------------------------------------------
# Predicate unit coverage
# ---------------------------------------------------------------------------


def test_predicate_merged_but_not_closed_excluded():
    """Verification #1: merged-but-pre-close stays excluded (reconcile pending).

    A PR that merged but whose node has not been closed yet (completed_at
    still None) must NOT be re-dispatched - the work is already done, the
    close just has not landed. (This is where the plan's illustrative snippet
    `merge_status != "merged"` was wrong; the criteria require True here.)
    """
    assert _has_unmerged_open_pr(
        {"pr_number": 515, "merge_status": "merged", "completed_at": None}
    ) is True


def test_predicate_done_node_not_excluded():
    """AC2-HP: a closed node is already out of the ready pool; do not interfere."""
    assert _has_unmerged_open_pr(
        {"pr_number": 515, "merge_status": None, "completed_at": "2026-06-13T01:00:00+00:00"}
    ) is False


# ---------------------------------------------------------------------------
# `fno backlog next` selector
# ---------------------------------------------------------------------------


def test_next_skips_node_with_open_pr(graph_file):
    """AC1-EDGE: when the only ready node has an open PR, `next` returns null."""
    graph = graph_file([_node("ab-aaaa1111", pr_number=515)])
    code, out, err = door_graph(graph, "next", "--all")
    assert code == 0, err
    assert out.strip() == "null"


# ---------------------------------------------------------------------------
# `fno backlog ready` selector
# ---------------------------------------------------------------------------


def test_ready_merged_pre_close_node_omitted(graph_file):
    """A merged-but-not-yet-closed node is also omitted from `ready`."""
    graph = graph_file([_node("ab-dddd4444", pr_number=515, merge_status="merged")])
    code, out, err = door_graph(graph, "ready", "--all")
    assert code == 0, err
    assert json.loads(out) == []


# ---------------------------------------------------------------------------
# Defer-contract parity: the open-PR guard is scoped to
# normal ready selection. An operator who EXPLICITLY surfaces paused nodes via
# --include-deferred must still see a deferred node that happens to carry a PR;
# the guard only suppresses AUTO re-selection of fresh ready work.
# ---------------------------------------------------------------------------


def test_ready_include_deferred_keeps_pr_bearing_deferred_node(graph_file):
    """`ready --include-deferred` still lists a deferred node that has a PR."""
    graph = graph_file([
        _node(
            "ab-eeee5555",
            pr_number=515,
            status="deferred",
            deferred_at="2026-06-13T02:00:00+00:00",
            deferred_reason="paused by operator",
        ),
    ])
    code, out, err = door_graph(graph, "ready", "--all", "--include-deferred")
    assert code == 0, err
    listed_ids = {e["id"] for e in json.loads(out)}
    assert "ab-eeee5555" in listed_ids


def test_ready_default_still_omits_deferred_pr_node(graph_file):
    """Without --include-deferred, the deferred node is absent anyway (status
    filter), so the scoping change does not leak it into the normal listing."""
    graph = graph_file([
        _node("ab-eeee5555", pr_number=515, status="deferred"),
        _node("ab-ffff6666", pr_number=515),  # ready + open PR -> excluded
        _node("ab-aaaa7777"),                  # ready, no PR -> listed
    ])
    code, out, err = door_graph(graph, "ready", "--all")
    assert code == 0, err
    listed_ids = {e["id"] for e in json.loads(out)}
    assert listed_ids == {"ab-aaaa7777"}
