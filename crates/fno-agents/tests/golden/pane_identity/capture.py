#!/usr/bin/env python3
"""Capture the pane-identity goldens from the Python leg.

The freeze bars cargo builds, so the capture drives the Python command
directly with the venv interpreter. Run from the repo root with the canonical
checkout's venv:

    <venv>/bin/python crates/fno-agents/tests/golden/pane_identity/capture.py

Each case writes <case>.exit, <case>.out and <case>.err beside this script,
plus <case>.panes and <case>.rows: the pane listing the stub `fno` served and
the registry rows, so the Rust test replays the same inputs. No case carries a
child pid, so the process-tree probe never decides a golden; the crosscheck's
own tests cover the signature direction.
"""

from __future__ import annotations

import json
import os
import subprocess
import sys
import tempfile
from pathlib import Path

REPO = Path(__file__).resolve().parents[5]
PY = Path(sys.executable)
OUT = Path(__file__).resolve().parent

SID = "3f2b8c1a-9d4e-4f6a-b7c8-2e5d1a0b9c8d"
OTHER = "7c1d4e9b-2a6f-4b8c-9d0e-5f1a2b3c4d5e"
SID_B = "5a6b7c8d-1e2f-4a3b-8c4d-9e0f1a2b3c4d"
SID_U = "8d9e0f1a-3b4c-4d5e-8f6a-1b2c3d4e5f6a"
SID_D = "6b7c8d9e-2f3a-4b4c-9d5e-0f1a2b3c4d5e"

STUB = """#!/bin/sh
if [ -n "$PANES_FAIL" ]; then echo "$PANES_FAIL" >&2; exit 1; fi
cat "$PANES_FILE"
"""


def pane(pane_id, fno_id=None, state=None):
    p = {"pane_id": pane_id, "fno_id": fno_id, "child_pid": None}
    if state is not None:
        p["fno_id_state"] = state
    return p


def row(name, mux=None, sid=SID):
    r = {"name": name, "cwd": "/work", "log_path": "", "harness": "claude",
         "status": "idle"}
    if sid is not None:
        r["harness_session_id"] = sid
    if mux is not None:
        r["mux"] = mux
    return r


def run(root: Path, case: str, panes_text: str, rows: list, argv: list, extra: dict):
    home = root / case
    home.mkdir(parents=True)
    (home / "registry.json").write_text(json.dumps({"schema_version": 43, "agents": rows}))
    panes_file = home / "panes.json"
    panes_file.write_text(panes_text)
    stub = root / "fno"
    env = dict(os.environ)
    for name in ("FNO_SERVER", "FNO_SESSION", "PANES_FAIL"):
        env.pop(name, None)
    (root / "state").mkdir(exist_ok=True)
    (root / "state" / ".path-migration-done").write_text("\n")
    env.update({
        "PYTHONPATH": str(REPO / "cli" / "src"),
        "FNO_AGENTS_HOME": str(home),
        "FNO_HOME": str(root / "state"),
        "FNO_STATE_DIR": str(root / "state"),
        "FNO_BIN": str(stub),
        "PANES_FILE": str(panes_file),
        "FNO_AGENTS_RUNTIME": "python",
    })
    env.update(extra)
    proc = subprocess.run(
        [str(PY), "-c", "from fno.cli import main; main()", "agents", "pane-identity", *argv],
        env=env, cwd=str(root), capture_output=True, text=True, timeout=120,
    )
    (OUT / f"{case}.exit").write_text(f"{proc.returncode}\n")
    (OUT / f"{case}.out").write_text(proc.stdout)
    (OUT / f"{case}.err").write_text(proc.stderr)
    (OUT / f"{case}.panes").write_text(panes_text)
    (OUT / f"{case}.rows").write_text(json.dumps(rows, indent=2) + "\n")
    return proc


def main() -> int:
    for stale in OUT.glob("*.exit"):
        for ext in (".exit", ".out", ".err", ".panes", ".rows"):
            stale.with_suffix(ext).unlink(missing_ok=True)
    root = Path(tempfile.mkdtemp(prefix="pane-identity-golden-"))
    stub = root / "fno"
    stub.write_text(STUB)
    stub.chmod(0o755)

    clean_panes = json.dumps([pane(1, "f-1", "resolved")])
    clean_rows = [row("alpha", {"session": "main", "pane_id": 1})]
    mixed_panes = json.dumps([
        pane(1, "f-1", "resolved"),
        pane(2, None, "unresolved:spawned-name"),
        pane(3, "f-3"),
        pane(4, None),
    ])
    mixed_rows = [
        row("alpha", {"session": "main", "pane_id": 1}),
        row("beta", {"session": "main", "pane_id": 2}, sid=SID_B),
        row("gamma", {"session": "main", "pane_id": 3}, sid=None),
        row("delta", {"session": "main", "pane_id": 9}, sid=SID_D),
        row("other", {"session": "side", "pane_id": 1}, sid=OTHER),
        row("unmuxed", sid=SID_U),
    ]
    cases = [
        ("clean-text", clean_panes, clean_rows, ["--server", "main"], {}),
        ("clean-json", clean_panes, clean_rows, ["--server", "main", "--json"], {}),
        ("empty-listing", "[]", [], ["--server", "main"], {}),
        ("mixed-text", mixed_panes, mixed_rows, ["--server", "main"], {}),
        ("mixed-json", mixed_panes, mixed_rows, ["--server", "main", "-J"], {}),
        ("env-server", clean_panes, clean_rows, [], {"FNO_SERVER": "main"}),
        ("alias-session-id", clean_panes, clean_rows, ["--session-id", "main"], {}),
        ("alias-session", clean_panes, clean_rows, ["--session", "main"], {}),
        ("listing-fails", "[]", [], ["--server", "main"], {"PANES_FAIL": "no server main"}),
        ("listing-empty", "", [], ["--server", "main"], {}),
        ("listing-not-list", "{}", [], ["--server", "main"], {}),
    ]
    for case, panes_text, rows, argv, extra in cases:
        proc = run(root, case, panes_text, rows, argv, extra)
        print(f"{case}: exit {proc.returncode}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
