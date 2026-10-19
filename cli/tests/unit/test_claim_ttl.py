"""TTL renewal + loud-expiry semantics (x-a7ab 1.4).

A ticking worker refreshes its node claim so a long-running loop never silently
expires its TTL and frees the node for a twin. When a worker stops ticking, an
expired claim must still surface its prior holder - a dispatcher reading the
claim never sees a silently-free node.
"""
import os

import psutil

from fno.claims.core import acquire_claim, claim_status, refresh_claim
from fno.claims.io import claim_path, serialize_claim
from fno.claims.types import Claim, now_ms


def test_expired_claim_status_names_prior_holder(tmp_path):
    # AC4-FR loud expiry: an expired claim whose pid is dead reads STALE, and its
    # status still names the prior holder - never a silently-free node.
    expired = Claim(
        schema_version=1,
        key="node:GHOST",
        holder="target-session:gone",
        acquired_at=now_ms() - 200_000,
        expires_at=now_ms() - 100_000,
        pid=999_999,  # definitely dead
        host=os.uname().nodename,
    )
    path = claim_path("node:GHOST", root=tmp_path)
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(serialize_claim(expired))
    status = claim_status("node:GHOST", root=tmp_path)
    assert status["state"] == "stale"
    assert status["holder"] == "target-session:gone"


# ---------------------------------------------------------------------------
# renewal re-anchors a corpse instead of preserving it (x-05be)
# ---------------------------------------------------------------------------


def _dead_pid():
    dead = 999_999
    while psutil.pid_exists(dead):
        dead += 1
    return dead


def _anchor(monkeypatch, pid):
    monkeypatch.setattr("fno.claims.session_pid.resolve_session_pid", lambda **_kw: pid)


class TestRefreshReanchorsADeadPid:
    """The root cause of SUSPECT meaning two things.

    A respawned worker renewing under a new pid used to leave a claim
    byte-identical to a dead worker's: dead pid, unexpired TTL. Nothing on disk
    separated them, so every reader that must not steal from the live one was
    forced to protect the dead one.
    """


    def test_a_re_anchoring_refresh_still_extends_the_deadline(
        self, tmp_path, monkeypatch
    ):
        """The deadline runs from NOW, never from the held acquire time. Tying
        it to `acquired_at` made this refresh extend the lease by zero, and on a
        short window it wrote a deadline in the PAST - the heartbeat driving its
        own claim from suspect to stale."""
        original = acquire_claim(
            key="node:x-deadline", holder="target-session:s", ttl_ms=3_600_000,
            pid=_dead_pid(), root=tmp_path,
        )
        _anchor(monkeypatch, os.getpid())
        refreshed = refresh_claim(
            key="node:x-deadline", holder="target-session:s", ttl_ms=3_600_000,
            root=tmp_path,
        )
        assert refreshed.acquired_at == original.acquired_at
        assert refreshed.expires_at > original.expires_at, (
            "a re-anchoring refresh did not extend the lease"
        )
        assert refreshed.expires_at > now_ms()

    def test_a_live_anchor_is_never_rewritten(self, tmp_path, monkeypatch):
        """A healthy claim keeps the anchor it was acquired with, so a peer
        knowing the holder string cannot take over a running session."""
        original = acquire_claim(
            key="node:x-healthy", holder="target-session:s", ttl_ms=3_600_000,
            pid=os.getpid(), root=tmp_path,
        )
        _anchor(monkeypatch, 424242)
        refreshed = refresh_claim(
            key="node:x-healthy", holder="target-session:s", ttl_ms=3_600_000,
            root=tmp_path,
        )
        assert refreshed.pid == os.getpid()
        assert refreshed.acquired_at == original.acquired_at
