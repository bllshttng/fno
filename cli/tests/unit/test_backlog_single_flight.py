"""One-in-flight gate tests for ``acquire_flight``.

The contract under test: a SECOND acquire for an in-flight scope reads
``held``; distinct scopes never hold each other; a dead holder never wedges a
scope shut; a named lease stamps the holder process.

Claim isolation: FNO_CLAIMS_ROOT / FNO_REPO_ROOT / FNO_EVENTS_PATH pinned per
test, so no gate state reaches a real store.
"""

from __future__ import annotations

import os
import subprocess
from pathlib import Path

import pytest

from fno.backlog.single_flight import acquire_flight
from fno.claims.core import acquire_claim, claim_status
from fno.rust_binary import find_dev_binary
from tests._table_seed import read_claim_row

requires_rust = pytest.mark.dev_build


@pytest.fixture
def iso(tmp_path: Path, monkeypatch: pytest.MonkeyPatch):
    """Pin claims + repo root + events under tmp_path, and point the gate's
    binary at THIS checkout's build."""
    monkeypatch.setenv("FNO_CLAIMS_ROOT", str(tmp_path))
    monkeypatch.setenv("FNO_REPO_ROOT", str(tmp_path))
    monkeypatch.delenv("FNO_AUTO_CONTINUE", raising=False)
    monkeypatch.setenv("FNO_EVENTS_PATH", str(tmp_path / ".fno" / "events.jsonl"))
    dev = find_dev_binary()
    if dev is None:
        pytest.skip("compiled fno-agents binary not present")
    monkeypatch.setenv("FNO_AGENTS_BIN", str(dev))
    return tmp_path


def test_a_dead_holder_does_not_wedge_the_scope(iso):
    """A holder whose process died is reclaimed on the pid probe, not the
    TTL: the next acquirer takes the scope and runs."""
    key = "flight:backlog-advance"
    child = subprocess.Popen(["true"])
    child.wait()
    acquire_claim(key, "dead-run", ttl_ms=600_000, pid=child.pid)

    gate = acquire_flight(key, scope="advance")
    assert gate is not None and not gate.held, "a dead holder must never read as held"
    gate.release()


@requires_rust
def test_acquire_flight_takes_a_name_root_and_ttl(iso):
    key = "post-merge-sync"
    root = iso / "canonical"
    flight = acquire_flight(
        key, scope="post-merge canonical sync", name="sync-canonical:7",
        root=root, ttl_ms=60_000,
    )
    assert flight is not None and not flight.held
    assert flight.holder.startswith("sync-canonical:7:"), flight.holder
    lock = read_claim_row(key, root)
    assert lock["pid"] == os.getpid()
    assert lock["pid_provenance"] == "holder-process"
    flight.release()
    assert claim_status(key, root=root)["state"] == "free", "release must drop the lock"


def test_distinct_epics_hold_distinct_gates(iso):
    first = acquire_flight("flight:backlog-advance:epic:x-epic-a", scope="advance --epic a")
    other = acquire_flight("flight:backlog-advance:epic:x-epic-b", scope="advance --epic b")
    same = acquire_flight("flight:backlog-advance:epic:x-epic-a", scope="advance --epic a")
    assert first is not None and not first.held
    assert other is not None and not other.held
    assert same is not None and same.held
    first.release()
    other.release()
