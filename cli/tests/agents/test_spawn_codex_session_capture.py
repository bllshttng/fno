"""US1 (x-86e4): capture a codex pane's session id at spawn.

Codex, like opencode, cannot be handed a pre-minted session id the way claude
can, so the id has to be discovered from the rollout store after the pane runs.
Without this capture every codex pane row carries ``harness_session_id=None``
forever, and ``_discover_from_registry`` drops a sid-less row before any name
matching runs -- so a live codex worker is unreachable by truth, peek, and the
mail name lane at once.

Coverage:
  - AC2-FR: a missed capture leaves the row live-only, never crashes the spawn.
  - The match is exact-directory + started-after-spawn, so a sibling worktree's
    session and a pre-existing session in the SAME cwd are both excluded.
  - Two same-cwd candidates stamp NEITHER row (the opencode ambiguity rule).

Every case drives an injected sessions dir: the suite must never read the
developer's real ``~/.codex``.
"""
from __future__ import annotations

import json
from datetime import datetime
from pathlib import Path

import pytest

from fno.agents import codex_pane, codex_rollout, mux_spawn
from fno.agents.discover import codex_session_ids_started_in
from fno.agents.mux_spawn import _backfill_codex_session_id

SID_A = "019cc081-de0d-7283-97cc-751c46742a07"
SID_B = "019cc082-1111-7283-97cc-751c46742a08"

T_SPAWN = "2026-07-24T18:00:00.000Z"
T_EARLIER = "2026-07-24T17:00:00.000Z"


def _ms(iso: str) -> int:
    return int(datetime.fromisoformat(iso).timestamp() * 1000)


def _rollout(root: Path, sid: str, cwd: str, started: str) -> Path:
    """Write a rollout jsonl whose first line is codex's real session_meta shape."""
    day = root / "2026" / "07" / "24"
    day.mkdir(parents=True, exist_ok=True)
    path = day / f"rollout-{started.replace(':', '-')}-{sid}.jsonl"
    path.write_text(
        json.dumps(
            {
                "timestamp": started,
                "type": "session_meta",
                "payload": {"id": sid, "timestamp": started, "cwd": cwd},
            }
        )
        + "\n",
        encoding="utf-8",
    )
    return path


def _no_sleep(_seconds: float) -> None:
    """Collapse the retry delay so tests do not pay it."""


# ---------------------------------------------------------------------------
# codex_session_ids_started_in: the store query
# ---------------------------------------------------------------------------


def test_sibling_worktree_in_another_cwd_is_excluded(tmp_path: Path) -> None:
    """Worktrees of one repo differ only by directory, so cwd must match exactly."""
    _rollout(tmp_path, SID_A, "/w/proj-other", T_SPAWN)
    assert (
        codex_session_ids_started_in(
            Path("/w/proj"), _ms(T_SPAWN) - 1, sessions_dir=tmp_path
        )
        == []
    )


def test_session_started_before_the_spawn_is_excluded(tmp_path: Path) -> None:
    """A codex session already open in this cwd is NOT the one we just spawned.

    Mtime cannot make this call: an older session that is still being typed into
    has a fresh mtime. Only the session_meta start timestamp separates them.
    """
    _rollout(tmp_path, SID_A, "/w/proj", T_EARLIER)
    assert (
        codex_session_ids_started_in(
            Path("/w/proj"), _ms(T_SPAWN), sessions_dir=tmp_path
        )
        == []
    )


# ---------------------------------------------------------------------------
# _backfill_codex_session_id: the spawn-side rule
# ---------------------------------------------------------------------------


def test_backfill_returns_the_unique_id(tmp_path: Path) -> None:
    _rollout(tmp_path, SID_A, "/w/proj", T_SPAWN)
    assert (
        _backfill_codex_session_id(
            Path("/w/proj"), 1, sessions_dir=tmp_path, sleep=_no_sleep
        )
        == SID_A
    )


