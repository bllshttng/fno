"""SIGINT-handling tests for `fno agents logs --follow` (AC2-FR).

Two test layers:

1. Unit-level — ``read_logs`` catches ``KeyboardInterrupt`` from the
   follow loop and returns exit 0 with no traceback on stderr. This is
   the load-bearing behavior the contract promises.

2. Subprocess-level — spawns the actual ``python -m fno.cli``
   process, lets it enter follow mode, sends ``SIGINT``, asserts a
   clean exit code and empty stderr. Skipped on platforms where
   ``signal.SIGINT`` does not behave as expected (e.g. Windows). The
   subprocess test is more brittle than the unit test, but it's the
   only check that catches real cleanup bugs the harness might mask.
"""
from __future__ import annotations

import io
import os
import select
import signal
import subprocess
import sys
import time

import pytest

from fno.paths_testing import use_tmpdir
from fno.agents.registry import AgentEntry, write_registry


def _codex(**kw) -> AgentEntry:
    base = dict(
        name="follow-target",
        harness="codex",
        cwd="/tmp",
        log_path="",
        harness_session_id="codex-followtest",
        created_at="2026-05-21T00:00:00Z",
        status="live",
        last_message_at=None,
    )
    base.update(kw)
    return AgentEntry(**base)


def test_keyboard_interrupt_during_the_tail_write_exits_clean(tmp_path, monkeypatch):
    """The window the subprocess test kept flaking on, pinned directly.

    The tail write is the readiness marker a follower waits on. A guard that
    opened only at the follow branch left that write unprotected, so a SIGINT
    arriving the moment the process looked ready took the default handler and
    killed it with 130. Interrupting the write itself is that exact window.
    """
    use_tmpdir(monkeypatch, tmp_path)
    log_file = tmp_path / "follow.jsonl"
    log_file.write_text('{"line": 1}\n', encoding="utf-8")
    write_registry([_codex(log_path=str(log_file))])

    from fno.agents import read as read_mod

    class InterruptingWriter(io.StringIO):
        def write(self, text: str) -> int:
            raise KeyboardInterrupt

    stderr_buf = io.StringIO()
    result = read_mod.read_logs(
        name="follow-target",
        tail=None,
        follow=True,
        stdout=InterruptingWriter(),
        stderr=stderr_buf,
    )

    assert result.exit_code == 0
    assert "traceback" not in stderr_buf.getvalue().lower()


def test_interrupt_on_a_one_shot_dump_is_not_swallowed(tmp_path, monkeypatch):
    """A truncated tail must not read as a complete one.

    The follow loop is a session the user ends on purpose, so it exits clean.
    A one-shot dump has no such contract: swallowing the interrupt returned 0
    with a half-written log, which a caller cannot tell from a full one.
    """
    use_tmpdir(monkeypatch, tmp_path)
    log_file = tmp_path / "follow.jsonl"
    log_file.write_text('{"line": 1}\n', encoding="utf-8")
    write_registry([_codex(log_path=str(log_file))])

    from fno.agents import read as read_mod

    class InterruptingWriter(io.StringIO):
        def write(self, text: str) -> int:
            raise KeyboardInterrupt

    # SystemExit(130), not a bare KeyboardInterrupt: cmd_logs traps neither, so
    # re-raising printed a traceback for an ordinary Ctrl-C. The code is what
    # makes a truncated dump distinguishable, and 130 carries it without one.
    with pytest.raises(SystemExit) as excinfo:
        read_mod.read_logs(
            name="follow-target",
            tail=None,
            follow=False,
            stdout=InterruptingWriter(),
            stderr=io.StringIO(),
        )
    assert excinfo.value.code == 130


def test_follow_jsonl_detects_window_spanning_truncate(tmp_path):
    """Truncate-then-refill within a single poll window must surface, not emit garbage.

    Without cross-iteration last_size tracking, this case would be missed:
    a writer truncates the file AND refills past our previous read offset
    before our next stat call. fh.tell() <= st_size so the offset-based
    check is silent, but the underlying records are different.

    READINESS HANDSHAKE (was @pytest.mark.flaky_socket; de-quarantined).
    -----------------------------------------------------------------
    The old version ran ``_follow_jsonl`` in the main thread and had a
    writer thread ``time.sleep(0.4)`` before truncating, gambling that the
    reader had attached (opened + seeked-to-end) within that fixed budget.
    Under a saturating concurrent build the reader could attach AFTER the
    one-shot truncate, never observe a size drop, and poll forever -> hang.
    The fix removes the sleep barrier: we run ``_follow_jsonl`` in a thread
    and keep APPENDING a sentinel line until the reader echoes one to stdout.
    The loop-feed is robust against the reader's seek-to-EOF timing (a single
    append could land before the seek and be skipped; a repeated append
    always reaches a reader polling at EOF). The echo is proof the reader is
    attached and polling, so the subsequent truncate is GUARANTEED to land
    inside its follow loop. No fixed sleep is load-bearing; the budgets below
    are bounded defense-in-depth.
    """
    import io as _io
    import threading

    from fno.agents import read as read_mod

    log = tmp_path / "follow.jsonl"
    # Initial content the producer wrote before we attached (skipped: the
    # follower seeks to EOF, so these never reach stdout).
    log.write_text('{"line": "first"}\n' * 20, encoding="utf-8")

    stdout_buf = _io.StringIO()
    stderr_buf = _io.StringIO()

    follow_done = threading.Event()

    def _run_follower():
        read_mod._follow_jsonl(log, stdout_buf, stderr_buf, poll_interval=0.05)
        follow_done.set()

    follow_thread = threading.Thread(target=_run_follower, daemon=True)
    follow_thread.start()

    # OBSERVED readiness barrier: keep appending a sentinel until the reader
    # echoes one. Looping (not a single append) defeats the seek-to-EOF race -
    # whenever the follower finishes seeking, a later sentinel reaches it.
    sentinel = '{"line": "__reader_ready__"}\n'
    deadline = time.monotonic() + 5.0
    while "__reader_ready__" not in stdout_buf.getvalue():
        with log.open("a", encoding="utf-8") as fh:
            fh.write(sentinel)
        assert time.monotonic() < deadline, "follower never echoed the readiness sentinel"
        time.sleep(0.02)

    # Now truncate-in-place then refill smaller than the original offset.
    # Inode unchanged, so the rotation branch is silent; the size drop is
    # what the detector must catch. The reader is provably polling, so this
    # lands inside its follow loop and the next poll surfaces it.
    log.write_text('{"line": "rotated"}\n' * 3, encoding="utf-8")

    # _follow_jsonl returns once it detects the shrink. Bounded join: a
    # timeout here would itself be a real detection bug, not a flake.
    assert follow_done.wait(timeout=5.0), "follower did not detect the truncate within 5s"
    follow_thread.join(timeout=1.0)
    assert not follow_thread.is_alive(), "follower thread did not exit cleanly"
    msg = stderr_buf.getvalue().lower()
    assert "truncat" in msg or "shrank" in msg


