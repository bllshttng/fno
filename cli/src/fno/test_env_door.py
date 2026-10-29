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


def _python_scrub(env: dict) -> dict:
    """Scrub ambient names without the binary, keeping the receipt standing.

    The classifier stays Python-side on purpose: fno.hermetic reads the same
    vocabulary the native assembly does, so this is a scrub through the one
    inventory, never a second neutralise. The pins are already right in a
    received env; only a marker planted after the fence needs to die here
    (the identity-scrub guard spawns exactly that child).
    """
    from fno.hermetic import ambient_names

    out = dict(env)
    for name in ambient_names(out):
        out.pop(name, None)
    out["FNO_TEST_HERMETIC"] = "1"
    return out


def apply_env(mode: str, env: dict, sandbox=None, fixtures=None) -> dict:
    """Apply the native neutralise/poison to one caller-supplied env."""
    received = env.get("FNO_TEST_HERMETIC") == "1"
    payload: dict = {"mode": mode, "env": dict(env)}
    if sandbox is not None:
        payload["sandbox"] = str(sandbox)
    if fixtures is not None:
        payload["fixtures"] = str(fixtures)
    # The claim-door contract: FNO_AGENTS_BIN outranks PATH. The smoke runner
    # pins it to a preserved copy of itself before scrubbing target/, so the
    # pytest legs reach the door with a fresh binary while the parity marker
    # stays deleted. FNO_AGENTS_FRONT is the CI job's declared build of the
    # same crate (smoke-setup exports it): a gate step that runs beside the
    # checkout has no PATH install, and the front pin is what keeps the door
    # native there.
    pinned = env.get("FNO_AGENTS_BIN") or ""
    front = env.get("FNO_AGENTS_FRONT") or ""
    if pinned and Path(pinned).is_file():
        binary = pinned
    elif front and Path(front).is_file():
        binary = front
    else:
        binary = shutil.which("fno-agents")
    if binary is None and not received:
        sys.stderr.write(
            "conftest: fno-agents is not on PATH or FNO_AGENTS_BIN; the "
            "hermetic env is native. Install the crate (cargo install "
            "--path crates/fno-agents) or run the suite through "
            "'fno doctor test'.\n"
        )
        raise SystemExit(3)
    if binary is not None:
        proc = subprocess.run(
            [binary, "test-run", "env", json.dumps(payload)],
            capture_output=True,
            text=True,
        )
        if proc.returncode == 0:
            try:
                env = json.loads(proc.stdout)["env"]
            except (json.JSONDecodeError, KeyError, TypeError):
                sys.stderr.write("conftest: test-run env door printed no env object\n")
                raise SystemExit(3) from None
            return {str(k): str(v) for k, v in env.items()}
        if not received:
            sys.stderr.write(proc.stderr)
            raise SystemExit(3)
        # A stale door behind an already-fenced env adds nothing the
        # Python-side scrub doesn't; degrade instead of killing the run.
    return _python_scrub(env)