def test_backfill_refuses_an_ambiguous_pair(tmp_path: Path) -> None:
    """Two panes racing in one cwd: stamping either row could point resume at
    the other pane's session, so neither gets an id."""
    _rollout(tmp_path, SID_A, "/w/proj", T_SPAWN)
    _rollout(tmp_path, SID_B, "/w/proj", "2026-07-24T18:00:01.000Z")
    assert (
        _backfill_codex_session_id(
            Path("/w/proj"), 1, sessions_dir=tmp_path, sleep=_no_sleep
        )
        is None
    )


def test_backfill_waits_out_a_transiently_unique_sibling(
    tmp_path: Path, monkeypatch
) -> None:
    """Codex P2 (#603): a sibling rollout surfacing before this pane's must not
    be stamped. The stability gate holds the first single sighting; when a second
    candidate appears on the next probe the backfill returns None rather than
    committing to the sibling."""
    from fno.agents import discover

    seq = iter([[SID_B], [SID_B, SID_A]])

    monkeypatch.setattr(
        discover, "codex_session_ids_started_in", lambda *_a, **_k: next(seq)
    )
    assert (
        _backfill_codex_session_id(
            Path("/w/proj"), _ms(T_SPAWN), sessions_dir=tmp_path, sleep=_no_sleep
        )
        is None
    )


# ---------------------------------------------------------------------------
# Race-free id via the child pid's open rollout (Codex P1, #603)
# ---------------------------------------------------------------------------


class _FakeOpenFile:
    def __init__(self, path: str) -> None:
        self.path = path


class _FakeProc:
    def __init__(self, files=(), children=()) -> None:
        self._files = list(files)
        self._children = list(children)

    def open_files(self) -> list:
        return self._files

    def children(self, recursive: bool = False) -> list:
        out = list(self._children)
        if recursive:
            for c in self._children:
                out += c.children(recursive=True)
        return out


class _FakePsutil:
    class NoSuchProcess(Exception):
        pass

    def __init__(self, proc) -> None:
        self._proc = proc

    def Process(self, pid: int):  # noqa: N802 (mirror psutil API)
        return self._proc


def _write_rollout_with_id(root: Path, filename_id: str, payload_id: str) -> Path:
    """A rollout whose filename UUID may differ from session_meta.payload.id, to
    prove the id is read from metadata, not the filename (Codex P2 r5, #603)."""
    day = root / "2026" / "07" / "24"
    day.mkdir(parents=True, exist_ok=True)
    path = day / f"rollout-2026-07-24T18-00-00-{filename_id}.jsonl"
    path.write_text(
        json.dumps(
            {
                "timestamp": T_SPAWN,
                "type": "session_meta",
                "payload": {"id": payload_id, "timestamp": T_SPAWN, "cwd": "/w/proj"},
            }
        )
        + "\n",
        encoding="utf-8",
    )
    return path


def test_codex_session_id_for_pid_uses_metadata_not_the_filename_uuid(
    tmp_path: Path,
) -> None:
    """Codex P2 r5 (#603): older turn-id layouts name the rollout by a UUID that
    is not session_meta.payload.id; the id must come from the metadata."""
    from fno.agents.mux_spawn import _codex_session_id_for_pid

    other = "11111111-2222-3333-4444-555566667777"  # filename UUID, not the session
    rollout = _write_rollout_with_id(tmp_path, other, SID_A)
    psu = _FakePsutil(_FakeProc([_FakeOpenFile(str(rollout))]))
    assert _codex_session_id_for_pid(4242, psutil_mod=psu) == SID_A


def test_codex_session_id_for_pid_refuses_an_ambiguous_tree(tmp_path: Path) -> None:
    """AC5-CON: the same-cwd sibling-safety property, at the one place that owns it.

    Two distinct rollouts open in one process tree means the correlator cannot say
    which session this pane is, so it must return None rather than pick. Without
    this the "siblings can never be cross-stamped" claim lives only in a docstring.
    """
    from fno.agents.mux_spawn import _codex_session_id_for_pid

    roll_a = _write_rollout_with_id(tmp_path / "a", SID_A, SID_A)
    roll_b = _write_rollout_with_id(tmp_path / "b", SID_B, SID_B)
    native = _FakeProc([_FakeOpenFile(str(roll_b))])
    psu = _FakePsutil(_FakeProc([_FakeOpenFile(str(roll_a))], children=[native]))
    assert _codex_session_id_for_pid(4242, psutil_mod=psu) is None


