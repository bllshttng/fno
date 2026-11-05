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
    steal_if_stale,
)


def _age(path: Path, seconds: float) -> None:
    """Backdate a lock dir's mtime, which is what the steal predicate reads."""
    old = time.time() - seconds
    os.utime(path, (old, old))


class TestStealHelper:

    def test_AC1_HP_corpse_stolen(self, tmp_path):
        lock = tmp_path / "a.lock.d"
        lock.mkdir()
        _age(lock, STALE_MUTEX_STEAL_S + 60)
        assert steal_if_stale(lock) is True
        assert not lock.exists()


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
