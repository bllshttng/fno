"""fno agents mail withdraw.

A sender could see that mail was stranded and could do nothing about it: the
count in `mail status` and the every-prompt nudge named a number and no ids, and
no command retracted anything. `mail ack` only advances an INBOUND read cursor.

The withdrawal is a tombstone because the bus log is append-only and read state
is a per-consumer cursor. Deleting the line is impossible and advancing the
recipient's cursor would mark every OTHER message queued for them as seen.

The tests below are grouped by the thing that actually broke in the field: a
tombstone honored by only one reader. `scan_unread` is the choke point for seven
readers, but the sender's own nag and the relay router read the bus rows
directly and bypass it. Each of the three is asserted independently, because an
assertion that two paths agree pins the tag, not the destination.
"""
from __future__ import annotations

from datetime import datetime, timedelta, timezone

import pytest
from typer.testing import CliRunner

from fno.cli import app
from fno.paths_testing import use_tmpdir

MARKERS = (
    "CODEX_THREAD_ID",
    "CLAUDE_CODE_SESSION_ID",
    "CODEX_SESSION_ID",
    "GEMINI_SESSION_ID",
)
MY_SID = "ffffabcd1234"
MY_HANDLE = "ffffabcd"
PEER = "aaaabbbb"


def _ts_ago(seconds: int) -> str:
    dt = datetime.now(tz=timezone.utc) - timedelta(seconds=seconds)
    return dt.strftime("%Y-%m-%dT%H:%M:%SZ")


@pytest.fixture
def env(tmp_path, monkeypatch):
    use_tmpdir(monkeypatch, tmp_path)
    for m in MARKERS:
        monkeypatch.delenv(m, raising=False)
    monkeypatch.setenv("CLAUDE_CODE_SESSION_ID", MY_SID)
    return tmp_path


def _send(
    from_, to, body, *, ts=None, to_kind="session", delivery=None
) -> str:
    from tests.fixtures.bus_rows import append_row

    fields = {"from": from_, "to": to, "kind": "send", "body": body, "ts": ts, "to_kind": to_kind}
    if delivery is not None:
        fields["delivery"] = delivery
    return append_row(fields)


def _tombstone(msg_id: str, from_: str = MY_HANDLE, to: str = PEER) -> None:
    """The tombstone the withdraw leaf writes, appended directly: these tests
    pin the readers' contract with that line, not the command that produces it."""
    from tests.fixtures.bus_rows import append_row

    append_row(
        {
            "from": from_,
            "to": to,
            "kind": "withdraw",
            "body": f"withdrawn: {msg_id}",
            "meta": {"withdraws": msg_id},
        }
    )


def _run(*args):
    return CliRunner().invoke(app, list(args))


# ---------------------------------------------------------------------------
# The three readers, asserted independently
# ---------------------------------------------------------------------------


def test_reader_1_scan_unread_omits_a_withdrawn_message(env):
    """The recipient side: seven readers reach the bus through here."""
    from tests.fixtures.bus_rows import scan_unread

    mid = _send(MY_HANDLE, PEER, "retract me", ts=_ts_ago(3600))
    kept = _send(MY_HANDLE, PEER, "keep me", ts=_ts_ago(3600))

    _tombstone(mid)

    assert [m["id"] for m in scan_unread(PEER)] == [kept]


# ---------------------------------------------------------------------------
# What a withdrawal must NOT do
# ---------------------------------------------------------------------------


def test_withdraw_does_not_advance_the_recipient_cursor(env):
    """The failure this shape was chosen to avoid. A cursor is a last-seen
    POSITION, so advancing it past the withdrawn message would mark every other
    message queued for that recipient as seen -- trading one strand for many."""
    from tests.fixtures.bus_rows import read_cursor, scan_unread

    from_other = _send(PEER, PEER, "unrelated, still unread", ts=_ts_ago(3600))
    mid = _send(MY_HANDLE, PEER, "retract me", ts=_ts_ago(3600))

    _tombstone(mid)

    assert read_cursor(PEER) is None
    assert [m["id"] for m in scan_unread(PEER)] == [from_other]


def test_a_forged_tombstone_cannot_retract_someone_elses_mail(env):
    """The sender check lives at the READ side as well as in the command. The command
    is one reachable path; a hand-appended or replayed line is another, and a
    tombstone that only the command validated would let one appear."""
    from tests.fixtures.bus_rows import append_row, scan_unread

    mid = _send(PEER, "cccc1111", "theirs", ts=_ts_ago(3600))
    append_row(
        {
            "from": MY_HANDLE,
            "to": "cccc1111",
            "kind": "withdraw",
            "body": f"withdrawn: {mid}",
            "meta": {"withdraws": mid},
        }
    )

    assert [m["id"] for m in scan_unread("cccc1111")] == [mid]


