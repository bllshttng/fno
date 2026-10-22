"""The read-only capacity probe behind `fno agents gate-status`.

The probe's verdict mapping (lanes at cap, the RAM floor, the CPU axis, the
lead share) is decided inside the ONE Rust gate and pinned by its tests
(`crates/fno-agents/src/spawn_gate_command.rs`). What stays Python-side is the
transport: the answer comes from the command verbatim, and an unanswered gate is
``verdict: unknown``, never saturation and never a raise."""
from __future__ import annotations

from fno.agents import spawn_gate


def _stub_probe(monkeypatch, answer=None, error=None):
    from fno import rust_binary

    def fake(command, payload, **_kwargs):
        assert command == "spawn-gate"
        assert payload["mode"] == "probe"
        return answer

    def boom(*_a, **_k):
        raise error

    monkeypatch.setattr(
        rust_binary, "command_call", boom if error is not None else fake
    )


def test_probe_never_raises_and_reports_unknown_on_an_unanswered_gate(monkeypatch):
    from fno.rust_binary import CommandUnavailable

    _stub_probe(monkeypatch, error=CommandUnavailable("exited 1"))
    answer = spawn_gate.probe_capacity()
    assert answer["verdict"] == "unknown"
    assert answer["reason"] == "gate_unavailable"
    assert "exited 1" in answer["error"]
