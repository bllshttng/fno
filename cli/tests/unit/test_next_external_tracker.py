"""Joined external-backend selection for `backlog next` / `backlog ready`.

Task 3.1's contract: ``list_open()`` exactly once, sidecar joined per open id
BEFORE the filters run, closed history never requested, footnote ranking
applied after the join, backend failures fail closed, and graph/external
winner parity. Every test points the seam at a contradictory local graph file:
if selection ever answered from it, the sentinels give the test away.
"""
from __future__ import annotations
from tests.fixtures.graph_seed import seed_graph

import json
from datetime import datetime, timedelta, timezone
from pathlib import Path

from typer.testing import CliRunner

from fno.cli import app

runner = CliRunner()


def _days_ago(n: int) -> str:
    """A fixture created_at relative to now.

    The literals these replaced (2026-08-01 / 08-02) aged past the
    selection guard's 21-day stale-quarantine on 2026-08-23, silently
    flipping every ranking assert: the fresh p3 started outranking the
    p1. Relative dates keep every age gap without crossing the line.
    """
    return (datetime.now(timezone.utc) - timedelta(days=n)).strftime(
        "%Y-%m-%dT00:00:00Z"
    )


class FakeTracker:
    """A NodeTracker fake over seam-only facts, with call accounting."""

    name = "fake-external"

    def __init__(self, rows, *, fail_list_open=False, fail_read_ids=()):
        from fno.tracker.types import TrackerCandidate, TrackerState

        self._T, self._S = TrackerCandidate, TrackerState
        self._rows = rows
        self._fail_list_open = fail_list_open
        self._fail_read_ids = set(fail_read_ids)
        self.list_open_calls = 0
        self.read_calls: list[str] = []

    def read(self, id):
        from fno.tracker.types import NodeNotFound

        self.read_calls.append(id)
        if id in self._fail_read_ids:
            raise RuntimeError(f"backend read blew up for {id}")
        for r in self._rows:
            if r["id"] == id:
                return self._T(
                    id=r["id"], title=r.get("title"),
                    state=self._S(r.get("state", "open")),
                    parent=r.get("parent"),
                    blocked_by=r.get("blocked_by", []),
                )
        raise NodeNotFound(id)

    def list_open(self):
        self.list_open_calls += 1
        if self._fail_list_open:
            raise RuntimeError("network partition")
        out = []
        for r in self._rows:
            if r.get("state", "open") == "open":
                out.append(self._T(
                    id=r["id"], title=r.get("title"), state=self._S.open,
                    parent=r.get("parent"), blocked_by=r.get("blocked_by", []),
                    priority=r.get("priority", "p2"),
                    rank=r.get("rank"), created_at=r.get("created_at"),
                ))
        return out

    def close(self, id):
        raise AssertionError("close is not part of selection")

    def _call(self, op, id=None):
        """The door-shaped document: open entries joined with sidecar fields,
        closed rows as tombstones."""
        assert op == "snapshot"
        import fno.tracker.sidecar as sidecar_store

        entries = []
        for c in self.list_open():
            try:
                sc = sidecar_store.load(c.id)
            except Exception as exc:
                raise RuntimeError(f"sidecar read failed for {c.id}: {exc}")
            entries.append({
                "id": c.id,
                "title": c.title,
                "state": "open",
                "status": _open_status(pr_number=sc.pr_number, plan_path=sc.plan_path),
                "parent": c.parent,
                "blocked_by": list(c.blocked_by),
                "priority": c.priority,
                "rank": c.rank,
                "created_at": c.created_at,
                "cwd": sc.cwd,
                "plan_path": sc.plan_path,
                "pr_number": sc.pr_number,
                "pr_url": sc.pr_url,
                "additional_prs": sc.additional_prs,
                "batch": sc.batch,
                "contained_in": sc.contained_in,
                "sessions": sc.sessions,
                "claimed_at": sc.claimed_at,
                "cost_usd": sc.cost_usd,
            })
        for r in self._rows:
            if r.get("state", "open") != "open":
                entries.append(
                    {"id": r["id"], "state": "closed", "status": "done", "completed_at": "closed"}
                )
        return {"backend": self.name, "entries": entries}


def _open_status(*, pr_number, plan_path):
    if pr_number:
        return "in_review"
    return "ready" if plan_path else "idea"


def _wire(monkeypatch, tmp_path, rows, sidecars, **tracker_kwargs):
    """Point tracker, sidecar store, claims, and the local graph at fakes."""
    tracker = FakeTracker(rows, **tracker_kwargs)
    monkeypatch.setattr("fno.tracker.get_tracker", lambda *a, **k: tracker)
    sidecar_dir = tmp_path / "sidecars"
    sidecar_dir.mkdir(exist_ok=True)
    for sid, fields in sidecars.items():
        payload = {"id": sid}
        payload.update(fields)
        (sidecar_dir / f"{sid}.json").write_text(json.dumps(payload))
    import fno.tracker.sidecar as sidecar_store

    monkeypatch.setattr(sidecar_store, "sidecar_path",
                        lambda i: sidecar_dir / f"{i}.json")
    # The contradictory local graph: every value here must never surface.
    g = tmp_path / "graph.json"
    seed_graph(g, json.dumps({"entries": [
        {"id": r["id"], "title": "GRAPH-SENTINEL", "cwd": "/graph-cwd",
         "status": "ready", "priority": "p0"}
        for r in rows
    ]}))
    monkeypatch.setattr("fno.paths.graph_json", lambda: g)
    monkeypatch.setenv("FNO_TRACKER_BACKEND", "github")
    monkeypatch.setenv("FNO_CLAIMS_ROOT", str(tmp_path / "claims"))
    return tracker, g


