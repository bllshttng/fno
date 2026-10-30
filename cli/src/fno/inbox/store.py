"""Inbox store: the durable mail write and its shared vocabulary.

A durable send is one envelope on the jsonl bus log (the system of record).
The chats store records it, and every reader reads the store. No per-message
markdown file is written.

Public API:
    Kind                    - enum: HEADS_UP, QUESTION, FYI
    DurableOwner            - who drains a durable message, and its TTL
    generate_msg_id         - generate a unique 'fmail-XXXXXXXXXXXX' id
    resolve_project         - resolve project name from .fno/config.toml
    ProjectIdentificationError
"""
from __future__ import annotations

import re
import secrets
from datetime import datetime, timezone
from enum import Enum
from pathlib import Path
from typing import Optional


# ---------------------------------------------------------------------------
# Enums
# ---------------------------------------------------------------------------

class Kind(str, Enum):
    HEADS_UP = "heads-up"
    QUESTION = "question"
    FYI = "fyi"
    # G2 cross-agent bus: async fire-and-forget message (send command)
    SEND = "send"


VALID_KINDS = frozenset({"send", "heads-up", "question", "fyi"})


class BusLockTimeout(TimeoutError):
    """The append door's sidecar lock stayed contended past its wait.

    The door reports the timeout as a ``CommandUnavailable`` whose text names
    it; converting here keeps the durable floor's failure an OSError, which is
    the contract the send path catches to render the exit-12 refusal instead of
    the agent chat lock's queue-and-exit-14 tier.
    """


class DurableOwner(str, Enum):
    """Who drains a durable thread (the four-terminal invariant, US6).

    ``queued (durable)`` is transitional, never terminal: every durable write is
    stamped with the owner that will drain it, so a thread nobody can reach is a
    ``dead-letter`` from birth rather than silent quicksand.
    """

    LIVE_DRAIN = "live-drain"      # recipient has a turn boundary; drains next turn
    WAKE_DAEMON = "wake-daemon"    # asleep-but-resumable; woken by the wake daemon
    INBOX_DRAIN = "inbox-drain"    # deliberate --kind/--to-project note; project inbox drains it
    DEAD_LETTER = "dead-letter"    # no live session, no resumable session, no drain wiring


# TTL horizon per owner class (hours). Executor's discretion (epic Claude's
# Discretion 1). A dead-letter's ttl_at is its birth: it escalates immediately,
# not after a wait. The others give their drain one horizon to run before the
# sweep reclassifies an unread thread as a stranded dead-letter.
_OWNER_TTL_HOURS: dict[str, float] = {
    DurableOwner.LIVE_DRAIN.value: 1.0,
    DurableOwner.WAKE_DAEMON.value: 6.0,
    DurableOwner.INBOX_DRAIN.value: 24.0,
    DurableOwner.DEAD_LETTER.value: 0.0,
}


# Map deprecated kinds -> what to use instead. Reading these tokens from
# the CLI exits non-zero with a hint pointing at the replacement.
DEPRECATED_KINDS: dict[str, str] = {
    "notification": "fyi",
    "lesson": "fyi --persist memory",
    "answer": "fyi --reply-to <msg-id>  (or any kind with --reply-to)",
    "complete": "fyi --reply-to <msg-id>",
}


# ---------------------------------------------------------------------------
# Exception
# ---------------------------------------------------------------------------

class ProjectIdentificationError(Exception):
    """Raised when project name cannot be resolved."""


# ---------------------------------------------------------------------------
# Path helpers
# ---------------------------------------------------------------------------

_PROJECT_NAME_RE = re.compile(r"^[A-Za-z0-9][A-Za-z0-9._:-]*$")


# ---------------------------------------------------------------------------
# Lock helper - mkdir mutex pattern, POSIX-atomic on macOS
# ---------------------------------------------------------------------------


# ---------------------------------------------------------------------------
# ID generation
# ---------------------------------------------------------------------------

def generate_msg_id() -> str:
    """Generate a 'fmail-XXXXXXXXXXXX' id with 12 hex characters."""
    return "fmail-" + secrets.token_hex(6)


# ---------------------------------------------------------------------------
# Format helpers
# ---------------------------------------------------------------------------

