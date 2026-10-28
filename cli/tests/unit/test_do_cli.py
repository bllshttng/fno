from __future__ import annotations

import click
import typer.main


def test_do_advertises_exactly_four_direct_children() -> None:
    from fno.do_cli import do_app

    command = typer.main.get_command(do_app)
    assert isinstance(command, click.Group)
    context = click.Context(command, info_name="do")
    # The review, research and think leaves moved native: the fno binary
    # claims them by literal prefix and they never register here. The target,
    # state, phase, and stub-manifest mounts joined them when the target
    # family ported native: the front door claims the whole `do target`,
    # `do state`, and `do phase` groups and `do pr stub-manifest`. `loops`
    # joined them when the fleet family ported: the front's fleet route
    # claims `do loops` and execs the fno-agents loops door itself.
    assert set(command.list_commands(context)) == {
        "delivery",
        "plan",
        "pr",
        "resume",
    }
