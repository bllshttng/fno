"""The keeper mail lane (x-0ea6): mail to a keeper-hosted lane-B thread.

The delivery-matrix file pins the whole name lane GIVEN an injector's answer;
this file adds the third transport. A keeper-hosted thread has neither lane-A
socket (claude control.sock, codex app-server), so before x-0ea6 it live-missed
on every rung and demoted to durable with a live thread right there. The
keeper's own unix socket is its transport, resolved by the verb from the
registry row the send already resolved the recipient from.

Test doubles sit at the same boundaries as test_mail_delivery_matrix.py: the
injector boundary for the ladder, and subprocess for the verb itself. These
tests pin what the CLI does GIVEN the verb's answer, never the keeper's own
correctness (that lives in the Rust suite's mail_inject tests).
"""

from __future__ import annotations

import subprocess

import pytest

from fno.agents.discover import DiscoveredSession
from fno.paths_testing import use_tmpdir

KEEPER_SID = "pi-thread-4f0ea6c1"
KEEPER_HANDLE = "pi-worker"


def _keeper_session() -> DiscoveredSession:
    return DiscoveredSession(
        session_id=KEEPER_SID,
        short_id=KEEPER_HANDLE,
        handle=KEEPER_HANDLE,
        pid=0,
        cwd="/repo",
        project=None,
        status="live",
        agent="pi",
    )


@pytest.fixture
def mailbox(tmp_path, monkeypatch):
    """Co-isolate the bus log and the inbox, same as the matrix file."""
    monkeypatch.delenv("FNO_BUS_DIR", raising=False)
    monkeypatch.setenv("FNO_INBOX_ROOT", str(tmp_path))
    use_tmpdir(monkeypatch, tmp_path)
    return tmp_path


def _send(monkeypatch, session, injector, capsys):
    """Drive the name-lane choke point directly with a resolved recipient.

    The registry row behind the session id is a keeper THREAD row: a socket
    under mux/threads, the predicate both the ladder guard and the verb read.
    """
    from types import SimpleNamespace

    from fno.mail import cli

    recorded: list[tuple] = []

    def _keeper(recipient, text, *, harness, **_k):
        recorded.append((recipient, text, harness))
        return injector(recipient, text, harness=harness, **_k)

    monkeypatch.setattr("fno.agents.dispatch._mail_inject_keeper", _keeper)

    # Lane A must stay untouched (AC3-EDGE): a call to either is a routing bug.
    def _wrong(_r, _t, **_k):
        raise AssertionError("lane-A injector fired for a keeper-lane recipient")

    monkeypatch.setattr("fno.agents.dispatch._mail_inject_claude", _wrong)
    monkeypatch.setattr("fno.agents.dispatch._mail_inject_codex", _wrong)
    monkeypatch.setattr("fno.agents.dispatch._mux_pane_send", _wrong)

    def _resolve_agent(_sid):
        return SimpleNamespace(
            entry=SimpleNamespace(
                status="live",
                messaging_socket_path="/x/.fno/mux/threads/wk-9.sock",
            )
        )

    monkeypatch.setattr("fno.agents.registry.resolve_agent", _resolve_agent)

    import typer

    try:
        cli._name_lane_send("ping", from_name="web", resolved=session)
    except typer.Exit:
        # A durable demotion that cannot prove landing ends NOT LANDED on a
        # non-zero exit; what it printed is still this helper's answer.
        return recorded, capsys.readouterr()
    return recorded, capsys.readouterr()


def test_a_keeper_recipient_routes_to_the_keeper_verb_and_reads_delivered(
    mailbox, monkeypatch, capsys
):
    """AC1: the receipt reads delivered, never queued (durable), never typed."""

    def _deliver(*_a, **_k):
        return True

    recorded, out = _send(monkeypatch, _keeper_session(), _deliver, capsys)

    assert recorded, "the keeper rung was never attempted"
    recipient, text, harness = recorded[0]
    assert recipient == KEEPER_SID
    assert harness == "pi", "the hosted harness names the settle-delay row"
    assert text.splitlines()[0].startswith("`@web · fmail-"), (
        "the keeper lane carries the delivered header"
    )
    assert "delivered (hosted)" in out.out
    assert "queued (durable)" not in out.out
    assert "typed (pane" not in out.out


