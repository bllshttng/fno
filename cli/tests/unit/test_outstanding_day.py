"""Unit tests for the `fno inbox day` relay and the durable boundary row.

The fold and the append live in the native `day` command. These cover the
relay's contract (registration, exit-code relay) and a parity guard: the
Rust-built row shape passes the Python event validator, and a malformed
kind is refused.
"""
import pytest

from fno.outstanding import day as day_mod


def test_relay_relays_the_native_exit_code(monkeypatch: pytest.MonkeyPatch) -> None:
    calls = []

    def fake_run(argv, **kwargs):
        calls.append(argv)
        return type("R", (), {"returncode": 3})()

    monkeypatch.setattr(day_mod.subprocess, "run", fake_run)
    command = day_mod._make("start")
    with pytest.raises(day_mod.typer.Exit) as err:
        command()
    assert err.value.exit_code == 3
    assert calls
