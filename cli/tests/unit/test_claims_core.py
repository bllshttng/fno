"""Unit tests for fno.claims.core: the six commands.

Tests are organized by command. Each command exercises the design-doc ACs:
HP (happy path), ERR (error path), EDGE (edge case), FR (functional req).

Filesystem isolation: every test uses a tmp_path root via the ``root``
argument supported by every command. Events emission goes to .fno/events.jsonl
which the typed-builders write best-effort; tests focus on lock-file state
and exceptions, not on event log content (the event types are covered by
the parity corpus and test_validator_parity.py).
"""
from __future__ import annotations

import os
import socket
import subprocess
import sys
from pathlib import Path

import psutil
import pytest

from fno.rust_binary import find_dev_binary

from fno.claims.core import (
    ClaimHeldByOther,
    ClaimValidationError,
    HolderMismatch,
    acquire_claim,
    claim_status,
    force_release_claim,
    refresh_claim,
    release_claim,
)
from fno.claims.io import claim_path, serialize_claim
from fno.claims.types import Claim, ClaimState, now_ms
from tests._table_seed import read_claim_row, update_claim


HOLDER_A = "target-session:sid-a"
HOLDER_B = "target-session:sid-b"


# ---------------------------------------------------------------------------
# acquire
# ---------------------------------------------------------------------------


class TestAcquire:


    def test_AC3_ERR_pid_unavailable_requires_ttl(self, tmp_path):
        with pytest.raises(ClaimValidationError, match="TTL"):
            acquire_claim("k", HOLDER_A, pid_unavailable=True, root=tmp_path)


    def test_AC1_EDGE_live_other_raises(self, tmp_path):
        acquire_claim("k", HOLDER_A, root=tmp_path)
        with pytest.raises(ClaimHeldByOther) as exc:
            acquire_claim("k", HOLDER_B, root=tmp_path)
        assert exc.value.holder == HOLDER_A
        assert exc.value.key == "k"


    def test_expired_live_ambient_pid_is_reclaimable(self, tmp_path):
        """THE SPECIMEN'S ACQUIRE SIDE: the same expired claim WITHOUT
        provenance (a foreign process merely answers for the pid) IS
        reclaimable. A live pid that was never proven to be the holder
        session's own process cannot outrank the TTL, or the lease is not a
        lease - this is what unblocks a fenced merge peer."""
        proc_create_ms = int(psutil.Process(os.getpid()).create_time() * 1000)
        # now-relative, not create-relative: acquired 100ms ago (after proc
        # start, so the pid-reuse guard passes) and expired 50ms ago. A
        # create-relative expiry is in the future on a fast runner and the
        # claim lands in the unexpired arm for an unrelated reason.
        assert now_ms() - 100 > proc_create_ms
        path = claim_path("k", root=tmp_path)
        path.parent.mkdir(parents=True, exist_ok=True)
        expired_foreign = Claim(
            key="k",
            holder=HOLDER_A,
            acquired_at=now_ms() - 100,
            expires_at=now_ms() - 50,
            pid=os.getpid(),
            host=socket.gethostname(),
            pid_provenance="ambient",
        )
        path.write_text(serialize_claim(expired_foreign))

        new = acquire_claim("k", HOLDER_B, root=tmp_path)
        assert new.holder == HOLDER_B


# ---------------------------------------------------------------------------
# pid provenance stamping: every writer earns its field or says ambient
# ---------------------------------------------------------------------------


class TestPidProvenanceStamping:
    """The corroborated hybrid arm is only as honest as the stamp, so the
    stamp is earned centrally at write time against the process-tree prover -
    never asserted by a writer that merely had a pid lying around."""


    def test_foreign_live_pid_stamps_ambient(self, tmp_path):
        """THE SPECIMEN WRITER SHAPE: a reattach resolved its incarnation
        through an ambient codex process tree and recorded a foreign live pid
        (a chat app's app-server). The pid is real and alive, but it is not
        the prover's answer for this session, so the stamp must say ambient -
        the claim then expires on its TTL instead of reading live forever."""
        foreign = subprocess.Popen([sys.executable, "-c", "import time; time.sleep(60)"])
        try:
            assert psutil.pid_exists(foreign.pid)
            claim = acquire_claim(
                "session:uuid-reattach", HOLDER_A, ttl_ms=120_000,
                pid=foreign.pid, root=tmp_path,
            )
            assert claim.pid == foreign.pid
            assert claim.pid_provenance == "ambient"
        finally:
            foreign.terminate()
            foreign.wait()


    def test_shared_host_harness_never_earns_the_prover_stamp(self, tmp_path, monkeypatch):
        """AC1 - the writer side of the specimen. The prover walk succeeds:
        the pid IS resolve_session_pid's answer, and both sides of that
        equality hold. Under codex they hold for the WRONG reason, because the
        answer is a shared `codex app-server` that hosts every session on the
        machine. Proving which process the pid is never proves that process
        dies with the session, so the stamp is refused and the TTL stays the
        lease it claims to be."""
        monkeypatch.setattr(
            "fno.claims.session_pid.resolve_session_pid", lambda from_pid=None: os.getpid()
        )
        claim = acquire_claim(
            "node:x-shared", HOLDER_A, ttl_ms=60_000, pid=os.getpid(),
            harness="codex", root=tmp_path
        )
        assert claim.pid_provenance == "ambient"
        # The stamp is gated on the harness the record STORES, so the two can
        # never disagree about which harness wrote it.
        assert claim.harness == "codex"


    def test_refresh_under_shared_host_harness_does_not_repoison(self, tmp_path, monkeypatch):
        """AC5-EDGE - refresh_claim re-derives the stamp instead of asserting
        it. Its old hardcode rested on 'the anchor IS the prover's answer',
        which is true and insufficient: under codex that answer is the
        multiplexer. Left hardcoded, every renewal would re-poison the record
        acquire had just stopped poisoning, and a refreshed lease would be
        permanent again."""
        monkeypatch.setattr(
            "fno.claims.session_pid.resolve_session_pid", lambda from_pid=None: os.getpid()
        )
        first = acquire_claim(
            "node:x-refresh-shared", HOLDER_A, ttl_ms=60_000, pid=os.getpid(),
            harness="codex", root=tmp_path
        )
        assert first.pid_provenance == "ambient"
        refreshed = refresh_claim(
            "node:x-refresh-shared", HOLDER_A, ttl_ms=60_000, root=tmp_path
        )
        assert refreshed is not None
        assert refreshed.pid_provenance == "ambient"