def _rows_basic():
    return [
        {"id": "EXT-hi", "title": "High prio leaf", "priority": "p1",
         "created_at": _days_ago(2)},
        {"id": "EXT-lo", "title": "Low prio leaf", "priority": "p3",
         "created_at": _days_ago(1)},
    ]


def test_next_joins_once_and_ranks_over_the_open_set(tmp_path, monkeypatch):
    """AC4-HP: list_open exactly once; the winner is tracker-ranked (p1 beats
    p3) with the title coming from the tracker, never GRAPH-SENTINEL."""
    tracker, _g = _wire(monkeypatch, tmp_path, _rows_basic(), {
        "EXT-hi": {"plan_path": "/plans/hi.md", "cwd": "/ext/hi"},
        "EXT-lo": {"plan_path": "/plans/lo.md", "cwd": "/ext/lo"},
    })
    r = runner.invoke(app, ["backlog", "next"], catch_exceptions=False)
    assert r.exit_code == 0, r.output
    doc = json.loads(r.output)
    assert doc["id"] == "EXT-hi"
    assert doc["title"] == "High prio leaf"
    assert doc["cwd"] == "/ext/hi"
    assert tracker.list_open_calls == 1


def test_next_fail_closed_names_the_backend(tmp_path, monkeypatch):
    """AC6-ERR: a failing backend exits nonzero with the backend named and
    never answers from the local graph (whose rows all say ready/p0)."""
    _wire(monkeypatch, tmp_path, _rows_basic(), {},
          fail_list_open=True)
    r = runner.invoke(app, ["backlog", "next"], catch_exceptions=False)
    assert r.exit_code == 1
    assert "fake-external" in r.output
    assert r.output.strip() != "null"


def test_next_fail_closed_names_the_id_on_sidecar_fault(tmp_path, monkeypatch):
    """AC6-ERR: a failing per-id sidecar read names the id; no silent
    selection of a different node."""
    rows = _rows_basic()
    tracker, _g = _wire(monkeypatch, tmp_path, rows, {
        "EXT-hi": {"plan_path": "/plans/hi.md"},
    })
    # Make the second candidate's sidecar unreadable at the store level by
    # failing its file read: a directory in place of the file raises on read.
    import fno.tracker.sidecar as sidecar_store

    real_load = sidecar_store.load
    monkeypatch.setattr(sidecar_store, "load", lambda i: real_load(i))
    (Path(sidecar_store.sidecar_path("EXT-lo").parent) / "EXT-lo.json").mkdir()

    r = runner.invoke(app, ["backlog", "next"], catch_exceptions=False)
    assert r.exit_code == 1
    assert "EXT-lo" in r.output


def test_the_external_claim_still_blocks_after_its_selector_exits(
    tmp_path, monkeypatch
):
    """The property that matters, and the one an in-process test cannot see.

    Selection runs in a process that exits as soon as it prints the node. A
    pid-liveness claim is therefore dead on arrival in production: it reads
    `stale`, `stale` does not block dispatch, and a worker launches onto the
    node this selector just handed out. Routing the root alone made the lock
    visible and left it unhonored.

    Under CliRunner the acquiring process is pytest, which stays alive, so the
    claim reads `live` and the bug is invisible. This test forces the recorded
    pid to a dead one, which is what the real selector's exit produces, and then
    asks the guard the question a dispatcher asks.
    """
    import json

    from fno.claims.core import claim_status
    from tests._table_seed import update_claim

    rows = _rows_basic()
    _wire(monkeypatch, tmp_path, rows, {
        "EXT-hi": {"plan_path": "/plans/hi.md"},
        "EXT-lo": {"plan_path": "/plans/lo.md"},
    })
    r = runner.invoke(
        app, ["backlog", "next", "--claim", "sess-ext-gone"], catch_exceptions=False
    )
    assert r.exit_code == 0, r.output
    assert json.loads(r.output)["id"] == "EXT-hi"

    key = "node:EXT-hi"
    dead = 999_999
    import psutil

    while psutil.pid_exists(dead):
        dead += 1
    update_claim(key, pid=dead)

    # Dead pid INSIDE the TTL is `suspect`, and suspect blocks. Without the TTL
    # the same reading is `stale`, which does not.
    assert claim_status(key)["state"] == "suspect"

    from fno.agents.cli import _spawn_guard_decision

    verdict, _code = _spawn_guard_decision(
        "EXT-hi", "spawn-cli:probe", no_reserve=True
    )
    assert verdict["verdict"] == "already-running", verdict
    assert verdict["holder"] == "sess-ext-gone"
