"""The spawn's semantic phase label: which command does the message name?

A phase stamps the node's sessions[] row and follows the assignment for
life - a review row is a retirement blocker the row never grows out of - so
the label comes from the command table (spawn_phase.toml, authored in the Rust
tree, shipped here as package data), never from a mangled prefix chain.
Explicit --session-phase wins before this helper runs; when the inference
answers "" on a --node spawn, cmd_spawn refuses with exit 2 and no worker
launches.
"""

from __future__ import annotations

from functools import cache

from fno.config._dispatch_commands import parse_command_token


@cache
def _command_phases() -> dict[str, str]:
    """The command-to-phase vocabulary from the ONE canonical spawn_phase table
    : authored in the Rust tree, shipped here as generated package
    data. The Python reader and any future Rust reader cannot drift."""
    import tomllib
    from importlib.resources import files

    table = tomllib.loads(
        files("fno.agents").joinpath("spawn_phase.toml").read_text(encoding="utf-8")
    )
    phases: dict[str, str] = {}
    for phase, commands in table["phases"].items():
        for command in commands:
            phases[command] = phase
    return phases


def infer_phase(message: str | None) -> str:
    """do | review | blueprint | think | ship | "" when unlabelable.

    First whitespace token, parsed through the one command-token owner; the
    command word looks up in the table. Anything else (prose, unmapped commands,
    empty) answers ""."""
    command = (message or "").lstrip().split(maxsplit=1)[0] if message else ""
    parsed = parse_command_token(command) if command else None
    return _command_phases().get(parsed[0], "") if parsed else ""
