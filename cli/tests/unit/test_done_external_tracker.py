"""Rich completion through NodeTracker.close under an external backend.

Task 4.1's contract (AC7/AC8): the shared gates run first, footnote-owned
rollups persist to the sidecar BEFORE the irreversible close, close(id) runs
exactly once, success prints only after it returns, a gate refusal preserves
its exit code with the item open, and a failed external close is loud and
retryable. A contradictory local graph file rides along: completion must
never answer from it.
"""
from __future__ import annotations
from tests.fixtures.graph_seed import seed_graph

import json
from pathlib import Path

from typer.testing import CliRunner

from fno.cli import app

runner = CliRunner()


class DoneFakeTracker:
    """A tracker-door fake: the door's payload shapes, close accounting, and
    real per-id sidecar files the sidecar-set arm writes through."""

    name = "fake-external"

    def __init__(self, rows, *, fail_close_ids=()):
        self._rows = {r["id"]: r for r in rows}
        self._fail_close = set(fail_close_ids)
        self.close_calls: list[str] = []
        # Snapshot of each sidecar at its close moment, to prove rollups land
        # BEFORE the irreversible close.
        self.sidecar_at_close: dict[str, dict] = {}

    def _node(self, r):
        return {
            "id": r["id"], "title": r.get("title"),
            "state": r.get("state", "open"),
            "parent": r.get("parent"), "blocked_by": r.get("blocked_by", []),
        }

    def _sc_path(self, sid):
        return self._sidecar_dir / f"{sid}.json"

    def read(self, id):
        r = self._rows.get(id)
        if r is None:
            return {"not_found": True}
        return {"node": self._node(r)}

    def list_open(self):
        return [self._node(r) for r in self._rows.values() if r.get("state", "open") == "open"]

    def close(self, id):
        self.close_calls.append(id)
        if id in self._fail_close:
            raise RuntimeError("backend 503")
        self._rows[id]["state"] = "closed"
        sc_path = self._sc_path(id)
        if sc_path.exists():
            self.sidecar_at_close[id] = json.loads(sc_path.read_text())

    def door(self, op, payload):
        if op == "read":
            return self.read(payload["id"])
        if op == "list-open":
            return {"candidates": self.list_open()}
        if op == "close":
            self.close(payload["id"])
            return {"closed": payload["id"]}
        if op == "sidecar-get":
            sid = payload["id"]
            path = self._sc_path(sid)
            body = {"id": sid}
            if path.exists():
                body.update(json.loads(path.read_text()))
            return {"sidecar": body}
        if op == "sidecar-set":
            sid = payload["id"]
            path = self._sc_path(sid)
            body = {"id": sid}
            if path.exists():
                body.update(json.loads(path.read_text()))
            body.update(payload["fields"])
            path.write_text(json.dumps(body))
            return {"saved": str(path)}
        if op == "sidecar-all":
            out = {}
            for path in sorted(self._sidecar_dir.glob("*.json")):
                out[path.stem] = json.loads(path.read_text())
            return {"sidecars": out}
        raise AssertionError(f"the close path never calls {op}")

    _sidecar_dir: Path = Path("/nonexistent")


def _wire(monkeypatch, tmp_path, rows, sidecars, **tracker_kwargs):
    tracker = DoneFakeTracker(rows, **tracker_kwargs)
    import fno.rust_binary as rb

    real_call = rb.command_call

    def _door_call(command, payload, *args, **kwargs):
        op = payload.get("tracker") if isinstance(payload, dict) else None
        if op:
            return tracker.door(op, payload)
        return real_call(command, payload, *args, **kwargs)

    monkeypatch.setattr(rb, "command_call", _door_call)
    sidecar_dir = tmp_path / "sidecars"
    sidecar_dir.mkdir(exist_ok=True)
    tracker._sidecar_dir = sidecar_dir
    for sid, fields in sidecars.items():
        payload = {"id": sid}
        payload.update(fields)
        (sidecar_dir / f"{sid}.json").write_text(json.dumps(payload))
    g = tmp_path / "graph.json"
    seed_graph(g, json.dumps({"entries": [
        {"id": r["id"], "title": "GRAPH-SENTINEL", "completed_at": None}
        for r in rows
    ]}))
    monkeypatch.setattr("fno.paths.graph_json", lambda: g)
    monkeypatch.setattr("fno.graph.cli._graph_path", lambda: g)
    monkeypatch.setenv("FNO_TRACKER_BACKEND", "github")
    monkeypatch.setenv("FNO_CLAIMS_ROOT", str(tmp_path / "claims"))
    # No ledger -> rollup fills nothing (keeps the close-order assertion tight).
    from fno.graph import _constants as gc

    monkeypatch.setattr(gc, "LEDGER_JSON", tmp_path / "absent-ledger.json")
    return tracker, sidecar_dir


def _graph_state(path: Path):
    from fno.graph.store import read_graph_strict, store_export_status

    return read_graph_strict(path), store_export_status(path)


