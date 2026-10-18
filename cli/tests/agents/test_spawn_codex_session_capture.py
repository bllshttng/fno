"""US1 (x-86e4): capture a codex pane's session id at spawn.

Codex, like opencode, cannot be handed a pre-minted session id the way claude
can, so the id has to be discovered from the rollout store after the pane runs.
Without this capture every codex pane row carries ``harness_session_id=None``
forever, and ``_discover_from_registry`` drops a sid-less row before any name
matching runs -- so a live codex worker is unreachable by truth, peek, and the
mail name lane at once.

Coverage:
  - The id is read race-free from the rollout the pane's own process tree holds
    open; an ambiguous tree binds nothing.
  - The app-server daemon oracle: one new id binds, gated on a repeat probe and
    a live pane.
  - The self-identity rollout witness and its fail-closed daemon fallback.

Every case drives injected rollouts and stubs: the suite must never read the
developer's real ``~/.codex``.
"""
from __future__ import annotations

import json
from pathlib import Path

from fno.agents import codex_pane, codex_rollout, mux_spawn

SID_A = "019cc081-de0d-7283-97cc-751c46742a07"
SID_B = "019cc082-1111-7283-97cc-751c46742a08"

T_SPAWN = "2026-07-24T18:00:00.000Z"


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
    is not session_meta.payload.id; the id must come from the metadata, read
    race-free from the pane's own open rollout."""
    from fno.agents.mux_spawn import _codex_session_id_for_pid

    other = "11111111-2222-3333-4444-555566667777"  # filename UUID, not the session
    rollout = _write_rollout_with_id(tmp_path, other, SID_A)
    psu = _FakePsutil(
        _FakeProc([_FakeOpenFile(str(rollout)), _FakeOpenFile("/unrelated.log")])
    )
    assert _codex_session_id_for_pid(4242, psutil_mod=psu) == SID_A


def test_codex_session_id_for_pid_walks_wrapper_descendants(tmp_path: Path) -> None:
    """Codex P1 r5 (#603): a wrapper launcher (@openai/codex Node shim) holds the
    pane pid while its native child opens the rollout; the descendant walk finds it."""
    from fno.agents.mux_spawn import _codex_session_id_for_pid

    rollout = _write_rollout_with_id(tmp_path, SID_A, SID_A)
    native = _FakeProc([_FakeOpenFile(str(rollout))])
    wrapper = _FakeProc(files=(), children=[native])  # pane pid is the wrapper
    psu = _FakePsutil(wrapper)
    assert _codex_session_id_for_pid(4242, psutil_mod=psu) == SID_A


def test_codex_session_id_for_pid_binds_when_the_tree_is_unambiguous(
    tmp_path: Path,
) -> None:
    """One rollout open twice across the tree is ONE session and still binds:
    the ambiguity refusal counts distinct ids, not open files."""
    from fno.agents.mux_spawn import _codex_session_id_for_pid

    rollout = _write_rollout_with_id(tmp_path / "a", SID_A, SID_A)
    native = _FakeProc([_FakeOpenFile(str(rollout))])
    psu = _FakePsutil(_FakeProc([_FakeOpenFile(str(rollout))], children=[native]))
    assert _codex_session_id_for_pid(4242, psutil_mod=psu) == SID_A


# ---------------------------------------------------------------------------
# The app-server daemon oracle: codex 0.148 hands session ownership to a
# detached `codex app-server --remote-control` daemon, which holds the
# rollout fd the process-tree probe above expects to find in the pane's own
# tree - measured live at 0/20 binds. This oracle asks the daemon directly.
# ---------------------------------------------------------------------------


def test_daemon_bind_accepts_the_one_new_id_for_this_cwd(monkeypatch) -> None:
    monkeypatch.setattr(
        codex_pane,
        "_codex_session_ids_loaded",
        lambda cwd: {SID_A, SID_B},
    )
    assert codex_pane._codex_daemon_candidate(Path("/w/proj"), {SID_B}) == SID_A


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


def test_session_ids_loaded_selects_the_requested_codex_home(monkeypatch) -> None:
    from fno.agents import discover as _discover

    seen: dict[str, str] = {}

    def threads(*, env):
        seen["codex_home"] = env["CODEX_HOME"]
        return []

    monkeypatch.setattr(_discover, "_codex_daemon_threads_raw", threads)

    assert codex_pane._codex_session_ids_loaded(
        Path("/w/proj"), codex_home=Path("/tmp/canary-home")
    ) == set()
    assert seen["codex_home"] == "/tmp/canary-home"


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