# ---------------------------------------------------------------------------
# release
# ---------------------------------------------------------------------------


class TestRelease:


    def test_AC2_ERR_release_strict_raises_on_mismatch(self, tmp_path):
        acquire_claim("k", HOLDER_A, root=tmp_path)
        with pytest.raises(HolderMismatch):
            release_claim("k", HOLDER_B, strict=True, root=tmp_path)


# ---------------------------------------------------------------------------
# refresh
# ---------------------------------------------------------------------------


class TestRefresh:


    def test_AC3_ERR_refresh_wrong_holder_raises(self, tmp_path):
        acquire_claim("k", HOLDER_A, ttl_ms=60_000, root=tmp_path)
        with pytest.raises(HolderMismatch):
            refresh_claim("k", HOLDER_B, root=tmp_path)


# ---------------------------------------------------------------------------
# status
# ---------------------------------------------------------------------------


class TestStatus:


    def test_rootless_node_key_routes_to_global_root(self, tmp_path, monkeypatch):
        """A rootless read of node:<id> must answer the global root, not the
        repo space (x-74aa): the repo space holds no node locks, so the
        pre-fix default read every live node claim as free."""
        monkeypatch.setenv("FNO_CLAIMS_ROOT", str(tmp_path / "global"))
        monkeypatch.setattr("fno.paths.space_dir", lambda: tmp_path / "space")
        acquire_claim("node:ab-1234", HOLDER_A, root=None)
        # Control: the repo space really is the wrong tree for this key.
        assert claim_status("node:ab-1234", root=tmp_path / "space")["state"] == (
            ClaimState.FREE.value
        )
        result = claim_status("node:ab-1234")
        assert result["state"] == ClaimState.LIVE.value
        assert result["holder"] == HOLDER_A


    def test_node_shaped_colonless_key_is_unknown_not_free(self):
        result = claim_status("ab-1234")
        assert result["state"] == "unknown"
        assert result["basis"] == "key-unrouted"
        assert "node:ab-1234" in result["detail"]
        assert result["state"] != ClaimState.FREE.value


# ---------------------------------------------------------------------------
# force-release
# ---------------------------------------------------------------------------


class TestForceRelease:


    def test_AC6_ERR_empty_reason_rejected(self, tmp_path):
        acquire_claim("k", HOLDER_A, root=tmp_path)
        with pytest.raises(ClaimValidationError):
            force_release_claim("k", reason="", root=tmp_path)

# ---------------------------------------------------------------------------
# session-keyed liveness (x-a613): the session id is the witness
# ---------------------------------------------------------------------------


def _dev_native_binary() -> str:
    """The worktree-built fno-agents: the session witness exists only here,
    and the installed binary the default resolver finds predates it."""
    return str(
        Path(__file__).resolve().parents[3] / "crates/fno-agents/target/debug/fno-agents"
    )


def _dead_pid_context():
    """A pid that is genuinely absent, plus proof it was alive once."""
    dead = subprocess.Popen([sys.executable, "-c", "import time; time.sleep(60)"])
    pid = dead.pid
    dead.terminate()
    dead.wait()
    return pid


RUST_BIN = find_dev_binary()
requires_rust = pytest.mark.skipif(
    RUST_BIN is None,
    reason="compiled fno-agents binary not present (the smoke pytest shard deletes it)",
)


