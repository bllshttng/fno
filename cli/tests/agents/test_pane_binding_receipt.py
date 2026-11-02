"""x-cdca: a pane spawn must not report "a pane exists" as "a worker is running".

The defect: ``dispatch_spawn_pane`` created a codex pane, failed to bind a
session within a 3.0s window, and returned an exit-0 receipt carrying
``status: spawning`` and an empty ``short_id``. A pane that would bind in four
seconds and a pane that had already died produced BYTE-IDENTICAL receipts, so
a caller had no way to tell a slow starter from a corpse -- and re-prompted the
corpse.

Coverage:
  - the three outcomes of ``_await_pane_binding`` and the exit code each earns
  - ambiguity resolves to still-booting, never to died (the asymmetry rule)
  - death evidence is captured BEFORE the pane is gone, and never fails a spawn
  - every unbound receipt names a reason; ``bound`` is tri-state by harness
  - a regression pin: the production codex binding path never keys on rollout
    mtime (the misattribution the node reported, whose code is already gone)
"""
from __future__ import annotations

import json
import subprocess
from typing import Any

import pytest

from fno.agents import mux_spawn


@pytest.fixture(autouse=True)
def _graphless_seed_resolver(monkeypatch):
    """The seed-word gate fails open on an unreadable graph; pin that here so
    the plumbing tests' fake node words never depend on the ambient store."""

    def _unreadable(*_a, **_k):
        raise ValueError("graph store unreadable")

    monkeypatch.setattr("fno.rust_binary.backlog_match", _unreadable)

MUX = {"session": "main", "pane_id": 81}
SID = "019cc081-de0d-7283-97cc-751c46742a07"
#: `fno mux pane wait --timeout 0` exit codes: 12 = the child exited
#: (EXIT_WAIT_EXITED, a POSITIVE death marker), 0/11 = still up, anything else
#: = the mux could not answer.
WAIT_DEAD = 12
WAIT_ALIVE = 0
WAIT_UNKNOWN = 3


def _proc(returncode: int = 0, stdout: str = "", stderr: str = "") -> subprocess.CompletedProcess:
    return subprocess.CompletedProcess(args=[], returncode=returncode, stdout=stdout, stderr=stderr)


def _runner(*, wait_rc: int = WAIT_ALIVE, tail: str = "", read_rc: int = 0, raises=None):
    """A fake `fno mux` answering every command the binding loop calls.

    The `ls` arm is EXPLICIT and lists our pane on purpose. An earlier version
    fell through to `_proc(0)` with empty stdout, so `json.loads("")` raised and
    the absence probe returned None - which made the ambiguity tests pass for a
    reason the real CLI never produces (it answers exit 0 with `[]`). A fake that
    supplies the answer the code is waiting for is how this PR shipped a broken
    death detector twice; the fake must model the real command, not a convenient one.
    """
    calls: list[list[str]] = []

    def run(argv, **_kw):
        calls.append(argv)
        # argv is [fno, "mux", "pane", <command>, ...]
        command = argv[3] if len(argv) > 3 else ""
        if command == "wait":
            return _proc(wait_rc)
        if command == "read":
            if raises is not None:
                raise raises
            return _proc(read_rc, tail)
        if command == "ls":
            return _proc(0, json.dumps([{"pane_id": MUX["pane_id"]}, {"pane_id": 99}]))
        return _proc(0)

    run.calls = calls  # type: ignore[attr-defined]
    return run


def _no_sleep(_seconds: float) -> None:
    """Collapse the poll delay so tests do not pay it."""


# ---------------------------------------------------------------------------
# The three outcomes
# ---------------------------------------------------------------------------


# ---------------------------------------------------------------------------
# The codex app-server daemon oracle, at the receipt level: composed through
# _make_codex_bind_probe into the same _await_pane_binding wait this file
# already exercises.
# ---------------------------------------------------------------------------


# ---------------------------------------------------------------------------
# Evidence: captured before the pane is gone, and never fatal
# ---------------------------------------------------------------------------


# ---------------------------------------------------------------------------
# The receipt invariant, on BOTH production callers of dispatch_spawn_pane
# ---------------------------------------------------------------------------


# ---------------------------------------------------------------------------
# Name reclamation is scoped to PROVED death, not to the terminal vocabulary
# ---------------------------------------------------------------------------


# ---------------------------------------------------------------------------
# `bound` is TRI-state: a harness that binds no session asserts nothing
# ---------------------------------------------------------------------------


# --- an argv seed's delivery is not observable from a pane frame -------------
# Measured 2026-08-22: three spawns minutes apart. Pane 93 reported
# `seed: unattempted` while the pane already showed the command and the agent was
# running it, and pane 96 reported the same word for a pane that genuinely got
# nothing. One word, two opposite realities, and the false reading correlates
# with machine load because frame paint is slower when the box is busy.
#
# The seed rides in the process argv for every pane harness except agy, so its
# delivery is committed at exec time, before a pane exists to read. The spawn
# knows what it put in the argv; that positive fact is what the receipt reports
# now, instead of inferring delivery from the seed's ABSENCE in a painted frame.


# --- AC7: the docstring's UNREACHABLE clause, asserted as a property ---------
# `_submit_spawn_seed.__doc__` has claimed since PR 1078 that `unattempted` is
# unreachable when `seed_in_argv` is set. One arm returned it anyway. A test per
# arm cannot stop that recurring: a NEW arm arrives with no test and the claim
# quietly goes false again. So the property is asserted over every arm, and the
# scenario table is proven to reach every arm rather than trusted to.

TRUST_FRAME = "Do you trust this folder?"


