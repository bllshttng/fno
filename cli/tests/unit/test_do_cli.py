from __future__ import annotations

import click
import typer.main


def test_do_advertises_exactly_five_direct_children() -> None:
    from fno.do_cli import do_app

    command = typer.main.get_command(do_app)
    assert isinstance(command, click.Group)
    context = click.Context(command, info_name="do")
    # The review, research and think leaves moved native: the fno binary
    # claims them by literal prefix and they never register here. The target,
    # state, phase, and stub-manifest mounts joined them when the target
    # family ported native: the front door claims the whole `do target`,
    # `do state`, and `do phase` groups and `do pr stub-manifest`.
    assert set(command.list_commands(context)) == {
        "delivery",
        "loops",
        "plan",
        "pr",
        "resume",
    }
