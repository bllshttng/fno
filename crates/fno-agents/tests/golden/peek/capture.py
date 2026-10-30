#!/usr/bin/env python3
"""Capture the peek goldens from the Python leg (run once, before it was
deleted). The freeze bars cargo builds, so the capture drives the Python
command with the venv interpreter:

    uv run --project cli python crates/fno-agents/tests/golden/peek/capture.py

The fixture world (registry, one claude transcript, the shared events file)
lives in world/ beside this script; goldens.json records each case's argv,
exit code, stdout and stderr. The Rust test copies the world to a temp root
and replays every case against the binary.
"""

from __future__ import annotations

import json
import os
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

REPO = Path(__file__).resolve().parents[5]
OUT = Path(__file__).resolve().parent
WORLD = OUT / "world"

CASES = [
    ("tail", ["alpha"]),
    ("tail-json", ["alpha", "--json"]),
    ("tail-two", ["alpha", "-n", "2"]),
    ("grep-hit", ["alpha", "--grep", "ledger"]),
    ("grep-miss", ["alpha", "--grep", "nomatch", "-J"]),
    ("lines-zero", ["alpha", "-n", "0"]),
    ("status-events", ["beta"]),
    ("status-events-json", ["beta", "--json"]),
    ("incomplete-identity", ["gamma"]),
    ("not-found", ["zzzz-nobody"]),
    ("all", ["--all"]),
    ("all-grep-json", ["--all", "--grep", "ledger", "--json"]),
    ("usage-none", []),
    ("usage-all-and-handle", ["--all", "alpha"]),
    ("usage-follow-grep", ["alpha", "-f", "--grep", "x"]),
    ("usage-negative-lines", ["alpha", "-n", "-1"]),
]


def env_for(root: Path) -> dict:
    env = dict(os.environ)
    env.update({
        "PYTHONPATH": str(REPO / "cli" / "src"),
        "HOME": str(root),
        "FNO_AGENTS_HOME": str(root / "agents"),
        "FNO_HOME": str(root / "state"),
        "FNO_STATE_DIR": str(root / "state"),
        "FNO_CLAUDE_PROJECTS_DIR": str(root / ".claude" / "projects"),
        "FNO_CLAUDE_SESSIONS_DIR": str(root / "sessions"),
        "FNO_CODEX_SESSIONS_DIR": str(root / "codex"),
        "FNO_OPENCODE_STORAGE_DIR": str(root / "opencode"),
        "OPENCODE_DB": str(root / "opencode" / "opencode.db"),
        "FNO_AGENTS_RUNTIME": "python",
    })
    return env


def main() -> int:
    out = {}
    for case, argv in CASES:
        root = Path(tempfile.mkdtemp(prefix="pg-"))
        shutil.copytree(WORLD, root, dirs_exist_ok=True)
        (root / "sessions").mkdir(exist_ok=True)
        (root / "state" / ".path-migration-done").write_text("\n")
        proc = subprocess.run(
            [sys.executable, "-c", "from fno.cli import main; main()", "agents", "peek", *argv],
            env=env_for(root), cwd=str(root), capture_output=True, text=True, timeout=120,
        )
        out[case] = {"argv": argv, "exit": proc.returncode, "out": proc.stdout, "err": proc.stderr}
        print(f"{case}: exit {proc.returncode}")
        shutil.rmtree(root, ignore_errors=True)
    (OUT / "goldens.json").write_text(json.dumps(out, indent=1) + "\n")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
