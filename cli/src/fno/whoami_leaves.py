"""The whoami leaves other families still own.

``fno whoami`` itself is native (the agent family's port deleted the Python
render), but its four hidden leaves still run Python: ``context`` serves
``fno.context_probe``, ``ledger`` serves ``fno.agents.history``,
``scoreboard`` serves ``fno.scoreboard.cli``, and ``cost`` serves
``fno.cost._session_cost``. Each is the same one-line re-registration the
deleted group mounted, so the retired roots (``fno context`` -> ``whoami
context``, ``fno cost`` -> ``whoami cost``) keep a registered destination
and every spelling stays invocable one release out.
"""
from __future__ import annotations

import contextlib
import io
import sys

import typer

whoami_app = typer.Typer(
    name="whoami",
    help="Operating-stack summary: project + fleet + walker + session + harness.",
)


def cost_command(
    ctx: typer.Context,
) -> None:
    """Forward all args to the in-package cost command (``fno.cost._session_cost``).

    The heavy import stays deferred to call time, so loading this group
    never pays for it. argparse owns the flag parsing; the bridge runs via
    ``sys.argv`` and translates its ``SystemExit`` into a ``typer.Exit`` so
    the exit code propagates cleanly.
    """
    from fno.cost import _session_cost

    argv_backup = sys.argv
    sys.argv = ["fno whoami cost", *list(ctx.args)]
    try:
        _session_cost.main()
    except SystemExit as exc:  # argparse exits on -h / parse error / explicit exit
        code = exc.code
        if code is None:
            code = 0
        elif not isinstance(code, int):
            code = 1
        raise typer.Exit(code=code)
    finally:
        sys.argv = argv_backup


whoami_app.command(
    "cost",
    hidden=True,
    context_settings={"allow_extra_args": True, "ignore_unknown_options": True},
)(cost_command)

from fno.context_probe import context_command  # noqa: E402
from fno.agents.history import history_command  # noqa: E402
from fno.scoreboard.cli import scoreboard_command  # noqa: E402

whoami_app.command("context", hidden=True)(context_command)
whoami_app.command("ledger", hidden=True)(history_command)
whoami_app.command("scoreboard", hidden=True)(scoreboard_command)
