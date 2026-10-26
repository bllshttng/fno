from __future__ import annotations

import click
import typer
import typer.main
from typer.testing import CliRunner


runner = CliRunner()


def test_doctor_lists_direct_actions() -> None:
    from fno.doctor_cli import doctor_app

    command = typer.main.get_command(doctor_app)
    assert isinstance(command, click.Group)
    context = click.Context(command, info_name="doctor")
    assert set(command.list_commands(context)) == {
        "bash-census",
        "bundle",
        "codemap",
        "evals",
        "event",
        "footprint",
        "harness",
        "harness-matrix",
        "intel",
        "lanes",
        "lint",
        "observer",
        "plugin-file",
        "reclaim",
        "scratch",
        "stalls",
        "test",
        "update",
    }


def test_harness_and_harness_matrix_refuse_without_the_binary(
    monkeypatch,
) -> None:
    """No binary, no fallback: the leaf refuses with the named reason and
    exit 127, the same shape `fno doctor scratch` has."""
    from fno.cli import app

    monkeypatch.setattr("fno.rust_binary.resolve_installed_binary", lambda: None)
    for argv in (
        ["doctor", "harness", "codex"],
        ["doctor", "harness-matrix"],
    ):
        result = runner.invoke(app, argv)
        assert result.exit_code == 127, (argv, result.output, result.stderr)
        assert "fno-agents" in (result.output + (result.stderr or "")), argv
