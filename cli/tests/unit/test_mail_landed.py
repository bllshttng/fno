"""The landed check (x-0c0b): proof a hosted row reached its recipient's
transcript, not just that injection reported success.

``fno agents mail sent`` used to report ``claimed: true`` on every hosted send,
because ``is_deliverable`` excluded hosted rows from the unclaimed scan
entirely -- there was no unclaimed set they could ever appear in. One message
reached its recipient only because the operator pressed ESC to interrupt a
busy loop; the row had already printed `claimed: true`.

Verify by marker, never by absence: a message is proven landed only when its
bare id appears in the recipient's own transcript. A read failure or a
self-send resolution must read as ``None``, never as a false ``False``
(AC3-ERR, AC9-ERR) -- an absence has three explanations (the real outcome,
"the instrument never ran", or a pipeline loss), and a caller that turns a
`None` into a `False` picks the wrong one.
"""
from __future__ import annotations

import json

from fno.agents import discover
from fno.mail.landed import _sent_unclaimed, landed_states, nag_line
from fno.paths_testing import use_tmpdir
from tests.fixtures.bus_rows import append_row, new_msg_id, read_rows, withdrawn_and_landed

SENDER_SESSION = "sender0001"
RECIPIENT_SESSION = "recipient01"


def _write_transcript(tmp_path, monkeypatch, session_id: str, body: str = "") -> None:
    """A fake claude transcript at the path `_transcript_path` resolves for
    ``session_id``, mirroring the existing `present_mail_ids` test fixture."""
    projects = tmp_path / "projects"
    proj = projects / "-Users-x-proj"
    proj.mkdir(parents=True, exist_ok=True)
    (proj / f"{session_id}.jsonl").write_text(body, encoding="utf-8")
    monkeypatch.setenv(discover.PROJECTS_DIR_ENV, str(projects))


def _envelope_line(msg_id: str) -> str:
    record = {
        "type": "user",
        "message": {
            "content": f'<fno_mail from="alice" harness="claude" id="{msg_id}"> hi </fno_mail>',
        },
    }
    return json.dumps(record) + "\n"


def _backdate_last(ts: str) -> None:
    """Envelopes are immutable once appended; rewrite the just-written line's
    timestamp in place so age-bound tests can backdate a send."""
    from tests.fixtures.bus_rows import bus_log_path

    path = bus_log_path()
    lines = path.read_text(encoding="utf-8").splitlines()
    obj = json.loads(lines[-1])
    obj["ts"] = ts
    lines[-1] = json.dumps(obj)
    path.write_text("\n".join(lines) + "\n", encoding="utf-8")


def _ts_ago(seconds: int) -> str:
    from datetime import datetime, timedelta, timezone

    dt = datetime.now(tz=timezone.utc) - timedelta(seconds=seconds)
    return dt.strftime("%Y-%m-%dT%H:%M:%SZ")


def _send_hosted(
    *, to_session: str | None = RECIPIENT_SESSION, to_harness: str = "claude",
    from_session: str = SENDER_SESSION, recipient: str = "bob", ts: str | None = None,
    body: str | None = None,
) -> str:
    msg_id = new_msg_id()
    row = {
        "id": msg_id,
        "from": "alice",
        "to": recipient,
        "kind": "send",
        "delivery": "hosted",
        "from_session": from_session,
        "body": body if body is not None else f'<fno_mail from="alice" id="{msg_id}">hi</fno_mail>',
    }
    meta = {}
    if to_session:
        meta["to_session"] = to_session
    if to_harness:
        meta["to_harness"] = to_harness
    if meta:
        row["meta"] = meta
    append_row(row)
    if ts is not None:
        _backdate_last(ts)
    return msg_id


def test_ac1_landed_true_when_id_is_in_recipient_transcript(tmp_path, monkeypatch):
    use_tmpdir(monkeypatch, tmp_path)
    mid = _send_hosted()
    _write_transcript(tmp_path, monkeypatch, RECIPIENT_SESSION, _envelope_line(mid))

    all_msgs = read_rows()
    states = landed_states(withdrawn_and_landed()[1], all_msgs)

    assert states[mid] is True

    # the post-send verify reads the same transcript RAW, so the id
    # inside a rendered tool output (an `unread` poll result) counts as the
    # pickup the record parsers drop. No <fno_mail> tag anywhere.
    from fno.mail.landed import post_send_landed

    send_id = append_row({"from": "alice", "to": "bob", "kind": "send", "body": "raw pickup"})
    tool_output = json.dumps(
        {"type": "tool_result", "content": f"{send_id} queued (durable) for bob"}
    ) + "\n"
    _write_transcript(tmp_path, monkeypatch, RECIPIENT_SESSION, tool_output)
    assert post_send_landed(
        send_id, to="bob", to_harness="claude", to_session=RECIPIENT_SESSION
    ) == (True, "recipient transcript")


def test_ac3_unresolvable_transcript_reads_null_never_false(tmp_path, monkeypatch):
    """A read failure is not evidence of absence: no transcript file at all
    (a session with no store record) must never masquerade as a proven miss."""
    use_tmpdir(monkeypatch, tmp_path)
    mid = _send_hosted()
    projects = tmp_path / "projects"
    projects.mkdir(parents=True, exist_ok=True)
    monkeypatch.setenv(discover.PROJECTS_DIR_ENV, str(projects))

    all_msgs = read_rows()

    assert landed_states(withdrawn_and_landed()[1], all_msgs)[mid] is None


