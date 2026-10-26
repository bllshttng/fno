from __future__ import annotations

import click
import json
import os
from pathlib import Path
import subprocess
import sys
import typer.main
from typer.testing import CliRunner


runner = CliRunner()
REPO_ROOT = Path(__file__).resolve().parents[3]


def _commands(app) -> set[str]:
    command = typer.main.get_command(app)
    assert isinstance(command, click.Group)
    return set(command.list_commands(click.Context(command)))


def test_agents_registers_the_six_folded_roots() -> None:
    # One launch command, and it is spawn. `restart` answers at the Rust front
    # door, so it is no folded root. mcp left the fold: the family is native
    # and the front's agents-mcp route answers the send command.
    from fno.agents.cli import agents_app

    assert {
        "autonomy",
        "claim",
        "lead",
        "mail",
        "roles",
        "worker",
    } <= _commands(agents_app)


def test_agents_lead_excludes_board_while_inbox_keeps_it() -> None:
    from fno.inbox.cli import inbox_app
    from fno.lead.cli import agents_lead_app

    assert "board" not in _commands(agents_lead_app)
    assert "board" in _commands(inbox_app)


def test_importing_agents_cli_does_not_import_folded_subgroups() -> None:
    modules = {
        "fno.claims.cli",
        "fno.dispatch",
        "fno.lead.cli",
        "fno.mail.cli",
        "fno.roles.cli",
        "fno.worker.cli",
    }
    script = (
        "import json, sys; import fno.agents.cli; "
        f"print(json.dumps(sorted(set(sys.modules) & {modules!r})))"
    )
    env = {**os.environ, "PYTHONPATH": str(REPO_ROOT / "cli/src")}
    result = subprocess.run(
        [sys.executable, "-c", script],
        cwd=REPO_ROOT,
        env=env,
        capture_output=True,
        text=True,
        check=True,
    )

    assert json.loads(result.stdout) == []
