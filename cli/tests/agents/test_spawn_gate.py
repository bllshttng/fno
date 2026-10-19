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


@pytest.fixture(autouse=True)
def _no_live_cpu_axis(monkeypatch):
    """The CPU axis is stubbed admitting unless a test asks otherwise.

    Since x-7783 every spawn takes the footprint reading, and that read is a
    ps snapshot against the real machine. Left unstubbed these tests refuse
    or hold according to what the developer's box happens to be doing. The
    prefetch is pinned too: run_gate takes the read before the decider, and
    an unpinned read would price a real ps snapshot per pass.
    """
    from fno import doctor_footprint
    from fno.footprint import Admission, Footprint

    idle = Footprint(0.0, 0.0, 0.1, 0, 0, 0, 0, 0.0, 0.2, [], 0, None)
    monkeypatch.setattr(
        spawn_gate, "_prefetch_fleet_reading", lambda: (idle, None)
    )
    monkeypatch.setattr(doctor_footprint, "_admission_config", lambda: 0.5)
    admit = Admission(
        verdict="admit",
        axis="fleet_cpu_share",
        reason="test admit",
        share_low=0.1,
        share_high=0.1,
        bound="exact",
        fleet_cores=1.2,
        machine_cores=6.0,
        capacity_cores=12.0,
        ceiling=0.5,
        gap=None,
    )
    monkeypatch.setattr(spawn_gate, "_cpu_axis", lambda *a, **k: admit)


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


class TestCensus:

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

    def test_union_counts_and_dedups_adopted_session(self, tmp_path, monkeypatch):
        """AC1-EDGE: 1 fno pane worker + 1 foreign roster worker + 1 adopted
        session (roster row AND minted registry row) -> count 3."""
        _write_roster(
            tmp_path,
            {
                "aaaaaaaa": {"sessionId": "aaaaaaaa-1-2-3-4", "pid": ALIVE},
                "bbbbbbbb": {"sessionId": "bbbbbbbb-1-2-3-4", "pid": ALIVE},
            },
        )
        rows = [
            _row("pane-worker", pid=ALIVE),  # fno-only
            _row("adopted", pid=ALIVE, short_id="bbbbbbbb"),  # dup of roster
        ]
        monkeypatch.setattr("fno.agents.registry.load_registry", lambda: rows)
        c = spawn_gate.census()
        assert c.count == 3
        assert not c.warnings

    def test_spawning_outlived_by_a_live_pid_renders_quiet_with_basis(
        self, monkeypatch
    ):
        """(x-d401 / x-0248) AC3-HP: a stored `spawning` token a live pid has
        outlived does not render a bare `spawning` - the row names the
        movement-derived state and a basis for the rewrite. The process is
        confirmed but the transcript is unread, so the served word is
        `quiet`, never a `live` token. AC3-EDGE: a row with no pid recorded
        yet keeps its honest token."""
        from datetime import datetime, timedelta, timezone

        stale = datetime.now(timezone.utc) - timedelta(hours=13)
        fresh = datetime.now(timezone.utc) - timedelta(seconds=5)
        rows = [
            _row("stale-spawn", status="spawning", pid=ALIVE),
            _row("fresh-spawn", status="spawning", pid=ALIVE),
        ]
        rows[0].created_at = stale.strftime("%Y-%m-%dT%H:%M:%SZ")
        rows[1].created_at = fresh.strftime("%Y-%m-%dT%H:%M:%SZ")
        monkeypatch.setattr("fno.agents.registry.load_registry", lambda: rows)

        by_name = {w.name: w for w in spawn_gate.census().workers}

        assert by_name["stale-spawn"].status == "quiet", (
            "a working row must not read spawning"
        )
        assert by_name["stale-spawn"].status_basis == "stale-spawning-live-pid"
        assert by_name["fresh-spawn"].status == "spawning", (
            "a live pid younger than the spawn timeout is still mid-spawn"
        )
        assert by_name["fresh-spawn"].status_basis is None

    def test_unknown_process_incarnation_counts_conservatively(self, monkeypatch):
        row = _row("unreadable-worker", pid=ALIVE)
        monkeypatch.setattr("fno.agents.registry.load_registry", lambda: [row])
        monkeypatch.setattr(spawn_gate, "_pid_alive", lambda *_args: None)

        result = spawn_gate.census()

        assert result.count == 1
        assert [worker.name for worker in result.workers] == ["unreadable-worker"]
        assert any("incarnation unreadable" in warning for warning in result.warnings)

    def test_malformed_roster_fails_open_with_warning(self, tmp_path, monkeypatch):
        (tmp_path / "daemon" / "roster.json").write_text("{ not json")
        rows = [_row("ok", pid=ALIVE)]
        monkeypatch.setattr("fno.agents.registry.load_registry", lambda: rows)
        c = spawn_gate.census()
        assert c.count == 1, "registry still counts when the roster is garbage"
        assert any("roster unreadable" in w for w in c.warnings)


