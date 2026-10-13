"""fno agents mail withdraw.

A sender could see that mail was stranded and could do nothing about it: the
count in `mail status` and the every-prompt nudge named a number and no ids, and
no verb retracted anything. `mail ack` only advances an INBOUND read cursor.

The withdrawal is a tombstone because the bus log is append-only and read state
is a per-consumer cursor. Deleting the line is impossible and advancing the
recipient's cursor would mark every OTHER message queued for them as seen.

The tests below are grouped by the thing that actually broke in the field: a
tombstone honored by only one reader. `scan_unread` is the choke point for seven
readers, but the sender's own nag and the relay router read `iter_messages`
directly and bypass it. Each of the three is asserted independently, because an
assertion that two paths agree pins the tag, not the destination.
"""
from __future__ import annotations

from datetime import datetime, timedelta, timezone

import json

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
    from fno.bus.log import Envelope, append

    env_ = Envelope.new(
        from_=from_, to=to, kind="send", body=body, ts=ts,
        to_kind=to_kind, delivery=delivery,
    )
    append(env_)
    return env_.id


def _tombstone(msg_id: str, from_: str = MY_HANDLE, to: str = PEER) -> None:
    """The tombstone the withdraw leaf writes, appended directly: these tests
    pin the readers' contract with that line, not the verb that produces it."""
    from fno.bus.log import WITHDRAW_KIND, Envelope, append

    append(
        Envelope.new(
            from_=from_,
            to=to,
            kind=WITHDRAW_KIND,
            body=f"withdrawn: {msg_id}",
            meta={"withdraws": msg_id},
        )
    )


def _run(*args):
    return CliRunner().invoke(app, list(args))


# ---------------------------------------------------------------------------
# The three readers, asserted independently
# ---------------------------------------------------------------------------


def test_reader_1_scan_unread_omits_a_withdrawn_message(env):
    """The recipient side: seven readers reach the bus through here."""
    from fno.bus.cursor import scan_unread

    mid = _send(MY_HANDLE, PEER, "retract me", ts=_ts_ago(3600))
    kept = _send(MY_HANDLE, PEER, "keep me", ts=_ts_ago(3600))

    _tombstone(mid)

    assert [m.id for m in scan_unread(PEER)] == [kept]


def test_reader_1_does_not_deliver_the_tombstone_itself(env):
    """Hiding the target but delivering the tombstone would hand the recipient
    a bare "this was withdrawn" line for a message they never saw."""
    from fno.bus.cursor import scan_unread

    mid = _send(MY_HANDLE, PEER, "retract me", ts=_ts_ago(3600))
    _tombstone(mid)

    assert scan_unread(PEER) == []


def test_reader_2_sent_unclaimed_stops_counting_it(env):
    """The nag reads iter_messages directly and bypasses scan_unread. A
    withdrawal that did not reach here would leave the every-prompt line firing
    forever on a message the sender already retracted -- the exact symptom this
    verb exists to end."""
    from fno.config import load_settings
    from fno.mail.landed import _sent_unclaimed

    mid = _send(MY_HANDLE, PEER, "retract me", ts=_ts_ago(3600))
    ttl = load_settings().inbox.unclaimed_ttl
    assert len(_sent_unclaimed(MY_HANDLE, ttl)) == 1

    _tombstone(mid)

    assert _sent_unclaimed(MY_HANDLE, ttl) == []


def test_reader_2_status_line_goes_quiet(env):
    """End to end through the surface a sender actually reads."""
    mid = _send(MY_HANDLE, PEER, "retract me", ts=_ts_ago(3600))
    assert "sent unclaimed: 1" in _run("mail", "status", "--from", "proj").stdout

    _tombstone(mid)

    assert "sent unclaimed: 0" in _run("mail", "status", "--from", "proj").stdout


def test_reader_3_relay_router_skips_it(env):
    """The router reads iter_messages directly too, and routes by each
    message's `to` rather than filtering to one inbox."""
    from fno.relay.daemon import _tail_after_cursor

    mid = _send(MY_HANDLE, PEER, "retract me", ts=_ts_ago(3600))
    kept = _send(MY_HANDLE, PEER, "keep me", ts=_ts_ago(3600))

    _tombstone(mid)

    routed, _ = _tail_after_cursor()
    assert [m.id for m in routed] == [kept]


# ---------------------------------------------------------------------------
# What a withdrawal must NOT do
# ---------------------------------------------------------------------------


def test_withdraw_does_not_advance_the_recipient_cursor(env):
    """The failure this shape was chosen to avoid. A cursor is a last-seen
    POSITION, so advancing it past the withdrawn message would mark every other
    message queued for that recipient as seen -- trading one strand for many."""
    from fno.bus.cursor import read_cursor, scan_unread

    from_other = _send(PEER, PEER, "unrelated, still unread", ts=_ts_ago(3600))
    mid = _send(MY_HANDLE, PEER, "retract me", ts=_ts_ago(3600))

    _tombstone(mid)

    assert read_cursor(PEER) is None
    assert [m.id for m in scan_unread(PEER)] == [from_other]


def test_withdraw_does_not_delete_the_line(env):
    """Append-only is an invariant of the log, not an implementation detail:
    `mail show` is the audit projection of the source of record, and a
    withdrawal that erased history would make it lie."""
    from fno.bus.log import iter_messages

    mid = _send(MY_HANDLE, PEER, "retract me", ts=_ts_ago(3600))

    _tombstone(mid)

    assert any(m.id == mid for m in iter_messages())


