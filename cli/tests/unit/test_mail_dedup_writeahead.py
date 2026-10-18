"""W2: the transcript ledger reader + W3: write-ahead durable.

W2 (here, ``-k dedup``): the drain's cross-delivery dedup reads THIS session's
transcript for ids that already landed -- the ledger instrument
(``present_mail_ids``), no new state. The dedup-at-drain behavior itself is the
Rust mail front's drain-self leaf, characterized byte-for-byte by its parity
suite; these tests keep the reader honest.

W3 (``-k writeahead``) is appended alongside.
"""

from __future__ import annotations


import pytest
from typer.testing import CliRunner

from fno.paths_testing import use_tmpdir


@pytest.fixture
def runner() -> CliRunner:
    return CliRunner()


# ---------------------------------------------------------------------------
# W3: write the durable record ahead of the live attempt
# ---------------------------------------------------------------------------


def _register_claude_peer(name: str = "red", mux: dict | None = None) -> str:
    """One live claude peer with a full session id; returns its canonical handle
    (the durable recipient a send addresses)."""
    from fno.agents.registry import AgentEntry, write_registry
    from fno.harness_identity import canonical_handle

    sid = "abcd1234-1111-7222-8333-444455556666"
    write_registry(
        [
            AgentEntry(
                name=name,
                harness="claude",
                harness_session_id=sid,
                cwd="/tmp",
                log_path="/tmp/red.log",
                status="live",
                # A mux row must not also carry a worker-socket key (one-live-ref).
                short_id="abcd1234" if mux is None else "",
                mux=mux,
            )
        ]
    )
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
    monkeypatch.setattr("fno.agents.dispatch._registered_family1_state", lambda _e: "asleep")

    def _no_live_lane(*_a, **_k):
        raise AssertionError("live lane ran for a transcript-vetoed recipient")

    monkeypatch.setattr("fno.agents.dispatch._deliver_live", _no_live_lane)

    res = runner.invoke(app, ["agents", "mail", "send", "red", "hi", "--from-name", "web"])
    assert res.exit_code == 14, f"exit={res.exit_code} out={res.output!r}"

    unread = scan_unread(recipient)
    assert unread, "the write-ahead placeholder did not land on the bus"
    assert all(m.kind != "withdraw" for m in unread)


def test_staged_row_rescues_a_missed_live_inject(runner, tmp_path, monkeypatch):
    """A missed inject leaves the staged body row and the durable fallback under
    one id; the drain surface shows the message exactly once, body intact."""
    from fno.bus.cursor import scan_unread
    from fno.bus.log import iter_messages
    from fno.cli import app

    use_tmpdir(monkeypatch, tmp_path)
    recipient = _register_claude_peer("red")
    monkeypatch.setattr("fno.agents.dispatch._registered_family1_state", lambda _e: "working")
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
    monkeypatch.setattr("fno.agents.dispatch._registered_family1_state", lambda _e: "working")

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
