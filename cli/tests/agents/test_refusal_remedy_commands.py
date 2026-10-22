"""Every remedy command an rm or spawn refusal names must parse at the front door.

A refusal that names a command the CLI does not have sends its reader to a dead
end: rm once told operators to `rotate` a blocked row, and no such command exists.
"""
from __future__ import annotations

import re
from pathlib import Path

import click
import typer.main

from fno.agents.cli import agents_app
from fno.agents.spawn_defaults import SEEDLESS_THREAD_REFUSAL

REPO = Path(__file__).resolve().parents[3]
COMMAND_RE = re.compile(r"fno agents ([a-z][a-z0-9-]*)")


def _resolves(command: str) -> bool:
    # The real agents group: it serves Python commands and synthesizes the
    # Rust-only commands (adopt, reap, ...), so one call covers both runtimes.
    group = typer.main.get_command(agents_app)
    ctx = click.Context(group, info_name="agents")
    return group.get_command(ctx, command) is not None


def _named_commands() -> set[str]:
    sources = [
        (REPO / "crates/fno-agents/src/daemon/rm_refusal_detail.rs").read_text(),
        (REPO / "cli/src/fno/agents/rm_notice.py").read_text(),
        SEEDLESS_THREAD_REFUSAL,
    ]
    return {m.group(1) for text in sources for m in COMMAND_RE.finditer(text)}


def test_every_remedy_command_named_by_an_rm_or_spawn_refusal_parses():
    commands = _named_commands()
    # Positive control: the scan read the sources it claims to read. rm
    # refusals stopped naming `stop` when rm began running its own claude
    # stop (law d-81c6da7e); `rm` is the marker the rm-refusal source
    # always carries now.
    assert {"rm", "adopt", "spawn"} <= commands, commands
    # Negative control: the same resolver refuses a command the CLI lacks.
    assert not _resolves("rotate")
    unresolved = sorted(v for v in commands if not _resolves(v))
    assert unresolved == [], f"refusal text names commands the CLI lacks: {unresolved}"


