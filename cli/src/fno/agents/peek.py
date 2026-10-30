"""The transcript record readers discovery and the session-truth reader share.

The ``fno agents peek`` command itself is native
(crates/fno-agents/src/peek.rs); this module keeps only the per-harness
record readers its Python importers still use.
"""
from __future__ import annotations

import dataclasses
import json
from pathlib import Path
from typing import Callable, Optional

@dataclasses.dataclass
class Record:
    """One rendered transcript turn: a role and its human-readable text."""

    role: str
    text: str
    timestamp: Optional[str] = None


class ObserveUnsupported(Exception):
    """Handle resolved to an agent with no ``recent_records`` arm (AC2-ERR)."""

    def __init__(self, agent: str) -> None:
        super().__init__(agent)
        self.agent = agent


# --------------------------------------------------------------------------
# Record extraction (harness-agnostic where the block shape allows)
# --------------------------------------------------------------------------


def _extract_text(content: object) -> str:
    """Flatten a message ``content`` to legible text.

    Handles a bare string, claude blocks (``text`` / ``tool_use``), and codex
    blocks (``input_text`` / ``output_text`` — both carry a ``text`` field).
    ``thinking`` and ``tool_result`` bodies are dropped as observe-noise;
    ``tool_use`` renders a compact marker so the peer's actions stay visible.
    """
    if isinstance(content, str):
        return content.strip()
    if not isinstance(content, list):
        return ""
    parts: list[str] = []
    for block in content:
        if not isinstance(block, dict):
            continue
        if isinstance(block.get("text"), str):
            parts.append(block["text"])
        elif block.get("type") == "tool_use":
            parts.append(f"[tool_use: {block.get('name', '?')}]")
    return " ".join(p.strip() for p in parts if p.strip())


def _parse_claude_record(rec: dict) -> Optional[Record]:
    """A claude transcript ``user``/``assistant`` line → Record, else None."""
    if rec.get("type") not in ("user", "assistant"):
        return None
    msg = rec.get("message")
    if not isinstance(msg, dict):
        return None
    text = _extract_text(msg.get("content"))
    if not text:
        return None
    raw_role = str(msg.get("role") or rec.get("type"))
    timestamp = rec.get("timestamp")
    return Record(
        role=raw_role,
        text=text,
        timestamp=timestamp if isinstance(timestamp, str) else None,
    )


def _parse_codex_record(rec: dict) -> Optional[Record]:
    """A codex rollout ``response_item`` message line → Record, else None."""
    if rec.get("type") != "response_item":
        return None
    payload = rec.get("payload")
    if not isinstance(payload, dict) or payload.get("type") != "message":
        return None
    text = _extract_text(payload.get("content"))
    if not text:
        return None
    raw_role = str(payload.get("role") or "?")
    timestamp = rec.get("timestamp")
    return Record(
        role=raw_role,
        text=text,
        timestamp=timestamp if isinstance(timestamp, str) else None,
    )

_TAIL_BYTES = 4 << 20  # 4 MiB holds the last 40 records of every live lead transcript


def _records_from_jsonl(
    path: Path, n: Optional[int], parse: Callable[[dict], Optional[Record]], tail: bool = True
) -> list[Record]:
    """Parse the last ``n`` renderable records from a JSONL transcript.

    Streams line-by-line into a bounded deque so memory stays O(n). A torn or
    non-JSON line (mid-write tail, AC2-EDGE) is skipped, never raised. ``n`` of
    0 or negative returns ``[]``; ``None`` returns every record.
    Seeks to the last ``_TAIL_BYTES``, reading the whole file when the tail is short.
    """
    import collections

    if n is not None and n <= 0:
        return []
    dq: "collections.deque[Record]" = collections.deque(maxlen=n)
    try:
        with path.open("rb") as fh:
            start = max(0, fh.seek(0, 2) - _TAIL_BYTES) if tail and n else 0
            fh.seek(start)  # a partial first line fails the parse below and is skipped
            for line in fh:
                line = line.strip()
                if not line:
                    continue
                try:
                    rec = json.loads(line)
                except (ValueError, UnicodeDecodeError):
                    continue  # torn/partial trailing record: skip
                if not isinstance(rec, dict):
                    continue
                record = parse(rec)
                if record is not None:
                    dq.append(record)
    except OSError:
        return []
    if start and n is not None and len(dq) < n:
        return _records_from_jsonl(path, n, parse, tail=False)
    return list(dq)


