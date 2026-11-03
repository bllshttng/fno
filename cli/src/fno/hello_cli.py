"""`fno hello` - print a greeting from the Python CLI.

Lazy entries keep group shape (fno._lazy_group.typer_group_shape), so the
greeting lives on the app callback: `fno hello` (no subcommand) prints it.
"""
from __future__ import annotations

import typer

app = typer.Typer(
    name="hello",
    help="Print a greeting from fno.",
    invoke_without_command=True,
)


@app.callback()
def hello() -> None:
    """Print the greeting."""
    typer.echo("hello from fno")