def _format_dt(ts: datetime) -> str:
    """ISO-8601 UTC. ``2026-05-08T13:48:58Z`` shape."""
    if ts.tzinfo is None:
        ts = ts.replace(tzinfo=timezone.utc)
    return ts.astimezone(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")


# ---------------------------------------------------------------------------
# Durable write (jsonl-canon)
# ---------------------------------------------------------------------------
# LD2: the global JSONL bus log is the system of record. ``_append_to_bus`` is
# the durable write the caller depends on: a failure RAISES and the send fails
# (never a silent loss).

def _append_to_bus(
    *,
    msg_id: str,
    thread_id: str,
    sender: str,
    recipient: str,
    kind: str,
    body: str,
    timestamp: datetime,
    in_reply_to: Optional[str] = None,
    persist_to_memory: bool = False,
    refs: Optional[dict[str, str]] = None,
    from_harness: Optional[str] = None,
    to_harness: Optional[str] = None,
    from_session: Optional[str] = None,
    from_model: Optional[str] = None,
    to_kind: Optional[str] = None,
    owner: Optional[str] = None,
    ttl_at: Optional[datetime] = None,
    word_count: Optional[int] = None,
    origin: Optional[str] = None,
    subject: Optional[str] = None,
) -> None:
    """Append a versioned envelope to the canonical bus log (the durable write).

    LD2 : the JSONL bus log is the durable-first system of record.
    A failure here FAILS the caller's send - the message is NOT durably stored,
    so reporting success would be a silent loss. This raises (it does NOT swallow).
    """
    from fno import paths, rust_binary

    meta: dict[str, object] = {}
    if refs:
        meta["refs"] = dict(refs)
    if persist_to_memory:
        meta["persist_to_memory"] = True
    # Terminal classification (US6): stamp who drains this durable thread and
    # when the dead-letter sweep should reclassify it if they have not. The bus
    # envelope is the system of record the sweep reads.
    if owner is not None:
        meta["owner"] = owner
    if ttl_at is not None:
        meta["ttl_at"] = _format_dt(ttl_at)
    # The envelope is the door's contract (v1, canonical key order, optional
    # tags omitted when unset, body last); the append door owns the lock, the
    # rotation and the record seam.
    row: dict[str, object] = {
        "v": 1,
        "id": msg_id,
        "ts": _format_dt(timestamp),
        "thread": thread_id,
        "from": sender,
        "to": recipient,
        "kind": kind,
    }
    if from_harness:
        row["from_harness"] = from_harness
    if to_harness:
        row["to_harness"] = to_harness
    if in_reply_to:
        row["in_reply_to"] = in_reply_to
    if from_session:
        row["from_session"] = from_session
    if from_model:
        row["from_model"] = from_model
    if to_kind:
        row["to_kind"] = to_kind
    if word_count is not None:
        row["word_count"] = word_count
    if origin:
        row["origin"] = origin
    if subject:
        row["subject"] = subject
    if meta:
        row["meta"] = meta
    row["body"] = body
    try:
        rust_binary.chats_command(["bus-append", str(paths.bus_dir() / "messages.jsonl")], row)
    except rust_binary.CommandUnavailable as exc:
        # The door's lock timeout must stay a distinct refusal tier: the send
        # flow treats a bare RuntimeError naming a lock as the agent chat lock
        # and queues with exit 14, while the bus lock refuses with exit 12.
        if "bus lock timeout" in str(exc):
            raise BusLockTimeout(str(exc)) from exc
        raise


# ---------------------------------------------------------------------------
# Project resolution
# ---------------------------------------------------------------------------

def _project_id_from_settings(data: dict) -> Optional[str]:
    """Extract the project id from parsed settings.yaml data.

    ``config.project.id`` is canonical; the top-level ``project`` key is the
    deprecated alias and may be either a bare string (legacy) or a mapping
    ``{id, vision, goals, constraints}`` (current). Mirrors the precedence in
    ``fno.paths`` (``config.project.id or project.id``). Returns None
    when no usable id is present so the caller keeps walking up the tree.
    """
    cfg = data.get("config")
    if isinstance(cfg, dict):
        cp = cfg.get("project")
        if isinstance(cp, dict) and cp.get("id"):
            return str(cp["id"])

    proj = data.get("project")
    if isinstance(proj, dict):
        return str(proj["id"]) if proj.get("id") else None
    if isinstance(proj, str) and proj:
        return proj
    return None


def resolve_project(
    cwd: Optional[Path] = None,
    override: Optional[str] = None,
    flag_hint: str = "--from",
) -> str:
    """Resolve the local project name from ``.fno/config.toml``.

    ``flag_hint`` names the flag the caller actually exposes, so the failure
    message points at a real flag (inbox commands have ``--from``; ``mail send``
    has ``--from-name``/``--from-self``)."""
    if override is not None:
        return override

    from fno.config import read_config_flat

    search = cwd if cwd is not None else Path.cwd()

    while True:
        fno_dir = search / ".fno"
        candidate = fno_dir / "config.toml"
        if not candidate.is_file():
            candidate = fno_dir / "settings.yaml"
        if candidate.is_file():
            # read_config_flat parses config.toml (or a legacy settings.yaml) into
            # the FLAT dict and degrades a malformed file to {}.
            data = read_config_flat(candidate)
            if isinstance(data, dict):
                pid = _project_id_from_settings(data)
                if pid:
                    return pid

        parent = search.parent
        if parent == search:
            break
        search = parent

    raise ProjectIdentificationError(
        f"set 'project' in .fno/config.toml or pass {flag_hint}"
    )


# ---------------------------------------------------------------------------
# Error log helper (for callers that need to record parse / dispatch errors)
# ---------------------------------------------------------------------------

