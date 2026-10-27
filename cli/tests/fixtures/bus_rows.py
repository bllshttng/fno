"""Bus fixture helpers for the native-door era: rows land through the append
door, reads go through the read door. The deleted Python bus family's last
fixture surface, one home instead of per-file copies."""
from __future__ import annotations

import json
import secrets
from datetime import datetime, timezone
from pathlib import Path
from typing import Any, Optional

from fno import paths, rust_binary


def new_msg_id() -> str:
    """A 'fmail-XXXXXXXXXXXX' id (12 hex)."""
    return "fmail-" + secrets.token_hex(6)


def bus_log_path() -> Path:
    return paths.bus_dir() / "messages.jsonl"


def append_row(fields: dict) -> str:
    """Append one bus row through the append door. An absent id mints one and
    returns it; thread defaults to the id; ts defaults to now; unset optional
    tags stay absent, exactly the envelope shape the door reads. ``from`` is a
    Python keyword, so the row rides one dict instead of kwargs."""
    f = dict(fields)
    mid = f.pop("id", None) or new_msg_id()
    ts = f.pop("ts", None) or datetime.now(tz=timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")
    row: dict[str, Any] = {
        "v": 1,
        "id": mid,
        "ts": ts,
        "thread": f.pop("thread", None) or mid,
    }
    for key in ("from", "to", "kind"):
        row[key] = f.pop(key, "")
    for key in (
        "from_harness", "to_harness", "request_id", "in_reply_to", "delivery",
        "from_session", "from_model", "to_kind", "origin", "subject",
    ):
        if f.get(key) is not None:
            row[key] = f.pop(key)
    if f.get("word_count") is not None:
        row["word_count"] = f.pop("word_count")
    if f.get("meta"):
        row["meta"] = f.pop("meta")
    row["body"] = f.pop("body", "")
    if f:
        raise TypeError(f"unknown envelope fields: {sorted(f)}")
    rust_binary.chats_command(["bus-append", str(bus_log_path())], row)
    return mid


def read_rows() -> list[dict]:
    """Every retained row oldest first, through the read door."""
    return rust_binary.command_call(
        ["chats", "bus-read", "rows", "--bus", str(bus_log_path())], {}
    )["rows"]


def scan_unread(name: str, *, exclude_from: Optional[set[str]] = None) -> list[dict]:
    """The cursor-bounded unread rows for one address, through the read door."""
    args = ["chats", "bus-read", "unread", "--name", name, "--bus", str(bus_log_path())]
    if exclude_from:
        args += ["--exclude", ",".join(sorted(exclude_from))]
    return rust_binary.command_call(args, {})[name]


def read_cursor(name: str) -> Optional[str]:
    """A consumer's last-seen id, or None."""
    return rust_binary.command_call(
        ["chats", "bus-read", "cursor", "--name", name, "--bus", str(bus_log_path())], {}
    ).get("last_seen_id")


def write_cursor(name: str, msg_id: str) -> None:
    """Atomically write ``name``'s cursor file."""
    import os
    import tempfile

    p = paths.bus_dir() / "cursors" / f"{name}.json"
    p.parent.mkdir(parents=True, exist_ok=True)
    payload = json.dumps(
        {"last_seen_id": msg_id, "ts": datetime.now(tz=timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")},
        ensure_ascii=False,
    )
    fd, tmp_str = tempfile.mkstemp(dir=str(p.parent), prefix=f".{p.name}.tmp.", suffix=".part")
    tmp = Path(tmp_str)
    try:
        with os.fdopen(fd, "w", encoding="utf-8") as f:
            f.write(payload)
        os.replace(str(tmp), str(p))
    except Exception:
        tmp.unlink(missing_ok=True)
        raise


def withdrawn_and_landed() -> tuple[set[str], set[str]]:
    """The withdrawn and landed id sets, through the read door."""
    sets = rust_binary.command_call(
        ["chats", "bus-read", "sets", "--bus", str(bus_log_path())], {}
    )
    return set(sets["withdrawn"]), set(sets["landed"])