def _codex_rollout_path(
    session_id: str, codex_sessions_dir: Optional[Path]
) -> Optional[Path]:
    """Locate the codex rollout whose ``session_meta`` id matches ``session_id``.

    Current Codex filenames end with the session id, so exact-id lookup avoids
    opening every historical rollout. Verify the embedded metadata before
    trusting that filename, then retain the store-wide scan as compatibility
    fallback for older or nonstandard layouts.
    """
    from fno.agents.discover import _codex_meta, default_codex_sessions_dir

    root = codex_sessions_dir or default_codex_sessions_dir()

    exact: list[tuple[float, Path]] = []
    try:
        for path in root.rglob(f"rollout-*{session_id}.jsonl"):
            try:
                exact.append((path.stat().st_mtime, path))
            except OSError:
                continue
    except OSError:
        pass
    for _mtime, path in sorted(exact, key=lambda item: item[0], reverse=True):
        meta = _codex_meta(path)
        if meta is not None and meta[0] == session_id:
            return path

    dated: list[tuple[float, Path]] = []
    try:
        for path in root.rglob("rollout-*.jsonl"):
            try:
                dated.append((path.stat().st_mtime, path))
            except OSError:
                continue  # vanished mid-scan: skip this file, never abort the scan
    except OSError:
        return None
    rollouts = [p for _mt, p in sorted(dated, key=lambda t: t[0], reverse=True)]
    for path in rollouts:
        meta = _codex_meta(path)
        if meta is not None and meta[0] == session_id:
            return path
    return None


def _opencode_storage_root(storage_dir: Optional[Path]) -> Path:
    from fno.agents.discover import default_opencode_storage_dir

    return storage_dir or default_opencode_storage_dir()


def _opencode_message_dir(
    session_id: str, storage_dir: Optional[Path]
) -> Optional[Path]:
    """The session's per-message dir, or None when absent (unknown ``ses_`` id).

    Unlike codex there is nothing to scan: the session id IS the directory name.
    """
    d = _opencode_storage_root(storage_dir) / "message" / session_id
    return d if d.is_dir() else None


def _opencode_part_text(part_dir: Path) -> str:
    """Join one message's renderable parts from the legacy on-disk tree.

    Filename order is creation order here — unlike message ids, part ids within
    one message are same-era and monotonic (measured: 230 multi-part messages,
    zero out of order).
    """
    blocks: list[dict] = []
    try:
        files = sorted(part_dir.glob("*.json"))
    except OSError:
        return ""
    for pf in files:
        try:
            p = json.loads(pf.read_text(encoding="utf-8"))
        except (OSError, ValueError, UnicodeDecodeError):
            continue  # torn/mid-write part: skip, never abort the message
        if isinstance(p, dict):
            blocks.append(p)
    return _opencode_join_parts(blocks)


def _opencode_join_parts(blocks: "list[dict]") -> str:
    """The part-rendering policy, shared by the SQLite and legacy readers.

    Mirrors ``_extract_text`` so peek reads uniformly across harnesses: any
    type not matched here (``reasoning`` is opencode's ``thinking``, plus the
    step/patch bookkeeping) is observe-noise. ``""`` means the caller skips the
    turn. One function so the two readers cannot drift apart on what a turn
    looks like.
    """
    parts: list[str] = []
    for p in blocks:
        if p.get("type") == "text" and isinstance(p.get("text"), str):
            parts.append(p["text"])
        elif p.get("type") == "tool":
            parts.append(f"[tool_use: {p.get('tool', '?')}]")
    return " ".join(x.strip() for x in parts if x.strip())


def _parse_opencode_record(msg: dict, part_root: Path) -> Optional[Record]:
    """One opencode message JSON + its parts dir → Record, else None.

    Only ``id`` is load-bearing (it locates the parts). A missing ``role``
    degrades to ``"?"`` rather than dropping the turn, matching the codex arm —
    otherwise a message written before its ``role`` lands would silently vanish
    from the tail and the user would read the previous turn as the peer's latest.
    """
    mid = msg.get("id")
    if not isinstance(mid, str) or not mid:
        return None
    text = _opencode_part_text(part_root / mid)
    if not text:
        return None
    raw_role = msg.get("role")
    role_str = raw_role if isinstance(raw_role, str) and raw_role else "?"
    return Record(role=role_str, text=text)


def _opencode_records_db(
    session_id: str, storage_dir: Optional[Path], n: Optional[int]
) -> list[Record]:
    """The last ``n`` renderable turns from opencode's SQLite store.

    Both orderings come from explicit ``time_created`` columns, so unlike the
    legacy tree there is no filename or mtime inference anywhere. Walks newest
    first and stops once ``n`` renderable turns are found, so parts are queried
    only for the messages actually returned.
    """
    import sqlite3

    from fno.agents.discover import default_opencode_db_path, opencode_connect

    if n is not None and n <= 0:
        return []
    con = opencode_connect(default_opencode_db_path(storage_dir))
    if con is None:
        return []
    # One connection for every query below, so the messages and the parts come
    # from a single snapshot of a store a live opencode is still writing to.
    # `id` breaks ties on the millisecond timestamps, which do collide in a real
    # store (measured: 15 tied message groups, 1291 tied part groups); without
    # it the render order would be whatever the query planner happened to pick.
    records: list[Record] = []
    try:
        rows = con.execute(
            "SELECT id, data FROM message WHERE session_id = ? "
            "ORDER BY time_created DESC, id DESC",
            (session_id,),
        ).fetchall()
        for mid, raw in rows:
            if not isinstance(mid, str) or not mid:
                continue
            try:
                msg = json.loads(raw) if isinstance(raw, str) else {}
            except ValueError:
                msg = {}
            if not isinstance(msg, dict):
                continue
            blocks: list[dict] = []
            for (praw,) in con.execute(
                "SELECT data FROM part WHERE message_id = ? "
                "ORDER BY time_created, id",
                (mid,),
            ):
                try:
                    block = json.loads(praw) if isinstance(praw, str) else None
                except ValueError:
                    continue
                if isinstance(block, dict):
                    blocks.append(block)
            text = _opencode_join_parts(blocks)
            if not text:
                continue
            raw_role = msg.get("role")
            role_str = raw_role if isinstance(raw_role, str) and raw_role else "?"
            records.append(
                Record(role=role_str, text=text)
            )
            if n is not None and len(records) >= n:
                break
    except sqlite3.Error:
        return []
    finally:
        con.close()
    records.reverse()
    return records


