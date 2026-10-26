"""The native test-env door: neutralise/poison for Python test runners.

The env assembly is native since the Rust test verb landed
(crates/fno-agents/src/test_env.rs); the two pytest roots apply the SAME
env the native verb runs in-process, through this door rather than a
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


def native_env(mode: str, sandbox=None, fixtures=None) -> dict:
    """Apply the native neutralise/poison to os.environ through the door."""
    return apply_env(mode, dict(os.environ), sandbox=sandbox, fixtures=fixtures)


def apply_env(mode: str, env: dict, sandbox=None, fixtures=None) -> dict:
    """Apply the native neutralise/poison to one caller-supplied env."""
    payload: dict = {"mode": mode, "env": dict(env)}
    if sandbox is not None:
        payload["sandbox"] = str(sandbox)
    if fixtures is not None:
        payload["fixtures"] = str(fixtures)
    binary = shutil.which("fno-agents")
    if binary is None:
        sys.stderr.write(
            "conftest: fno-agents is not on PATH; the hermetic env is native. "
            "Install the crate (cargo install --path crates/fno-agents) or "
            "run the suite through 'fno doctor test'.\n"
        )
        raise SystemExit(3)
    proc = subprocess.run(
        [binary, "test-env", json.dumps(payload)], capture_output=True, text=True
    )
    if proc.returncode != 0:
        sys.stderr.write(proc.stderr)
        raise SystemExit(3)
    try:
        env = json.loads(proc.stdout)["env"]
    except (json.JSONDecodeError, KeyError, TypeError):
        sys.stderr.write("conftest: test-env door printed no env object\n")
        raise SystemExit(3)
    return {str(k): str(v) for k, v in env.items()}