def _gh_state(state, url="https://github.com/o/r/pull/7"):
    from fno.graph._reconcile import PrMergeState

    return PrMergeState(number=7, state=state, url=url,
                        merged_at="2026-08-16T00:00:00Z" if state == "MERGED" else None)


def test_gates_then_sidecar_then_exactly_one_close(tmp_path, monkeypatch):
    """AC7-HP: merged evidence passes the gate, the sidecar carries the PR url
    BEFORE the close fires, close runs exactly once, success after."""
    rows = [{"id": "EXT-1", "title": "Shipped thing"}]
    tracker, sc_dir = _wire(monkeypatch, tmp_path, rows, {
        "EXT-1": {"pr_number": 7, "cwd": "/ext",
                  "pr_url": "https://github.com/o/r/pull/7"},
    })
    monkeypatch.setattr(
        "fno.graph.cli._done_gh_query",
        lambda pr, **kw: _gh_state("MERGED"),
    )

    r = runner.invoke(app, ["backlog", "done", "EXT-1"],
                      catch_exceptions=False)
    assert r.exit_code == 0, r.output
    assert "Marked EXT-1 done" in r.output
    assert tracker.close_calls == ["EXT-1"]
    # The sidecar already carried the evidencing url at close time.
    assert tracker.sidecar_at_close["EXT-1"]["pr_url"] == "https://github.com/o/r/pull/7"
    # The item is closed on the backend and untouched locally.
    assert tracker._rows["EXT-1"].get("state") == "closed"


def test_open_pr_refusal_keeps_item_open(tmp_path, monkeypatch):
    """AC8-ERR: an OPEN PR is not closing evidence - exit 5 (the graph-mode
    contract), close never called, item stays open."""
    rows = [{"id": "EXT-1", "title": "Awaiting merge"}]
    tracker, _ = _wire(monkeypatch, tmp_path, rows, {
        "EXT-1": {"pr_number": 7},
    })
    monkeypatch.setattr(
        "fno.graph.cli._done_gh_query",
        lambda pr, **kw: _gh_state("OPEN"),
    )

    r = runner.invoke(app, ["backlog", "done", "EXT-1"],
                      catch_exceptions=False)
    assert r.exit_code == 5
    assert tracker.close_calls == []
    assert tracker._rows["EXT-1"].get("state", "open") == "open"


def test_failed_external_close_is_loud_and_retryable(tmp_path, monkeypatch):
    """AC8-ERR: a backend close failure exits nonzero naming the backend, the
    item stays open, and a retry after the backend recovers closes it."""
    rows = [{"id": "EXT-1", "title": "Shipped thing"}]
    tracker, _ = _wire(monkeypatch, tmp_path, rows, {
        "EXT-1": {"pr_number": 7},
    }, fail_close_ids={"EXT-1"})
    monkeypatch.setattr(
        "fno.graph.cli._done_gh_query",
        lambda pr, **kw: _gh_state("MERGED"),
    )

    r = runner.invoke(app, ["backlog", "done", "EXT-1"],
                      catch_exceptions=False)
    assert r.exit_code == 1
    assert "github" in r.output
    assert tracker._rows["EXT-1"].get("state", "open") == "open"

    tracker._fail_close.clear()
    r2 = runner.invoke(app, ["backlog", "done", "EXT-1"],
                       catch_exceptions=False)
    assert r2.exit_code == 0, r2.output
    assert tracker._rows["EXT-1"].get("state") == "closed"


def test_cascade_closes_all_done_parents(tmp_path, monkeypatch):
    """The ancestor cascade rides tracker parent edges: the epic closes once
    its last open child closes."""
    rows = [
        {"id": "EXT-kid", "title": "Last child", "parent": "EXT-epic"},
        {"id": "EXT-epic", "title": "The epic"},
    ]
    tracker, _ = _wire(monkeypatch, tmp_path, rows, {
        "EXT-kid": {"pr_number": 7},
        "EXT-epic": {},
    })
    monkeypatch.setattr(
        "fno.graph.cli._done_gh_query",
        lambda pr, **kw: _gh_state("MERGED"),
    )
    r = runner.invoke(app, ["backlog", "done", "EXT-kid"],
                      catch_exceptions=False)
    assert r.exit_code == 0, r.output
    assert tracker.close_calls == ["EXT-kid", "EXT-epic"]


def test_backfill_sweep_refused_externally_without_touching_graph(
    tmp_path, monkeypatch
):
    """The graph-store sweep (enumerate done nodes, write rollups back) has no
    legal target under an external backend: it must refuse before any read and
    leave the contradictory local graph file byte-identical."""
    rows = [{"id": "EXT-1", "state": "closed", "title": "Done thing"}]
    _wire(monkeypatch, tmp_path, rows, {"EXT-1": {}})
    g = tmp_path / "graph.json"
    before = _graph_state(g)

    r = runner.invoke(app, ["done", "--backfill"], catch_exceptions=False)
    assert r.exit_code == 1
    assert "refused" in r.output
    assert _graph_state(g) == before
