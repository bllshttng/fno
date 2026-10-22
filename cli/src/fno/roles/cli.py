"""The ``fno agents roles`` group. Every op runs in fno-agents
(``spawn-overlay roles <op>``); the fno front claims it before this group.
"""

from __future__ import annotations

from pathlib import Path

import typer

from fno.handoff.output import merge_json_flag


roles_app = typer.Typer(
    name="roles",
    help="Inspect bounded business-role definitions and resolutions.",
    no_args_is_help=True,
)


@roles_app.callback()
def _roles_callback(
    ctx: typer.Context,
    json_output: bool = typer.Option(
        False,
        "--json",
        "-J",
        help="Emit stable structured JSON.",
    ),
    root: Path | None = typer.Option(
        None,
        "--root",
        help="Read layered JSON definitions from this root.",
    ),
    source: list[str] | None = typer.Option(
        None,
        "--source",
        help="Also inspect a source using layer=path (repeatable).",
    ),
) -> None:
    merge_json_flag(ctx, json_output)
    ctx.ensure_object(dict)
    if root is not None:
        ctx.obj["roles_root"] = root
    if source:
        ctx.obj["role_sources"] = tuple(source)
