"""US8 (x-f07d): send-time human escalation for `--kind question`.

A `--kind question` send notifies the human at send time (Locked Decision 7:
question NEVER autonomous-responds), debounced per (sender, recipient) so a
chatty peer cannot spam the human's queue. The durable question thread is always
written, so the ambient unread count stays truthful even when the notifier is
debounced.
"""
from __future__ import annotations

import pytest
from typer.testing import CliRunner

import fno.events
from fno.cli import app
from fno.paths_testing import use_tmpdir


@pytest.fixture
def runner():
    return CliRunner()


@pytest.fixture
def mailbox(tmp_path, monkeypatch):
    monkeypatch.delenv("FNO_BUS_DIR", raising=False)
    monkeypatch.setenv("FNO_CLAUDE_DAEMON_DIR", str(tmp_path / "daemon-empty"))
    monkeypatch.setenv("FNO_INBOX_ROOT", str(tmp_path))
    use_tmpdir(monkeypatch, tmp_path)
    return tmp_path


@pytest.fixture
def emitted_events(monkeypatch):
    """Capture the mail_escalation events the escalation helper emits.

    Replaces the project events.jsonl write with an in-memory capture so the
    test asserts emission + schema-validity without depending on the
    cwd-relative events.jsonl path. The builder still runs real (it validates
    via _build), so a captured event is exactly what would have been appended.
    """
    events: list[dict] = []

    def capture(event: dict) -> None:
        events.append(event)

    monkeypatch.setattr("fno.events.append_event", capture)
    return events


def _unread_count(runner, name: str) -> int:
    # A plain parse of the seeded log, the same read the deleted Python
    # helpers made: no binary, no cursor, so the count holds in any shard.
    # No acks happen in these tests, so deliverable minus withdrawn is
    # exactly what the unread reader would report.
    import json

    from fno import paths

    live = paths.bus_dir() / "messages.jsonl"
    msgs: list[dict] = []
    if live.exists():
        for raw in live.read_text(encoding="utf-8").splitlines():
            raw = raw.strip()
            if not raw:
                continue
            try:
                m = json.loads(raw)
            except ValueError:
                continue
            if m.get("to") == name:
                msgs.append(m)
    by_id = {m["id"]: m for m in msgs if isinstance(m.get("id"), str)}
    withdrawn: set[str] = set()
    for m in msgs:
        if m.get("kind") != "withdraw":
            continue
        withdrawn.add(m["id"])
        target = by_id.get((m.get("meta") or {}).get("withdraws"))
        if target is not None and target.get("from") == m.get("from") and target.get("to") == m.get("to"):
            withdrawn.add(target["id"])
    return sum(
        1
        for m in msgs
        if m.get("kind") not in ("withdraw", "landed")
        and m.get("delivery") not in ("hosted", "typed", "cross-session")
        and m["id"] not in withdrawn
    )


def test_question_send_emits_one_valid_overlay_event(runner, mailbox, monkeypatch):
    # The escalation surfaces in the needs-me overlay via a mail_escalation event
    # (US1/US2): exactly one per non-debounced escalation, schema-valid. The
    # send body lives in the rust leaf, so the read goes through the committed
    # store; the python emitter capture never sees the subprocess's write.
    from tests._event_rows import event_rows

    scratch = mailbox / "scratch" / "events.jsonl"
    monkeypatch.setenv("FNO_EVENTS_PATH", str(scratch))
    res = runner.invoke(
        app,
        ["mail", "send", "--to-project", "web", "--kind", "question",
         "--from-name", "etl", "--body", "which schema wins?"],
    )
    assert res.exit_code == 0, res.output
    rows = event_rows(scratch, types=["mail_escalation"])
    assert len(rows) == 1, "exactly one mail_escalation event per escalation"
    ev = rows[0]
    assert ev["type"] == "mail_escalation"
    fno.events.validate(ev)  # raises if the envelope/shape is invalid
    d = ev["data"]
    assert d["reason"] == "question"
    assert d["sender"] == "etl"
    assert d["recipient"] == "web"
    assert "which schema wins?" in d["summary"]
    assert d["msg_id"].startswith("fmail-"), "carries the mail id for correlation"


def test_escalation_failure_never_breaks_the_send(runner, mailbox, monkeypatch):
    def boom(title: str, message: str):
        raise RuntimeError("no display")

    monkeypatch.setattr("fno.rust_binary.notify_operator", boom)
    res = runner.invoke(
        app,
        ["mail", "send", "--to-project", "web", "--kind", "question",
         "--from-name", "etl", "--body", "still delivers"],
    )
    assert res.exit_code == 0, res.output
    assert _unread_count(runner, "web") == 1


# ---------------------------------------------------------------------------
# Attended-recipient live-miss lane (US3): a send to an operator-attended
# session that misses live delivery escalates; a worker or a live-confirmed
# send does not. Drives _name_lane_send directly with a constructed resolved
# session + mocked injectors, so the assertion is the escalation site itself.
# ---------------------------------------------------------------------------


def _skip_mux(monkeypatch):
    """Force the mux-pane rung to skip (resolve_agent raises) so a live-miss
    falls straight to the durable floor."""
    from fno.agents.registry import AgentResolutionError

    def _raise(*_a, **_k):
        raise AgentResolutionError("test: skip mux rung")

    monkeypatch.setattr("fno.agents.registry.resolve_agent", _raise)


# ---------------------------------------------------------------------------
# AC8 (x-c24d Wave 1): a codex send that demotes to durable with no app-server
# daemon prints the fix command, not only the live-miss reason token. The
# codex daemon socket is the live-mail prerequisite; naming `codex app-server
# daemon start` at the demote site is self-teaching runtime text.
# ---------------------------------------------------------------------------


#: The five axes are never inferred from a value: `claude` is a HARNESS, and
#: `register_existing_session`'s parameter is named `provider`. Name the axis
#: once here rather than bind the literal at each call site - the same idiom
#: as `CODEX_HARNESS` in `cli/tests/agents/test_spawn_pane.py`.
CLAUDE_HARNESS = "claude"


def test_store_healing_never_changes_an_origin_a_row_already_has(mailbox):
    """A refresh may FILL an empty origin and may never CHANGE one.

    The earlier rule here was "`adopted` never lands on a refresh, not even on a
    row with no origin", which protected a spawn row by refusing every restamp.
    That protection was aimed at a spawn row carrying `origin=None`, and spawn
    rows now state `spawned` at birth. Carrying the marker is the stronger
    guard: refusing every restamp still let `origin="operator"` land on a
    worker, which took it out of the retire lane for good and into the attended
    mail escalation. Write-once refuses both.

    A row that never made a claim is a different case. Nothing is lost by giving
    it the only claim anyone has made, and the retire lane reads `None` and
    `adopted` identically, so filling one cannot change a lane verdict.
    """
    from fno.agents.registry import register_existing_session, load_registry

    sid = "4a1c82d1-22bb-4cc3-8dd4-6e7f8a90b2c3"
    register_existing_session(
        provider=CLAUDE_HARNESS, session_id=sid, cwd=str(mailbox), origin="spawn"
    )
    register_existing_session(
        provider=CLAUDE_HARNESS, session_id=sid, cwd=str(mailbox), origin="adopted"
    )
    register_existing_session(
        provider=CLAUDE_HARNESS, session_id=sid, cwd=str(mailbox), origin="operator"
    )

    row = next(r for r in load_registry() if r.harness_session_id == sid)
    assert row.origin == "spawn", "a heal must not exempt a spawned worker from the lane"
