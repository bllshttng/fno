"""Unit tests for `fno help [command...]` — git-style help subcommand."""
from __future__ import annotations


from typer.testing import CliRunner

from fno.cli import app

runner = CliRunner()


def test_help_root_matches_dashdash_help():
    """AC-FR: `fno help` and `fno --help` produce equivalent top-level catalogs.

    We compare a few stable substrings rather than full output because rich
    rendering may differ across runs and the help string is long.
    """
    via_help = runner.invoke(app, ["help"])
    via_dashdash = runner.invoke(app, ["--help"])
    assert via_help.exit_code == 0
    assert via_dashdash.exit_code == 0
    # Advertised commands appear in both curated catalogs (x-71b6 tiering).
    # `setup` moved under `config` (unit 6, x-9d6c) and left the menu.
    for needle in ("backlog", "config", "doctor"):
        assert needle in via_help.output, f"missing {needle} in `fno help` output"
        assert needle in via_dashdash.output, f"missing {needle} in `fno --help` output"
    # Hidden commands are reachable only through the full-surface door.
    via_all = runner.invoke(app, ["help", "--all"])
    assert via_all.exit_code == 0
    for needle in ("backlog", "mux", "version", "update"):
        assert needle in via_all.output, f"missing {needle} in `fno help --all` output"
