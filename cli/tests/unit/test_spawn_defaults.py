"""US8 spawn-seam injector: config.agents.defaults -> argv (x-de9d).

Precedence explicit flag > config > builtin, resolved field-by-field. Provider
validated (exit 2 on a bad name); config-sourced effort degrades open on a
no-surface provider while an explicit --effort stays fail-closed downstream.
"""
from __future__ import annotations
import pytest



@pytest.mark.parametrize(
    ("provider", "computed_dirs", "expected"),
    [
        ("codex", ["/tmp/state", "/tmp/claims-root"], True),
        ("codex", ["/tmp/state"], False),
    ],
)
def test_claim_store_writable_is_tri_state(provider, computed_dirs, expected, monkeypatch):
    from pathlib import Path

    from fno.agents import mux_spawn

    monkeypatch.setattr("fno.claims.io.global_claims_root", lambda: Path("/tmp"))
    monkeypatch.setattr("fno.claims.io.claims_dir", lambda root=None: Path("/tmp/claims-root"))

    assert mux_spawn._claim_store_writable(provider, computed_dirs) is expected


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


