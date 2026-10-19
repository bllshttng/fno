"""Unit tests for `fno help [verb...]` — git-style help subcommand."""
from __future__ import annotations

import os
import re
import subprocess
import sys

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
    # Advertised verbs appear in both curated catalogs (x-71b6 tiering).
    # `setup` moved under `config` (unit 6, x-9d6c) and left the menu.
    for needle in ("backlog", "agents", "config", "doctor"):
        assert needle in via_help.output, f"missing {needle} in `fno help` output"
        assert needle in via_dashdash.output, f"missing {needle} in `fno --help` output"
    # Hidden verbs are reachable only through the full-surface door. Moved
    # spellings render nowhere in it (x-6233: the block is gone and each
    # alias prints its new home on use), so the needles are real roots.
    via_all = runner.invoke(app, ["help", "--all"])
    assert via_all.exit_code == 0
    for needle in ("backlog", "inbox", "mux", "version", "update"):
        assert needle in via_all.output, f"missing {needle} in `fno help --all` output"
    for gone in ("claim", "event", "pr", "done", "mail", "workspace"):
        assert not re.search(rf"^\s+{gone}\s", via_all.output, re.MULTILINE), (
            f"moved spelling {gone!r} must not render as a row"
        )


def test_help_subcommand_works_in_module_mode():
    """AC-FR: `python -m fno.cli help <verb>` works (codex P2 fix).

    Regression for chatgpt-codex-connector P2 on PR #250: the original
    implementation forwarded via `sys.argv[0]`, which is the module file
    (cli.py) when invoked as `python -m fno.cli`. That file is
    typically not chmod +x and has no shebang, so the forward crashed
    with PermissionError or FileNotFoundError. The fix falls back to
    `[sys.executable, "-m", "fno.cli", ...]` when sys.argv[0] is
    not directly executable.

    This test invokes the module-mode form via a real subprocess and
    asserts the help output renders for a known subcommand (`claim`).
    """
    # Use COLUMNS/NO_COLOR to keep the help output stable in CI.
    env = {
        **os.environ,
        "COLUMNS": "240",
        "NO_COLOR": "1",
        "TERM": "dumb",
    }
    result = subprocess.run(
        [sys.executable, "-m", "fno.cli", "help", "claim"],
        capture_output=True,
        text=True,
        env=env,
        timeout=300,
    )
    assert result.returncode == 0, (
        f"expected rc=0, got rc={result.returncode}.\n"
        f"stdout={result.stdout}\nstderr={result.stderr}"
    )
    # `fno agents claim --help` output mentions the subcommand verbs the claim
    # app registers (acquire, release, ...). ("gate" died with ab-d0337fbc.)
    assert "acquire" in result.stdout
    assert "release" in result.stdout
