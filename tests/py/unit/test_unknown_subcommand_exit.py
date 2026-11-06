"""An unknown subcommand must exit nonzero.

A missing command that exits 0 is byte-identical, to a caller gating on the
exit code, to a genuinely empty result: four no-match readings were once
read as "no node exists" because the command was `find`, not `search`. This
pins the refusal so no future front-door change can flatten it to silence.
"""
from __future__ import annotations

import os
import subprocess

import pytest

from _bins import resolve_front


def test_unknown_command_exits_nonzero_with_diagnostic():
    """Caller-facing contract: `fno <group> <missing-command>`.

    Pins both halves a safe count-or-existence read needs: a nonzero exit
    AND a stderr diagnostic, so an empty-stdout reading can never masquerade
    as an empty result set.
    """
    front = resolve_front()
    if front is None:
        pytest.skip("no fno binary built")
    env = {**os.environ, "COLUMNS": "240", "NO_COLOR": "1", "TERM": "dumb"}
    result = subprocess.run(
        [front, "backlog", "search", "anything"],
        capture_output=True,
        text=True,
        env=env,
        timeout=300,
    )
    assert result.returncode != 0, (
        f"expected nonzero rc, got {result.returncode}.\n"
        f"stdout={result.stdout}\nstderr={result.stderr}"
    )
    assert "No such command" in result.stderr, result.stderr
