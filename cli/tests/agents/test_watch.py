"""Tests for `fno agents watch` (Group 2, Task 4.3): the observe surface for a
held stream-json thread. Read-only; renders the turn lifecycle from the worker's
frame log. The poll loop's IO is injected so no real socket is needed."""
from __future__ import annotations

from pathlib import Path

import pytest
from typer.testing import CliRunner

from fno.agents.cli import (
    _watch_loop,
    agents_app,
)


@pytest.fixture
def runner() -> CliRunner:
    return CliRunner()


def test_watch_loop_renders_then_exits_on_child_dead(capsys) -> None:
    seq = iter([
        {"frames": [{"kind": "user_echo"}, {"kind": "assistant", "text": "hello"},
                    {"kind": "result", "is_error": False}], "next": 3, "child_alive": True},
        {"frames": [], "next": 3, "child_alive": False},
    ])
    rc = _watch_loop(lambda c: next(seq), sleep_fn=lambda: None)
    assert rc == 0
    out = capsys.readouterr().out
    assert "delivered" in out and "hello" in out and "complete" in out
    assert "thread exited" in out


def test_watch_loop_unreachable_returns_1(capsys) -> None:
    rc = _watch_loop(lambda c: None, sleep_fn=lambda: None)
    assert rc == 1
    assert "not live" in capsys.readouterr().err


def test_cmd_watch_unknown_name_exits_2(tmp_path: Path, monkeypatch, runner: CliRunner) -> None:
    # No registry.json -> name cannot resolve -> exit 2. Also pins that `watch`
    # routes to the Python command (it is not a Rust client verb).
    monkeypatch.setenv("FNO_AGENTS_HOME", str(tmp_path / "agents"))
    monkeypatch.setenv("FNO_AGENTS_RUNTIME", "python")
    result = runner.invoke(agents_app, ["watch", "nobody"])
    assert result.exit_code == 2, (result.stdout or "") + (result.stderr or "")
