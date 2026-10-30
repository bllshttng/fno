"""Spawn gate (x-c5cc): union live-count, RAM floor, queue loop, QoS wrap.

FNO_THINK_SPAWN=0 discipline is irrelevant here (nothing dispatches), but
every test redirects FNO_CLAUDE_DAEMON_DIR + FNO_CLAIMS_ROOT so no real
roster or claims dir is touched.
"""
from __future__ import annotations

import json
import os
from pathlib import Path

import pytest

from fno.agents import spawn_gate
from fno.agents.registry import AgentEntry


@pytest.fixture(autouse=True)
def _isolated_world(tmp_path, monkeypatch):
    """No test reads the real roster, claims root, or settings."""
    daemon = tmp_path / "daemon"
    daemon.mkdir()
    monkeypatch.setenv("FNO_CLAUDE_DAEMON_DIR", str(daemon))
    monkeypatch.setenv("FNO_CLAIMS_ROOT", str(tmp_path / "claims-root"))
    monkeypatch.setenv("FNO_THINK_SPAWN", "0")
    # conftest disables the gate suite-wide; re-arm it here — these tests
    # exercise the gate itself.
    monkeypatch.delenv("FNO_SPAWN_GATE", raising=False)
    yield


def _write_roster(tmp_path, workers: dict) -> None:
    roster = {"proto": 1, "supervisorPid": 1, "workers": workers}
    (tmp_path / "daemon" / "roster.json").write_text(json.dumps(roster))


def _row(name: str, *, status="live", pid=None, short_id=""):
    return AgentEntry(
        name=name,
        harness="claude",
        cwd="/tmp",
        log_path="/tmp/log",
        status=status,
        pid=pid,
        short_id=short_id,
    )


ALIVE = os.getpid()  # a pid that is definitely alive (this test process)


class TestPidLiveness:

    def test_pid_start_token_mismatch_is_not_our_process(self, monkeypatch):
        """A reused numeric PID must not keep an old registry row alive."""
        class Proc:
            def is_running(self):
                return True

            def status(self):
                return "running"

        class Psutil:
            STATUS_ZOMBIE = "zombie"

            @staticmethod
            def Process(_pid):
                return Proc()

        monkeypatch.setattr(
            spawn_gate, "_process_start_time",
            lambda _pid, _psutil=None: 42_000_000,
            raising=False,
        )
        assert not spawn_gate._pid_alive(4242, 41_000_000, _psutil=Psutil)
        assert spawn_gate._pid_alive(4242, 42_000_000, _psutil=Psutil)

        monkeypatch.setattr(spawn_gate, "_process_start_time", lambda *_args: None)
        assert spawn_gate._pid_alive(4242, 42_000_000, _psutil=Psutil) is None
        assert not spawn_gate._pid_alive(4242, 42_000_000, _psutil=Psutil)


def _stub_command(monkeypatch, answer=None, error=None):
    """Stub the gate command: a fixed JSON answer, or a failure."""
    from fno import rust_binary

    if error is not None:
        def boom(*_a, **_k):
            raise error
        monkeypatch.setattr(rust_binary, "command_call", boom)
        return boom

    def fake(command, payload, **_kwargs):
        assert command == "spawn-gate", f"the transport must call the gate command, got {command}"
        fake.payload = payload
        return answer

    fake.payload = None
    monkeypatch.setattr(rust_binary, "command_call", fake)
    return fake


# --- parent-edge visibility (x-7b36 change 12) --------------------------------
# A null spawned_by_session is sometimes correct (an ambiguous identity resolve
# records no lineage rather than a wrong one); the defect was its SILENCE. The
# receipt line names it at spawn time, and the list row carries the field so
# lineage questions debugged through `agents list -J` measure something.


class TestParentEdgeNotice:
    def test_parent_edge_notice_names_reason_and_orphan_check(
        self, monkeypatch, capsys
    ):
        from fno.agents.spawn_lineage import _report_unlinked_parent
        from fno.harness_identity import OwnedHarnessIdentity

        monkeypatch.setattr(
            "fno.claims.self_identity.resolve_self_identity",
            lambda: OwnedHarnessIdentity(
                session_id=None,
                harness=None,
                markers_present=(
                    ("CLAUDE_CODE_SESSION_ID", "claude", "x"),
                    ("CODEX_THREAD_ID", "codex", "y"),
                ),
                disposition="ambiguous",
            ),
        )
        _report_unlinked_parent(None)
        err = capsys.readouterr().err
        assert "parent edge NOT recorded" in err
        assert "disposition=ambiguous" in err
        assert "CLAUDE_CODE_SESSION_ID" in err
        # The consequence, not just the cause: the spawner learns the child is
        # invisible to its orphan check.
        assert "will not appear in its spawner's orphan check" in err


# ---------------------------------------------------------------------------
# AC3-HP/EDGE: the durable fleet incident gate (x-77db)
# ---------------------------------------------------------------------------

# ---------------------------------------------------------------------------
# A nonzero gate exit is reported as itself, never as a strip crash
# ---------------------------------------------------------------------------

def _fake_gate_binary(tmp_path: Path, body: str) -> Path:
    fake = tmp_path / "bin" / "fno-agents"
    fake.parent.mkdir(parents=True, exist_ok=True)
    fake.write_text(body)
    fake.chmod(0o755)
    return fake


def _point_binary(monkeypatch, fake: Path) -> None:
    from fno import rust_binary

    monkeypatch.setattr(rust_binary, "find_dev_binary", lambda: fake)
    monkeypatch.setattr(rust_binary, "resolve_binary", lambda: fake)


