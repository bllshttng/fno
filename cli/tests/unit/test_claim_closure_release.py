"""Node closure releases its claim; reap settles node-aware (x-94f8).

Measured defect: a session that finished one node and moved to the next kept
a LIVE claim on the dead one for 16 hours. Closing a node had no release on
any path, and the reaper could only ask "is the holder alive?" - never "is
the holder still on THIS node?". These tests pin the two halves: the store's
closure-release hook (every closure path funnels through it) and the
settlement reading in the single reap decision, plus the board queue and the
graph lock mirror that made the leak read as a stall.
"""

from __future__ import annotations

import os
import socket

from pathlib import Path

from fno.claims.cli import RosterReading, _node_settlement
from fno.claims.core import sweep_verdict
from fno.claims.types import Claim, now_ms
from fno.graph.store import commit_rows_via_store, read_graph_strict
from tests.fixtures.graph_seed import seed_graph


HOLDER = "target-session:sid-a"


def _write_claim(key: str, *, holder: str, pid: int, expires_at_ms: int, root: Path) -> None:
    from fno.claims.core import acquire_claim
    from tests._table_seed import update_claim

    acquire_claim(key, holder, pid=os.getpid(), ttl_ms=3_600_000, root=root)
    update_claim(
        key,
        root=root,
        pid=pid,
        acquired_at=now_ms() - 7_200_000,
        expires_at=expires_at_ms,
    )


def _held(key: str, root: Path) -> bool:
    from fno.claims.core import claim_status

    return claim_status(key, root=root)["state"] != "free"


def _make_graph(tmp_path: Path, entries: list[dict]) -> Path:
    """Seed the graph the way the store writes it: every row carries the
    fields the typed api requires (the store stamps them on every write)."""
    complete = []
    for e in entries:
        row = {"type": "feature", "priority": "p2", "status": "idea", **e}
        row.setdefault("title", e.get("id", "node"))
        row.setdefault("slug", e.get("id", "node"))
        complete.append(row)
    p = tmp_path / "graph.json"
    seed_graph(p, complete)
    return p


# ---------------------------------------------------------------------------
# Task 1: the store's closure-release hook
# ---------------------------------------------------------------------------


class TestClosureReleaseHook:
    def _graph_with_claimed_node(self, tmp_path, monkeypatch) -> tuple[Path, Path]:
        global_root = tmp_path / "global"
        monkeypatch.setenv("FNO_CLAIMS_ROOT", str(global_root))
        graph = _make_graph(
            tmp_path,
            [
                {
                    "id": "x-doen",
                    "title": "claimed node",
                    "status": "in_progress",
                    "locked_by": HOLDER,
                    "claimed_at": "2026-08-21T00:00:00Z",
                    "session_id": HOLDER,
                    "completion_note": "fixture close",
                }
            ],
        )
        # The release fires only when the mutated graph IS the process's
        # configured graph (a scratch graph closing a same-id node must not
        # release the configured fleet's claim).
        monkeypatch.setattr("fno.paths.graph_json", lambda: graph)
        _write_claim(
            "node:x-doen",
            holder=HOLDER,
            pid=os.getpid(),
            expires_at_ms=now_ms() + 3_600_000,
            root=global_root,
        )
        return graph, global_root


    def test_scratch_graph_closure_does_not_release(self, tmp_path, monkeypatch):
        """A non-configured graph (tests, capture flows) owns no global claim:
        its closure leaves the global claim held, and the read projects that
        live holder instead of a cleared mirror."""
        graph, global_root = self._graph_with_claimed_node(tmp_path, monkeypatch)
        monkeypatch.setattr("fno.paths.graph_json", lambda: tmp_path / "the-configured-one.json")

        def _close(entries):
            for e in entries:
                if e["id"] == "x-doen":
                    e["completed_at"] = "2026-08-21T03:16:00Z"
            return entries

        commit_rows_via_store(graph, _close)
        assert read_graph_strict(graph)[0]["status"] == "done"
        assert read_graph_strict(graph)[0]["locked_by"] == HOLDER
        assert _held("node:x-doen", global_root)


    def test_done_releases_claim_and_clears_mirror(self, tmp_path, monkeypatch):
        graph, global_root = self._graph_with_claimed_node(tmp_path, monkeypatch)

        def _close(entries):
            for e in entries:
                if e["id"] == "x-doen":
                    e["completed_at"] = "2026-08-21T03:16:00Z"
            return entries

        commit_rows_via_store(graph, _close)

        out = read_graph_strict(graph)[0]
        assert out["status"] == "done"
        assert out["locked_by"] is None
        assert out["locked_at"] is None
        # session_id on a done node is work/cost provenance, not a lock.
        assert out["session_id"] == HOLDER
        assert not _held("node:x-doen", global_root)


# ---------------------------------------------------------------------------
# Task 2: the helper itself, and the no-claim case
# ---------------------------------------------------------------------------