def test_backfill_with_child_pid_never_consults_the_cwd_store(
    tmp_path: Path, monkeypatch
) -> None:
    """Codex P1 (#603): with the child pid, the id comes race-free from the
    child's open rollout. The cwd/timestamp store is never read, so a sibling
    rollout present in the same cwd cannot be stamped onto this row."""
    from fno.agents import discover
    from fno.agents.mux_spawn import _backfill_codex_session_id

    rollout = _write_rollout_with_id(tmp_path, SID_A, SID_A)
    # A sibling the old discovery path would have picked up.
    _rollout(tmp_path, SID_B, "/w/proj", T_SPAWN)
    monkeypatch.setattr(
        discover,
        "codex_session_ids_started_in",
        lambda *_a, **_k: pytest.fail("cwd store was consulted on the pid path"),
    )
    psu = _FakePsutil(_FakeProc([_FakeOpenFile(str(rollout))]))
    assert (
        _backfill_codex_session_id(
            Path("/w/proj"),
            _ms(T_SPAWN),
            sessions_dir=tmp_path,
            child_pid=4242,
            sleep=_no_sleep,
            psutil_mod=psu,
        )
        == SID_A
    )


# ---------------------------------------------------------------------------
# mtime-bounded rollout scan (Codex P2 perf, #603)
# ---------------------------------------------------------------------------


# ---------------------------------------------------------------------------
# The app-server daemon oracle: codex 0.148 hands session ownership to a
# detached `codex app-server --remote-control` daemon, which holds the
# rollout fd the process-tree probe above expects to find in the pane's own
# tree - measured live at 0/20 binds. This oracle asks the daemon directly.
# ---------------------------------------------------------------------------


def test_daemon_bind_refuses_two_new_ids_as_ambiguous(monkeypatch) -> None:
    monkeypatch.setattr(
        codex_pane,
        "_codex_session_ids_loaded",
        lambda cwd: {SID_A, SID_B},
    )
    assert codex_pane._codex_daemon_candidate(Path("/w/proj"), set()) is None


def test_session_ids_loaded_filters_by_cwd_and_distinguishes_none_from_empty(
    monkeypatch,
) -> None:
    from fno.agents import discover as _discover

    monkeypatch.setattr(_discover, "_codex_daemon_threads_raw", lambda: None)
    assert codex_pane._codex_session_ids_loaded(Path("/w/proj")) is None

    monkeypatch.setattr(
        _discover,
        "_codex_daemon_threads_raw",
        lambda: [
            {"session_id": SID_A, "cwd": "/w/proj"},
            {"session_id": SID_B, "cwd": "/w/other"},
        ],
    )
    assert codex_pane._codex_session_ids_loaded(Path("/w/proj")) == {SID_A}
    assert codex_pane._codex_session_ids_loaded(Path("/w/other")) == {SID_B}
    assert codex_pane._codex_session_ids_loaded(Path("/w/nothing-here")) == set()


# ---------------------------------------------------------------------------
# _make_codex_bind_probe: the daemon oracle is stability-gated (the same
# candidate must repeat across two consecutive probes) and liveness-checked
# (a daemon-sourced id is not itself proof the pane is up) before it binds.
# ---------------------------------------------------------------------------


def _probe_kwargs(monkeypatch, **overrides):
    monkeypatch.setattr(codex_pane, "_CODEX_DAEMON_PROBE_INTERVAL_S", 0.0)
    monkeypatch.setattr(
        mux_spawn, "_backfill_codex_session_id", lambda *a, **k: None
    )
    kwargs = dict(
        cwd=Path("/w/proj"),
        spawn_started_ms=0,
        child_pid=4242,
        codex_sessions_dir=None,
        daemon_baseline_ids=set(),
        mux={"session": "main", "pane_id": 7},
        runner=lambda *a, **k: None,
    )
    kwargs.update(overrides)
    return kwargs


