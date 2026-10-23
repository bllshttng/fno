"""Unit tests for fno.graph.store - the keeper-backed client.

These tests are ported from tests/test_graph.py and target the extracted module.
They run the module functions directly (no subprocess) for speed.
"""
from __future__ import annotations

from pathlib import Path

import pytest

from tests.fixtures.graph_seed import seed_graph
from fno.graph.store import (
    append_session_record,
    _read_json,
    commit_rows_via_store,
    read_graph_strict,
)


# Since the store port every test here rides the keeper, so the module needs
# the compiled runtime and skips whole where the smoke harness deleted the
# worker binary (the parity-test convention).
requires_rust = pytest.mark.dev_build

pytestmark = requires_rust


# -- helpers --


def _make_graph(tmp_path: Path, entries: list[dict]) -> Path:
    p = tmp_path / "graph.json"
    seed_graph(p, entries)
    return p


# -- tests --


def test_ac1_hp_commit_rows_via_store(tmp_path):
    """AC1-HP: commit_rows_via_store reads, applies mutator, writes back."""
    path = tmp_path / "graph.json"

    def mutator(entries):
        entries.append({"id": "ab-newnode0", "title": "New"})
        return entries

    commit_rows_via_store(path, mutator)
    # The store write lands in graph.db; the json file is only an export.
    result = read_graph_strict(path)
    assert any(e.get("id") == "ab-newnode0" for e in result)


def test_touched_at_stamped_on_curation_change(tmp_path):
    """Positive control (x-7dcb): a priority change DOES stamp touched_at.
    Without this, AC4-EDGE's negative case proves nothing - a guard that
    never fires and a guard that always fires both pass a cwd-only test."""
    path = _make_graph(tmp_path, [{"id": "ab-1", "title": "T", "priority": "p2"}])

    def mutator(entries):
        for e in entries:
            if e["id"] == "ab-1":
                e["priority"] = "p1"
        return entries

    commit_rows_via_store(path, mutator)
    result = _read_json(path)
    node = next(e for e in result if e["id"] == "ab-1")
    assert node.get("touched_at")


def test_touched_at_unchanged_on_blocked_node_unrelated_write(tmp_path):
    """Regression (x-7dcb): a blocked node's read-time readiness overlay
    (`_apply_readiness_overlay`, applied by every `_apply_graph_defaults`
    call) stamps `status: "blocked"` into the pre-mutator snapshot, but
    `recompute_statuses` never derives "blocked" - so an unguarded
    comparison would misread EVERY mutation on a blocked node as a status
    change, even one that only edits an unrelated field. The blocker here
    stays unresolved across the mutation, so the overlay is unchanged too;
    touched_at must not move."""
    path = _make_graph(
        tmp_path,
        [
            {"id": "ab-blocker", "title": "Blocker", "status": "ready"},
            {
                "id": "ab-blocked",
                "title": "Blocked",
                "status": "idea",
                "blocked_by": ["ab-blocker"],
                "touched_at": "2020-01-01T00:00:00+00:00",
            },
        ],
    )

    def mutator(entries):
        for e in entries:
            if e["id"] == "ab-blocked":
                e["details"] = "unrelated edit"
        return entries

    commit_rows_via_store(path, mutator)
    result = _read_json(path)
    blocked = next(e for e in result if e["id"] == "ab-blocked")
    assert blocked.get("touched_at") == "2020-01-01T00:00:00+00:00"


def _ready_plan_entry(tmp_path: Path, node_id: str = "ab-open0001") -> tuple[Path, dict]:
    plan = tmp_path / "plan.md"
    plan.write_text("---\nstatus: ready\n---\n", encoding="utf-8")
    return plan, {
        "id": node_id,
        "slug": node_id,
        "title": node_id,
        "type": "feature",
        "status": "idea",
        "priority": "p2",
        "cwd": str(tmp_path),
        "plan_path": plan.name,
        "sessions": [],
    }


def test_ac1_err_wrong_shape_harness_is_refused_and_writes_nothing(tmp_path):
    """A codex-shaped id stamped `harness: claude` is the phantom-twin defect:
    ValueError naming the shape and the harness, node unchanged. The same id
    under its own harness stamps fine."""
    _plan, entry = _ready_plan_entry(tmp_path)
    path = _make_graph(tmp_path, [entry])
    codex_id = "01a06886-9405-74a1-8afd-5b67baf89604"

    with pytest.raises(
        ValueError, match=r"is a codex id; refusing harness claude"
    ):
        append_session_record(
            path, entry["id"], phase="execute", harness="claude", session_id=codex_id,
        )
    assert read_graph_strict(path)[0]["sessions"] == []

    found, added = append_session_record(
        path, entry["id"], phase="execute", harness="codex", session_id=codex_id,
    )
    assert (found, added) == (True, True)

    # A v4 id under claude is legal, and a grok thread carrying a minted v4
    # id is never refused on shape.
    found, added = append_session_record(
        path, entry["id"], phase="review", harness="claude",
        session_id="b936b571-e0aa-40ed-a07d-97acb9a87db1",
    )
    assert (found, added) == (True, True)
    found, added = append_session_record(
        path, entry["id"], phase="ship", harness="grok",
        session_id="8ad8e13c-1111-4222-8333-444455556666",
    )
    assert (found, added) == (True, True)


