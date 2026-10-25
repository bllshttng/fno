"""The native ``lead-history --verdict`` relay that ``escalate`` still reads.

``fno agents org history`` and ``verdict`` answer at the front door now; this
relay stays only for the Python ``escalate`` command. Contract:
docs/architecture/lead.md.
"""
from __future__ import annotations

import subprocess
from pathlib import Path


class HistoryUnreadable(Exception):
    """No resolvable role, or a corrupt journal line."""


def verdict_read(events_paths: "list[Path]", scope: "str | None", as_json: bool) -> "tuple[int, str, str]":
    """Relay to the native ``lead-history --verdict`` read; ``(code, stdout, stderr)``."""
    from fno.agents.rust_runtime import refuse_without_binary
    from fno.rust_binary import resolve_binary

    argv = [
        str(resolve_binary() or refuse_without_binary("lead verdict")),
        "lead-history",
        "--verdict",
        "--cwd",
        str(Path.cwd()),
        *(["--scope", scope] if scope else []),
        *[x for p in events_paths for x in ("--events-path", str(p))],
        *(["--json"] if as_json else []),
    ]
    proc = subprocess.run(argv, capture_output=True, text=True, check=False)
    return proc.returncode, proc.stdout, proc.stderr
