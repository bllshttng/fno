"""``fno agents lead ledger`` - the term ledger page.

Identity and the team adjudication stay in Python; the page assembly and
the default out path are the native ``term-ledger`` command (the lead-history
split), so the Python-tree ratchet holds. Contract: docs/architecture/lead.md.
"""
from __future__ import annotations

import json
import os
import subprocess
from datetime import datetime, timezone
from pathlib import Path
from typing import Optional


def build_ledger_data(rows=None, *, fold_fn=None) -> dict:
    """gather_team plus the native scope fold; the ledger's whole input."""
    from fno.agents.team import fold_scope_nodes, gather_team

    team = gather_team(rows)
    roles = team.get("roles")
    if roles:
        (fold_fn or fold_scope_nodes)(roles)
    return team


def write_ledger(team: dict, path: Optional[Path] = None) -> Path:
    """Relay the team to the native renderer; returns the path written.

    With no path, the command resolves its own default out through the layout
    table and this reads the path it names from the ``rundown: `` line.
    """
    from fno.paths import graph_json
    from fno.rust_binary import resolve_binary

    binary = resolve_binary()
    if binary is None:
        raise RuntimeError(
            "the fno-agents binary was not found: the term ledger page is "
            "rendered by the native lead-rundown command"
        )
    argv = [
        str(binary),
        "lead-rundown",
        "--team-json",
        "-",
        "--graph",
        str(graph_json()),
        "--generated",
        datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ"),
    ]
    if path is not None:
        argv += ["--out", str(Path(path))]
    # Load-scaled bound: 60s idle, capped at half the arm's 300s beat, so a
    # starved machine's page render is not killed into a permanent error.
    try:
        load = os.getloadavg()[0] / (os.cpu_count() or 1)
    except (AttributeError, OSError):
        load = 0.0
    proc = subprocess.run(
        argv, input=json.dumps(team), capture_output=True, text=True, check=False,
        timeout=min(150.0, 60.0 * max(1.0, load / 2.0)),
    )
    if proc.returncode != 0:
        raise RuntimeError(proc.stderr.strip() or f"lead-rundown exited {proc.returncode}")
    if path is not None:
        return Path(path)
    for line in proc.stdout.splitlines():
        if line.startswith("rundown: "):
            return Path(line[len("rundown: "):].strip())
    raise RuntimeError("lead-rundown printed no rundown: path")
