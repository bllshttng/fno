"""cmd_spawn gate wiring (x-c5cc task 2.1): the gate runs BEFORE the substrate
fan-out, with the effective substrate, and the receipt stays byte-identical.
"""
from __future__ import annotations

import os

import pytest
from typer.testing import CliRunner

from fno.agents import spawn_gate


@pytest.fixture(autouse=True)
def _canonical_is_caller(monkeypatch):
    # x-85fe: pin canonical == caller so a node-less spawn does NOT move to the
    # canonical root (the AC1-EDGE no-op). These tests exercise gate wiring and
    # receipt shape, not the cwd move; without this, a run from a linked worktree
    # resolves canonical to the main checkout and the receipt/redirect note drift.
    monkeypatch.setenv("FNO_REPO_ROOT", os.getcwd())


@pytest.fixture
def runner() -> CliRunner:
    return CliRunner()


@pytest.fixture
def gate_calls(monkeypatch):
    """Record run_gate invocations; return a releasable guard."""
    calls: list[dict] = []

    class FakeGuard:
        released = 0

        def release(self):
            FakeGuard.released += 1

    def fake_run_gate(
        name, substrate, *, force=False, no_wait=False, route_provider=None,
        account=None, seed=None, session_phase=None, succession_scope=None,
    ):
        calls.append(
            {
                "name": name,
                "substrate": substrate,
                "force": force,
                "no_wait": no_wait,
                "route_provider": route_provider,
                "account": account,
                "seed": seed,
                "session_phase": session_phase,
                "succession_scope": succession_scope,
            }
        )
        return FakeGuard()

    monkeypatch.setattr(spawn_gate, "run_gate", fake_run_gate)
    calls_guard = (calls, FakeGuard)
    return calls_guard


def _fake_created(monkeypatch):
    """Stub dispatch_spawn to a created claude bg result."""
    from fno.agents import dispatch as dispatch_mod

    class R:
        kind = "created"
        name = "w1"
        short_id = "abcd1234"
        provider = "claude"
        reply = None

    monkeypatch.setattr(dispatch_mod, "dispatch_spawn", lambda **kw: R())


def test_bg_spawn_gates_as_bg_and_receipt_is_byte_identical(
    runner, gate_calls, monkeypatch
):
    """AC1-HP: under-cap spawn dispatches; stdout receipt shape unchanged."""
    calls, FakeGuard = gate_calls
    _fake_created(monkeypatch)
    from fno.agents.cli import agents_app

    result = runner.invoke(
        agents_app,
        ["spawn", "--name", "w1", "hi", "--harness", "claude", "--substrate", "thread"],
    )
    assert result.exit_code == 0, result.output
    assert calls == [
        {
            "name": "w1",
            "substrate": "bg",
            "force": False,
            "no_wait": False,
            "route_provider": None,
            "account": None,
            "seed": "hi",
            "session_phase": "",
            "succession_scope": None,
        }
    ]
    # Hand-rolled f-string receipt, byte-parity with the Rust path (LD10).
    # harness axis under `harness`; no -P -> provider/model absent (AC5).
    line = result.stdout.strip().splitlines()[-1]
    assert (
        line
        == '{"name": "w1", "short_id": "abcd1234", "harness": "claude", "status": "live"}'
    )
    assert FakeGuard.released >= 1


def test_succeed_passes_the_role_scope_to_the_gate(runner, gate_calls, monkeypatch):
    calls, _ = gate_calls
    _fake_created(monkeypatch)
    from fno.agents import role
    from fno.agents.cli import agents_app

    monkeypatch.setattr(role, "resolve_role", lambda scopes: (1, "x-epic"))
    result = runner.invoke(
        agents_app,
        [
            "spawn", "--name", "w1", "hi", "--harness", "claude",
            "--substrate", "thread", "--promote", "x-epic", "--succeed",
        ],
    )
    assert result.exit_code == 0, result.output
    assert calls[0].get("succession_scope") == "x-epic"


def test_gate_refusal_propagates_exit_code(runner, monkeypatch):
    """A gate refusal exits with the gate's code; nothing dispatches."""

    def refuse(*a, **k):
        raise spawn_gate.GateRefused(spawn_gate.EXIT_NO_WAIT)

    monkeypatch.setattr(spawn_gate, "run_gate", refuse)
    dispatched = []
    from fno.agents import dispatch as dispatch_mod

    monkeypatch.setattr(
        dispatch_mod, "dispatch_spawn", lambda **kw: dispatched.append(1)
    )
    from fno.agents.cli import agents_app

    result = runner.invoke(
        agents_app,
        [
            "spawn", "--name", "w1", "hi", "--harness", "claude", "--substrate", "thread",
            "--no-wait",
        ],
    )
    assert result.exit_code == spawn_gate.EXIT_NO_WAIT
    assert not dispatched


def test_sandbox_policy_refusal_happens_before_the_gate(
    runner, gate_calls, tmp_path
):
    """A refusal after admission would leak the gate mutex; this one refuses first."""
    calls, _ = gate_calls
    policy = tmp_path / "policy.json"
    policy.write_text("{}")
    from fno.agents.cli import agents_app

    result = runner.invoke(
        agents_app,
        [
            "spawn", "--name", "w1", "hi", "--harness", "claude",
            "--substrate", "pane", "--sandbox-write-policy", str(policy),
        ],
    )
    assert result.exit_code == 2, result.output
    assert "--sandbox-write-policy needs the thread substrate" in result.output
    assert calls == []


