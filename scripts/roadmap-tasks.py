#!/usr/bin/env python3
"""Compatibility shim. Real implementation lives in the backlog graph CLI.

Everything forwards through the ``fno`` front door's backlog compat
spelling, which rides the same graph surface the retired direct import
used; the in-repo ``fno.graph.cli`` import leg is gone with the consumers
repoint.
"""
import os
import subprocess
import sys

GUARD = "FNO_ROADMAP_SHIM_ACTIVE"


def _forward(argv: list[str]) -> int:
    """One front-door call; a missing `fno` refuses loudly.

    The marker rides the child env: a `fno` that resolves back to THIS shim
    (a PATH loop, as in a mocked test sandbox) re-enters here and refuses
    after one hop instead of forking until the machine dies.
    """
    if os.environ.get(GUARD):
        sys.stderr.write(
            "error: roadmap-tasks shim re-entered through `fno`; refusing to "
            "recurse. The `fno` on PATH loops back to this shim.\n"
        )
        return 3
    try:
        return subprocess.call(["fno", "backlog", *argv],
                               env={**os.environ, GUARD: "1"})
    except FileNotFoundError:
        sys.stderr.write(
            "error: fno CLI not found. Install fno, then re-run: "
            "fno backlog " + " ".join(argv) + "\n"
        )
        return 3


if __name__ == "__main__":
    sys.exit(_forward(sys.argv[1:]))
