"""The whoami leaves other families still own.

``fno whoami`` itself is native (the agent family's port deleted the Python
render), but its hidden leaves still register here: ``ledger`` serves
``fno.agents.history``, ``scoreboard`` serves ``fno.scoreboard.cli``, and
``cost`` forwards to the native session-cost leg. Each is the same one-line
re-registration the deleted group mounted, so the retired roots
(``fno cost`` -> ``whoami cost``) keep a registered destination and every
spelling stays invocable one release out. The ``context`` leaf is gone:
the diag family's port claimed it natively (``context-run --probe
--summary``).
"""
from __future__ import annotations

import subprocess

import typer

whoami_app = typer.Typer(
    name="whoami",
    help="Operating-stack summary: project + fleet + walker + session + harness.",
)


def cost_command(
    ctx: typer.Context,
) -> None:
    """Forward all args to the native session-cost leg.

    The leaf keeps the command spelling and forwards verbatim; the native
    body owns the flag parsing and the exit code, so this wrapper never
    interprets the args.
    """
    from fno.rust_binary import resolve_binary

    binary = resolve_binary()
    if binary is None:
        raise typer.Exit(code=14)
    result = subprocess.run(
        [str(binary), "context-run", "--session-cost", *list(ctx.args)],
        check=False,
    )
    raise typer.Exit(code=result.returncode)


whoami_app.command(
    "cost",
    hidden=True,
    context_settings={"allow_extra_args": True, "ignore_unknown_options": True},
)(cost_command)

from fno.agents.history import history_command  # noqa: E402
from fno.scoreboard.cli import scoreboard_command  # noqa: E402

whoami_app.command("ledger", hidden=True)(history_command)
whoami_app.command("scoreboard", hidden=True)(scoreboard_command)
