"""The thread viewport door shared by retask and mail --force.

The retask transaction is native (`fno-agents rename retask`, ported in
crates/fno-agents/src/retask/front.rs); this module keeps only the one
piece two Python callers share: opening a thread's dedicated viewport.
"""

from __future__ import annotations

import json
import os
import subprocess
import time
from typing import Callable, Mapping, Optional

from fno.agents.mux_spawn import resolve_mux_session
from fno.agents.registry import AgentEntry


class RetaskTransportError(RuntimeError):
    """A pane read or send exceeded its bounded transport timeout."""

    def __init__(self, reason: str, detail: Optional[str] = None):
        super().__init__(reason)
        self.detail = detail

def resolve_thread_viewport(
    entry: AgentEntry,
    *,
    runner: Optional[Callable[..., subprocess.CompletedProcess[str]]] = None,
) -> tuple[str, int]:
    """Open a thread's dedicated viewport and return its positive pane id."""
    runner = runner or subprocess.run
    thread_id = entry.fno_id
    session = resolve_mux_session(None).strip()
    if not isinstance(thread_id, str) or not thread_id.strip():
        # Name the row defect. A bare transport code here read as a broken
        # pipe, so an absent field looked like something a retry could fix.
        # Same reason word `detect_retask` already uses for this case.
        raise RetaskTransportError(
            f"worker_has_no_thread_ref: {entry.name} is "
            f"substrate={entry.substrate or 'unknown'} and its registry row "
            "carries no thread reference, so there is no thread to open. "
            "A retry cannot fix it."
        )
    fno_bin = os.environ.get("FNO_BIN") or "fno"

    def invoke(args: list[str], timeout: int) -> subprocess.CompletedProcess[str]:
        try:
            return runner([fno_bin, *args], capture_output=True, text=True, timeout=timeout, check=False)
        except (OSError, subprocess.TimeoutExpired) as exc:
            raise RetaskTransportError("thread_view_open_timeout") from exc

    # The door answers the row NAME, not the session uuid; fno_id guards the
    # join. The reach carries no baked placement: no flag tunes the row's
    # open portal, else the door's own portal 0 opens for it.
    door = invoke(
        ["mux", "thread", entry.name, "--server", session],
        30,
    )
    if door.returncode:
        lines = (door.stderr or door.stdout or "").strip().splitlines()
        raise RetaskTransportError(
            "thread_view_unavailable",
            detail=lines[-1] if lines else f"exit {door.returncode}",
        )
    # The pane opened above stays open on a join miss and its name stamping
    # can lag the open, so the join retries; the miss names the opened pane.
    for _ in range(3):
        try:
            panes = invoke(["mux", "pane", "ls", "--server", session, "--json"], 10)
            rows = json.loads(panes.stdout)
        except RetaskTransportError:
            raise
        except ValueError as exc:
            # A different cause from the absent field above: the pane listing
            # itself came back unparseable. One word for both is what made the
            # row defect read as a transport failure.
            raise RetaskTransportError("thread_pane_listing_unreadable") from exc
        matches = [
            row
            for row in rows
            if isinstance(row, Mapping)
            and row.get("name") == entry.name
            and row.get("fno_id") == thread_id
            and isinstance(row.get("pane_id"), int)
            and row["pane_id"] > 0
        ] if panes.returncode == 0 and isinstance(rows, list) else []
        if len(matches) == 1:
            return session, matches[0]["pane_id"]
        time.sleep(0.5)
    raise RetaskTransportError("thread_view_join_missed")
