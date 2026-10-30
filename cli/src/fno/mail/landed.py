"""Sender-side "did this land" helpers: sent-unclaimed predicate (shared with
`fno agents mail status`) and the landed check. Moved out of shrink-only `mail/cli.py`."""
from __future__ import annotations

import re
from datetime import datetime
from pathlib import Path
from random import shuffle
from time import monotonic
from typing import Optional

# The delivery states meaning the bytes already reached the recipient; the
# native read door and its controls own the vocabulary now, and this file
# reads the states off the rows.
HOSTED_DELIVERY = "hosted"
TYPED_DELIVERY = "typed"
CROSS_SESSION_DELIVERY = "cross-session"
# The nag renders inside hooks/inject-mail-notify.sh's 2s timeout; bound reads.
_LANDED_READ_BUDGET_S = 0.5
_REMINDER_TAG = re.compile(r"<\s*(/?)\s*system-reminder\s*>", re.IGNORECASE)


def _bus_rows(dedupe_last: bool = False) -> list[dict]:
    """Every retained bus row oldest first, through the native read door.

    ``--dedupe last`` reads the receipt row's final state for the outbox and
    unclaimed scans; the raw pass serves the landed check, which matches rows
    by id on the raw log.
    """
    from fno import paths, rust_binary

    live = paths.bus_dir() / "messages.jsonl"
    args = ["chats", "bus-read", "rows", "--bus", str(live)]
    if dedupe_last:
        args += ["--dedupe", "last"]
    return rust_binary.command_call(args, {})["rows"]


def _bus_sets(live: Path) -> tuple[set[str], set[str]]:
    """The withdrawn and landed id sets over the raw log, one door call."""
    from fno import rust_binary

    sets = rust_binary.command_call(["chats", "bus-read", "sets", "--bus", str(live)], {})
    return set(sets["withdrawn"]), set(sets["landed"])


def _read_cursor(name: str) -> Optional[str]:
    """A consumer's last-seen id, or None: absent, blank and corrupt all read
    as no cursor, and the door's warning rides stderr."""
    from fno import paths, rust_binary

    try:
        answer = rust_binary.command_call(
            [
                "chats", "bus-read", "cursor", "--name", name,
                "--bus", str(paths.bus_dir() / "messages.jsonl"),
            ],
            {},
        )
    except Exception:  # noqa: BLE001 - a failed read claims nothing
        return None
    return answer.get("last_seen_id")


def _defang_reminder(s: str) -> str:
    return _REMINDER_TAG.sub(r"[\1system-reminder]", s)


def _bounded_names(names: list[str], cap: int = 3) -> str:
    """De-dupe (first-seen), defang, then cap at ``cap`` names + ``+K more``."""
    seen = list(dict.fromkeys(names))
    shown = [_defang_reminder(n) for n in seen[:cap]]
    extra = len(seen) - cap
    return ", ".join(shown) + (f", +{extra} more" if extra > 0 else "")


def _parse_ts(ts: str) -> Optional[datetime]:
    """Parse a bus ISO ``ts``, or ``None`` if unparseable (never raises)."""
    try:
        return datetime.fromisoformat(ts.replace("Z", "+00:00") if ts.endswith("Z") else ts)
    except (ValueError, TypeError, AttributeError):
        return None


