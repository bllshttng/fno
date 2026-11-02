"""US8 spawn-seam injector: config.agents.defaults -> argv (x-de9d).

Precedence explicit flag > config > builtin, resolved field-by-field. Provider
validated (exit 2 on a bad name); config-sourced effort degrades open on a
no-surface provider while an explicit --effort stays fail-closed downstream.
"""
from __future__ import annotations
import pytest


def test_is_command_seed_is_the_first_token_fire_test_over_both_sigils():
    """The fire test accepts both sigils at index 0 and still reads a
    command inside prose as a conversation."""
    from fno.config._dispatch_commands import is_command_seed

    assert is_command_seed("$fno:target x-caf8") is True
    assert is_command_seed("/fno:target x-caf8") is True
    assert is_command_seed("do a /fno:blueprint") is False
    assert is_command_seed("/absolute/path/to/thing") is False
    assert is_command_seed("review /fno:target x-1") is False


@pytest.fixture
def overlay_answer(monkeypatch):
    calls = []

    def install(answer):
        def fake(payload, *a, **k):
            calls.append(payload)
            return dict(answer)

        monkeypatch.setattr(
            "fno.agents.spawn_overlay_client.spawn_overlay_call", fake
        )
        return calls

    return install