@pytest.mark.skipif(
    sys.platform == "win32",
    reason="SIGINT signal-delivery on Windows is process-group-specific.",
)
def test_subprocess_follow_clean_sigint_exit(tmp_path, monkeypatch):
    """AC2-FR end-to-end — spawn the CLI, send SIGINT, assert clean exit.

    Uses the codex provider path because:
    - It does not shell out to a real ``claude`` binary (which isn't on
      PATH in CI).
    - The poll loop is in our own code, so SIGINT cleanup is our
      responsibility (not delegated to a subprocess).

    The test prepares a codex registry entry whose log_path exists (so
    the verb doesn't take the "US4 not yet shipped" exit-13 branch),
    spawns fno via ``python -m fno.cli``, lets it enter the
    follow loop, sends ``SIGINT``, and asserts the process exits with
    code 0 and no traceback on stderr.
    """
    use_tmpdir(monkeypatch, tmp_path)
    log_file = tmp_path / "follow.jsonl"
    log_file.write_text('{"seed": true}\n', encoding="utf-8")
    write_registry([_codex(log_path=str(log_file))])

    # Pass FNO_CONFIG through the env so the spawned process
    # resolves to the same tmp registry.
    env = os.environ.copy()
    env["FNO_CONFIG"] = str(tmp_path / ".fno" / "settings.yaml")
    # PYTHONUNBUFFERED so the seed line flushes before we SIGINT.
    env["PYTHONUNBUFFERED"] = "1"

    # Run as ``python -m fno.cli`` rather than relying on a
    # console-scripts entry point that may not be installed in CI.
    cmd = [
        sys.executable,
        "-m",
        "fno.cli",
        "agents",
        "logs",
        "follow-target",
        "--follow",
    ]
    proc = subprocess.Popen(
        cmd,
        env=env,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        cwd=str(tmp_path),
    )
    try:
        # Wait for the process to actually reach the follow loop before
        # signalling, rather than a fixed sleep. read_logs writes the JSONL
        # tail (the seed line) to stdout once it has loaded the registry and
        # is about to enter _follow_jsonl, whose caller traps KeyboardInterrupt
        # for the clean exit. Under pytest-xdist load the heavy
        # ``python -m fno.cli`` import is slow, so SIGINT that lands during
        # startup is caught by the default handler and kills the process
        # (returncode 130) - the same load-sensitivity family the suite is
        # being hardened against.
        # The readiness wait has to be load-BEARING. Falling through on timeout
        # and signalling anyway is what makes this test flaky: SIGINT lands
        # during startup, the default handler kills the process (130), and the
        # failure reads as "SIGINT handling is broken" when startup was merely
        # slow. Distinguish the two, and give startup room - a genuine hang is
        # still caught by the 5s post-signal communicate() timeout below.
        deadline = time.monotonic() + 60.0
        reached_loop = False
        while time.monotonic() < deadline:
            if proc.poll() is not None:
                pytest.fail(
                    f"follow exited before reaching the loop;"
                    f" returncode={proc.returncode}"
                )
            ready, _, _ = select.select([proc.stdout], [], [], 0.2)
            if ready and proc.stdout.readline():
                reached_loop = True
                break   # tail (seed) line written -> entering the follow loop
        if not reached_loop:
            proc.kill()
            proc.communicate()
            pytest.fail(
                "follow never reached the loop within 60s, so SIGINT was never"
                " sent; this is a startup-speed problem, NOT a SIGINT-handling"
                " failure"
            )
        proc.send_signal(signal.SIGINT)
        try:
            stdout, stderr = proc.communicate(timeout=5.0)
        except subprocess.TimeoutExpired:
            proc.kill()
            stdout, stderr = proc.communicate()
            pytest.fail(
                f"follow did not exit within 5s of SIGINT;"
                f" stdout={stdout!r} stderr={stderr!r}"
            )
    finally:
        if proc.poll() is None:
            proc.kill()

    assert proc.returncode == 0, (
        f"expected clean SIGINT exit, got returncode={proc.returncode}\n"
        f"stdout={stdout!r}\nstderr={stderr!r}"
    )
    decoded_err = stderr.decode("utf-8", errors="replace")
    assert "traceback" not in decoded_err.lower(), (
        f"SIGINT produced a traceback:\n{decoded_err}"
    )
