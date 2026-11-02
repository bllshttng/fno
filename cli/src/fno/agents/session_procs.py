"""Resolve a registry/roster row to the process that IS the session (W2).

For a claude bg row the recorded pid names the PTY HOST (`claude bg-pty-host`),
not the worker: measured 2026-08-22, row 55f9847a carried pid 98779 whose `ps`
comm was `bg-pty-host` hours older than the worker. The session's own process
is found through the claude daemon's rendezvous socket farm:
``/tmp/cc-daemon-<uid>/<daemon-id>/rv/<short>.sock``, one socket per live bg
session, whose lsof holder is the process that BECOMES the session. The socket
stem is the claude jobId, the first 8 hex of the session uuid.

One lsof call prices the whole farm; every consumer (census, ``agents top``,
roster discovery) shares the same map so the cost view and the gate cannot
disagree about which process a row owns.
"""
from __future__ import annotations

import os
import re
import subprocess
from pathlib import Path
from typing import Optional

from fno.harness_identity import claude_transport_short_id


def transport_join_key(short_id: Optional[str], session_id: Optional[str]) -> str:
    """The 8-hex key the rv farm and roster join on; a longer stored short_id derives from the session id."""
    short = str(short_id or "")
    if re.fullmatch(r"[0-9a-f]{8}", short) or not session_id:
        return short
    return claude_transport_short_id(str(session_id))


def rv_socket_root() -> Path:
    """The rendezvous socket farm root (``FNO_CC_DAEMON_RV_ROOT`` overrides it)."""
    override = os.environ.get("FNO_CC_DAEMON_RV_ROOT")
    return Path(override) if override else Path(f"/tmp/cc-daemon-{os.getuid()}")


def _pid_map_from_lsof(text: str) -> dict[str, int]:
    """Parse ``lsof -F pfn`` output into ``{socket stem: pid}``.

    The stem of the ``n`` line's path is the session's 8-hex jobId. A process
    block repeats one ``p`` line and then an ``f``/``n`` pair per descriptor
    (one holder measured with two fds on the same socket), so the first pid
    seen for a stem wins and later pairs for the same stem are dropped.
    """
    out: dict[str, int] = {}
    pid: Optional[int] = None
    for line in text.splitlines():
        if line.startswith("p") and line[1:].isdigit():
            pid = int(line[1:])
        elif line.startswith("n") and pid is not None:
            stem = Path(line[1:]).stem
            if stem and stem not in out:
                out[stem] = pid
    return out


def bg_socket_pid_map(
    root: Optional[Path] = None, *, timeout: float = 15.0
) -> dict[str, int]:
    """8-hex claude jobId -> pid of the process that IS that bg session.

    Best-effort by design: a missing farm, a dead lsof, or a timeout all mean
    ``{}``, which undercounts a bg row's cost, so callers that gate on cost
    must treat an empty map as "unknown", never as "no sessions".
    """
    base = root if root is not None else rv_socket_root()
    try:
        socks = sorted(base.glob("*/rv/*.sock"))
    except OSError:
        return {}
    if not socks:
        return {}
    try:
        proc = subprocess.run(
            ["lsof", "-F", "pfn", "--", *[str(s) for s in socks]],
            capture_output=True,
            text=True,
            timeout=timeout,
        )
    except (OSError, subprocess.SubprocessError):
        return {}
    # lsof exits 1 when no listed file has a holder; its stdout is then empty
    # and parses to {}, which is the correct answer for "no live bg sessions".
    return _pid_map_from_lsof(proc.stdout)


def resolve_session_pid(
    *,
    harness: str,
    short_id: str = "",
    session_id: Optional[str] = None,
    pid: Optional[int] = None,
    socket_map: Optional[dict[str, int]] = None,
) -> Optional[int]:
    """The pid of the process that IS the session, falling back to the recorded pid.

    Only claude bg sessions have the host/session split (codex and pane workers
    are their own process); every other harness returns ``pid`` unchanged.
    """
    if harness == "claude":
        key = transport_join_key(short_id, session_id)
        if key:
            m = bg_socket_pid_map() if socket_map is None else socket_map
            hit = m.get(key)
            if hit:
                return hit
    return pid