def test_daemon_candidate_changing_between_probes_never_binds(monkeypatch) -> None:
    seen = iter([SID_A, SID_B, SID_B])
    monkeypatch.setattr(codex_pane, "_codex_daemon_candidate", lambda *a, **k: next(seen))
    monkeypatch.setattr(mux_spawn, "_mux_pane_alive", lambda *a, **k: True)
    probe = codex_pane._make_codex_bind_probe(**_probe_kwargs(monkeypatch))
    assert probe() is None  # SID_A observed, nothing to compare yet
    assert probe() is None  # SID_B != SID_A, restarts the stability count
    assert probe() == SID_B  # SID_B repeats, pane alive -> trusted


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


def test_codex_session_ids_for_pid_returns_every_distinct_id(tmp_path: Path) -> None:
    """The set view of the tree scan: two rollouts in one tree surface both ids,
    while the one-element wrapper still refuses an ambiguous tree (None)."""
    from fno.agents.codex_rollout import _codex_session_id_for_pid, _codex_session_ids_for_pid

    roll_a = _write_rollout_with_id(tmp_path / "a", SID_A, SID_A)
    roll_b = _write_rollout_with_id(tmp_path / "b", SID_B, SID_B)
    proc = _FakeProc([_FakeOpenFile(str(roll_a)), _FakeOpenFile(str(roll_b))])
    psu = _FakePsutil(proc)
    assert _codex_session_ids_for_pid(4242, psutil_mod=psu) == {SID_A, SID_B}
    assert _codex_session_id_for_pid(4242, psutil_mod=psu) is None


def test_codex_rollout_witness_skips_non_codex(monkeypatch) -> None:
    """Only codex gets the rollout witness; nothing is probed for any other
    harness (no psutil walk, no daemon round trip)."""
    monkeypatch.setattr(
        codex_rollout, "_codex_session_ids_for_pid",
        lambda *_a, **_k: (_ for _ in ()).throw(AssertionError("tree scanned")),
    )
    monkeypatch.setattr(
        "fno.agents.discover._codex_daemon_threads_raw",
        lambda *_a, **_k: (_ for _ in ()).throw(AssertionError("daemon probed")),
    )
    assert codex_rollout.codex_rollout_witness("claude") == frozenset()


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


def test_codex_rollout_witness_daemon_fallback_unique_row(monkeypatch, tmp_path) -> None:
    """Tree scan empty (the x-a095 shape): one daemon thread matching the
    present CODEX_THREAD_ID at this exact cwd witnesses the id."""
    monkeypatch.setattr(
        "fno.claims.session_pid.resolve_session_pid", lambda from_pid=None: None
    )
    monkeypatch.chdir(tmp_path)
    monkeypatch.setenv("CODEX_THREAD_ID", SID_A)
    monkeypatch.setattr(
        "fno.agents.discover._codex_daemon_threads_raw",
        lambda *_a, **_k: [{"session_id": SID_A, "cwd": str(tmp_path)}],
    )
    assert codex_rollout.codex_rollout_witness("codex") == frozenset({SID_A})


def test_codex_rollout_witness_daemon_unavailable_fails_closed(monkeypatch) -> None:
    """None from the daemon oracle (down, timeout, bad JSON) witnesses nothing."""
    monkeypatch.setattr(
        "fno.claims.session_pid.resolve_session_pid", lambda from_pid=None: None
    )
    monkeypatch.setenv("CODEX_THREAD_ID", SID_A)
    monkeypatch.setattr(
        "fno.agents.discover._codex_daemon_threads_raw", lambda *_a, **_k: None
    )
    assert codex_rollout.codex_rollout_witness("codex") == frozenset()


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


def test_codex_rollout_witness_blank_thread_fails_closed(monkeypatch) -> None:
    """No CODEX_THREAD_ID in env gives the fallback nothing to match; it never
    guesses from cwd alone."""
    monkeypatch.setattr(
        "fno.claims.session_pid.resolve_session_pid", lambda from_pid=None: None
    )
    monkeypatch.delenv("CODEX_THREAD_ID", raising=False)
    monkeypatch.setattr(
        "fno.agents.discover._codex_daemon_threads_raw",
        lambda *_a, **_k: (_ for _ in ()).throw(AssertionError("daemon probed")),
    )
    assert codex_rollout.codex_rollout_witness("codex") == frozenset()
