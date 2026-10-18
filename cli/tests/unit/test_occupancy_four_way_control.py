"""The four-way occupancy control.

One question - is someone working this node? - used to get three different
words from readers that never said which store they read. This control
drives the four situations through the core reader and asserts four DISTINCT verdicts, with the held
case asserting a positive marker (state live AND the holder string), not
merely "not free" - four flavours of nothing would pass a broken fixture.
The target classifier that shared this file is the native start port now;
its answers are the claim-status parity goldens' to pin.
"""
from __future__ import annotations
import os
import socket

import psutil
import pytest
from fno.claims.core import claim_status
from fno.claims.io import claim_path, serialize_claim
from fno.claims.types import Claim, now_ms

HELD_ID = "ab-11aa22bb"
EXPIRED_ID = "ab-33cc44dd"
FREE_ID = "ab-55ee66ff"
HELD_KEY = f"node:{HELD_ID}"
HOLDER = "target-session:sid-ctl"


def _claim_with(expires_at, key=HELD_KEY):
    """A claim past (or inside) its TTL under a provably live pid.

    pid_provenance="session-prover" plus a pid-dies-with-session harness is
    the arm the native verdict keeps Live past the TTL; acquired_at sits a
    beat after THIS process's birth so the verdict cannot read PID reuse.
    """
    return Claim(
        key=key,
        holder=HOLDER,
        acquired_at=int(psutil.Process(os.getpid()).create_time() * 1000) + 2_000,
        pid=os.getpid(),
        host=socket.gethostname(),
        machine_id=__import__("fno.claims.hostid", fromlist=["machine_id"]).machine_id(),
        harness="claude",
        pid_provenance="session-prover",
        expires_at=expires_at,
    )


@pytest.fixture()
def four_way(tmp_path, monkeypatch):
    """Plant the global root with: held, ttl-expired-with-live-holder; leave
    one routed key empty; the unrouted key needs no file at all."""
    monkeypatch.setenv("FNO_CLAIMS_ROOT", str(tmp_path / "global"))
    monkeypatch.setattr("fno.paths.space_dir", lambda: tmp_path / "space")

    def plant(key, claim):
        p = claim_path(key)
        p.parent.mkdir(parents=True, exist_ok=True)
        p.write_text(serialize_claim(claim))

    plant(HELD_KEY, _claim_with(now_ms() + 3_600_000))
    plant(f"node:{EXPIRED_ID}", _claim_with(now_ms() - 60_000, key=f"node:{EXPIRED_ID}"))
    return tmp_path


def test_four_distinct_words_through_the_core_reader(four_way):
    """AC7-HP: live / live-with-expiry / free / key-unrouted never share a
    reading, and the held case carries its holder as a positive marker."""
    held = claim_status(HELD_KEY)
    assert held["state"] == "live"
    assert held["holder"] == HOLDER

    expired = claim_status(f"node:{EXPIRED_ID}")
    assert expired["state"] == "live"
    assert expired.get("expired") is True

    free = claim_status(f"node:{FREE_ID}")
    assert free["state"] == "free"

    unrouted = claim_status(EXPIRED_ID)
    assert unrouted["state"] == "unknown"
    assert unrouted["basis"] == "key-unrouted"

    readings = {
        (held["state"], held.get("expired")),
        (expired["state"], expired.get("expired")),
        (free["state"], free.get("expired")),
        (unrouted["state"], unrouted.get("expired")),
    }
    assert len(readings) == 4
