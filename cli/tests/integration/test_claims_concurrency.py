"""Multi-process integration tests for fno agents claim concurrency.

Uses multiprocessing.Process to drive real-process contention on the same
filesystem path. Flakiness here is a regression.

These tests do NOT use threads because the O_EXCL race we exercise is at
the kernel level - threads would share file descriptors and produce
unrealistic results.
"""
from __future__ import annotations

import multiprocessing as mp
from pathlib import Path


from fno.claims.core import (
    ClaimHeldByOther,
    acquire_claim,
    claim_status,
    release_claim,
)

_PROCESS_START_TIMEOUT_SECONDS = 30.0
_RACE_HOLD_SECONDS = 2.0


def _try_acquire(root_str: str, key: str, holder: str, result_queue, hold_secs: float = 0.0) -> None:
    """Child-process worker. Reports outcome via the queue.

    If hold_secs > 0, a winner sleeps that long before exiting so its PID
    stays alive past the assertion. Without this, a winner exits, its PID
    dies, and a sibling worker may legitimately stale-recover - which is
    correct system behavior but breaks the "exactly one winner" invariant
    the test wants to assert.
    """
    import time as _t
    try:
        claim = acquire_claim(key=key, holder=holder, root=Path(root_str))
        result_queue.put(("won", holder, claim.acquired_at))
        if hold_secs > 0:
            _t.sleep(hold_secs)
    except ClaimHeldByOther as exc:
        result_queue.put(("lost", holder, exc.holder))
    except Exception as exc:
        result_queue.put(("error", holder, repr(exc)))


def _run_race(
    root: Path, key: str, n_workers: int, hold_secs: float = _RACE_HOLD_SECONDS
) -> list[tuple]:
    """Spawn n_workers processes racing on (key); return list of outcomes.

    hold_secs keeps the winner's process alive through the native verdict
    subprocess so siblings cannot validly stale-recover. Losers report their
    outcome and exit immediately - their PID dying does not affect the assertion.
    """
    ctx = mp.get_context("spawn")
    queue = ctx.Queue()
    procs = []
    for i in range(n_workers):
        p = ctx.Process(
            target=_try_acquire,
            args=(str(root), key, f"worker-{i}", queue, hold_secs),
        )
        procs.append(p)

    # Start all then collect outcomes BEFORE joining so the winner's
    # process is still alive while siblings make their decisions.
    for p in procs:
        p.start()

    outcomes: list[tuple] = []
    deadline = mp_now() + _PROCESS_START_TIMEOUT_SECONDS
    while len(outcomes) < n_workers and mp_now() < deadline:
        try:
            outcomes.append(queue.get(timeout=0.5))
        except Exception:
            continue

    for p in procs:
        p.join(timeout=_PROCESS_START_TIMEOUT_SECONDS)

    return outcomes


def mp_now() -> float:
    import time as _t
    return _t.monotonic()


# ---------------------------------------------------------------------------
# Concurrent acquire race
# ---------------------------------------------------------------------------


def test_two_processes_race_one_wins(tmp_path):
    """Exactly one worker wins; the other gets ClaimHeldByOther."""
    outcomes = _run_race(tmp_path, key="race-key", n_workers=2)
    wins = [o for o in outcomes if o[0] == "won"]
    losses = [o for o in outcomes if o[0] == "lost"]
    errors = [o for o in outcomes if o[0] == "error"]
    assert len(wins) == 1, f"expected 1 winner, got {outcomes}"
    assert len(losses) == 1, f"expected 1 loser, got {outcomes}"
    assert errors == [], f"errors {errors}"


# ---------------------------------------------------------------------------
# Worktree canonical-root resolution for claims
# ---------------------------------------------------------------------------


def test_claims_resolve_to_one_root_across_linked_worktrees(tmp_path, monkeypatch):
    """A claim taken from a linked worktree with no root is visible from the
    canonical checkout: claims are cross-worktree coordination state."""
    import os
    import subprocess

    canonical = tmp_path / "canonical"
    canonical.mkdir()
    subprocess.run(["git", "init", str(canonical)], check=True, capture_output=True)
    subprocess.run(
        ["git", "-C", str(canonical), "commit", "--allow-empty", "-m", "init"],
        check=True,
        capture_output=True,
        env={**os.environ, "GIT_AUTHOR_NAME": "test", "GIT_AUTHOR_EMAIL": "t@t.com",
             "GIT_COMMITTER_NAME": "test", "GIT_COMMITTER_EMAIL": "t@t.com"},
    )
    linked = tmp_path / "linked"
    subprocess.run(
        ["git", "-C", str(canonical), "worktree", "add", str(linked), "--detach"],
        check=True,
        capture_output=True,
    )
    monkeypatch.chdir(linked)
    monkeypatch.delenv("FNO_CLAIMS_ROOT", raising=False)
    monkeypatch.delenv("FNO_REPO_ROOT", raising=False)

    acquire_claim(key="worktree-test-claim", holder="test-holder", root=None)
    try:
        monkeypatch.chdir(canonical)
        status = claim_status("worktree-test-claim", root=None)
        assert status.get("state") in ("live", "stale"), status
        assert status.get("holder") == "test-holder", status
    finally:
        release_claim(key="worktree-test-claim", holder="test-holder", root=None)