def _seed_scenarios() -> list[tuple[str, str, Any]]:
    """(name, provider, make_runner) covering every return in `_submit_spawn_seed`.

    The third element BUILDS a runner rather than being one. Each scenario runs
    more than once (both `seed_in_argv` values), and the agy arms consume a
    two-read script, so a shared runner would exhaust its own iterator on the
    second run and fail as a StopIteration nowhere near the cause.
    """
    from fno.agents.mux_spawn import DispatchAskError

    def reader(read_rc: int, read_out: str, send=None, second_read=None):
        def make():
            reads = iter([(read_rc, read_out), second_read or (read_rc, read_out)])

            def run(argv, **_kw):
                command = argv[3]
                if command == "wait":
                    return _proc(WAIT_ALIVE)
                if command == "read":
                    nxt = next(reads)
                    if nxt == "raise":
                        raise DispatchAskError("mux read failed", exit_code=1)
                    return _proc(nxt[0], nxt[1])
                if command == "send":
                    if send == "raise":
                        raise DispatchAskError("mux send failed", exit_code=1)
                    return _proc(0 if send is None else send)
                raise AssertionError(argv)

            return run

        return make

    def raising_read():
        def run(argv, **_kw):
            if argv[3] == "read":
                raise DispatchAskError("mux read failed", exit_code=1)
            raise AssertionError(argv)

        return run

    return [
        ("read_raises", "claude", raising_read),
        ("read_failed_blank", "claude", reader(1, "")),
        ("read_blank_ok", "claude", reader(0, "   \n")),
        ("frame_without_seed", "claude", reader(0, "idle composer")),
        ("frame_with_seed", "claude", reader(0, "seed text")),
        ("send_raises", "claude", reader(0, "seed text", send="raise")),
        ("send_refused", "claude", reader(0, "seed text", send=17)),
        ("agy_trust_submit_raises", "agy", reader(0, TRUST_FRAME, send="raise")),
        ("agy_trust_submit_refused", "agy", reader(0, TRUST_FRAME, send=17)),
        (
            "agy_trust_readback_raises",
            "agy",
            reader(0, TRUST_FRAME, second_read="raise"),
        ),
        (
            "agy_trust_modal_persists",
            "agy",
            reader(0, TRUST_FRAME, second_read=(0, TRUST_FRAME)),
        ),
    ]


# ---------------------------------------------------------------------------
# A probe never fails a spawn (the pane already exists by the time it runs)
# ---------------------------------------------------------------------------


def test_a_raising_liveness_probe_answers_unknown_instead_of_escaping() -> None:
    """An escape here orphans a live pane with no registry row, which is worse
    than every reason the probe might have failed. Held to the same standard as
    the tail read it sits beside."""

    def run(_argv, **_kw):
        raise RuntimeError("x")

    assert mux_spawn._mux_pane_alive(MUX, run) is None


# ---------------------------------------------------------------------------
# The reaped-pane case, which is the NORMAL death and the one exit 12 misses
# ---------------------------------------------------------------------------


def _ls_runner(*, wait_rc: int, panes: list, ls_rc: int = 0, ls_out=None):
    """A mux whose `pane wait` answers `wait_rc` and whose `pane ls` lists `panes`."""

    def run(argv, **_kw):
        command = argv[3] if len(argv) > 3 else ""
        if command == "wait":
            return _proc(wait_rc)
        if command == "ls":
            body = ls_out if ls_out is not None else json.dumps(
                [{"pane_id": p} for p in panes]
            )
            return _proc(ls_rc, body)
        if command == "read":
            return _proc(0, "")
        return _proc(0)

    return run


def test_a_reaped_pane_is_death_even_though_wait_cannot_say_so() -> None:
    """When a pane's child exits the mux drops the pane, so a later `pane wait`
    finds nothing to watch and returns the GENERIC error code 1 - which also
    covers io failure, version skew, and server error. Exit 12 only fires when
    the child dies while a watcher is already subscribed, which a --timeout 0
    probe almost never is. Without the listing fallback the death branch would
    be near-unreachable and a corpse would land as `spawning`."""
    assert mux_spawn._mux_pane_alive(MUX, _ls_runner(wait_rc=1, panes=[99])) is False


@pytest.mark.parametrize(
    "kwargs",
    [
        {"ls_rc": 1},                    # the enumeration itself failed
        # THE ONE THAT MATTERS: `fno mux pane ls --json` prints exactly this and
        # exits 0 when the session socket is refused or absent, so an empty list
        # is "I could not reach the session" and "the session is empty" wearing
        # the same bytes. Reading it as death condemns a live worker whenever the
        # mux is briefly unreachable - and this helper's False is the condemn
        # signal for reconcile and reachability.pane_falsifier, not just spawn.
        {"ls_out": "[]"},
    ],
)
def test_an_unsuccessful_enumeration_is_never_read_as_death(kwargs) -> None:
    """The positive control is a NON-EMPTY listing. Without one, "not in the
    list" is an absence with two explanations."""
    assert mux_spawn._mux_pane_alive(MUX, _ls_runner(wait_rc=1, panes=[], **kwargs)) is None


# ---------------------------------------------------------------------------
# Regression pin for the node's claim 4 (already fixed; this keeps it fixed)
# ---------------------------------------------------------------------------


# ---------------------------------------------------------------------------
# x-1595: a blown bind window reports the CONDITION it observed, never only
# the clock.
#
# Four codex pane spawns died at `binding-window-expired` on 2026-08-27/28.
# The word names a CLOCK; the real condition was never reported, so three
# confident causes (machine load, a source-ahead registry schema, a dead shared
# daemon) were each built on circumstance and each died against its own
# control. The refusal also routed the operator to `fno doctor --codex-bind`,
# which binds a canary in a scratch cwd with its own baseline and read green at
# 3.39s and 0.92s while the spawns were dying: a green control aimed at a path
# the failure does not take is not evidence.
# ---------------------------------------------------------------------------


