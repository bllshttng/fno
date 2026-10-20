"""AC4-HP: a retired spawn axis refuses, naming the native form.

The eleven axis flags stay DECLARED as hidden refusal stubs so the door
never silently reopens; each callback names the lane template and points
at docs/HARNESSES.md.
"""
from __future__ import annotations

import pytest
from typer.testing import CliRunner

from fno.agents.cli import agents_app

RETIRED_VALUES = [
    ("--model", "opus"),
    ("--provider", "zai"),
    ("--yolo", None),
    ("--permission-mode", "default"),
    ("--effort", "high"),
    ("--resume", "12345678-1234-1234-1234-123456789012"),
    ("--add-dir", "/w"),
    ("--agent", "reviewer"),
    ("--tools", "Read"),
    ("--deny-tools", "Bash"),
]


@pytest.mark.parametrize(
    "flag,value", RETIRED_VALUES, ids=[f[0] for f in RETIRED_VALUES]
)
def test_a_retired_spawn_axis_refuses_naming_the_native_form(
    flag: str, value: str | None
) -> None:
    argv = ["spawn", "x", "--name", "w", flag, *([value] if value else [])]
    result = CliRunner().invoke(agents_app, argv)
    assert result.exit_code == 2, result.output
    assert "retired" in result.output, result.output
    assert "HARNESSES" in result.output, result.output


def test_as_refuses_an_unknown_lane_and_substrate_answers_as_its_alias() -> None:
    result = CliRunner().invoke(agents_app, ["spawn", "x", "--name", "w", "--as", "bg"])
    assert result.exit_code == 2, result.output
    assert "--as must be thread, pane, headless, or cloud" in result.output
    result = CliRunner().invoke(agents_app, ["spawn", "x", "--name", "w", "--substrate", "bg"])
    assert result.exit_code == 2, result.output
    assert "--substrate is now --as" in result.output
