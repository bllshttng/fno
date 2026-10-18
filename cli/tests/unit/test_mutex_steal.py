"""Age-based rename-steal of stale mkdir mutexes.

Covers the design-doc ACs for the two mutexes that share the disease:
``events.jsonl.lock.d`` (AC1/AC2/AC3) and ``<claim>.lock.recovery.d`` (AC4).

The live incident these guard against: a process died holding both, and every
waiter spun against the corpse for eight days.
"""

from __future__ import annotations

import os
import shutil
import time
from pathlib import Path


from fno import mutex
from fno.mutex import (
    STALE_MUTEX_STEAL_S,
    acquire_dir_mutex,
    steal_if_stale,
)


def _age(path: Path, seconds: float) -> None:
    """Backdate a lock dir's mtime, which is what the steal predicate reads."""
    old = time.time() - seconds
    os.utime(path, (old, old))


def _age_nofollow(path: Path, seconds: float) -> None:
    """Backdate a symlink itself rather than the target it points at."""
    old = time.time() - seconds
    os.utime(path, (old, old), follow_symlinks=False)


class _FrozenClock:
    """Stands in for the `time` module so an exact age is decidable."""

    def __init__(self, lock_dir: Path, age_s: float) -> None:
        self._now = lock_dir.lstat().st_mtime + age_s

    def time(self) -> float:
        return self._now

    def monotonic_ns(self) -> int:
        return 0


