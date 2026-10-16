"""W2: the transcript ledger reader + W3: write-ahead durable.

W2 (here, ``-k dedup``): the drain's cross-delivery dedup reads THIS session's
transcript for ids that already landed -- the ledger instrument
(``present_mail_ids``), no new state. The dedup-at-drain behavior itself is the
Rust mail front's drain-self leaf, characterized byte-for-byte by its parity
suite; these tests keep the reader honest.

W3 (``-k writeahead``) is appended alongside.
"""
from __future__ import annotations

import json

import pytest
from typer.testing import CliRunner

from fno.paths_testing import use_tmpdir


@pytest.fixture
def runner() -> CliRunner:
    return CliRunner()


# ---------------------------------------------------------------------------
# W2: the transcript ledger reader
# ---------------------------------------------------------------------------


def test_present_mail_ids_reads_envelope_ids_from_transcript(tmp_path, monkeypatch):
    """Positive control for the ledger instrument: a real claude transcript
    carrying <fno_mail id="..."> is read and its ids returned. Proves AC4's
    silence is the transcript match, not a no-op."""
    from fno.agents import discover
    from fno.harness_identity import resolve_harness_identity  # noqa: F401 -- import warm

    sid = "abcd1234"
    projects = tmp_path / "projects"
    proj = projects / "-Users-x-proj"
    proj.mkdir(parents=True, exist_ok=True)
    # A live-injected envelope lands verbatim in the jsonl (JSON-escaped quotes).
    record = {
        "type": "user",
        "message": {
            "content": (
                '<fno_mail from="alice" harness="claude" id="m-real">'
                " landed live </fno_mail>"
            )
        },
    }
    (proj / f"{sid}.jsonl").write_text(json.dumps(record) + "\n", encoding="utf-8")
    monkeypatch.setenv(discover.PROJECTS_DIR_ENV, str(projects))
    monkeypatch.setenv("CLAUDE_CODE_SESSION_ID", sid)
    monkeypatch.delenv("OPENCODE_SESSION_ID", raising=False)

    from fno.mail.reply_resolve import present_mail_ids

    ids = present_mail_ids()
    assert ids is not None, "a present transcript read as unreadable"
    assert "m-real" in ids


# ---------------------------------------------------------------------------
# W3: write the durable record ahead of the live attempt
# ---------------------------------------------------------------------------


def _register_claude_peer(name: str = "red", mux: dict | None = None) -> str:
    """One live claude peer with a full session id; returns its canonical handle
    (the durable recipient a send addresses)."""
    from fno.agents.registry import AgentEntry, write_registry
    from fno.harness_identity import canonical_handle

    sid = "abcd1234-1111-7222-8333-444455556666"
    write_registry([
        AgentEntry(
            name=name, harness="claude", harness_session_id=sid,
            cwd="/tmp", log_path="/tmp/red.log", status="live",
            # A mux row must not also carry a worker-socket key (one-live-ref).
            short_id="abcd1234" if mux is None else "",
            mux=mux,
        )
    ])
    return canonical_handle(sid)


def test_writeahead_writes_durable_for_asleep_recipient(runner, tmp_path, monkeypatch):
    """AC8-HP: a send to a recipient we will not attempt live (asleep, in a pane)
    writes the durable placeholder ahead. The recipient cannot drain during a
    live window (there is none), and a sender crash before it wakes leaves the
    message on the bus, drainable exactly once."""
    from fno.bus.cursor import scan_unread
    from fno.cli import app

    use_tmpdir(monkeypatch, tmp_path)
    # The pane ref keeps this row transcript-vetoed; a paneless claude row is
    # live-first now and would skip the write-ahead.
    recipient = _register_claude_peer("red", mux={"session": "main", "pane_id": 11})
    # Asleep: not live and not unknown -> not attemptable -> the write-ahead path.
    monkeypatch.setattr(
        "fno.agents.dispatch._registered_family1_state", lambda _e: "asleep"
    )

    def _no_live_lane(*_a, **_k):
        raise AssertionError("live lane ran for a transcript-vetoed recipient")

    monkeypatch.setattr("fno.agents.dispatch._deliver_live", _no_live_lane)

    res = runner.invoke(app, ["agents", "mail", "send", "red", "hi", "--from-name", "web"])
    assert res.exit_code == 14, f"exit={res.exit_code} out={res.output!r}"

    unread = scan_unread(recipient)
    assert unread, "the write-ahead placeholder did not land on the bus"
    assert all(m.kind != "withdraw" for m in unread)


