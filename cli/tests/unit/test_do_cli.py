from __future__ import annotations

import click
import typer.main


def test_do_advertises_exactly_ten_direct_children() -> None:
    from fno.do_cli import do_app

    command = typer.main.get_command(do_app)
    assert isinstance(command, click.Group)
    context = click.Context(command, info_name="do")
    # The review leaf moved native: `do review` is claimed by the literal
    # prefix in the fno binary and never registers here.
    assert set(command.list_commands(context)) == {
        "delivery",
        "loops",
        "phase",
        "plan",
        "pr",
        "research",
        "resume",
        "state",
        "target",
        "think",
    }