def test_daemon_candidate_binds_once_it_repeats_and_the_pane_is_alive(monkeypatch) -> None:
    monkeypatch.setattr(codex_pane, "_codex_daemon_candidate", lambda *a, **k: SID_A)
    monkeypatch.setattr(mux_spawn, "_mux_pane_alive", lambda *a, **k: True)
    oracle_used: list = []
    probe = codex_pane._make_codex_bind_probe(
        **_probe_kwargs(monkeypatch, oracle_used=oracle_used)
    )
    assert probe() is None
    assert probe() == SID_A
    assert oracle_used == ["daemon"]


def test_a_repeated_daemon_candidate_refuses_to_bind_a_dead_pane(monkeypatch) -> None:
    monkeypatch.setattr(codex_pane, "_codex_daemon_candidate", lambda *a, **k: SID_A)
    monkeypatch.setattr(mux_spawn, "_mux_pane_alive", lambda *a, **k: False)
    probe = codex_pane._make_codex_bind_probe(**_probe_kwargs(monkeypatch))
    assert probe() is None
    assert probe() is None


# ---------------------------------------------------------------------------
# The rollout witness (x-a409): a codex pane proves its own session id from
# the rollout fd its tree holds open, with a fail-closed daemon fallback for
# the post-x-a095 shape where the daemon holds the fd instead.
# ---------------------------------------------------------------------------


def test_codex_rollout_witness_prefers_the_tree_scan(monkeypatch) -> None:
    """A pid whose tree holds a rollout answers from the fd - the strong
    oracle - and the daemon is never consulted."""
    monkeypatch.setattr(
        "fno.claims.session_pid.resolve_session_pid", lambda from_pid=None: 4242
    )
    monkeypatch.setattr(
        codex_rollout, "_codex_session_ids_for_pid", lambda pid: frozenset({SID_A})
    )
    monkeypatch.setattr(
        "fno.agents.discover._codex_daemon_threads_raw",
        lambda *_a, **_k: (_ for _ in ()).throw(AssertionError("daemon probed")),
    )
    assert codex_rollout.codex_rollout_witness("codex") == frozenset({SID_A})


def test_codex_rollout_witness_daemon_cwd_mismatch_fails_closed(
    monkeypatch, tmp_path
) -> None:
    """A daemon thread with the right id at ANOTHER cwd is a bystander: the
    fallback carries no pid, so cwd is half its proof."""
    monkeypatch.setattr(
        "fno.claims.session_pid.resolve_session_pid", lambda from_pid=None: None
    )
    monkeypatch.chdir(tmp_path)
    monkeypatch.setenv("CODEX_THREAD_ID", SID_A)
    monkeypatch.setattr(
        "fno.agents.discover._codex_daemon_threads_raw",
        lambda *_a, **_k: [{"session_id": SID_A, "cwd": "/somewhere/else"}],
    )
    assert codex_rollout.codex_rollout_witness("codex") == frozenset()


def test_codex_rollout_witness_daemon_ambiguous_fails_closed(
    monkeypatch, tmp_path
) -> None:
    """Two daemon rows matching id and cwd cannot name one session: refuse."""
    monkeypatch.setattr(
        "fno.claims.session_pid.resolve_session_pid", lambda from_pid=None: None
    )
    monkeypatch.chdir(tmp_path)
    monkeypatch.setenv("CODEX_THREAD_ID", SID_A)
    monkeypatch.setattr(
        "fno.agents.discover._codex_daemon_threads_raw",
        lambda *_a, **_k: [
            {"session_id": SID_A, "cwd": str(tmp_path)},
            {"session_id": SID_A.upper(), "cwd": str(tmp_path)},
        ],
    )
    assert codex_rollout.codex_rollout_witness("codex") == frozenset()
