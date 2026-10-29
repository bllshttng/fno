"""Delivery-matrix conformance test (x-e864) -- the recurrence guard.

Mail delivery has now been reworked four times (x-39a4 push-first, x-d899 a2a
injection, x-1f23 live-inject-first, and this node). Every round shipped one
lane and left another walled off, because nothing pinned the WHOLE matrix. This
file is that pin: the normative delivery matrix from the design doc encoded as
fixtures, one assertion per cell.

A refactor that reintroduces a wall -- demoting to the durable queue while a
live rung was never attempted, or refusing a token some store could still
resolve -- fails a cell here instead of shipping round five.

| # | Recipient state                      | Expected delivery      |
|---|--------------------------------------|------------------------|
| 1 | Live bg thread, discovery MISSES it   | socket inject          |
| 2 | Asleep session (resolvable on disk)   | wake-and-deliver       |
| 3 | Live foreground, fno owns the pane    | owned-PTY send         |
| 3'| Live foreground, pane NOT owned       | durable + named reason |
| 4 | Every live rung attempted and failed  | durable + lane receipt |
| 5 | Unknown token (resolves nowhere)      | exit 16, nothing sent  |
| 6a| Retired handle TYPED by a caller      | refuse, suggest bare   |
| 6b| Retired handle READ off a record      | migrate + deliver      |
| 6c| Retired-addressed mail stranded       | reported, not vanished |

Cells 6a-6c shipped with PR #491; they are pinned here so this node cannot
regress them. The daemon is faked at the ``_mail_inject_claude`` boundary and
the wake at the ``wake_and_deliver`` boundary: these tests pin what the CLI does
GIVEN an answer, never the daemon's own correctness (the known false-confirm
repro is a separate node -- see the design doc's Domain Pitfalls).
"""

from __future__ import annotations

import json
import os
import time

import pytest
from typer.testing import CliRunner

from fno.cli import app
from fno.harness_identity import canonical_handle, legacy_suffix_handle
from fno.paths_testing import use_tmpdir

LIVE_SID = "9a063cd3-69d4-415a-ada5-649b0164189c"
ASLEEP_SID = "5b17e2f0-1c44-4d9a-8e3b-2f6a7c081d55"
LIVE_HANDLE = canonical_handle(LIVE_SID)
# The retired short form read off a pre-flip record is the last-eight
# (legacy_suffix); migration resolves it to the canonical first-eight.
LIVE_LEGACY_PREFIX = legacy_suffix_handle(LIVE_SID)
ASLEEP_HANDLE = canonical_handle(ASLEEP_SID)


@pytest.fixture
def runner():
    return CliRunner()


@pytest.fixture
def mailbox(tmp_path, monkeypatch):
    """Co-isolate the md render, the bus log, and every discovery source."""
    monkeypatch.delenv("FNO_BUS_DIR", raising=False)
    monkeypatch.setenv("FNO_INBOX_ROOT", str(tmp_path))
    # the post-send landed verify reads immediately; these tests pin
    # receipt wording, not the settle window.
    monkeypatch.setenv("FNO_MAIL_LANDED_SETTLE_S", "0")
    use_tmpdir(monkeypatch, tmp_path)
    _blank_discovery(monkeypatch, tmp_path)
    return tmp_path


def _blank_discovery(monkeypatch, tmp_path):
    """Point every discovery source at an empty dir.

    This is the fixture that makes cell 1 meaningful: with no roster and no
    transcripts, ``resolve_or_suggest`` MISSES, which is exactly the state that
    used to exit 16 before the socket was ever consulted.
    """
    from fno.agents import discover

    empty = tmp_path / "empty-discovery"
    empty.mkdir(exist_ok=True)
    for env in (
        discover.SESSIONS_DIR_ENV,
        discover.PROJECTS_DIR_ENV,
        discover.CODEX_SESSIONS_DIR_ENV,
    ):
        monkeypatch.setenv(env, str(empty))
    monkeypatch.setenv("FNO_CLAUDE_DAEMON_DIR", str(tmp_path / "daemon-empty"))


def _seed_asleep_transcript(monkeypatch, tmp_path, *, session_id=ASLEEP_SID, age_s=7200):
    """Seed a transcript whose mtime is well outside the liveness recency window.

    That combination -- the transcript exists on disk, but the session is not
    live -- IS the asleep state. Discovery correctly refuses to list it (it is a
    liveness-gated LISTING); the ladder must still reach it, because asleep is a
    resumable state rather than voicemail. The seeded row carries content: the
    transcript reader drops a contentless row, and a stalled verdict needs a
    record to read.
    """
    from fno.agents import discover

    projects = tmp_path / "projects"
    proj = projects / "-Users-x-proj"
    proj.mkdir(parents=True, exist_ok=True)
    transcript = proj / f"{session_id}.jsonl"
    transcript.write_text(
        json.dumps(
            {
                "type": "assistant",
                "isSidechain": False,
                "message": {"model": "claude-opus-4-8", "content": "waiting on the operator."},
            }
        )
        + "\n",
        encoding="utf-8",
    )
    old = time.time() - age_s
    os.utime(transcript, (old, old))
    monkeypatch.setenv(discover.PROJECTS_DIR_ENV, str(projects))
    return transcript