def test_live_recipient_hosted_writes_audit_only_outbox_row(
    runner, tmp_path, monkeypatch
):
    """A confirmed live send is auditable without becoming deliverable again."""
    from fno.bus.cursor import scan_unread
    from fno.bus.log import iter_messages
    from fno.cli import app

    use_tmpdir(monkeypatch, tmp_path)
    recipient = _register_claude_peer("red")
    monkeypatch.setattr(
        "fno.agents.dispatch._registered_family1_state", lambda _e: "working"
    )
    injected: list[str] = []

    def _deliver(_entry, body, _from_name, mail, **_kwargs):
        from fno.mail.envelope import wrap_fno_mail

        injected.append(
            wrap_fno_mail(
                body,
                from_=mail.from_,
                node=mail.node,
                to=mail.to,
                id=mail.id,
            )
        )
        return True

    monkeypatch.setattr("fno.agents.dispatch._deliver_live", _deliver)

    res = runner.invoke(app, ["agents", "mail", "send", "red", "hi", "--from-name", "web"])
    assert res.exit_code == 0, f"exit={res.exit_code} out={res.output!r}"

    rows = list(iter_messages())
    # Staged body row first, hosted receipt second; one id, two rows.
    assert len(rows) == 2
    staged, audit = rows
    assert staged.id == audit.id
    assert (staged.from_, staged.to, staged.kind, staged.delivery) == (
        "web", recipient, "send", None
    )
    assert staged.body == injected[0]
    assert audit.id in res.output
    assert (audit.from_, audit.to, audit.kind, audit.delivery) == (
        "web", recipient, "send", "hosted"
    )
    assert audit.body == injected[0]
    assert audit.meta == {
        "to_session": "abcd1234-1111-7222-8333-444455556666",
        "to_harness": "claude",
    }
    # The hosted receipt supersedes the staged twin: nothing left to drain.
    assert scan_unread(recipient) == []

    sent = runner.invoke(app, ["mail", "sent", "--from-name", "web", "--json"])
    assert sent.exit_code == 0, sent.output
    assert json.loads(sent.stdout) == [
        {
            "id": audit.id,
            "to": recipient,
            "to_kind": "session",
            "kind": "send",
            "ts": audit.ts,
            "delivery": "hosted",
            # A hosted row has no cursor, so `claimed` is `null`, never the
            # unconditional `true` this used to assert -- that was the bug
            # this node exists to fix. `landed` is `null` too: this test's
            # recipient has no real transcript file for the grep to read,
            # which is honest "cannot prove it" rather than a false "no".
            "claimed": None,
            # A hosted row IS claimable; only a typed one is not. Asserted as
            # exact equality on purpose, so a new key has to be looked at rather
            # than absorbed.
            "claimable": True,
            "landed": None,
        }
    ]


def test_live_recipient_audit_failure_does_not_queue_retry(
    runner, tmp_path, monkeypatch
):
    """The receipt row fails AFTER a confirmed inject: the staged body row is
    already on the bus, so nothing dangles and a retry would double-deliver."""
    from fno.bus.log import iter_messages
    from fno.cli import app

    use_tmpdir(monkeypatch, tmp_path)
    _register_claude_peer("red")
    monkeypatch.setattr(
        "fno.agents.dispatch._registered_family1_state", lambda _e: "working"
    )
    monkeypatch.setattr("fno.agents.dispatch._deliver_live", lambda *_a, **_k: True)
    monkeypatch.setattr(
        "fno.bus.log.record_hosted_delivery",
        lambda **_k: (_ for _ in ()).throw(OSError("audit disk full")),
    )

    res = runner.invoke(app, ["mail", "send", "red", "hi", "--from-name", "web"])

    assert res.exit_code == 0, res.output
    assert "delivered (hosted)" in res.stdout
    assert "receipt row failed" in (res.stderr or "")
    assert "do not retry" in (res.stderr or "")
    rows = list(iter_messages())
    assert [m.delivery for m in rows] == [None]
    assert "hi" in rows[0].body


