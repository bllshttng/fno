"""The native test-env door: neutralise/poison for Python test runners.

The env assembly is native since the Rust test command landed
(crates/fno-agents/src/test_env.rs); the two pytest roots apply the SAME
env the native command runs in-process, through this door rather than a
second Python implementation. A missing or broken door refuses loudly:
running this tree unisolated is the failure mode the hermetic fence
exists to prevent.
"""

from __future__ import annotations

import json
import os
import shutil
import subprocess
import sys
from pathlib import Path


def native_env(mode: str, sandbox=None, fixtures=None) -> dict:
    """Apply the native neutralise/poison to os.environ through the door."""
    return apply_env(mode, dict(os.environ), sandbox=sandbox, fixtures=fixtures)


def apply_env(mode: str, env: dict, sandbox=None, fixtures=None) -> dict:
    """Apply the native neutralise/poison to one caller-supplied env."""
    if env.get("FNO_TEST_HERMETIC") == "1":
        # The fence's own receipt: the runner neutralises in-process before
        # the pytest legs run, then deletes the dev build so the parity
        # suites skip. The env already came through this assembly, and the
        # binary is legitimately absent there - re-applying would refuse.
        return dict(env)
    payload: dict = {"mode": mode, "env": dict(env)}
    if sandbox is not None:
        payload["sandbox"] = str(sandbox)
    if fixtures is not None:
        payload["fixtures"] = str(fixtures)
    # The claim-door contract: FNO_AGENTS_BIN outranks PATH. The smoke runner
    # pins it to a preserved copy of itself before scrubbing target/, so the
    # pytest legs reach the door with a fresh binary while the parity marker
    # stays deleted.
    pinned = env.get("FNO_AGENTS_BIN") or ""
    binary = pinned if pinned and Path(pinned).is_file() else shutil.which("fno-agents")
    if binary is None:
        sys.stderr.write(
            "conftest: fno-agents is not on PATH or FNO_AGENTS_BIN; the "
            "hermetic env is native. Install the crate (cargo install --path "
            "crates/fno-agents) or run the suite through 'fno doctor test'.\n"
        )
        raise SystemExit(3)
    proc = subprocess.run(
        [binary, "test-run", "env", json.dumps(payload)], capture_output=True, text=True
    )
    if proc.returncode != 0:
        sys.stderr.write(proc.stderr)
        raise SystemExit(3)
    try:
        env = json.loads(proc.stdout)["env"]
    except (json.JSONDecodeError, KeyError, TypeError):
        sys.stderr.write("conftest: test-run env door printed no env object\n")
        raise SystemExit(3)
    return {str(k): str(v) for k, v in env.items()}