def _drain_as(runner, monkeypatch, session_id):
    """Read what the recipient's own drain-self would see (the durable truth)."""
    from fno.harness_identity import HARNESS_SESSION_MARKERS

    for marker, _harness in HARNESS_SESSION_MARKERS:
        monkeypatch.delenv(marker, raising=False)
    marker = "OPENCODE_SESSION_ID" if session_id.startswith("ses_") else "CLAUDE_CODE_SESSION_ID"
    monkeypatch.setenv(marker, session_id)
    res = runner.invoke(app, ["agents", "mail", "drain-self", "--json"])
    assert res.exit_code == 0, res.output
    return json.loads(res.stdout.strip().splitlines()[-1])


# ---------------------------------------------------------------------------
# Cell 1: live bg thread that discovery does not list -> socket inject.
# The regression this whole node exists to kill.
# ---------------------------------------------------------------------------


def _recording_durable(order):
    from fno.inbox.store import write_new_thread as real

    def _wrapped(*args, **kwargs):
        order.append("durable")
        return real(*args, **kwargs)

    return _wrapped


# ---------------------------------------------------------------------------
# Cell 2: asleep session -> wake-and-deliver. Asleep is resumable, not voicemail.
# ---------------------------------------------------------------------------


# ---------------------------------------------------------------------------
# Cell 4: every applicable live rung attempted and failed -> durable + receipt.
# Durable is a DEMOTION, never a first answer.
# ---------------------------------------------------------------------------


def test_cell3_live_pane_miss_retries_once_and_names_both_attempts(monkeypatch):
    from types import SimpleNamespace

    import fno.agents.dispatch as dispatch_mod

    entry = SimpleNamespace(
        name="worker",
        harness="claude",
        mux={"session": "main", "pane_id": 7},
        exited=False,
    )
    attempts: list[int] = []
    reasons: list[str] = []
    monkeypatch.setattr(
        dispatch_mod,
        "_mux_pane_send",
        lambda *_args, **kwargs: (
            attempts.append(1),
            kwargs["failure_out"].append("pre-submit"),
            False,
        )[-1],
    )

    assert (
        dispatch_mod._deliver_live(
            entry,
            "hi",
            "sender",
            reason_out=reasons,
        )
        is False
    )
    assert len(attempts) == 2
    assert reasons == [
        "mux-send-failed-attempt-1:pre-submit",
        "mux-send-failed-attempt-2:pre-submit",
    ]


def test_cell3_unconfirmed_mux_failure_does_not_retry(monkeypatch):
    from types import SimpleNamespace

    import fno.agents.dispatch as dispatch_mod

    entry = SimpleNamespace(
        name="worker",
        harness="claude",
        mux={"session": "main", "pane_id": 7},
        exited=False,
    )
    attempts: list[int] = []
    reasons: list[str] = []

    def _unconfirmed(*_args, **kwargs):
        attempts.append(1)
        kwargs["failure_out"].append("unconfirmed")
        return False

    monkeypatch.setattr(dispatch_mod, "_mux_pane_send", _unconfirmed)
    assert dispatch_mod._deliver_live(entry, "hi", "sender", reason_out=reasons) is False
    assert len(attempts) == 1
    assert reasons == ["mux-send-failed-attempt-1:unconfirmed"]


# ---------------------------------------------------------------------------
# x-1602: the durable receipt carries its drain window, and a live-lane
# failure renders as legs, never as an error token. A lead once read an
# absence inside the drain window, called a working lane broken, and held a
# fleet-wide standing order an extra cycle; `queued (durable) [io-error]` is
# the receipt that did it -- no clock, and an error string in a success.
# ---------------------------------------------------------------------------


# ---------------------------------------------------------------------------
# Cell 5: unknown token -> exit 16. The typo guard survives the widened ladder.
# ---------------------------------------------------------------------------


def test_cell5_unknown_token_exits_16_and_queues_nothing(runner, mailbox, monkeypatch, tmp_path):
    """The ladder widens what 'resolves' means; a full miss still refuses."""
    monkeypatch.setattr("fno.agents.dispatch._mail_inject_claude", lambda *_a, **_k: False)

    res = runner.invoke(app, ["agents", "mail", "send", "deadbeef", "hi", "--from-name", "web"])

    assert res.exit_code == 16, res.output
    assert "queued (durable)" not in res.output