def _opencode_records(
    session_id: str, storage_dir: Optional[Path], n: Optional[int]
) -> list[Record]:
    """The last ``n`` renderable opencode turns, chronologically (tail parity).

    Ordering is by ``time.created``: the message filename does NOT sort
    chronologically (measured across a real 166-message session, zero of them in
    order), so a message missing that field falls back to its file mtime rather
    than to a constant. A constant would collapse every message to one sort key
    and silently degrade the whole render to filename order — a scrambled
    transcript presented as chronological, which is worse than an empty one.

    Parts are joined only for the messages actually returned. The sibling jsonl
    reader bounds its work with a ``deque(maxlen=n)``; the equivalent here is
    walking the sorted list backwards until ``n`` renderable turns are found, so
    a ``--lines 5`` on a long session reads ~5 messages' parts, not every one.
    """
    if n is not None and n <= 0:
        return []
    msg_dir = _opencode_message_dir(session_id, storage_dir)
    if msg_dir is None:
        return []
    part_root = _opencode_storage_root(storage_dir) / "part"
    dated: list[tuple[float, str, dict]] = []
    try:
        files = list(msg_dir.glob("*.json"))
    except OSError:
        return []
    for mf in files:
        try:
            msg = json.loads(mf.read_text(encoding="utf-8"))
        except (OSError, ValueError, UnicodeDecodeError):
            continue  # torn/mid-write message: skip
        if not isinstance(msg, dict):
            continue
        t = msg.get("time")
        created = t.get("created") if isinstance(t, dict) else None
        if not isinstance(created, (int, float)):
            try:
                # opencode's timestamps are milliseconds; st_mtime is seconds.
                # Mixing the units unconverted would sort every fallback-dated
                # message ~1000x too early, i.e. always to the front.
                created = mf.stat().st_mtime * 1000.0
            except OSError:
                continue
        dated.append((float(created), mf.name, msg))
    dated.sort(key=lambda t: (t[0], t[1]))
    records: list[Record] = []
    for _created, _name, msg in reversed(dated):
        record = _parse_opencode_record(msg, part_root)
        if record is not None:
            records.append(record)
            if n is not None and len(records) >= n:
                break
    records.reverse()
    return records


def _resolve_peer_roles(records: list[Record]) -> list[Record]:
    """Flip ``user`` roles whose text IS a delivered mail turn to ``peer``."""
    from fno.rust_binary import mail_shape

    user_records = [r for r in records if r.role == "user"]
    if not user_records:
        return records
    shapes = mail_shape([r.text for r in user_records])
    for rec, shape in zip(user_records, shapes):
        if shape["framing"] != "bare":
            rec.role = "peer"
    return records


def recent_records(
    agent: str,
    session_id: str,
    cwd: str,
    n: Optional[int],
    *,
    projects_root: Optional[Path] = None,
    codex_sessions_dir: Optional[Path] = None,
    opencode_storage_dir: Optional[Path] = None,
    transcript_path: Optional[Path] = None,
) -> list[Record]:
    """The per-harness reader seam: a uniform ``Record`` list or empty; an
    unregistered harness raises ``ObserveUnsupported``."""
    records: list[Record] = []
    if agent == "claude":
        if transcript_path is not None:
            records = _records_from_jsonl(
                transcript_path, n, _parse_claude_record
            )
        else:
            from fno.provenance.resolver import resolve_transcript

            rt = resolve_transcript(
                "claude", session_id, cwd, projects_root=projects_root
            )
            if rt.resolved and rt.transcript_path:
                records = _records_from_jsonl(
                    Path(rt.transcript_path), n, _parse_claude_record
                )
    elif agent == "codex":
        path = transcript_path or _codex_rollout_path(session_id, codex_sessions_dir)
        if path is not None:
            records = _records_from_jsonl(path, n, _parse_codex_record)
    elif agent == "opencode":
        from fno.agents.discover import default_opencode_db_path

        # Branch on the database EXISTING, never on it yielding no records: an
        # idle session, a locked store, or an unknown id all read as empty, and
        # falling back on those would serve a stale legacy transcript as if it
        # were the session's current one.
        if default_opencode_db_path(opencode_storage_dir).exists():
            records = _opencode_records_db(session_id, opencode_storage_dir, n)
        else:
            records = _opencode_records(session_id, opencode_storage_dir, n)
    else:
        raise ObserveUnsupported(agent)
    return _resolve_peer_roles(records)
