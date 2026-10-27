"""The dispatch-model transport at the target seam: pins bypass the command
without a round trip, and an unreachable binary is a named refusal - the
contracts the route family's deleted transport pinned, now pinned at the
surviving caller.
"""
from __future__ import annotations

from fno.target_cli import _resolve_dispatch_model


def test_explicit_pin_bypasses_the_command(monkeypatch) -> None:
    def explode(_command: str, _payload: dict, *a, **k):
        raise AssertionError("a pin must not reach the command")

    monkeypatch.setattr("fno.rust_binary.command_call", explode)
    model, source, chain = _resolve_dispatch_model(explicit="glm-5.3[1m]")
    assert (model, source, chain) == ("glm-5.3[1m]", "explicit", ["explicit"])


def test_task_pin_bypasses_the_command(monkeypatch) -> None:
    def explode(_command: str, _payload: dict, *a, **k):
        raise AssertionError("a pin must not reach the command")

    monkeypatch.setattr("fno.rust_binary.command_call", explode)
    model, source, chain = _resolve_dispatch_model(task_model="gpt-5.6-sol")
    assert (model, source, chain) == ("gpt-5.6-sol", "task-pin", ["task-pin"])


def test_unrouted_dispatch_rides_the_command_and_degrades_named(monkeypatch) -> None:
    from fno import rust_binary

    seen: list[dict] = []

    def fake_call(command: str, payload: dict, *a, **k):
        seen.append(payload)
        return {
            "model": "glm-4.7",
            "source": "provider-default(no-difficulty)",
            "chain": ["provider-default(no-difficulty)"],
        }

    monkeypatch.setattr(rust_binary, "command_call", fake_call)
    model, source, chain = _resolve_dispatch_model(provider="claude")
    assert model == "glm-4.7"
    assert source == "provider-default(no-difficulty)"
    assert chain == ["provider-default(no-difficulty)"]
    assert seen[0]["mode"] == "dispatch_model"
    assert seen[0]["provider"] == "claude"


def test_unreachable_binary_is_a_named_refusal(monkeypatch) -> None:
    from fno import rust_binary

    def fail_call(_command: str, _payload: dict, *a, **k):
        raise rust_binary.CommandUnavailable("route-slot: no binary")

    monkeypatch.setattr(rust_binary, "command_call", fail_call)
    model, source, chain = _resolve_dispatch_model(provider="claude")
    assert model is None
    assert source == "provider-default(no-difficulty)"
    assert "dispatch-model route-slot-unavailable" in chain