class TestStealHelper:
    def test_AC2_EDGE_exactly_at_threshold_is_held(self, tmp_path, monkeypatch):
        """The predicate is `<=`, so equal-to-threshold is held, not stolen.

        Backdating with utime cannot pin this: microseconds elapse before the
        predicate reads the clock, pushing the age just past the threshold. A
        frozen clock is what makes `<=` vs `<` actually decidable.
        """
        lock = tmp_path / "a.lock.d"
        lock.mkdir()
        monkeypatch.setattr(mutex, "time", _FrozenClock(lock, STALE_MUTEX_STEAL_S))

        assert steal_if_stale(lock) is False
        assert lock.exists()

    def test_AC1_HP_corpse_stolen(self, tmp_path):
        lock = tmp_path / "a.lock.d"
        lock.mkdir()
        _age(lock, STALE_MUTEX_STEAL_S + 60)
        assert steal_if_stale(lock) is True
        assert not lock.exists()

    def test_AC2_ERR_dangling_symlink_never_spins(self, tmp_path):
        """A dangling symlink is EEXIST to mkdir but ENOENT to a following stat.

        Returning "retry now" there would spin the caller's loop forever at
        100% CPU with its timeout unreachable, so the age must be read with
        lstat and an old dangling link stolen like any other corpse.
        """
        lock = tmp_path / "a.lock.d"
        lock.symlink_to(tmp_path / "nonexistent-target")
        _age_nofollow(lock, STALE_MUTEX_STEAL_S + 60)

        assert steal_if_stale(lock) is True
        assert not lock.is_symlink()

    def test_AC3_EDGE_a_live_lock_swapped_in_is_not_stolen(self, tmp_path, monkeypatch):
        """The rename is atomic per path, not per token.

        A waiter can be descheduled after reading the age and owner token,
        another stealer win, and a fresh holder acquire at the same path
        (stamping its own owner token). Renaming then moves a LIVE lock using
        the corpse's age; the owner-token mismatch is what detects it and puts
        the live lock back.

        The swap must clear the corpse's ``owner`` file with ``rmtree``, not
        ``rmdir``: a bare ``rmdir`` on the non-empty aged dir raised ENOTEMPTY,
        which ``steal_if_stale``'s own exception handler swallowed as "could
        not steal" - this test passed for years without ever reaching the
        owner-token mismatch it claims to cover.
        """
        lock = tmp_path / "a.lock.d"
        lock.mkdir()
        (lock / "owner").write_text("corpse-token")
        _age(lock, STALE_MUTEX_STEAL_S + 60)

        real_rename = os.rename

        def swap_then_rename(src, dst):
            # Stand in for the race: the corpse is replaced by a fresh holder
            # (with its own owner token) between our age read and our rename.
            shutil.rmtree(src)
            os.mkdir(src)
            (src / "owner").write_text("fresh-token")
            return real_rename(src, dst)

        monkeypatch.setattr(mutex.os, "rename", swap_then_rename)

        assert steal_if_stale(lock) is False, "stole a freshly acquired lock"
        assert lock.is_dir(), "the live lock was not restored"
        assert (lock / "owner").read_text() == "fresh-token", (
            "the swap never reached the owner-token mismatch path"
        )

    def test_AC2_HP_restored_dir_earns_a_grace_window_not_a_fresh_reprieve(
        self, tmp_path, monkeypatch
    ):
        """A restored dir is a zombie unless it is backdated on the way back.

        The put-back preserves whatever mtime the disturbed dir already had -
        fresh, for a live lock. If its real holder released into the gap while
        the dir was away (the interleaving this reconstructs), nobody is left
        to remove it, and a fresh mtime would shield that orphan for the full
        ``STALE_MUTEX_STEAL_S`` instead of the short ``RESTORE_GRACE_S`` window
        a disturbed-but-still-live holder actually needs.
        """
        lock = tmp_path / "a.lock.d"
        lock.mkdir()
        (lock / "owner").write_text("corpse-token")
        _age(lock, STALE_MUTEX_STEAL_S + 60)

        real_rename = os.rename
        swapped = False

        def swap_once_then_rename(src, dst):
            nonlocal swapped
            if not swapped:
                # Fire only on the steal's own rename: the holder released and
                # a fresh one acquired between our age read and this call. A
                # second interception here would also swap the RESTORE's own
                # rename, which is a different race this test isn't reconstructing.
                swapped = True
                shutil.rmtree(src)
                os.mkdir(src)
                (src / "owner").write_text("fresh-token")
            return real_rename(src, dst)

        monkeypatch.setattr(mutex.os, "rename", swap_once_then_rename)

        assert steal_if_stale(lock) is False, "stole a freshly acquired lock"
        assert lock.is_dir(), "the live lock was not restored"

        restored_mtime = lock.stat().st_mtime
        expected = time.time() - (STALE_MUTEX_STEAL_S - mutex.RESTORE_GRACE_S)
        assert abs(restored_mtime - expected) < 2.0, (
            f"restored dir kept a fresh mtime instead of a grace-window backdate: "
            f"{restored_mtime} vs expected ~{expected}"
        )

        monkeypatch.setattr(mutex.os, "rename", real_rename)

        monkeypatch.setattr(mutex, "time", _FrozenClock(lock, STALE_MUTEX_STEAL_S - 1))
        assert steal_if_stale(lock) is False, "stolen before its grace window elapsed"
        assert lock.is_dir()

        monkeypatch.setattr(mutex, "time", _FrozenClock(lock, STALE_MUTEX_STEAL_S + 1))
        assert steal_if_stale(lock) is True, "not stealable once its grace window elapsed"


class TestRecoveryMutex:
    def test_AC4_ERR_dangling_recovery_lock_does_not_spin(self, tmp_path):
        """acquire_dir_mutex must not follow symlinks either.

        A dangling `.recovery.d` is EEXIST to `mkdir` but ENOENT to a
        following `stat`. acquire_claim/refresh_claim/compare_and_rebind all
        take the recovery mutex via `acquire_dir_mutex` (mkdir + `lstat`-based
        `steal_if_stale`, never a following `.exists()`), so a fresh dangling
        symlink must poll out to the full timeout, not return instantly and
        send the caller straight back into unpaused recursion.
        """
        recovery_lock = tmp_path / "k.lock.recovery.d"
        recovery_lock.symlink_to(tmp_path / "nonexistent-target")

        started = time.monotonic()
        token = acquire_dir_mutex(recovery_lock, timeout_s=1.0, poll_s=0.05)

        assert token is None
        assert time.monotonic() - started >= 1.0, (
            "acquire_dir_mutex returned instantly on a dangling mutex; the caller would recurse without pause"
        )