def test_a_keeper_miss_demotes_durably_naming_the_verb_reason(mailbox, monkeypatch, capsys):
    """AC2: an unreachable keeper demotes honestly and states why. The roster
    pane rung sits this out (asserted by _send's pane double raising): the
    thread row hosts no pane by design, and a stale ref would type into an
    unrelated pane and read as delivered."""

    def _miss(*_a, reason_out, **_k):
        if reason_out is not None:
            reason_out.append("no-keeper-listener")
        return False

    _recorded, out = _send(monkeypatch, _keeper_session(), _miss, capsys)

    assert "delivered" not in out.out
    assert "queued (durable)" in out.out
    assert "[no-keeper-listener]" in out.out, "the receipt must name why it demoted"


def test_a_pane_hosted_keeper_lane_harness_keeps_the_pane_rung(mailbox, monkeypatch, capsys):
    """A keeper-LANE harness in a live mux pane (opencode, agy) is NOT a
    keeper thread: no mux/threads socket on the row. The ladder must leave it
    on its pane rung - intercepting it stranded every pane-hosted opencode
    worker on the durable queue with a live pane right there."""

    def _wrong(_r, _t, **_k):
        raise AssertionError("the keeper rung fired for a pane-hosted recipient")

    monkeypatch.setattr("fno.agents.dispatch._mail_inject_keeper", _wrong)
    monkeypatch.setattr(
        "fno.agents.dispatch._mail_inject_claude",
        lambda *_a, **_k: False,
    )
    pane_entries: list = []

    def _pane_send(entry, _text, **_k):
        pane_entries.append(entry)
        return True

    monkeypatch.setattr("fno.agents.dispatch._mux_pane_send", _pane_send)

    from types import SimpleNamespace

    def _resolve_agent(_sid):
        return SimpleNamespace(entry=SimpleNamespace(status="live"))

    monkeypatch.setattr("fno.agents.registry.resolve_agent", _resolve_agent)
    from fno.mail import cli

    session = DiscoveredSession(
        session_id="ses-opencode-1",
        short_id="oc",
        handle="oc",
        pid=0,
        cwd="/repo",
        project=None,
        status="live",
        agent="opencode",
    )
    cli._name_lane_send("ping", from_name="web", resolved=session)
    out = capsys.readouterr()
    assert pane_entries, "the roster pane rung never ran for a pane-hosted row"
    assert "delivered (hosted)" in out.out
    assert "queued (durable)" not in out.out


# ---------------------------------------------------------------------------
# The verb boundary itself: argv shape and the reason side-channel.
# ---------------------------------------------------------------------------


def _fake_verb(monkeypatch, stdout: str):
    calls: list[list[str]] = []
    real_run = subprocess.run

    def _run(argv, **_k):
        # Only the verb's own call is captured; the registry read that gates
        # bus-only policy shells out for its paths and runs unimpeded.
        if "mail-inject" not in argv:
            return real_run(argv, **_k)
        calls.append(list(argv))
        return subprocess.CompletedProcess(args=argv, returncode=0, stdout=stdout, stderr="")

    monkeypatch.setattr("fno.rust_binary.resolve_installed_binary", lambda: "/fake/fno-agents")
    monkeypatch.setattr("fno.agents.dispatch.subprocess.run", _run)
    return calls


def test_the_keeper_verb_argv_carries_the_hosted_harness_and_no_delay(monkeypatch):
    """The verb defaults --enter-delay-ms from the hosted harness's own
    contract row, so the Python side passes no delay: one fact, one source."""
    import fno.agents.dispatch as d

    calls = _fake_verb(monkeypatch, '{"delivered": true, "reason": "delivered"}')
    assert d._mail_inject_keeper(KEEPER_SID, "<fno_mail>hi</fno_mail>", harness="pi")
    argv = calls[0]
    assert argv[:2] == ["/fake/fno-agents", "mail-inject"]
    assert argv[argv.index("--session") + 1] == KEEPER_SID
    assert argv[argv.index("--harness") + 1] == "pi"
    assert "--enter-delay-ms" not in argv, "the verb resolves the delay itself"


def test_a_bus_only_keeper_recipient_is_refused_before_the_binary(monkeypatch):
    import fno.agents.dispatch as d

    monkeypatch.setattr(d, "_delivery_policy_refusal", lambda *_a: d.BUS_ONLY_POLICY)

    def _run(_argv, **_k):
        raise AssertionError("a bus-only recipient reached the binary")

    monkeypatch.setattr("fno.rust_binary.resolve_installed_binary", lambda: "/fake/fno-agents")
    monkeypatch.setattr("fno.agents.dispatch.subprocess.run", _run)
    reasons: list = []
    assert not d._mail_inject_keeper(KEEPER_SID, "x", harness="pi", reason_out=reasons)
    assert reasons == [d.BUS_ONLY_POLICY]
