"""Inbox store: the durable mail write and its shared vocabulary.

A durable send is one envelope on the jsonl bus log (the system of record).
The chats store records it, and every reader reads the store. No per-message
markdown file is written.

Public API:
    Kind                    - enum: HEADS_UP, QUESTION, FYI
    DurableOwner            - who drains a durable message, and its TTL
    write_new_thread        - append one durable envelope, return its msg-id
    generate_msg_id         - generate a unique 'fmail-XXXXXXXXXXXX' id
    resolve_project         - resolve project name from .fno/config.toml
    ProjectIdentificationError
"""
from __future__ import annotations

import json
import os
import re
import secrets
import time
from datetime import datetime, timezone
from enum import Enum
from pathlib import Path
from typing import Optional


from fno.mail.kinds import AUTHORED_MAIL_KINDS


# ---------------------------------------------------------------------------
# Enums
# ---------------------------------------------------------------------------

class Kind(str, Enum):
    HEADS_UP = "heads-up"
    QUESTION = "question"
    FYI = "fyi"
    # G2 cross-agent bus: async fire-and-forget message (send command)
    SEND = "send"


VALID_KINDS = AUTHORED_MAIL_KINDS


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


def classify_durable_owner(
    *,
    param_forced: bool,
    recipient_live: bool,
    recipient_resumable: bool,
) -> DurableOwner:
    """Classify a durable write's owner from the signals a send choke point has.

    Precedence is deliberate: a ``--kind``/``--to-project`` send chose the durable
    inbox lane on purpose (``param_forced``), so it owns as ``inbox-drain`` even
    when a live peer exists. Otherwise a live recipient drains on its next turn,
    an asleep-but-resumable one is woken by the daemon, and anything else is a
    dead-letter at birth.
    """
    if param_forced:
        return DurableOwner.INBOX_DRAIN
    if recipient_live:
        return DurableOwner.LIVE_DRAIN
    if recipient_resumable:
        return DurableOwner.WAKE_DAEMON
    return DurableOwner.DEAD_LETTER


def owner_ttl_hours(owner: DurableOwner | str) -> float:
    """The unread horizon (hours) the sweep enforces; the receipt's drain
    window quotes this same table. Unknown classes return 0."""
    key = owner.value if isinstance(owner, DurableOwner) else owner
    return _OWNER_TTL_HOURS.get(key, 0.0)


def ttl_at_for(owner: DurableOwner | str, created: datetime) -> datetime:
    """The ``ttl_at`` horizon for an owner class, measured from ``created``."""
    from datetime import timedelta

    return created + timedelta(hours=owner_ttl_hours(owner))

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


def _validate_recipient(project: str) -> None:
    """Refuse a recipient that is not one safe address segment.

    It must match ``^[A-Za-z0-9][A-Za-z0-9._:-]*$`` (``:`` admitted so a job
    address like ``node:<id>`` is a first-class recipient) and contain no
    ``..``.
    """
    if "/" in project or "\\" in project:
        raise ValueError(f"project name must not contain path separators: {project!r}")
    if ".." in project:
        raise ValueError(f"project name must not contain '..': {project!r}")
    if not _PROJECT_NAME_RE.match(project):
        raise ValueError(
            f"project name must match {_PROJECT_NAME_RE.pattern}: {project!r}"
        )


# ---------------------------------------------------------------------------
# Lock helper - mkdir mutex pattern, POSIX-atomic on macOS
# ---------------------------------------------------------------------------

def _acquire_lock(path: Path, timeout: float = 30.0, poll: float = 0.05) -> Path:
    """Acquire a per-path mkdir lock. Returns the lock-dir path; caller releases."""
    lock_dir = path.with_suffix(path.suffix + ".lock.d")
    deadline = time.time() + timeout
    while True:
        try:
            lock_dir.parent.mkdir(parents=True, exist_ok=True)
            os.mkdir(str(lock_dir))
            return lock_dir
        except FileExistsError:
            if time.time() >= deadline:
                raise TimeoutError(f"could not acquire lock at {lock_dir} within {timeout}s")
            time.sleep(poll)


def _release_lock(lock_dir: Path) -> None:
    try:
        os.rmdir(str(lock_dir))
    except OSError as exc:
        log_inbox_error(
            "lock_release_failed",
            path=str(lock_dir),
            error=f"{type(exc).__name__}: {exc}",
        )


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


def _mark_control_pending(recipient: str) -> None:
    """Stamp the tool-boundary pending flag (best-effort)."""
    try:
        from fno import paths

        flag = paths.bus_dir() / "control-pending" / f"{recipient}.flag"
        flag.parent.mkdir(parents=True, exist_ok=True)
        flag.touch()
    except OSError:
        pass


def write_new_thread(
    recipient: str,
    sender: str,
    kind: str,
    body: str,
    *,
    msg_id: Optional[str] = None,
    timestamp: Optional[datetime] = None,
    replies_to: Optional[str] = None,
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
) -> str:
    """Append one durable envelope to the bus log. Returns its msg-id.

    ``owner``/``ttl_at`` stamp the terminal classification (US6): who will
    drain the message and when the dead-letter sweep should reclassify it if
    they have not. They ride the envelope ``meta``, the record the sweep reads.
    A bus failure raises: the message is not durably stored.
    """
    if kind not in VALID_KINDS:
        raise ValueError(f"invalid kind: {kind!r}; expected one of {sorted(VALID_KINDS)}")
    _validate_recipient(recipient)
    if msg_id is None:
        msg_id = generate_msg_id()
    if timestamp is None:
        timestamp = datetime.now(tz=timezone.utc)
    # An explicit ttl_at (a caller reclassifying) wins over the owner's horizon.
    if owner is not None and ttl_at is None:
        ttl_at = ttl_at_for(owner, timestamp)

    _append_to_bus(
        msg_id=msg_id,
        thread_id=msg_id,
        sender=sender,
        recipient=recipient,
        kind=kind,
        body=body,
        timestamp=timestamp,
        in_reply_to=replies_to,
        persist_to_memory=persist_to_memory,
        refs=refs,
        from_harness=from_harness,
        to_harness=to_harness,
        from_session=from_session,
        from_model=from_model,
        to_kind=to_kind,
        owner=owner,
        ttl_at=ttl_at,
        word_count=word_count,
        origin=origin,
        subject=subject,
    )

    from fno.mail.budget import is_control

    if is_control(body):
        _mark_control_pending(recipient)

    return msg_id


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

def log_inbox_error(reason: str, **extra) -> None:
    """Append a JSON line to ``.fno/inbox-errors.jsonl``."""
    from fno.paths import project_log

    errors_path = project_log("inbox-errors.jsonl")
    errors_path.parent.mkdir(parents=True, exist_ok=True)
    entry = {
        "ts": datetime.now(timezone.utc).isoformat(),
        "reason": reason,
        **extra,
    }
    with errors_path.open("a", encoding="utf-8") as f:
        f.write(json.dumps(entry) + "\n")
