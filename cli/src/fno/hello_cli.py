"""The ``fno hello`` leaf: one line on stdout, exit 0.

A dependency-free probe a fresh install can answer: the command resolves
through the lazy registry (``fno.cli`` -> ``fno.hello_cli``) as a plain
single-command function (the ``cost_command`` shape, not a group), so a green
``fno-py hello`` proves the wheel imports and a lazy entry loads, without a
database, the Rust binary, or config.
"""

from __future__ import annotations

import typer


def hello_command() -> None:
    """Print the greeting."""
    typer.echo("hello from fno")