def test_ac5_landed_proof_survives_transcript_rotation(tmp_path, monkeypatch):
    """Once landed, the proof is a durable bus row, not a re-grep: a transcript
    that later rotates away must not un-flip an already-proven message."""
    from datetime import datetime, timedelta, timezone

    use_tmpdir(monkeypatch, tmp_path)
    old_ts = (datetime.now(tz=timezone.utc) - timedelta(minutes=5)).strftime(
        "%Y-%m-%dT%H:%M:%SZ"
    )
    mid = _send_hosted(ts=old_ts)
    _write_transcript(tmp_path, monkeypatch, RECIPIENT_SESSION, _envelope_line(mid))

    outstanding = _sent_unclaimed("alice", ttl_seconds=0)
    assert outstanding == []  # landed at first sighting; recorded, not returned

    assert mid in withdrawn_and_landed()[1]

    # The transcript is gone now (rotated away, session dead); the durable
    # proof must still hold. The landed set rides in FIRST, so the proven id
    # never re-greets the cleared transcript.
    _write_transcript(tmp_path, monkeypatch, RECIPIENT_SESSION, "")
    all_msgs = read_rows()
    assert landed_states(withdrawn_and_landed()[1], all_msgs)[mid] is True


def test_ac2_err_readable_transcript_without_the_id_nags(tmp_path, monkeypatch):
    """The transcript was readable and the id is absent: a proven miss. The
    row stays outstanding and the nag names its recipient."""
    use_tmpdir(monkeypatch, tmp_path)
    mid = _send_hosted(ts=_ts_ago(3600))
    _write_transcript(tmp_path, monkeypatch, RECIPIENT_SESSION, "")

    outstanding = _sent_unclaimed("alice", ttl_seconds=0)

    assert [m["id"] for m in outstanding] == [mid]
    assert "bob" in (nag_line(outstanding) or "")


def test_ac3_edge_durable_row_with_unread_cursor_still_nags(tmp_path, monkeypatch):
    """The durable rule does not move: a past-TTL row whose recipient cursor
    is still behind it is outstanding even with no landed verdict."""
    use_tmpdir(monkeypatch, tmp_path)

    send_id = append_row(
        {"from": "alice", "to": "lead", "kind": "send", "body": "status report", "ts": _ts_ago(3600)}
    )

    outstanding = _sent_unclaimed("alice", ttl_seconds=0)

    assert [m["id"] for m in outstanding] == [send_id]

    # the sender-side verify reads the same cursor, and an advanced
    # cursor IS the pickup proof - the bus claim the transcript grep is blind
    # to. Before the claim the verdict is a proven False; the nag above still
    # applies its own TTL rule, which this arm does not touch.
    from tests.fixtures.bus_rows import write_cursor
    from fno.mail.landed import post_send_landed

    assert post_send_landed(send_id, to="lead") == (False, "")
    write_cursor("lead", send_id)
    assert post_send_landed(send_id, to="lead") == (
        True,
        "bus claim (recipient read past it)",
    )
    # A same-pair landed control row is the stronger proof and answers first.
    append_row({"from": "alice", "to": "lead", "kind": "landed", "body": "", "meta": {"landed": send_id}})
    assert post_send_landed(send_id, to="lead") == (True, "bus landed record")


def test_ac9_self_send_refuses_the_resolution(tmp_path, monkeypatch):
    """A hosted row whose recipient session IS the sender's own must refuse the
    grep: the send's own output already put the id in that transcript, which
    proves nothing about cross-session delivery."""
    use_tmpdir(monkeypatch, tmp_path)
    mid = _send_hosted(to_session=SENDER_SESSION, from_session=SENDER_SESSION)
    _write_transcript(tmp_path, monkeypatch, SENDER_SESSION, _envelope_line(mid))

    all_msgs = read_rows()

    assert landed_states(withdrawn_and_landed()[1], all_msgs)[mid] is None


def _durable_send_and_ack(*, to: str = "lead") -> str:
    """One durable `send` row plus the `landed` row acknowledging it: the exact
    pair the field incident rendered as a blank message (x-22ce)."""
    send_id = append_row({"from": "worker", "to": to, "kind": "send", "body": "status report"})
    append_row({"from": "worker", "to": to, "kind": "landed", "body": "", "meta": {"landed": send_id}})
    return send_id


def test_reader_1_scan_unread_shows_the_message_never_the_ack(
    tmp_path, monkeypatch
):
    """The inbox choke point: seven readers drain through scan_unread. The
    equality names the surviving id, kind and body; a merely-shorter list would
    also pass for a reader that hid everything."""
    use_tmpdir(monkeypatch, tmp_path)
    from tests.fixtures.bus_rows import scan_unread

    send_id = _durable_send_and_ack()

    unread = scan_unread("lead")

    assert [(m["id"], m["kind"], m.get("body", "")) for m in unread] == [
        (send_id, "send", "status report")
    ]
