"""Read-time dependency readiness: the three-way result, delivered through
the ported store.

recompute_statuses (statuses.py) no longer derives `status` from `blocked_by`
at write time - see test_graph_statuses.py's *_at_write_time tests. The
derivation itself was ported to the store (graph_store.rs
compute_readiness/readiness_status), so these tests drive it the way every
reader does: through `_apply_graph_defaults`, the one seam the real read
uses, asserting the (status, blocked_reason) pair the overlay publishes.
"""
from __future__ import annotations

from pathlib import Path

import pytest

from tests.fixtures.graph_seed import seed_graph

from fno.graph.statuses import recompute_statuses
from fno.graph.store import read_graph_strict

# Since the store port every test here rides the keeper, so the module needs
# the compiled runtime and skips whole where the smoke harness deleted the
# worker binary (the parity-test convention).
requires_rust = pytest.mark.dev_build

pytestmark = requires_rust


def _entry(eid: str, **kwargs) -> dict:
    base = {
        "id": eid,
        "title": eid,
        "completed_at": None,
        "session_id": None,
        "claimed_at": None,
        "blocked_by": [],
        "plan_path": f"plans/{eid}.md",
        "status": "ready",
    }
    base.update(kwargs)
    return base


def _write(tmp_path: Path, entries: list[dict]) -> Path:
    p = tmp_path / "graph.json"
    seed_graph(p, {"entries": entries})
    return p


# -- compute_readiness (pure function) --


# -- read-time overlay via read_graph / _apply_graph_defaults --


def test_read_graph_overlays_blocked_for_an_unknown_dependency(tmp_path: Path):
    p = _write(tmp_path, [_entry("ab-kkkkkkkk", blocked_by=["ab-nonexistent"])])
    rows = {e["id"]: e for e in read_graph_strict(p)}
    assert rows["ab-kkkkkkkk"]["status"] == "blocked"
    assert rows["ab-kkkkkkkk"]["blocked_reason"] == "unknown-dep:ab-nonexistent"
    assert rows["ab-kkkkkkkk"]["status"] != "ready"


def test_read_graph_overrides_a_stale_persisted_ready(tmp_path: Path):
    """The property this whole change exists for: a stale on-disk `ready`
    (e.g. written before this change, or by a node whose blocker regressed
    after its own last mutation) must not be trusted. The read-time overlay
    recomputes from blocked_by + the blocker's own completed_at every time."""
    p = _write(
        tmp_path,
        [
            _entry("ab-llllllll"),  # blocker: not completed
            _entry("ab-mmmmmmmm", blocked_by=["ab-llllllll"], status="ready"),
        ],
    )
    rows = {e["id"]: e for e in read_graph_strict(p)}
    assert rows["ab-mmmmmmmm"]["status"] == "blocked"
    assert rows["ab-mmmmmmmm"]["blocked_reason"] == "blocked-by:ab-llllllll"


def test_terminal_statuses_outrank_blocked(tmp_path: Path):
    """done/superseded/deferred/in_review are direct facts about the node
    itself and must not be demoted to blocked just because blocked_by names
    an open dependency.

    The overlay trusts the PERSISTED status for exactly these four buckets
    (they are direct, single-node facts that recompute_statuses still
    derives correctly at write time - only the blocked_by cross-node join
    moved to read time), so this fixture sets `status` explicitly to what
    recompute_statuses would already have written, rather than relying on
    read_graph to derive it from completed_at/superseded_by/deferred_at
    (read_graph applies defaults, it does not re-run recompute_statuses).
    """
    p = _write(
        tmp_path,
        [
            _entry("ab-open0001"),
            _entry(
                "ab-done0001",
                blocked_by=["ab-open0001"],
                completed_at="2026-01-01T00:00:00Z",
                status="done",
            ),
            _entry(
                "ab-super0001", blocked_by=["ab-open0001"], superseded_by="ab-x", status="superseded"
            ),
            _entry(
                "ab-defer0001",
                blocked_by=["ab-open0001"],
                deferred_at="2026-01-01T00:00:00Z",
                status="deferred",
            ),
            _entry("ab-review0001", blocked_by=["ab-open0001"], pr_number=42, status="in_review"),
        ],
    )
    rows = {e["id"]: e for e in read_graph_strict(p)}
    assert rows["ab-done0001"]["status"] == "done"
    assert rows["ab-super0001"]["status"] == "superseded"
    assert rows["ab-defer0001"]["status"] == "deferred"
    assert rows["ab-review0001"]["status"] == "in_review"


def test_recompute_statuses_never_round_trips_a_stale_blocked_reason():
    """The write path (commit_rows_via_store) reads via _apply_graph_defaults
    BEFORE handing entries to the mutator, so a dict can arrive at
    recompute_statuses already carrying a `blocked_reason` the read-time
    overlay set moments earlier. recompute_statuses must scrub it rather
    than let it round-trip to disk - `blocked_reason` is answered fresh on
    every read (compute_readiness) and must never be a field a write can
    leave stale.
    """
    entry = _entry(
        "ab-ppppppp1",
        blocked_by=["ab-still-open"],
        blocked_reason="blocked-by:ab-still-open",  # simulates the read-time leak
    )
    result = recompute_statuses([entry])
    assert result[0].get("blocked_reason") is None


# -- children summaries derive through the same overlay --

# The parent's `children` array is the surface a lead surveys an epic with.
# Its `status` must speak the derived truth (open blocker -> blocked), never
# the stored field, which by design never encodes blocked.


def test_read_graph_children_summary_derives_blocked(tmp_path: Path):
    """The exact on-disk shape a mis-dispatched epic child carries today:
    a child stored `ready` with a non-empty blocked_by over an open blocker
    must read `blocked` BOTH at top level and inside the parent's summary."""
    p = _write(
        tmp_path,
        [
            _entry("ab-epic00001"),
            _entry("ab-block0001"),
            _entry(
                "ab-child0002",
                parent="ab-epic00001",
                blocked_by=["ab-block0001"],
            ),
        ],
    )
    rows = {e["id"]: e for e in read_graph_strict(p)}
    assert rows["ab-child0002"]["status"] == "blocked"
    summary = rows["ab-epic00001"]["children"][0]
    assert summary["id"] == "ab-child0002"
    assert summary["status"] == "blocked"
