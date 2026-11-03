"""`fno hello`: the round-trip leaf for the lazy registry.

A bare `fno-py hello` must answer without a subcommand, so the app carries its
payload on the group callback (`invoke_without_command=True`) rather than a
child command - a child command would turn the entry into a two-word group
(the `paths` shape).
"""

from __future__ import annotations

import typer

app = typer.Typer(
    name="hello",
    help="Print a hello from fno.",
    invoke_without_command=True,
    add_completion=False,
)


@app.callback()
def hello() -> None:
    """Print a hello from fno."""
    typer.echo("hello from fno")
