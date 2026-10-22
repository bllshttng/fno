"""The route the wheel keeps for commands whose bodies moved to the Rust
dispatcher: forward the whole argv to `fno-agents backlog <command>`.

The triage forward carried this shape first; intake and decompose joined
when their wheel bodies ported. The wheel keeps the route so a front door
whose `fno` resolves to this wheel still serves the command, and the
classification/collapse registries keep describing a live command.
"""

from __future__ import annotations

import subprocess

import typer


def _native_forward(command: str):
    def _forward(ctx: typer.Context) -> None:
        """The native door owns the command; the wheel keeps the route.

        The whole argv rides `fno-agents backlog <command>`; its output is
        echoed back through the wheel and its exit code returned. The child
        gets `FNO_STATE_DIR` set to the state root the wheel's own graph
        resolver answers, so both legs read one store even when that root
        came from config or an in-process pin the child cannot see.
        """
        import os

        from fno import paths, rust_binary

        binary = rust_binary.resolve_binary()
        if binary is None:
            typer.echo(
                f"Error: `fno backlog {command}` is served by the native door; "
                "no fno-agents binary found.",
                err=True,
            )
            raise typer.Exit(code=2)
        anchor = paths.graph_json().parent
        state_root = anchor.parent if anchor.name == "db" else anchor
        proc = subprocess.run(
            [str(binary), "backlog", command, *ctx.args],
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            env={**os.environ, "FNO_STATE_DIR": str(state_root)},
        )
        if proc.stdout:
            typer.echo(proc.stdout, nl=False)
        if proc.stderr:
            typer.echo(proc.stderr, nl=False, err=True)
        raise typer.Exit(code=proc.returncode)

    return _forward


def register_native_forward(cli, command_name: str, *, hidden: bool = True) -> None:
    """Register the hidden `<command>` route on the backlog group."""
    cli.command(
        command_name,
        hidden=hidden,
        context_settings={"allow_extra_args": True, "ignore_unknown_options": True},
        add_help_option=False,
    )(_native_forward(command_name))
