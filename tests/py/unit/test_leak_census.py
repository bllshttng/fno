"""Real-process tests for the leak census (x-ec81 change 2).

Every case spawns a REAL process tree - never a fabricated table - because
the unit under test is a live-process reaper, and a fake table proves the
fake. Each orphan's real ppid is passed as ``reaper`` so the census's
orphan-only cwd read (_leak_census.iter_processes reads cwd for
``ppid in reaper_set(reaper)`` only) sees exactly the trees this test made,
under any subreaper arrangement Linux provides.
"""
from __future__ import annotations

import os
import subprocess
import time

from _leak_census import reap_rooted

_SLEEP = 60  # seconds; every spawned process is killed by the test or reaper


def _spawn_rooted(cwd, command: str) -> subprocess.Popen:
    proc = subprocess.Popen(
        ["/bin/sh", "-c", command],
        cwd=str(cwd),
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
    )
    # Give the shell a moment to fork its children so the tree is real
    # before the census reads it.
    time.sleep(0.3)
    return proc


def _wait_children(proc: subprocess.Popen, count: int, timeout_s: float = 5.0) -> bool:
    """Poll until ``proc`` has at least ``count`` live children.

    A fixed sleep here is a load flake: a CI runner can delay the fork past
    any constant, so the tree test polls for the shape it asserts on.
    """
    import psutil

    deadline = time.monotonic() + timeout_s
    while time.monotonic() < deadline:
        try:
            if len(psutil.Process(proc.pid).children()) >= count:
                return True
        except psutil.NoSuchProcess:
            return False
        time.sleep(0.05)
    return False


def test_reap_rooted_takes_the_whole_tree(tmp_path):
    """An orphan shell rooted under the root owns a descendant rooted
    elsewhere ('/'); the descendant comes back too, and everything is
    terminal within 5 s."""
    leaf = tmp_path / "leaf"
    leaf.mkdir()
    # Two background children: one sleep inheriting the leaf cwd, one
    # subshell that cds to / before exec'ing sleep. The /-rooted sleep is
    # NOT under the root, so the tree capture - not the cwd match - is what
    # reaches it.
    shell = _spawn_rooted(
        leaf,
        f"/bin/sleep {_SLEEP} & (cd / && exec /bin/sleep {_SLEEP}) & wait",
    )
    try:
        assert shell.poll() is None
        assert _wait_children(shell, 2), "shell never forked its two children"

        started = time.monotonic()
        rows = reap_rooted([str(tmp_path)], reaper=os.getpid())
        elapsed = time.monotonic() - started

        pids = {r["pid"] for r in rows}
        assert shell.pid in pids
        # The /-rooted descendant: captured through the tree, so its report
        # row carries no matched cwd of its own.
        assert any(
            r["root_pid"] == shell.pid and r["cwd"] is None for r in rows
        ), rows
        assert all(r["terminal"] for r in rows), rows
        assert elapsed < 5, elapsed
        assert shell.poll() is not None
    finally:
        shell.kill()
        shell.wait()