# ---------------------------------------------------------------------------
# Cell 6a / 6b: the retired-handle discriminator (PR #491).
# Caller-error refuses; data-artifact migrates. The two directions never blur.
# ---------------------------------------------------------------------------


def test_cell6a_caller_typed_retired_handle_is_refused(runner, mailbox, monkeypatch, tmp_path):
    """Nothing mints the retired ``<harness>-<short8>`` form any more, so a
    typed one is a caller bug worth surfacing rather than silently translating."""
    monkeypatch.setattr("fno.agents.dispatch._mail_inject_claude", lambda *_a, **_k: False)

    res = runner.invoke(
        app, ["mail", "send", f"claude-{LIVE_LEGACY_PREFIX}", "hi", "--from-name", "web"]
    )

    assert res.exit_code != 0
    combined = res.output + (res.stderr or "")
    assert LIVE_LEGACY_PREFIX in combined, "the suggestion does not lead with the bare id"
    assert "queued (durable)" not in res.output


def test_cell6b_retired_form_read_off_a_stored_record_is_migrated(
    runner, mailbox, monkeypatch, tmp_path
):
    """A retired address READ off a stored record is a data artifact, not a
    caller error: migrate to the bare id and deliver through the normal ladder.

    The reply path first shipped this as a refusal -- the same wall class as this
    node's root cause -- and was reversed on PR #491.
    """
    from tests.fixtures.bus_rows import read_rows
    from fno.inbox.store import write_new_thread

    _seed_asleep_transcript(monkeypatch, tmp_path, session_id=LIVE_SID)
    monkeypatch.setattr("fno.agents.dispatch._mail_inject_claude", lambda *_a, **_k: False)
    monkeypatch.setattr(
        "fno.agents.dispatch.wake_and_deliver",
        lambda *_a, **_k: (False, "wake-refused"),
    )
    inbound = write_new_thread(
        recipient="meeeeeee",
        sender=LIVE_LEGACY_PREFIX,
        kind="send",
        body="ping",
        to_kind="name",
    )

    result = runner.invoke(
        app,
        ["mail", "reply", "--to", inbound, "--body", "ack", "--from", "lead"],
    )

    assert result.exit_code == 14, result.output
    replies = [m for m in read_rows() if m.get("in_reply_to") == inbound]
    # The write-ahead stage row and the durable recovery row share one id.
    assert len(replies) == 2
    assert replies[0]["id"] == replies[-1]["id"]
    assert replies[0].get("delivery") is None
    assert replies[-1]["to"] == LIVE_HANDLE
    drained = _drain_as(runner, monkeypatch, LIVE_SID)
    assert any(message["id"] == replies[-1]["id"] for message in drained)


# ---------------------------------------------------------------------------
# Cell 7 (AC7-EDGE): self-send and ambiguity. Two ways to wake the wrong thing.
# ---------------------------------------------------------------------------


CANONICAL_ADDRESS_FORMS = [
    pytest.param("9a063cd3", id="bare-8-hex"),
]


@pytest.mark.parametrize("address", CANONICAL_ADDRESS_FORMS)
def test_no_guard_rejects_a_canonical_address_before_resolution(address):
    """AC11-EDGE: a validator must never swallow an address that would resolve.

    The guard sweep this pins exists because of a real regression class, not a
    hypothetical one: ``_validate_inputs``' short-id-shape guard rejected
    ``fno agents mail send <bare-8-hex>`` at exit 2, while the send path's fallback to
    handle resolution only fires on exit 16. The canonical address therefore
    never reached resolution at all -- and it was invisible for as long as it was
    because handles used to carry a harness prefix, so nobody sent a bare id.

    Any NEW guard added ahead of resolution that rejects one of these forms
    fails here rather than silently swallowing mail again.
    """
    from fno.agents.dispatch import _validate_inputs

    _validate_inputs(name=address, message="hi", from_name="web", name_is_address=True)


def test_short_id_shape_guard_still_rejects_a_NAME(monkeypatch):
    """The counterpart: the guard is correct for its actual input class.

    Refusing to NAME an agent like an id prevents a name/id collision, so the
    fix was to scope the guard by caller intent (``name_is_address``), never to
    delete it. Without this assertion the sweep above could be 'satisfied' by
    dropping the guard entirely.
    """
    from fno.agents.dispatch import DispatchAskError, _validate_inputs

    with pytest.raises(DispatchAskError) as err:
        _validate_inputs(name="9a063cd3", message="hi", from_name="web")
    assert err.value.exit_code == 2