# ---------------------------------------------------------------------------
# Task 3: the node-aware settlement
# ---------------------------------------------------------------------------


def _reading(workers_by_node: dict, *, consulted: bool = True) -> RosterReading:
    rows_by_session = {row["row_id"]: row for rows in workers_by_node.values() for row in rows}
    return RosterReading(
        consulted=consulted,
        rows_scanned=sum(len(v) for v in workers_by_node.values()) or 1,
        workers_by_node=workers_by_node,
        rows_by_session=rows_by_session,
    )


def _row(session_id: str = "sid-a") -> dict:
    return {"name": "w", "state": "working", "cwd": "/w", "row_id": session_id}


def _expired_live_claim(key: str = "node:x-gone") -> Claim:
    """The specimen shape: TTL long past, holder pid alive -> classify LIVE."""
    return Claim(
        key=key,
        holder="target-session:sid-a",
        acquired_at=now_ms() - 7_200_000,
        expires_at=now_ms() - 3_600_000,
        pid=os.getpid(),
        host=socket.gethostname(),
    )


def _native_verdict(claim: Claim) -> dict:
    expired = claim.expires_at is not None and now_ms() >= claim.expires_at
    return {
        "key": claim.key,
        "expired": expired,
        "provably_dead": False,
        "bucket": "suspect" if expired else "live",
    }


class TestNodeSettlement:
    def test_terminal_node_keeps_a_live_unexpired_holder(self, tmp_path, monkeypatch):
        """The 2026-09-05 specimen, pinned: a live holder's fresh lease on a
        CLOSED node survives the sweep. Four claims died this way (x-a114
        twice, x-04ce, x-9223-node) - each unexpired, pid alive, node done -
        reaped by the terminal arm on every sweep, killing active loop-check
        leases and opening the dup-PR window each time. Closure heals a leak,
        it never kills a living holder."""
        graph = _make_graph(
            tmp_path, [{"id": "x-gone", "status": "done", "completed_at": "2026-09-05T14:08:00Z"}]
        )
        monkeypatch.setattr("fno.paths.graph_json", lambda: graph)
        claim = Claim(
            key="node:x-gone",
            holder="target-session:sid-a",
            acquired_at=now_ms(),
            expires_at=now_ms() + 7_200_000,
            pid=os.getpid(),
            host=socket.gethostname(),
        )
        settlement = _node_settlement(_reading({}))
        assert settlement(claim, native_verdict=_native_verdict(claim)) is None
        verdict, bucket = sweep_verdict(
            claim,
            node_settlement=settlement,
            native_verdict=_native_verdict(claim),
        )
        assert verdict is False and bucket == "live"


    def test_terminal_node_settles_a_live_expired_claim(self, tmp_path, monkeypatch):
        """The healing read: a node the graph closed has no legitimate holder,
        whatever the pid table says."""
        graph = _make_graph(
            tmp_path, [{"id": "x-gone", "status": "done", "completed_at": "2026-08-21T03:16:00Z"}]
        )
        monkeypatch.setattr("fno.paths.graph_json", lambda: graph)
        verdict, bucket = sweep_verdict(
            _expired_live_claim(),
            node_settlement=_node_settlement(_reading({})),
            native_verdict=_native_verdict(_expired_live_claim()),
        )
        assert verdict is True and bucket == ""

    def test_legacy_defer_sentinel_is_not_terminal(self, tmp_path, monkeypatch):
        """A pre-migration row carries deferral inside completed_at; deferral
        is a RETURNABLE rung, and settling a live claim on it would hand the
        node to a second worker mid-deferral."""
        graph = _make_graph(
            tmp_path,
            [{"id": "x-gone", "completed_at": "deferred:2026-08-01T00:00:00Z"}],
        )
        monkeypatch.setattr("fno.paths.graph_json", lambda: graph)
        settlement = _node_settlement(_reading({"x-gone": [_row()]}))
        # The holder is on THIS node and the lease reads unexpired to the
        # roster arm's gate; nothing here may settle.
        claim = Claim(
            key="node:x-gone",
            holder="target-session:sid-a",
            acquired_at=now_ms(),
            expires_at=now_ms() + 3_600_000,
            pid=os.getpid(),
            host=socket.gethostname(),
        )
        assert settlement(claim, native_verdict=_native_verdict(claim)) is None

    def test_holder_on_a_different_node_settles_an_expired_lease(self):
        settlement = _node_settlement(_reading({"x-other": [_row()]}))
        claim = _expired_live_claim()
        assert settlement(claim, native_verdict=_native_verdict(claim)) is True

    def test_holder_on_this_node_is_not_settled(self):
        settlement = _node_settlement(_reading({"x-gone": [_row()]}))
        claim = _expired_live_claim()
        assert settlement(claim, native_verdict=_native_verdict(claim)) is None


# The done-node exclusion moved with build_board into the Rust collector
# (x-25b8): lead_board.rs stalled_holder_excludes_done_nodes /
# stalled_holder_still_names_a_live_open_node cover them.
