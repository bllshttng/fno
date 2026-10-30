"""The keeper mail lane (x-0ea6): mail to a keeper-hosted lane-B thread.

The delivery-matrix file pins the whole name lane GIVEN an injector's answer;
this file adds the third transport. A keeper-hosted thread has neither lane-A
socket (claude control.sock, codex app-server), so before x-0ea6 it live-missed
on every rung and demoted to durable with a live thread right there. The
keeper's own unix socket is its transport, resolved by the command from the
registry row the send already resolved the recipient from.

Test doubles sit at the same boundaries as test_mail_delivery_matrix.py: the
injector boundary for the ladder, and subprocess for the command itself. These
tests pin what the CLI does GIVEN the command's answer, never the keeper's own
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


# ---------------------------------------------------------------------------
# The command boundary itself: argv shape and the reason side-channel.
# ---------------------------------------------------------------------------


def _fake_command(monkeypatch, stdout: str):
    calls: list[list[str]] = []
    real_run = subprocess.run

    def _run(argv, **_k):
        # Only the command's own call is captured; the registry read that gates
        # bus-only policy shells out for its paths and runs unimpeded.
        if "mail-inject" not in argv:
            return real_run(argv, **_k)
        calls.append(list(argv))
        return subprocess.CompletedProcess(args=argv, returncode=0, stdout=stdout, stderr="")

    monkeypatch.setattr("fno.rust_binary.resolve_installed_binary", lambda: "/fake/fno-agents")
    monkeypatch.setattr("fno.agents.dispatch.subprocess.run", _run)
    return calls


def test_the_keeper_command_argv_carries_the_hosted_harness_and_no_delay(monkeypatch):
    """The command defaults --enter-delay-ms from the hosted harness's own
    contract row, so the Python side passes no delay: one fact, one source."""
    import fno.agents.dispatch as d

    calls = _fake_command(monkeypatch, '{"delivered": true, "reason": "delivered"}')
    assert d._mail_inject_keeper(KEEPER_SID, "<fno_mail>hi</fno_mail>", harness="pi")
    argv = calls[0]
    assert argv[:2] == ["/fake/fno-agents", "mail-inject"]
    assert argv[argv.index("--session") + 1] == KEEPER_SID
    assert argv[argv.index("--harness") + 1] == "pi"
    assert "--enter-delay-ms" not in argv, "the command resolves the delay itself"


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
