"""The ``fno agents roles`` group. Every op runs in fno-agents
(``spawn-overlay roles <op>``); the fno front claims it before this group.
"""

from __future__ import annotations

from pathlib import Path

import typer


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
    # merge_json_flag's 3-line merge, inlined (x-9516): the handoff module
    # dies with the family port and this caller survives it.
    ctx.ensure_object(dict)
    if json_output:
        ctx.obj["json"] = True
    if root is not None:
        ctx.obj["roles_root"] = root
    if source:
        ctx.obj["role_sources"] = tuple(source)
