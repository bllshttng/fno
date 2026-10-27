#!/usr/bin/env python3
"""Repository entry point for the native worktree-status read.

The status join lives in the fno-agents binary
(crates/fno-agents/src/worktree/status.rs) since the worktree family port;
this shim forwards argv verbatim so the bash lifecycle script and
tests/test-worktree-status.sh keep their spelling.
"""
import os
import shutil
import subprocess
import sys
from pathlib import Path


def _binary() -> str | None:
    override = os.environ.get("FNO_AGENTS_BIN")
    if override and Path(override).is_file():
        return override
    return shutil.which("fno-agents")


if __name__ == "__main__":
    binary = _binary()
    if binary is None:
        print(
            "worktree-status: the fno-agents binary was not found; "
            "run `fno doctor update --rust`.",
            file=sys.stderr,
        )
        raise SystemExit(127)
    raise SystemExit(
        subprocess.run([binary, "worktree-status", *sys.argv[1:]]).returncode
    )