def test_live_recipient_live_miss_writes_durable(runner, tmp_path, monkeypatch):
    """An attemptable recipient whose live attempt misses falls back to the
    durable write (live-first), drainable exactly once."""
    from fno.bus.cursor import scan_unread
    from fno.cli import app

    use_tmpdir(monkeypatch, tmp_path)
    recipient = _register_claude_peer("red")
    monkeypatch.setattr(
        "fno.agents.dispatch._registered_family1_state", lambda _e: "working"
    )
    monkeypatch.setattr("fno.agents.dispatch._deliver_live", lambda *_a, **_k: False)

    res = runner.invoke(app, ["agents", "mail", "send", "red", "hi", "--from-name", "web"])
    assert res.exit_code == 14, f"exit={res.exit_code} out={res.output!r}"

    unread = scan_unread(recipient)
    assert unread, "the live-first durable fallback did not write"
    assert all(m.kind != "withdraw" for m in unread)


def test_staged_row_rescues_a_missed_live_inject(runner, tmp_path, monkeypatch):
    """A missed inject leaves the staged body row and the durable fallback under
    one id; the drain surface shows the message exactly once, body intact."""
    from fno.bus.cursor import scan_unread
    from fno.bus.log import iter_messages
    from fno.cli import app

    use_tmpdir(monkeypatch, tmp_path)
    recipient = _register_claude_peer("red")
    monkeypatch.setattr(
        "fno.agents.dispatch._registered_family1_state", lambda _e: "working"
    )
    monkeypatch.setattr("fno.agents.dispatch._deliver_live", lambda *_a, **_k: False)

    res = runner.invoke(app, ["agents", "mail", "send", "red", "hi", "--from-name", "web"])
    assert res.exit_code == 14, f"exit={res.exit_code} out={res.output!r}"

    rows = [m for m in iter_messages() if m.to == recipient]
    assert len(rows) == 2, "expected the staged row and its durable fallback"
    assert len({m.id for m in rows}) == 1, "the twins stopped sharing an id"
    assert all("hi" in m.body for m in rows)
    unread = scan_unread(recipient)
    assert len(unread) == 1, "one id surfaced twice to the drain"
    assert "hi" in unread[0].body


def test_stage_failure_skips_the_live_attempt(runner, tmp_path, monkeypatch):
    """The bus door refuses the stage, so the inject never runs: a confirmed
    inject into an id with no row is the dangling-Read-line bug. The durable
    floor carries the message instead."""
    from fno.bus.cursor import scan_unread
    from fno.cli import app

    use_tmpdir(monkeypatch, tmp_path)
    recipient = _register_claude_peer("red")
    monkeypatch.setattr(
        "fno.agents.dispatch._registered_family1_state", lambda _e: "working"
    )

    def _no_stage(**_kw):
        raise OSError("bus lock timeout")

    def _no_live(*_a, **_k):
        raise AssertionError("live lane ran on an unstaged send")

    monkeypatch.setattr("fno.bus.log.stage_before_live", _no_stage)
    monkeypatch.setattr("fno.agents.dispatch._deliver_live", _no_live)

    res = runner.invoke(app, ["agents", "mail", "send", "red", "hi", "--from-name", "web"])
    assert res.exit_code == 14, f"exit={res.exit_code} out={res.output!r}"
    assert "body row not staged" in (res.stderr or "")

    unread = scan_unread(recipient)
    assert unread, "the durable floor did not carry the unstaged message"
    assert all(m.delivery is None for m in unread)