class TestSessionWitnessVerdicts:
    """The witness heals a live session's verdict and bounds the unknown.
    These shell the NATIVE classifier, pinned to the worktree build."""

    def _write_claim(self, tmp_path, key, session_id, pid, expires_delta):
        path = claim_path(key, root=tmp_path)
        path.parent.mkdir(parents=True, exist_ok=True)
        rec = Claim(
            key=key,
            holder=HOLDER_A,
            acquired_at=now_ms() - 100,
            expires_at=now_ms() + expires_delta,
            pid=pid,
            host=socket.gethostname(),
            pid_provenance="session-prover",
            session_id=session_id,
        )
        path.write_text(serialize_claim(rec))
        return rec

    def _pin_native_binary(self, monkeypatch):
        monkeypatch.setenv("FNO_AGENTS_BIN", _dev_native_binary())

    def _registry_home_with_live_row(self, tmp_path, session_id):
        """A registry the NATIVE binary reads: FNO_AGENTS_HOME tmp + row pid +
        start time in the convention daemon.process_start_time compares."""
        import json as _json

        home = tmp_path / "agents-home"
        home.mkdir(parents=True, exist_ok=True)
        if sys.platform == "darwin":
            start = int(psutil.Process(os.getpid()).create_time() * 1_000_000)
        else:
            with open(f"/proc/{os.getpid()}/stat") as fh:
                stat = fh.read().rsplit(")", 1)[1].split()
            start = int(stat[19])
        row = {
            "name": "w",
            "cwd": str(tmp_path),
            "harness": "claude",
            "harness_session_id": session_id,
            "pid": os.getpid(),
            "pid_start_time": start,
            "status": "busy",
            "created_at": "2026-09-06T00:00:00Z",
        }
        (home / "registry.json").write_text(
            _json.dumps({"schema_version": 24, "agents": [row]})
        )
        return home

    @requires_rust
    def test_status_live_session_never_reads_stale(self, tmp_path, monkeypatch):
        """AC3 + x-0c29: an EXPIRED claim whose session's registry row is live
        reads LIVE with the registry basis - a session that wrote seconds ago
        must never be provably dead."""
        self._pin_native_binary(monkeypatch)
        dead_pid = _dead_pid_context()
        self._write_claim(tmp_path, "k", "ses_live", dead_pid, -60_000)
        monkeypatch.setenv(
            "FNO_AGENTS_HOME", str(self._registry_home_with_live_row(tmp_path, "ses_live"))
        )
        status = claim_status("k", root=tmp_path)
        assert status["state"] == "live", status
        assert status["basis"] == "registry-session-live"
        assert status["session_basis"] == "registry-session-live"


# ---------------------------------------------------------------------------
# Contracts ported from the lockfile era: each seeds through the public commands
# and ages the row in the claims table instead of hand-writing a lockfile.
# ---------------------------------------------------------------------------


def _gone_pid() -> int:
    pid = 999_999
    while psutil.pid_exists(pid):
        pid += 1
    return pid


def test_AC4_EDGE_stale_pid_recovered(tmp_path):
    """A claim whose holder process is dead is reclaimable by another holder."""
    acquire_claim("k", HOLDER_A, root=tmp_path)
    update_claim("k", root=tmp_path, pid=_gone_pid(), acquired_at=now_ms() - 100_000)
    new = acquire_claim("k", HOLDER_B, root=tmp_path)
    assert new.holder == HOLDER_B
    assert claim_status("k", root=tmp_path)["holder"] == HOLDER_B


def test_hybrid_expired_live_pid_not_reclaimable(tmp_path):
    """An expired TTL claim whose pid is a live prover-proven process stays
    held: acquire honors the same hybrid liveness as status."""
    proc_create_ms = int(psutil.Process(os.getpid()).create_time() * 1000)
    assert now_ms() - 100 > proc_create_ms
    acquire_claim("k", HOLDER_A, ttl_ms=60_000, root=tmp_path)
    update_claim(
        "k", root=tmp_path, acquired_at=now_ms() - 100, expires_at=now_ms() - 50,
        pid=os.getpid(), pid_provenance="session-prover", session_id=None,
    )
    with pytest.raises(ClaimHeldByOther) as exc:
        acquire_claim("k", HOLDER_B, root=tmp_path)
    assert exc.value.holder == HOLDER_A
    assert claim_status("k", root=tmp_path)["holder"] == HOLDER_A


def test_AC2_FR_release_silently_skips_other_holder(tmp_path):
    acquire_claim("k", HOLDER_A, root=tmp_path)
    release_claim("k", HOLDER_B, root=tmp_path)
    assert claim_status("k", root=tmp_path)["holder"] == HOLDER_A


def test_AC2_ERR_refresh_refuses_verdict_stale_and_leaves_the_row_alone(tmp_path):
    """A stale verdict (dead holder, no live witness) refuses, and the
    refusal leaves the row unchanged."""
    acquire_claim("k", HOLDER_A, ttl_ms=60_000, root=tmp_path)
    update_claim(
        "k", root=tmp_path, expires_at=now_ms() - 1, pid=_gone_pid(),
        pid_provenance="session-prover", session_id=None,
    )
    before = read_claim_row("k", root=tmp_path)
    with pytest.raises(ClaimValidationError, match="holder reads dead"):
        refresh_claim("k", HOLDER_A, ttl_ms=7_200_000, root=tmp_path)
    assert read_claim_row("k", root=tmp_path) == before
