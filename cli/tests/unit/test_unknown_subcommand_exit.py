"""An unknown subcommand must exit nonzero.

A missing verb that exits 0 is byte-identical, to a caller gating on the
exit code, to a genuinely empty result: four no-match readings were once
read as "no node exists" because the verb was `find`, not `search`. Click's
own default already answers nonzero; these tests pin that default so no
future wrapper or entry-point change can flatten it back to silence.
"""
from __future__ import annotations

import os
import subprocess
import sys


def test_unknown_verb_module_mode_exits_nonzero_with_diagnostic():
    """Caller-facing contract: `python -m fno.cli <group> <missing-verb>`.

    Pins both halves a safe count-or-existence read needs: a nonzero exit
    AND a stderr diagnostic, so an empty-stdout reading can never masquerade
    as an empty result set.
    """
    env = {
        **os.environ,
        "COLUMNS": "240",
        "NO_COLOR": "1",
        "TERM": "dumb",
    }
    result = subprocess.run(
        [sys.executable, "-m", "fno.cli", "backlog", "search", "anything"],
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
