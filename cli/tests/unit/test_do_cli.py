from __future__ import annotations

import click
import typer.main


def test_do_advertises_exactly_eight_direct_children() -> None:
    from fno.do_cli import do_app

    command = typer.main.get_command(do_app)
    assert isinstance(command, click.Group)
    context = click.Context(command, info_name="do")
    # The review, research and think leaves moved native: the fno binary
    # claims them by literal prefix and they never register here.
    assert set(command.list_commands(context)) == {
        "delivery",
        "loops",
        "phase",
        "plan",
        "pr",
        "resume",
        "state",
        "target",
    }