def _stub_verb(monkeypatch, answer=None, error=None):
    """Stub the gate verb: a fixed JSON answer, or a failure."""
    from fno import rust_binary

    if error is not None:
        def boom(*_a, **_k):
            raise error
        monkeypatch.setattr(rust_binary, "verb_call", boom)
        return boom

    def fake(verb, payload, **_kwargs):
        assert verb == "spawn-gate", f"the transport must call the gate verb, got {verb}"
        fake.payload = payload
        return answer

    fake.payload = None
    monkeypatch.setattr(rust_binary, "verb_call", fake)
    return fake


class TestTransport:
    """The transport contract: run_gate asks the ONE gate verb and carries
    the answer out; the axes themselves are pinned in Rust."""

    def test_admitted_answer_becomes_a_guard_holding_the_verbs_keys(self, monkeypatch):
        stub = _stub_verb(
            monkeypatch,
            {
                "status": "admitted",
                "gate_key": "gate:spawn",
                "gate_holder": "spawn-gate:4242:w1",
                "worker_key": None,
                "worker_holder": None,
            },
        )
        guard = spawn_gate.run_gate("w1", "bg")
        assert guard._gate_holder == "spawn-gate:4242:w1"
        assert guard._worker_key is None
        assert guard._route_provider is None
        assert guard._spawn_name == "w1"
        assert guard._substrate == "bg"
        # The caller's pid travels in the payload: the verb's claims are
        # owned by the CALLER across the process boundary (locked decision 3).
        assert stub.payload["holder_pid"] == os.getpid()
        assert stub.payload["mode"] == "gate"

    def test_refused_answer_raises_gate_refused_with_the_receipt(self, monkeypatch, capsys):
        events = []
        monkeypatch.setattr(
            spawn_gate, "_emit_gate_event", lambda kind, **data: events.append((kind, data))
        )
        receipt = {
            "status": "refused",
            "reason": "provider_cap",
            "provider": "zai",
            "cap": 5,
            "count": 5,
            "current_count": 5,
        }
        _stub_verb(
            monkeypatch,
            {
                "status": "refused",
                "exit_code": spawn_gate.EXIT_PROVIDER_CAP,
                "receipt": receipt,
                "event": {"reason": "provider_cap", "provider": "zai"},
            },
        )
        with pytest.raises(spawn_gate.GateRefused) as exc:
            spawn_gate.run_gate("w", "pane", route_provider="zai")
        assert exc.value.code == spawn_gate.EXIT_PROVIDER_CAP
        assert exc.value.receipt == receipt
        # Exactly ONE refusal event lands, carrying the receipt fields.
        kinds = [k for k, _ in events]
        assert kinds == ["spawn_gate_refused"]
        data = events[0][1]
        assert data["exit_code"] == spawn_gate.EXIT_PROVIDER_CAP
        assert data["reason"] == "provider_cap"
        assert data["provider"] == "zai"
        assert data["gate"] == "python"
        assert data["name"] == "w"
        assert data["substrate"] == "pane"

    def test_missing_binary_refuses_exit_87_gate_unavailable(self, monkeypatch, capsys):
        from fno.rust_binary import VerbUnavailable

        events = []
        monkeypatch.setattr(
            spawn_gate, "_emit_gate_event", lambda kind, **data: events.append((kind, data))
        )
        _stub_verb(monkeypatch, error=VerbUnavailable("the fno-agents binary was not found"))
        with pytest.raises(spawn_gate.GateRefused) as exc:
            spawn_gate.run_gate("w", "pane")
        assert exc.value.code == spawn_gate.EXIT_GATE_UNAVAILABLE
        assert exc.value.receipt["reason"] == "gate_unavailable"
        assert "not found" in exc.value.receipt["error"]
        assert [k for k, _ in events] == ["spawn_gate_refused"]
        # The transport prints its OWN verdict line, so a spawn-failure reader
        # keys on it the same way it keys on the Rust gate's verdict (x-d769).
        err_lines = [ln for ln in capsys.readouterr().err.splitlines() if ln.strip()]
        assert err_lines[-1].startswith(
            f"spawn-gate: refused on gate (gate_unavailable, exit {spawn_gate.EXIT_GATE_UNAVAILABLE})"
        )

    def test_probe_never_raises_on_an_unanswered_gate(self, monkeypatch):
        from fno.rust_binary import VerbUnavailable

        _stub_verb(monkeypatch, error=VerbUnavailable("exited 1"))
        answer = spawn_gate.probe_capacity()
        assert answer["verdict"] == "unknown"
        assert answer["reason"] == "gate_unavailable"
        assert "exited 1" in answer["error"]