def age_minutes(ts: str) -> Optional[int]:
    """Minutes elapsed since bus ISO ``ts``, or ``None`` if unparseable."""
    from datetime import timezone as _tz

    sent_at = _parse_ts(ts)
    return None if sent_at is None else int((datetime.now(tz=_tz.utc) - sent_at).total_seconds() // 60)


def _age_exceeds(ts: str, ttl_seconds: int, now: datetime) -> bool:
    """True iff ``ts`` is strictly older than TTL. Unparseable -> False."""
    sent_at = _parse_ts(ts)
    return sent_at is not None and (now - sent_at).total_seconds() > ttl_seconds


def _is_self_send(m: dict) -> bool:
    """Recipient IS the sender: its own transcript trivially carries the id."""
    to_session = (m.get("meta") or {}).get("to_session")
    return bool(to_session) and to_session == m.get("from_session")


def mail_ids_in_transcript(harness: str, session_id: str) -> Optional[set[str]]:
    """Every delivered-mail id in ``(harness, session_id)``'s own transcript -
    a header line's middle token, or an old ``<fno_mail id="...">`` tag.
    ``None`` (not an empty set) means a read failure, never evidence of
    absence. Moved here from ``reply_resolve`` when the reply leaf ported;
    the landed check proving a message reached a DIFFERENT session reuses it."""
    from fno.mail.envelope import mail_shape

    path = _transcript_path(harness, session_id)
    if path is None:
        return None
    try:
        text = path.read_text(encoding="utf-8", errors="replace")
    except OSError:
        return None
    return set(mail_shape([text.replace('\\"', '"')])[0]["ids"])


def _transcript_path(harness: str, session_id: str) -> Optional[Path]:
    """Locate the invoking session's transcript, mirroring self_stamp's resolver
    (claude: ``<projects>/*/<id>.jsonl``; codex: rollout embedding the id)."""
    if harness == "claude":
        from fno.agents.discover import default_projects_dir

        return next(default_projects_dir().glob(f"*/{session_id}.jsonl"), None)
    if harness == "codex":
        from fno.agents.discover import default_codex_sessions_dir

        return next(default_codex_sessions_dir().rglob(f"*{session_id}*.jsonl"), None)
    return None


def _landed_map(msgs: list[dict], budget_s: float | None = None) -> dict[str, bool | None]:
    """Per-message landed verdict, one transcript read per recipient session.
    ``None`` when coordinates are missing, self-send, or unreadable."""
    out: dict[str, Optional[bool]] = dict.fromkeys((str(m.get("id")) for m in msgs))
    by_store: dict[tuple[str, str], list[dict]] = {}
    for m in msgs:
        if _is_self_send(m):
            continue
        to_session = (m.get("meta") or {}).get("to_session")
        to_harness = (m.get("meta") or {}).get("to_harness")
        if not to_session or not to_harness or f'id="{m.get("id", "")}"' not in (m.get("body") or ""):
            continue
        by_store.setdefault((to_harness, to_session), []).append(m)
    stores = list(by_store.items())
    shuffle(stores)
    stop = None if budget_s is None else monotonic() + budget_s
    for (harness, session_id), rows in stores:
        ids = mail_ids_in_transcript(harness, session_id)
        for m in rows:
            mid = str(m.get("id"))
            out[mid] = None if ids is None else mid in ids
        if stop is not None and monotonic() > stop:
            break
    return out


def landed_states(
    already_landed: set[str], msgs: list[dict], budget_s: Optional[float] = None
) -> dict[str, Optional[bool]]:
    """Landed tri-state for ``msgs``: durable proof first, then a transcript read."""
    to_check = [m for m in msgs if m.get("id") not in already_landed]
    out: dict[str, Optional[bool]] = dict.fromkeys((str(m.get("id")) for m in msgs), True)
    out.update(_landed_map(to_check, budget_s))
    return out


def _sent_unclaimed(handle: str, ttl_seconds: int) -> list:
    """Sent mail outstanding past TTL: not landed, past its cursor (durable
    only), older than ``ttl_seconds``. Past ``landed_abandon_ttl`` drops out."""
    from datetime import datetime as _dt, timezone as _tz

    from fno import paths
    from fno.config import load_settings

    now = _dt.now(tz=_tz.utc)
    all_msgs = _bus_rows(dedupe_last=True)
    live = paths.bus_dir() / "messages.jsonl"
    retracted, already_landed = _bus_sets(live)
    abandon_ttl = load_settings().inbox.landed_abandon_ttl
    sent = [
        m for m in all_msgs
        if m.get("kind") == "send" and m.get("from") == handle and m.get("id") not in retracted
        and m.get("id") not in already_landed
        and m.get("delivery") not in (TYPED_DELIVERY, CROSS_SESSION_DELIVERY)
        and not _age_exceeds(m.get("ts", ""), abandon_ttl, now)
    ]
    if not sent:
        return []
    pos = {m.get("id"): i for i, m in enumerate(all_msgs)}
    cursor_pos: dict[str, int] = {}
    for r in {str(m.get("to")) for m in sent if m.get("delivery") != HOSTED_DELIVERY}:
        cid = _read_cursor(r)
        cursor_pos[r] = pos.get(cid, -1) if cid else -1
    candidates = []
    for m in sent:
        if m.get("delivery") != HOSTED_DELIVERY and pos[m.get("id")] <= cursor_pos.get(str(m.get("to")), len(all_msgs)):
            continue  # durable: claimed via cursor
        if not _age_exceeds(m.get("ts", ""), ttl_seconds, now):  # still fresh (strict >)
            continue
        candidates.append(m)
    if not candidates:
        return []
    states = landed_states(already_landed, candidates, budget_s=_LANDED_READ_BUDGET_S)
    out = []
    for m in candidates:
        mid = str(m.get("id"))
        if states.get(mid) is True:
            # Persists past a transcript rotation; later scans skip the grep.
            _record_landed(msg_id=m.get("id", ""), sender=m.get("from", ""), recipient=m.get("to", ""))
            continue
        if m.get("delivery") == HOSTED_DELIVERY and states.get(mid) is not False:
            continue  # hosted: unknown is not a proven miss
        out.append(m)
    return out


def _record_landed(*, msg_id: str, sender: str, recipient: str) -> None:
    """Append one control row proving ``msg_id`` reached ``recipient``'s
    transcript, through the append door. The row is control traffic that no
    inbox renders; it only proves delivery."""
    import secrets
    from datetime import datetime, timezone

    from fno import paths, rust_binary

    mid = "fmail-" + secrets.token_hex(6)
    ts = datetime.now(tz=timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")
    rust_binary.chats_command(
        ["bus-append", str(paths.bus_dir() / "messages.jsonl")],
        {
            "v": 1,
            "id": mid,
            "ts": ts,
            "thread": mid,
            "from": sender,
            "to": recipient,
            "kind": "landed",
            "body": "",
            "meta": {"landed": msg_id},
        },
    )


def nag_line(unclaimed: list) -> Optional[str]:
    """Turn-boundary nag line for outstanding mail, or ``None`` when all landed."""
    if not unclaimed:
        return None
    who = _bounded_names([m.get("to", "") for m in unclaimed])
    age_min = age_minutes(unclaimed[0].get("ts", "")) or 0
    n = len(unclaimed)
    noun = "message" if n == 1 else "messages"
    return (
        f"{n} {noun} to {who} handed {age_min}m ago, not in transcript - "
        "they are mid-loop, ESC to steer"
    )
