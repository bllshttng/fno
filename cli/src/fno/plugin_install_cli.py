"""``fno config plugin install <claude|codex|opencode|agy> [--force] [--dev]``.

The dev channel: it installs your local checkout for development. Over a
release-channel plugin (stable, rc or nightly) it refuses unless ``--dev``.

Thin Typer front door over ``fno-agents plugin-install``: the Rust command owns
the stage build, the claude/opencode/agy arms, the env cleanup and the
reclaim call. The codex arm stays on the Python ``converge`` engine, which
receives the stage via ``plugin-install --stage-only``.
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
    ~/.zshrc, ~/.bashrc, ~/.claude/settings.json and the Codex config.toml.
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
    harness, extra = tail[0], tail[1:]
    force = "--force" in extra or "-F" in extra
    argv = ["plugin-install"]
    if harness == "codex":
        # The codex arm stays on the Python converge engine (ship-phase ruling).
        stage_out = subprocess.run(
            [str(binary), "plugin-install", "--stage-only", "codex", *extra],
            capture_output=True,
            text=True,
            check=False,
        )
        if stage_out.returncode != 0:
            typer.echo(stage_out.stderr or "stage build failed", err=True)
            raise typer.Exit(code=stage_out.returncode)
        stage = Path(stage_out.stdout.strip().splitlines()[-1])
        from fno.setup.codex_plugin import CodexPluginError, converge

        try:
            result = converge(channel="dev", refresh=force, source_root=stage)
        except CodexPluginError as exc:
            typer.echo(f"codex arm: {exc.stage}: {exc.detail}", err=True)
            raise typer.Exit(code=1) from exc
        typer.echo(
            f"plugin install codex: converged {result.plugin_id} {result.version} "
            f"(action={result.action})"
        )
        argv = ["plugin-install", "--env-only"]
    else:
        argv += [harness, *extra]
    proc = subprocess.run([str(binary), *argv], check=False)
    if proc.returncode != 0:
        raise typer.Exit(code=proc.returncode)
