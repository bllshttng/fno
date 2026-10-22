"""Read-only role definition and resolution inspection commands."""

from __future__ import annotations

import json
from datetime import UTC, datetime
from pathlib import Path
from typing import Any

import typer

from fno.config import load_settings
from fno.handoff.output import json_mode, merge_json_flag
from fno.roles.context import build_artifact_catalog, catalog_revision


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


def _json_requested(ctx: typer.Context, local: bool) -> bool:
    merge_json_flag(ctx, local)
    return json_mode(ctx)


def _emit_json(value: Any) -> None:
    typer.echo(json.dumps(value, indent=2, sort_keys=True))


@roles_app.command("context")
def context_catalog(
    ctx: typer.Context,
    snapshot: str | None = typer.Option(
        None,
        "--snapshot",
        help="Alignment revision stamped on every reference (pass to `resolve --snapshot`).",
    ),
    json_output: bool = typer.Option(
        False,
        "--json",
        "-J",
        help="Emit a stable JSON array (pipes into `resolve --context`).",
    ),
) -> None:
    """Emit honest ContextReference observations for configured artifacts.

    Reads the generic ``[context.artifacts]`` map and produces one observation
    per entry: a real sha256 and byte size for a readable file, or
    ``readable=false`` with a naming reason for an unreadable one. Composes
    with ``resolve``: ``fno agents roles resolve --snapshot <rev> --context <(fno
    agents roles context --json --snapshot <rev>) ...``.
    """
    settings = load_settings()
    artifacts = settings.context.artifacts
    revision = snapshot or catalog_revision(artifacts)
    references = build_artifact_catalog(
        artifacts, snapshot_revision=revision, clock=datetime.now(UTC)
    )
    if _json_requested(ctx, json_output):
        _emit_json([reference.model_dump(mode="json") for reference in references])
        return
    if not references:
        typer.echo(
            "no artifacts configured ([context.artifacts] in .fno/config.toml); "
            f"revision={revision}"
        )
        return
    for reference in references:
        if reference.readable:
            digest = reference.content_digest[:12] if reference.content_digest else "(none)"
            state = f"ok digest={digest}"
        else:
            state = f"unreadable: {reference.unavailable_reason}"
        typer.echo(
            f"{reference.identifier} sensitivity={reference.sensitivity.value} "
            f"bytes={reference.byte_size} {state} revision={revision}"
        )
