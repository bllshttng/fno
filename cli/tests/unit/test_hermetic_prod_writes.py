"""A hermetic run must not write to a live operator surface.

On 2026-08-27 six ``think_offered`` rows carrying the fixture node id
``x-2222aaaa`` sat in the developer's live ``.fno/events.jsonl`` and surfaced as
a born-with-why offer for a node that does not exist. The row's ``offer_line``
named ``/x/t.jsonl``, which is the default transcript path of ``_resolved`` in
``test_spawn_think.py`` and appears nowhere else in the repository.

The cause was a private path builder: ``spawn_think._events_path`` composed
``Path.cwd() / ".fno" / "events.jsonl"`` and so consulted neither
``FNO_EVENTS_PATH`` (the journal pin ``fno.hermetic`` exists to set) nor
``FNO_REPO_ROOT`` (which the leaking test's own fixture sets). A worktree's
``.fno/events.jsonl`` is a symlink to the canonical journal, so a worker
running the suite inside a worktree reached the operator's file.

These tests pin the fix at both altitudes: the caller that had the bug, and the
shared seam that stops the next hand-built path instead of letting it land in
production. The notification pair covers the same class on the other
surface a test can reach the operator through.
"""
from __future__ import annotations

from pathlib import Path

import pytest

from fno.events import HermeticEscapeError, _build, append_event
from fno.provenance import spawn_think as st

_DATA = {
    "node_id": "x-2222aaaa",
    "trigger": "birth",
    "presence": "away",
    "resolved": True,
    "offer_line": "/think x-2222aaaa",
    "rank": "env",
}


def _event() -> dict:
    return _build("think_offered", "backlog", dict(_DATA))


# ---------------------------------------------------------------------------
# The caller that had the bug
# ---------------------------------------------------------------------------


def test_unpathed_events_path_ignores_the_process_cwd(monkeypatch, tmp_path):
    """The specific regression: chdir must not steer the journal."""
    from fno.paths import project_events_json

    pinned = tmp_path / "sandbox" / "events.jsonl"
    monkeypatch.setenv("FNO_EVENTS_PATH", str(pinned))
    decoy = tmp_path / "decoy"
    decoy.mkdir()
    monkeypatch.chdir(decoy)
    assert st._events_path(None) != decoy / ".fno" / "events.jsonl"
    assert st._events_path(None) == project_events_json()


# ---------------------------------------------------------------------------
# The shared seam
# ---------------------------------------------------------------------------


def test_append_event_refuses_a_write_outside_the_sandbox(monkeypatch, tmp_path):
    """The guard that closes the class, for any path builder named or not."""
    home = tmp_path / "home"
    home.mkdir()
    outside = tmp_path / "checkout" / ".fno" / "events.jsonl"
    monkeypatch.setenv("FNO_TEST_HERMETIC", "1")
    monkeypatch.setenv("TMPDIR", str(home))
    monkeypatch.delenv("FNO_EVENTS_PATH", raising=False)

    with pytest.raises(HermeticEscapeError) as exc:
        append_event(_event(), outside)
    assert str(outside) in str(exc.value)
    # append_event itself refuses before its mkdir. Scoped deliberately to this
    # function: gate_escape's failure log lands beside the journal and used to
    # recreate the directory a refusal had just prevented. That leg carries its
    # own guard now, covered by the gate_escape test below.
    assert not outside.parent.exists()


def test_append_event_refuses_a_symlink_that_resolves_outside(monkeypatch, tmp_path):
    """Only the RESOLVED path is judged.

    A worktree's ``.fno/events.jsonl`` is a symlink to the canonical journal,
    which is the whole reason a test run reached the operator's file. Accepting
    the raw path when it merely SITS inside the sandbox reopens exactly that
    door: measured at 200 bytes of a production-shaped row landing in the
    outside file before the guard judged the realpath alone.
    """
    home = tmp_path / "home"
    home.mkdir()
    outside = tmp_path / "checkout" / ".fno"
    outside.mkdir(parents=True)
    target = outside / "events.jsonl"
    link = home / "events.jsonl"
    link.symlink_to(target)

    monkeypatch.setenv("FNO_TEST_HERMETIC", "1")
    monkeypatch.setenv("TMPDIR", str(home))
    monkeypatch.delenv("FNO_EVENTS_PATH", raising=False)

    with pytest.raises(HermeticEscapeError):
        append_event(_event(), link)
    assert not target.exists()


def test_gate_escape_failure_log_respects_the_sandbox(monkeypatch, tmp_path):
    """A refused journal must not be followed by its failure log recreating the
    directory. Measured before the fix: events.jsonl absent, and
    ``<outside>/.fno/gate_escape_emit_failures.jsonl`` present."""
    from fno.events.gate_escape import failure_log_path, record_emit_failure

    sandbox = tmp_path / "sandbox"
    sandbox.mkdir()
    monkeypatch.setenv("FNO_TEST_HERMETIC", "1")
    monkeypatch.setenv("TMPDIR", str(sandbox))
    monkeypatch.delenv("FNO_EVENTS_PATH", raising=False)

    outside = tmp_path / "checkout" / ".fno" / "events.jsonl"
    record_emit_failure(
        failure_log_path(outside), "n-1", "reason", HermeticEscapeError("refused")
    )
    assert not outside.parent.exists()


# ---------------------------------------------------------------------------
# The third declaration state: no pin at all, under a test runner (x-3d21)
# ---------------------------------------------------------------------------
#
# The pin being ABSENT used to be fail-open, so a lane that skipped the
# conftest chain resolved the operator root in silence. On 2026-09-06 one of
# them overwrote the live graph with a 64-byte fixture payload.

_OUTSIDE = Path("/nonexistent-operator-root/.fno/ledger.json")


def test_undeclared_root_refuses_under_pytest(monkeypatch):
    from fno.hermetic import UndeclaredStateRootError, declared_root

    monkeypatch.delenv("FNO_TEST_HERMETIC", raising=False)
    with pytest.raises(UndeclaredStateRootError) as excinfo:
        declared_root(_OUTSIDE)
    message = str(excinfo.value)
    assert str(_OUTSIDE) in message
    assert "OPERATOR" in message
    for declaration in (
        "FNO_TEST_HERMETIC=1",
        "FNO_TEST_HERMETIC=0",
        "use_tmpdir",
    ):
        assert declaration in message, f"the refusal must name {declaration}"


def test_no_pin_outside_a_test_process_is_production(monkeypatch):
    """Production never meets the refusal: the marker is the runner itself."""
    import sys

    from fno.hermetic import declared_root

    monkeypatch.delenv("FNO_TEST_HERMETIC", raising=False)
    monkeypatch.delitem(sys.modules, "pytest")
    assert declared_root(_OUTSIDE) == _OUTSIDE


def test_doctor_test_build_dir_stays_under_the_sandbox():
    """The child CARGO_BUILD_BUILD_DIR must live in the sandbox, never operator state.

    The first CI run after the build-dir feature shipped wrote 1298 cargo
    intermediates into the runner's real ``~/.fno/cargo-build``: the child env
    computed the value from the PARENT's state root, and the state canary
    refused the whole shard. This pins the value under the sandbox the child
    actually runs in.
    """
    from fno import test_cmd

    env = test_cmd._child_env(Path(__file__).resolve().parents[2])
    value = env["CARGO_BUILD_BUILD_DIR"]
    base = Path(value.split("{workspace-path-hash}")[0])
    assert base.is_relative_to(test_cmd._sandbox()), value
    assert value.endswith(".fno/cargo-build/{workspace-path-hash}")