def test_sweep_kills_only_the_keeper_whose_graph_is_gone(tmp_path):
    """Positive control for the orphan sweep (the keeper-leak class of
    2026-09-04).

    The sweep's kill decision is the ``--graph`` path's existence, never the
    command line: the canonical keepers and the leaked ones share a command
    line, and an argv match killed the two processes serving the real graph
    on 2026-09-04. Two real sleepers advertise identical keeper argv; only
    the one whose graph directory is deleted may die. Both must be visible
    to the SAME probe the sweep uses before it acts - a zero-after read is
    meaningless unless the probe first named its target.
    """
    import shutil
    import subprocess
    import sys

    from fno.graph import store as store_mod

    def _advertised_keeper(graph: Path) -> subprocess.Popen:
        # A real child whose argv carries the keeper's flags. The sleeper
        # body ignores them; the ps scan must not.
        return subprocess.Popen(
            [sys.executable, "-c", "import time; time.sleep(60)",
             "--store-keeper", "--sock", f"{graph}.sock",
             "--graph", str(graph), "--session", "sweep-test"],
        )

    doomed_dir = tmp_path / "doomed"
    doomed_dir.mkdir()
    doomed_graph = doomed_dir / "graph.json"
    seed_graph(doomed_graph, [])
    doomed = _advertised_keeper(doomed_graph)
    kept_graph = tmp_path / "kept" / "graph.json"
    kept_graph.parent.mkdir()
    seed_graph(kept_graph, [])
    kept = _advertised_keeper(kept_graph)
    try:
        assert doomed.poll() is None and kept.poll() is None
        # Pre-kill census through the sweep's own probe, by pid not by
        # count: with both graphs present neither is a candidate (the
        # discriminator), and after the rmtree the doomed one must be a
        # NAMED candidate - the non-zero control for the sweep's kill.
        assert doomed.pid not in store_mod._orphaned_keeper_pids()
        shutil.rmtree(doomed_dir)
        candidates = store_mod._orphaned_keeper_pids()
        assert doomed.pid in candidates, (
            f"the graph-gone sleeper must be a named candidate before the "
            f"sweep; the probe saw {candidates}"
        )
        assert kept.pid not in candidates, (
            "the graph-alive sleeper must survive the probe: identical argv, "
            "the graph's existence decides"
        )

        assert store_mod.sweep_orphaned_keepers(timeout=15.0) == []
        doomed.wait(timeout=15)
        assert doomed.poll() is not None, "the graph-gone keeper must die"
        assert kept.poll() is None, "the graph-alive keeper must survive"
    finally:
        for proc in (doomed, kept):
            proc.kill()
            proc.wait(timeout=15)


def test_read_nodes_by_ids_returns_none_when_the_keeper_predates_the_command(tmp_path, monkeypatch):
    """AC10-EDGE: a stale keeper (installed worker behind the source) answers
    `unknown store method`; the fast path degrades to None and the caller
    falls back, so an old binary never breaks a current client."""
    from fno.graph import store as store_mod

    def stale_request(self, method, params):
        raise RuntimeError("store error (invalid): unknown store method \"read_ids\"")

    path = _make_graph(tmp_path, [{"id": "ab-1", "title": "One"}])
    monkeypatch.setattr(store_mod._Keeper, "request", stale_request)
    monkeypatch.setattr(store_mod._ExecClient, "request", stale_request)
    assert store_mod.read_nodes_by_ids(path, ["ab-1"]) is None


from fno.graph import store as store_mod  # noqa: E402 - the tx-loop section


def test_dead_socket_serves_by_exec_and_never_spawns(tmp_path, monkeypatch):
    """The spawn-needed branch execs a one-shot lane: no resident keeper is
    minted, so nothing holds the store to grow on. A live incumbent is still
    preferred, and the exec client answers typed helpers."""
    graph = tmp_path / "graph.json"
    seed_graph(graph, [])
    monkeypatch.setattr(
        store_mod,
        "_worker_binary",
        lambda: tmp_path / "absent-worker",
    )
    client = store_mod._client_for(graph)
    assert isinstance(client, store_mod._ExecClient)
    assert not hasattr(client, "sock"), "the exec transport opens no socket"

    # A live incumbent still rides the socket: the connect probe answers, so
    # _client_for returns the _Keeper without consulting the exec route.
    class _FakeStream:
        def close(self):
            pass

    monkeypatch.setattr(
        store_mod._Keeper,
        "_connect",
        lambda self: _FakeStream(),
    )
    keeper = store_mod._client_for(graph)
    assert isinstance(keeper, store_mod._Keeper)
    assert keeper.sock == store_mod.store_socket_for(graph)


