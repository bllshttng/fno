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


def test_agents_registers_the_folded_roots() -> None:
    # One launch command, and it is spawn. `restart` answers at the Rust front
    # door, so it is no folded root. The worker family port retired `worker`;
    # its answers live at `fno do pr`, `fno backlog reconcile`, and spawn.
    # mcp and lead left the fold: both families are native and the front
    # door answers them (the front's agents-mcp route answers the send
    # command). The roles port moved its last callers onto the native door.
    # The fleet family port retired `autonomy`: the front's fleet route
    # claims the spelling and the native spawn-overlay door answers it.
    from fno.agents.cli import agents_app

    assert {
        "claim",
    } <= _commands(agents_app)


def test_importing_agents_cli_does_not_import_folded_subgroups() -> None:
    modules = {
        "fno.claims.cli",
        "fno.dispatch",
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
