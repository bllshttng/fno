"""Tests for the `fno agents` advertised menu and hidden-flag wiring.

Typer renders help via Rich, which wraps long lines and intersperses
ANSI escapes. We strip ANSI before substring-checking so the test is
robust to terminal-width-dependent rendering.
"""
from __future__ import annotations

import re

import pytest
from typer.testing import CliRunner

from fno.agents.cli import agents_app


_ANSI_RE = re.compile(r"\x1b\[[0-9;]*[A-Za-z]")


def _strip_ansi(s: str) -> str:
    """Remove ANSI escape sequences and collapse whitespace for substring checks."""
    return _ANSI_RE.sub("", s)


@pytest.fixture
def runner() -> CliRunner:
    return CliRunner()


def _run(runner: CliRunner, *args: str) -> tuple[int, str]:
    result = runner.invoke(agents_app, list(args), terminal_width=200)
    return result.exit_code, _strip_ansi(result.stdout)


# The advertised `fno agents` menu. `history` joins as the advertised
# session-history reader.
_ADVERTISED_AGENTS_COMMANDS = {
    "spawn", "list", "logs", "watch", "attach", "stop", "resume", "status",
    "watchdog", "history", "gate-status", "incident",
}


def test_agents_help_advertises_only_the_menu_commands(runner: CliRunner) -> None:
    """AC1-HP: `fno agents --help` lists at most 12 commands, and exactly the
    advertised set."""
    import click
    import typer.main

    group = typer.main.get_command(agents_app)
    ctx = click.Context(group)
    listed = [
        name
        for name in group.list_commands(ctx)
        if not (cmd := group.get_command(ctx, name)) or not cmd.hidden
    ]
    assert set(listed) == _ADVERTISED_AGENTS_COMMANDS, (
        f"advertised agents commands drifted: {sorted(listed)}"
    )
    assert len(listed) <= 12