_CHUNK_CAP = 8192


def test_recv_exact_refuses_silent_close_midframe():
    """AC1-ERR: a stream that goes silent after half the payload
    raises StoreUnavailable (state silent, keeper-closed wording), never a
    short frame."""
    from fno.graph.store import STATE_SILENT, StoreUnavailable, _recv_exact

    class _DyingStream:
        def __init__(self, serve: int):
            self.serve = serve

        def recv_into(self, view, n=None) -> int:
            if self.serve <= 0:
                return 0
            take = min(_CHUNK_CAP, len(view), self.serve)
            view[:take] = b"\x00" * take
            self.serve -= take
            return take

    stream = _DyingStream(serve=16_044_243 // 2)
    with pytest.raises(StoreUnavailable) as exc:
        _recv_exact(stream, 16_044_243)
    assert exc.value.state == STATE_SILENT
    assert exc.value.detail == "keeper closed the connection mid-frame"


def test_sent_write_against_stale_keeper_is_unconfirmed(monkeypatch):
    from fno.graph import store as store_mod

    client = store_mod._Keeper(Path("/tmp/graph.store.sock"))
    monkeypatch.setattr(
        store_mod._Keeper,
        "request",
        lambda self, method, params: (_ for _ in ()).throw(
            RuntimeError('store error (invalid): unknown store method "write_status"')
        ),
    )

    with pytest.raises(store_mod.WriteUnconfirmed) as exc:
        client._resolve_write(
            "r1", "commit_rows", store_mod.StoreUnavailable(store_mod.STATE_SILENT, "timed out")
        )
    assert exc.value.state == store_mod.STATE_UNCONFIRMED
    assert "commit_rows was sent" in str(exc.value)


def test_unconfirmed_commit_names_changed_ids_before_retrying(tmp_path, monkeypatch):
    from fno.graph import store as store_mod

    class UnconfirmedClient:
        path = tmp_path / "graph.json"

        def request(self, method, params):
            if method == "begin":
                return {
                    "version": "v1",
                    "entries": [{"id": "x-seed", "title": "seed", "type": "feature",
                                 "status": "idea", "priority": "p2"}],
                    "base_digests": {"x-seed": "d1"},
                }
            if method == "commit_rows":
                raise store_mod.WriteUnconfirmed(store_mod.STATE_UNCONFIRMED, "outcome unknown")
            raise AssertionError(f"unexpected method {method}")

    monkeypatch.setattr(store_mod, "_client_for", lambda _path: UnconfirmedClient())
    with pytest.raises(store_mod.WriteUnconfirmed) as exc:
        store_mod.commit_rows_via_store(
            UnconfirmedClient.path,
            lambda entries: entries + [{"id": "x-minted", "title": "minted", "type": "feature",
                                        "status": "idea", "priority": "p2"}],
        )
    assert exc.value.ids == ["x-minted"]
    assert "x-minted" in str(exc.value)
    assert "fno backlog get" in str(exc.value)


def test_write_connect_failure_says_write_was_not_sent(monkeypatch):
    from fno.graph import store as store_mod

    client = store_mod._Keeper(Path("/tmp/graph.store.sock"))

    def no_listener():
        raise store_mod.StoreUnavailable(store_mod.STATE_NO_LISTENER, "nothing is listening")

    monkeypatch.setattr(client, "_connect", no_listener)
    with pytest.raises(store_mod.StoreUnavailable) as exc:
        client.request("commit_rows", {})
    assert not isinstance(exc.value, store_mod.WriteUnconfirmed)
    assert "the write was not sent" in str(exc.value)


def test_cli_renders_invalid_store_refusal_as_one_line(monkeypatch, capsys):
    """AC1-HP: invalid keeper refusals are concise CLI errors."""
    from fno import cli
    from fno.graph import store

    def app():
        store._raise_store_error("invalid", "prose budget exceeded on x-1: limit=5000.")

    monkeypatch.setattr(cli, "app", app)
    with pytest.raises(SystemExit) as exc:
        cli.main()

    assert exc.value.code == 1
    assert capsys.readouterr().err == "Error: prose budget exceeded on x-1: limit=5000.\n"
