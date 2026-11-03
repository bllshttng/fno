"""Read-only helpers over claude 2.1.143's session + jobs filesystem layout.

US2 reverse-engineered three external surfaces from the
``claude`` binary that have no documented schema. The provider adapter
imports them through this thin wrapper so a future replacement (MCP
channel server, daemon-backend wake) only needs to swap this module
rather than touching ``harnesses.claude``:

- ``~/.claude/sessions/<pid>.json`` — one entry per supervisor session.
  Function ``IE7`` in claude 2.1.143 owns this schema. The fields we
  use: ``messagingSocketPath`` (Unix socket; ``null`` when suspended),
  ``jobId`` (the 8-hex short-id matching ``backgrounded · <id> · …``),
  ``kind`` (``"bg"`` for background supervisor sessions; we ignore
  ``"interactive"``), ``sessionId``, ``cwd``.

- ``~/.claude/jobs/<short-id>/state.json`` — atomic-rename-written
  snapshot of the recipient session's current state. Fields used:
  ``state`` (``"running"`` | ``"done"`` | ``"completed"`` | ``"failed"``
  | ``"needs-input"`` | …), ``updatedAt`` (ISO string, the polling
  baseline), ``output.result`` (the reply text when terminal),
  ``intent``.

- ``~/.claude/jobs/<short-id>/timeline.jsonl`` — append-only event log.
  Each row carries ``at``, ``state``, ``detail``, ``text``. ``text`` is
  the surface output the recipient emitted; we concatenate ``text``
  from rows whose ``state`` is in the terminal-or-needs-input set when
  ``output.result`` is missing.

All paths derive from :func:`_claude_roots` (HOME's root first, never
replaced, then an ambient ``CLAUDE_CONFIG_DIR``, each account's dir).

This module is read-only: a future schema drift in claude is detected
either by ``locate_session`` returning ``None`` (jobId moved or kind
field disappeared) or by ``read_state_json`` raising ``JSONDecodeError``
after one retry. Either failure surfaces upward as an orphan or as a
SocketError in the provider adapter; we never silently misread.
"""
from __future__ import annotations

import json
import os
from pathlib import Path

from fno.harness_identity import claude_transport_short_id


def _claude_roots() -> list[Path]:
    roots = [Path.home() / ".claude"]
    roots += [Path(v) for v in [os.environ.get("CLAUDE_CONFIG_DIR")] if v]
    try:
        from fno.rust_binary import command_call
        roots += [Path(d) for d in command_call(["spawn-overlay", "accounts", "claude-dirs"], {}, timeout=5)["dirs"]]
    except Exception:  # noqa: BLE001 - no binary or an unreadable config leaves the ambient roots
        pass
    return roots


def session_dirs() -> list[Path]:
    return list(dict.fromkeys((root / "sessions").resolve() for root in _claude_roots()))


def _daemon_dir() -> Path:
    """Return the claude daemon dir, honoring ``FNO_CLAUDE_DAEMON_DIR`` (tests)."""
    override = os.environ.get("FNO_CLAUDE_DAEMON_DIR")
    return Path(override) if override else Path.home() / ".claude" / "daemon"


def roster_sessions() -> list[dict]:
    """Live claude sessions from the daemon roster, shaped for discover.

    Each ``workers[*]`` row yields a discover-compatible dict
    ``{session_id, short_id, pid, cwd, status, agent}``. Lenient like
    :func:`roster_live`: a missing/torn/type-drifted roster yields ``[]`` and
    never raises. Roster PRESENCE surfaces the row -- the ``mail-inject`` connect
    is the authoritative liveness gate, so a stale row costs one failed inject and
    a durable floor, never a wrong delivery. A bg worker leaves no pid-sidecar, so
    this is the only source that surfaces it (the send-resolve bug this fixes).

    The pid is resolved to the process that IS the session (W2): the
    daemon's recorded pid for a bg row names the PTY HOST, so each row joins
    through the rendezvous socket farm before leaving here. A join miss keeps
    the recorded pid -- an operator-run interactive session has no rv socket
    and its recorded pid is already the session."""
    try:
        raw = json.loads((_daemon_dir() / "roster.json").read_text(encoding="utf-8"))
    except (FileNotFoundError, OSError, ValueError, UnicodeDecodeError):
        return []
    workers = raw.get("workers") if isinstance(raw, dict) else None
    if not isinstance(workers, dict):
        return []
    try:
        from fno.agents.session_procs import bg_socket_pid_map, resolve_session_pid

        sock_map = bg_socket_pid_map()
    except Exception:  # noqa: BLE001 - discovery never breaks on the join
        sock_map = {}
    rows: list[dict] = []
    seen: set[str] = set()
    for w in workers.values():
        if not isinstance(w, dict):
            continue
        sid = w.get("sessionId")
        if not isinstance(sid, str) or not sid or sid in seen:
            continue
        seen.add(sid)
        try:
            pid = int(w.get("pid"))  # type: ignore[arg-type]  # None/bad -> caught below
        except (TypeError, ValueError):
            pid = 0
        short_id = claude_transport_short_id(sid)
        rows.append(
            {
                "session_id": sid,
                "short_id": short_id,
                "pid": resolve_session_pid(
                    harness="claude", short_id=short_id, pid=pid, socket_map=sock_map
                ),
                "cwd": str(w.get("cwd") or ""),
                "status": None,
                "agent": "claude",
            }
        )
    return rows