def test_reader_4_the_markdown_inbox_lane_honors_it(env):
    """The lane the three bus filters missed. `write_new_thread` renders a
    markdown file that the project drain consumes without ever reading the bus,
    so a withdrawn message could still be triaged, woken on, and persisted after
    the sender was told it would not be delivered."""
    from fno.inbox.store import read_unread_threads, write_new_thread

    th = write_new_thread(
        recipient="proj", sender=MY_HANDLE, kind="fyi", body="retract me"
    )
    from fno.bus.log import Envelope, append

    append(
        Envelope.new(
            id=th.thread_id, from_=MY_HANDLE, to="proj", kind="send", body="retract me"
        )
    )
    assert [h.thread_id for h in read_unread_threads("proj")] == [th.thread_id]

    _tombstone(th.thread_id, to="proj")

    assert read_unread_threads("proj") == []


def test_reader_4_fails_open_when_the_bus_is_unreadable(env):
    """Every other failure posture in the inbox errs toward never losing
    unprocessed mail. A withdrawal is not important enough to invert that, so an
    unreadable bus shows the thread rather than hiding it."""
    from fno.bus.log import bus_log_path
    from fno.inbox.store import read_unread_threads, write_new_thread

    th = write_new_thread(
        recipient="proj", sender=MY_HANDLE, kind="fyi", body="keep me"
    )
    bus_log_path().parent.mkdir(parents=True, exist_ok=True)
    bus_log_path().write_text("{not json at all\n", encoding="utf-8")

    assert [h.thread_id for h in read_unread_threads("proj")] == [th.thread_id]


def test_reader_2_the_labelled_sent_lane_honors_the_tombstone(env):
    """`stamp_from` returns an explicit `--from-name` label verbatim, so mail
    sent under one is owned by that label. A tombstone from that label must
    clear the label-scoped listing too, or the labelled send path keeps nagging
    about mail it can no longer deliver."""
    mid = _send("release-bot", PEER, "labelled", ts=_ts_ago(3600))
    rows = json.loads(_run("mail", "sent", "--from-name", "release-bot", "--json").stdout)
    assert [r["id"] for r in rows] == [mid]

    _tombstone(mid, from_="release-bot")

    rows = json.loads(_run("mail", "sent", "--from-name", "release-bot", "--json").stdout)
    assert rows == []


def test_a_forged_tombstone_cannot_retract_someone_elses_mail(env):
    """The sender check lives at the READ side as well as in the verb. The verb
    is one reachable path; a hand-appended or replayed line is another, and a
    tombstone that only the verb validated would let one appear."""
    from fno.bus.cursor import scan_unread
    from fno.bus.log import WITHDRAW_KIND, Envelope, append

    mid = _send(PEER, "cccc1111", "theirs", ts=_ts_ago(3600))
    append(
        Envelope.new(
            from_=MY_HANDLE,
            to="cccc1111",
            kind=WITHDRAW_KIND,
            body=f"withdrawn: {mid}",
            meta={"withdraws": mid},
        )
    )

    assert [m.id for m in scan_unread("cccc1111")] == [mid]


def test_a_typed_row_is_not_reported_as_claimed(env):
    """`typed` is the one state that must never imply the recipient consumed it.

    A typed row is excluded from the unclaimed scan the way a hosted row is, so
    it fell through to "claimed" and told the sender the recipient had read it.
    Bytes written into a PTY can be discarded by the prompt they land on, which
    is the whole reason this transport's receipt says `typed`, never
    `delivered`.
    """
    _send(MY_HANDLE, PEER, "ship it", ts=_ts_ago(60), delivery="typed")

    result = _run("agents", "mail", "sent")

    assert result.exit_code == 0, result.output
    # The JSON lane is the one machines read, so it carries the same answer.
    row = json.loads(result.output)[0]
    assert row["delivery"] == "typed"
    assert row["claimed"] is False
    # And it must distinguish "not claimed" from "can never be claimed". A typed
    # row is excluded from the unclaimed scan, withdraw refuses it, and the
    # recipient cannot drain it, so `claimed: false` alone made it identical to
    # an UNCLAIMED durable row that can still clear. This renderer and
    # `--unclaimed-only` disagreed about the same row.
    assert row["claimable"] is False


def test_withdrawing_a_typed_message_refuses_instead_of_pretending(env):
    """There is nothing to retract once the bytes are at a prompt.

    Withdraw guarded on `hosted` alone, so a forced pane message wrote its
    tombstone and printed success while retracting nothing. That is worse than
    refusing: the sender walks away believing the message is gone.
    """
    msg_id = _send(MY_HANDLE, PEER, "wrong ruling", ts=_ts_ago(60), delivery="typed")

    result = _run("agents", "mail", "withdraw", msg_id)

    assert result.exit_code == 1
    assert "cannot be recalled" in (result.stderr or result.output)


def test_acking_a_tombstone_names_it_a_tombstone(env):
    """The kind filter routes a tombstone into the ack refusal branch, where
    "already delivered (hosted)" would be false about a row that retracted mail
    and was never delivered."""
    from fno.bus.log import WITHDRAW_KIND, iter_messages

    mid = _send(MY_HANDLE, PEER, "retract me", ts=_ts_ago(3600))
    _tombstone(mid)
    tombstone = next(m for m in iter_messages() if m.kind == WITHDRAW_KIND)

    res = _run("mail", "ack", tombstone.id, "--name", PEER)

    assert res.exit_code == 2
    assert "tombstone" in res.stderr
