"""``fno config plugin install <claude|codex|opencode|agy> [--force] [--dev]``.

The dev channel: it installs your local checkout for development. Over a
release-channel plugin (stable, rc or nightly) it refuses unless ``--dev``.

Thin Typer front door over ``fno-agents plugin-install``, which owns the
stage build, every harness arm, the env cleanup and the reclaim call.
"""
from __future__ import annotations

import subprocess
from pathlib import Path

import typer

plugin_app = typer.Typer(help="Install your local checkout of footnote into a harness, for development")


# --- stage -------------------------------------------------------------------

def _binary() -> Path:
    from fno.rust_binary import resolve_binary

    binary = resolve_binary()
    if binary is None:
        typer.echo(
            "fno config plugin install: the fno-agents binary was not found. "
            "Reinstall fno, run `fno doctor update --rust`, or set FNO_AGENTS_BIN.",
            err=True,
        )
        raise typer.Exit(code=2)
    return binary




@plugin_app.command(
    "install",
    context_settings={"allow_extra_args": True, "ignore_unknown_options": True},
)
def install(
    # typer.Context, never click.Context: typer 0.26+ vendors its own click,
    # so the standalone click.Context annotation is an unsupported parameter
    # type and building this command raises RuntimeError.
    ctx: typer.Context,
    harness: str = typer.Argument("", help="claude | codex | opencode | agy"),
) -> None:
    """Install your local checkout for development. --dev replaces a release-channel plugin.

    Side effects. It builds a stage under ~/.fno/plugin-stage/fno and points
    the harness at it. It removes stale plugin copies. It strips the machine-wide
    CARGO_BUILD_BUILD_DIR and SCCACHE_DIR lines an older install wrote, from
    ~/.zshrc, ~/.bashrc, the Claude settings.json and the Codex config.toml.
    It then runs a reclaim pass over fno's own caches, without the uv cache
    prune. The receipt names every file it rewrote.
    """
    binary = _binary()
    # With allow_extra_args the command-name token arrives bound to the
    # first positional and ctx.args carries everything after it, so the
    # harness word is rebuilt from both instead of trusted from one.
    tail = list(ctx.args)
    if harness and harness != "install":
        tail = [harness, *tail]
    if not tail:
        typer.echo(
            "usage: fno config plugin install <claude|codex|opencode|agy> [flags...]",
            err=True,
        )
        raise typer.Exit(code=2)
    proc = subprocess.run([str(binary), "plugin-install", *tail], check=False)
    if proc.returncode != 0:
        raise typer.Exit(code=proc.returncode)
