"""Incarnation fence (x-eea5 1.3): a losing incarnation refuses outward actions."""
import socket
import subprocess
import sys
from pathlib import Path

from fno.claims.incarnation import incarnation_fence_blocks, resolve_fence_session_uuid
from fno.claims.io import claim_path, serialize_claim
from fno.claims.types import Claim, now_ms


def _wire(monkeypatch, status, *, own_pid=None):
    monkeypatch.setattr("fno.claims.core.claim_status", lambda key, root=None: status)
    monkeypatch.setattr(
        "fno.claims.session_pid.resolve_session_pid", lambda from_pid=None: own_pid
    )
    # Ownership compares the claim's machine_id, not a raw gethostname().
    # Fixtures below carry machine_id="h" to match.
    monkeypatch.setattr("fno.claims.hostid.machine_id", lambda: "h")


def test_corrupted_claim_fails_closed(monkeypatch):
    # F3: claim_status returns state="corrupted" (no raise) for a malformed claim
    # file. An unverifiable single-writer state must fail closed, not read clear.
    _wire(monkeypatch, {"state": "corrupted", "error": "bad json"})
    blocked, reason = incarnation_fence_blocks("uuid1")
    assert blocked
    assert "corrupt" in reason.lower()


def test_resolve_uuid_from_env(monkeypatch):
    # F1: the fence keys on the TRANSCRIPT uuid (CLAUDE_CODE_SESSION_ID), not the
    # target run id (TARGET_SESSION_ID); the single-writer claim is held under the
    # transcript uuid, so the run id would read a nonexistent key as clear.
    monkeypatch.setenv("TARGET_SESSION_ID", "run-id")
    monkeypatch.setenv("CLAUDE_CODE_SESSION_ID", "transcript-uuid")
    assert resolve_fence_session_uuid() == "transcript-uuid"


def test_run_merge_blocked_by_fence(monkeypatch, tmp_path):
    # The merge outward action refuses when the fence blocks (before any merge work).
    # run_merge's hold-check runs BEFORE the fence check; an empty graph_json
    # keeps it a no-op so this test exercises the fence, not leftover graph
    # state from another test in the same xdist worker (round-12 review fix).
    monkeypatch.setattr("fno.paths.graph_json", lambda: tmp_path / "graph.json")
    monkeypatch.setattr(
        "fno.rust_binary.verb_call", lambda verb, payload, *a, **k: {"worktree": str(payload.get("cwd") or repo)}
    )
    monkeypatch.setattr(
        "fno.claims.incarnation.resolve_fence_session_uuid", lambda cwd=None: "uuid1"
    )
    monkeypatch.setattr(
        "fno.claims.incarnation.incarnation_fence_blocks",
        lambda u, **k: (True, "session:uuid1 held by other"),
    )
    from fno.pr import _merge

    rc = _merge.run_merge(["123"], cwd=str(tmp_path))
    assert rc == 2


# ---------------------------------------------------------------------------
# the 2026-08-21 specimen, replayed end to end against a real claim file
# ---------------------------------------------------------------------------


def _write_specimen_claim(root: Path, *, pid: int, provenance=None) -> None:
    """The PR 1031 shape: a 120-second session-writer claim, long expired."""
    started = now_ms() - 100
    claim = Claim(
        key="session:119e3c52-specimen",
        holder="119e3c52-specimen-holder",
        acquired_at=started,
        expires_at=now_ms() - 50,  # the 120s TTL lapsed ~700 minutes ago
        pid=pid,
        host=socket.gethostname(),
        pid_provenance=provenance,
    )
    path = claim_path(claim.key, root=root)
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(serialize_claim(claim))


def test_specimen_expired_foreign_pid_no_longer_blocks(tmp_path):
    """THE SPECIMEN, end to end: a session-writer claim whose TTL expired
    ~700 minutes ago while a live foreign process (a chat app's app-server)
    answered for the recorded pid. Before the corroborated hybrid arm the
    claim read live forever and the fence blocked every merge on the
    lineage; now the unproven pid cannot outrank the TTL, the claim reads
    stale, and the fence clears - the merge path proceeds."""
    foreign = subprocess.Popen([sys.executable, "-c", "import time; time.sleep(60)"])
    try:
        _write_specimen_claim(tmp_path, pid=foreign.pid, provenance="ambient")
        blocked, reason = incarnation_fence_blocks(
            "119e3c52-specimen", claims_root=tmp_path
        )
        assert blocked is False
        assert reason == ""
    finally:
        foreign.terminate()
        foreign.wait()


# ---------------------------------------------------------------------------
# pool-machinery carve-out (the bg-spare hosts the worker's own lineage)
# ---------------------------------------------------------------------------


def _fake_ancestry(monkeypatch, chain):
    """Replace psutil.Process so parent() walks CHAIN: the pid list of the
    caller's ancestors, nearest first. Every pid maps onto the same chain."""
    import psutil

    class _Proc:
        def __init__(self, pid, rest):
            self.pid = pid
            self._rest = rest

        def parent(self):
            if not self._rest:
                return None
            return _Proc(self._rest[0], self._rest[1:])

    monkeypatch.setattr(psutil, "Process", lambda pid: _Proc(pid, list(chain)))


def test_rival_worker_under_another_spare_still_blocks(monkeypatch):
    # The P1 the first cut missed: a LOSING original on the same machine sees
    # the same session key and a pool-machinery holder, but the holder is NOT
    # in its ancestry (it hangs off its own spare). Positive correlation or
    # no pass: the fence still blocks. Also carries the AC3-ERR baseline this
    # file guarded before via test_other_live_blocks (live foreign holder
    # blocks, holder and key named).
    _wire(
        monkeypatch,
        {"state": "live", "holder": "revive:64225", "pid": 999_999,
         "host": "h", "machine_id": "h"},
        own_pid=None,
    )
    _fake_ancestry(monkeypatch, [555_555, 1])  # own spare, not the holder
    blocked, reason = incarnation_fence_blocks("uuid1")
    assert blocked
    assert "revive:64225" in reason
    assert "session:uuid1" in reason
    assert "fno agents claim release session:uuid1 --force --reason" in reason