class TestQos:

    def test_demote_failure_is_nonfatal_warning(self, monkeypatch, capsys):
        """AC3-ERR: taskpolicy failure warns once, never raises."""
        monkeypatch.setattr(spawn_gate, "_qos_enabled", lambda: True)
        import subprocess

        def boom(*a, **k):
            raise FileNotFoundError("taskpolicy not found")

        monkeypatch.setattr(subprocess, "run", boom)
        spawn_gate.qos_demote_pid(12345)
        err = capsys.readouterr().err
        assert "non-fatal" in err
        # Pass-path lines carry the note prefix: only refusals say `spawn-gate:`
        assert err.strip().startswith("spawn-gate note:")


# --- parent-edge visibility (x-7b36 change 12) --------------------------------
# A null spawned_by_session is sometimes correct (an ambiguous identity resolve
# records no lineage rather than a wrong one); the defect was its SILENCE. The
# receipt line names it at spawn time, and the list row carries the field so
# lineage questions debugged through `agents list -J` measure something.


class TestParentEdgeNotice:
    def test_parent_edge_notice_names_reason_and_orphan_check(
        self, monkeypatch, capsys
    ):
        from fno.agents.dispatch import _report_unlinked_parent
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


class TestGateExitReporting:

    def test_run_gate_refusal_names_the_real_exit_not_the_strip_error(
        self, tmp_path, monkeypatch
    ):
        fake = _fake_gate_binary(tmp_path, "#!/bin/sh\nexit 87\n")
        _point_binary(monkeypatch, fake)
        events = []
        monkeypatch.setattr(
            spawn_gate,
            "_emit_gate_event",
            lambda kind, **data: events.append((kind, data)),
        )
        with pytest.raises(spawn_gate.GateRefused):
            spawn_gate.run_gate("w", "pane")
        assert [k for k, _ in events] == ["spawn_gate_refused"]
        data = events[0][1]
        assert data["reason"] == "gate_unavailable"
        assert "exited 87" in data["error"]
        assert "NoneType" not in data["error"]


