"""The groom skill brief's lever contract (`skills/groom/SKILL.md`).

The pass itself is native (`crates/fno-agents/src/backlog/groom.rs`); the
skill the worker loads stays here, bound to the real CLI.
"""
from __future__ import annotations

from pathlib import Path

import pytest

SKILL = Path(__file__).resolve().parents[3] / "skills" / "groom" / "SKILL.md"


# (command, flags the brief teaches for it), each bound to the real CLI below.
LEVERS = [
    ("supersede", ("--replaces", "--reason")),
    ("defer", ("--reason",)),
    ("update", ("--priority", "--blocked-by")),
    # rank left this table when it became the operator's pin: the verb refuses
    # a groom worker, so teaching it here would teach a refusal. The vote it
    # was replaced by is the lever below.
    ("encounter", ("--evidence",)),
]


@pytest.mark.parametrize("command,flags", LEVERS)
def test_brief_levers_exist_on_the_real_cli(command, flags):
    """The brief must not teach a flag the CLI does not have.

    A worker follows this brief literally, so a wrong signature fails at exit 2
    on the lever rather than anywhere visible. Substring checks on the doc alone
    cannot catch that - this binds it to the actual command.
    """
    text = SKILL.read_text()
    assert f"fno backlog {command}" in text
    assert all(flag in text for flag in flags)

    import click
    import typer.main

    from fno.graph.cli import cli as graph_cli

    root = typer.main.get_command(graph_cli)
    sub = root.get_command(click.Context(root), command)
    if sub is None:
        # Retired python leaves answer natively; bind the lever to the
        # binary's own help surface instead.
        from tests._native_door import run_native

        code, out, err = run_native("backlog", command, "--help")
        assert code == 0, f"`fno backlog {command}` does not exist natively"
        missing = {f for f in flags if f not in out + err}
        assert not missing, f"`fno backlog {command}` has no {sorted(missing)}"
        return

    available: set[str] = set()
    for param in sub.params:
        available.update(param.opts)
    missing = set(flags) - available
    assert not missing, f"`fno backlog {command}` has no {sorted(missing)}"
