from __future__ import annotations

import subprocess
from pathlib import Path

from typer.testing import CliRunner


def test_door_forwards_extra_args_to_fno_agents(monkeypatch) -> None:
    """The plugin-install door forwards every harness and flag to the Rust
    command byte-exact: the opencode arm's --yes/--dry-run and the codex
    arm's --force live in Rust, and the Typer door must accept them without
    declaring them."""
    from fno.setup.plugin_install_cli import plugin_app

    class _Proc:
        returncode = 0

    captured: list = []

    def fake_run(argv, check=False):
        captured.append(argv)
        return _Proc()

    monkeypatch.setattr(subprocess, "run", fake_run)
    monkeypatch.setattr("fno.setup.plugin_install_cli._binary", lambda: Path("/bin/true"))
    for args in (["opencode", "--dry-run", "--yes"], ["codex", "--force", "--source", "/src"]):
        result = CliRunner().invoke(plugin_app, ["install", *args])
        assert result.exit_code == 0, result.output
        assert captured[-1][1:] == ["plugin-install", *args]
