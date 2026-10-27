from __future__ import annotations

import click
import typer.main


def test_do_advertises_exactly_seven_direct_children() -> None:
    from fno.do_cli import do_app

    command = typer.main.get_command(do_app)
    assert isinstance(command, click.Group)
    context = click.Context(command, info_name="do")
    # The review, research and think leaves moved native: the fno binary
    # claims them by literal prefix and they never register here. `loops`
    # joined them when the fleet family ported: the front's fleet route
    # claims `do loops` and execs the fno-agents loops door itself.
    assert set(command.list_commands(context)) == {
        "delivery",
        "phase",
        "plan",
        "pr",
        "resume",
        "state",
        "target",
    }
