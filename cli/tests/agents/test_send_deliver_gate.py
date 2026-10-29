"""Tests for Task 2.3: Python daemon-RPC wiring for _deliver_live + gate.

Covers AC4-ERR (Python side): codex/gemini delivery via daemon RPC,
daemon-down degradation to durable, and the durable-first invariant.
All existing test_send.py tests remain unchanged.
"""
from __future__ import annotations

import json
import os
import socket
import struct
import subprocess
import sys
import threading
import time
from pathlib import Path

import pytest
from typer.testing import CliRunner

from fno.paths_testing import use_tmpdir


@pytest.fixture
def runner() -> CliRunner:
    return CliRunner()


@pytest.fixture(autouse=True)
def _bypass_a2a_confirm(monkeypatch):
    """ab-098967b4: these tests exercise the _switchboard_exchange relay logic
    directly, so bypass the US6 first-use confirm (which would otherwise
    downgrade auto->observed under pytest's no-TTY). The confirm itself is
    covered in test_a2a_confirm.py."""
    monkeypatch.setenv("FNO_A2A_NO_CONFIRM", "1")
    from fno.agents import dispatch as dispatch_mod

    # This suite isolates transport delivery after liveness has admitted the
    # recipient. Family-1 routing decisions have their own send tests.
    monkeypatch.setattr(
        dispatch_mod, "_registered_family1_state", lambda _entry: "working"
    )


# ---------------------------------------------------------------------------
# Helper: register live peers
# ---------------------------------------------------------------------------

def _register_codex_peer(name: str = "codex-agent") -> None:
    from fno.agents.registry import AgentEntry, write_registry

    write_registry([
        AgentEntry(
            name=name,
            harness="codex",
            cwd="/tmp",
            log_path="/tmp/codex-agent.log",
            harness_session_id="deadbeef-0000-0000-0000-000000000001",
            status="live",
        )
    ])


def _sb_identity(name: str) -> dict:
    return {
        "harness": "claude",
        "session_id": f"session-{name}",
        "short_id": f"short-{name}",
        "created_at": f"created-{name}",
    }


def _sb_identities(*names: str) -> dict[str, dict]:
    return {name: _sb_identity(name) for name in names}


# ---------------------------------------------------------------------------
# Minimal fake daemon (4-byte-LE-u32 + JSON framing) using a short /tmp path
# ---------------------------------------------------------------------------

def _read_frame(conn: socket.socket) -> dict:
    header = b""
    while len(header) < 4:
        chunk = conn.recv(4 - len(header))
        if not chunk:
            raise EOFError("connection closed before frame header")
        header += chunk
    (length,) = struct.unpack_from("<I", header)
    data = b""
    while len(data) < length:
        chunk = conn.recv(length - len(data))
        if not chunk:
            raise EOFError("connection closed during frame body")
        data += chunk
    return json.loads(data.decode("utf-8"))


# ---------------------------------------------------------------------------
# AC4-ERR (Python): daemon returns delivered=true -> delivery becomes "hosted"
# Uses monkeypatching to avoid Unix socket path-length issues on macOS.
# ---------------------------------------------------------------------------

def test_deliver_live_codex_daemon_delivered_true(
    tmp_path: Path, monkeypatch
) -> None:
    """AC4-ERR (Python): daemon RPC delivered=true -> dispatch_send returns hosted."""
    use_tmpdir(monkeypatch, tmp_path)
    _register_codex_peer()

    # Monkeypatch _daemon_rpc to simulate a successful daemon response.
    rpc_calls: list[dict] = []

    def _mock_rpc(method: str, params: dict, **kwargs):
        rpc_calls.append({"method": method, "params": params})
        if method == "agent.deliver" and params.get("name") == "codex-agent":
            return {"delivered": True, "transport": "pty"}
        return None

    from fno.agents import dispatch as dispatch_mod
    monkeypatch.setattr(dispatch_mod, "_daemon_rpc", _mock_rpc)

    from fno.agents.dispatch import dispatch_send
    from tests.fixtures.bus_rows import read_rows

    cwd = tmp_path / "work"
    cwd.mkdir()
    result = dispatch_send(from_name="lead",
        name="codex-agent",
        message="hey codex via PTY",
        provider=None,
        cwd=cwd,
    )

    assert result.delivery == "hosted", (
        f"daemon delivered=true must produce delivery='hosted', got {result.delivery!r}"
    )
    assert result.msg_id.startswith("fmail-")

    # Bus demotion (node x-1f23): a hosted delivery is self-recording (transcript),
    # NOT also queued durable.
    queued = [m for m in read_rows() if m["to"] == "00000001" and m.get("delivery") != "hosted"]
    assert queued == [], "hosted delivery must not queue durable"

    # The deliver RPC carried the delivered-mail header turn (codex/gemini share
    # the envelope now), not the raw body.
    assert len(rpc_calls) == 1
    rpc = rpc_calls[0]
    assert rpc["method"] == "agent.deliver"
    assert rpc["params"]["name"] == "codex-agent"
    body = rpc["params"]["body"]
    assert body.splitlines()[0].startswith("`@lead · fmail-"), body[:80]
    assert "hey codex via PTY" in body


# ---------------------------------------------------------------------------
# AC4-ERR (Python): daemon returns delivered=false -> delivery becomes "durable"
# ---------------------------------------------------------------------------

def test_deliver_live_codex_daemon_delivered_false(
    tmp_path: Path, monkeypatch
) -> None:
    """AC4-ERR (Python): daemon RPC delivered=false -> queued durable."""
    use_tmpdir(monkeypatch, tmp_path)
    _register_codex_peer()

    def _mock_rpc(method: str, params: dict, **kwargs):
        return {"delivered": False, "reason": "injection-gate-unverified"}

    from fno.agents import dispatch as dispatch_mod
    monkeypatch.setattr(dispatch_mod, "_daemon_rpc", _mock_rpc)

    from fno.agents.dispatch import dispatch_send
    from tests.fixtures.bus_rows import read_rows

    cwd = tmp_path / "work"
    cwd.mkdir()
    result = dispatch_send(from_name="lead",
        name="codex-agent",
        message="hey queued",
        provider=None,
        cwd=cwd,
    )

    assert result.delivery == "durable"
    assert result.msg_id.startswith("fmail-")

    assert len({m["id"] for m in read_rows() if m["to"] == "deadbeef"}) == 1


# ---------------------------------------------------------------------------
# x-de10: a hosted codex THREAD row routes through the switchboard (turn
# acceptance), never agent.deliver or a pane send. Old shape: the row fell to
# the route_harness != "claude" demote, whose agent.deliver RPC the daemon
# does not implement for threads, so every live mail send silently demoted to
# the durable queue.
# ---------------------------------------------------------------------------

def _register_codex_thread_peer(name: str = "codex-thread-agent") -> None:
    from fno.agents.registry import AgentEntry, write_registry

    write_registry([
        AgentEntry(
            name=name,
            harness="codex",
            cwd="/tmp",
            log_path="/tmp/codex-thread-agent.log",
            harness_session_id="0198c0de-0000-7000-8000-00000000000a",
            host_mode="interactive",
            status="live",
        )
    ])


def test_deliver_live_codex_thread_routes_through_switchboard_not_deliver(
    tmp_path: Path, monkeypatch
) -> None:
    use_tmpdir(monkeypatch, tmp_path)
    _register_codex_thread_peer()

    rpc_calls: list[dict] = []

    def _mock_rpc(method: str, params: dict, **kwargs):
        rpc_calls.append({"method": method, "params": params})
        if method == "agent.switchboard_v2":
            assert params["to"] == "codex-thread-agent"
            assert params["recipient_identity"]["harness"] == "codex"
            assert params["recipient_identity"]["short_id"] == ""
            assert params["mirror"] is False, "a thread reply cannot be mirrored mid-turn"
            return {
                "delivered": True,
                "identity_verified": True,
                "transport": "switchboard",
                "turn_id": "turn-1",
            }
        return None

    from fno.agents import dispatch as dispatch_mod
    monkeypatch.setattr(dispatch_mod, "_daemon_rpc", _mock_rpc)

    from fno.agents.dispatch import dispatch_send

    cwd = tmp_path / "work"
    cwd.mkdir()
    result = dispatch_send(from_name="lead", 
        name="codex-thread-agent",
        message="hey hosted thread",
        provider=None,
        cwd=cwd,
    )

    assert result.delivery == "hosted", f"got {result.delivery!r}"
    methods = [call["method"] for call in rpc_calls]
    assert methods == ["agent.switchboard_v2"], (
        f"a thread row must reach ONLY the switchboard, saw {methods}"
    )


# ---------------------------------------------------------------------------
# Claude peer + switchboard (Group 2, Task 3.1): claude now probes the
# versioned switchboard RPC first; a non-stream-thread demotes to socket/MCP
# (purely additive over the 2.1 socket/MCP contract).
# ---------------------------------------------------------------------------

def test_deliver_live_claude_switchboard_demotes_to_socket(
    tmp_path: Path, monkeypatch
) -> None:
    """Claude peer that is NOT a live stream thread: the switchboard probe
    demotes and delivery falls through to the socket path (2.1 behavior)."""
    use_tmpdir(monkeypatch, tmp_path)

    from fno.agents.registry import AgentEntry, write_registry
    write_registry([
        AgentEntry(
            name="claude-peer",
            harness="claude",
            cwd="/tmp",
            log_path="/tmp/claude-peer.log",
            short_id="abcd1234",
            status="live",
        ),
        AgentEntry(
            name="fno",
            harness="claude",
            cwd="/tmp",
            log_path="/tmp/fno.log",
            short_id="fno12345",
            harness_session_id="aaaaaaaa-2222-3333-4444-555555555555",
            status="live",
        ),
    ])

    from fno.agents import dispatch as dispatch_mod
    from fno.agents.harnesses import claude as claude_mod
    monkeypatch.setattr(claude_mod, "mcp_channel_reachable", lambda *a, **kw: False)

    # The control.sock inject (mail-inject command) is the socket-path successor; it
    # succeeds here so the demote falls through to it.
    inject_calls: list = []

    def _ok_inject(recipient: str, text: str, **_k) -> bool:
        inject_calls.append({"recipient": recipient, "text": text})
        return True

    monkeypatch.setattr(dispatch_mod, "_mail_inject_claude", _ok_inject)

    rpc_calls: list = []

    def _mock_rpc(method: str, params: dict, **kwargs):
        rpc_calls.append({"method": method, "params": params})
        # B is not a live stream thread -> the daemon demotes.
        return {"delivered": False, "reason": "not-a-live-stream-thread"}

    monkeypatch.setattr(dispatch_mod, "_daemon_rpc", _mock_rpc)

    from fno.agents.dispatch import dispatch_send

    cwd = tmp_path / "work"
    cwd.mkdir()
    result = dispatch_send(from_name="lead", 
        name="claude-peer",
        message="hi claude",
        provider=None,
        cwd=cwd,
    )

    assert result.delivery == "hosted"
    assert len(inject_calls) == 1, "demote must fall through to the control.sock inject"
    assert len(rpc_calls) == 1, "claude must probe the switchboard RPC"
    assert rpc_calls[0]["method"] == "agent.switchboard_v2"
    assert rpc_calls[0]["params"]["to"] == "claude-peer"
    assert rpc_calls[0]["params"]["recipient_identity"]["short_id"] == "abcd1234"


def test_deliver_live_claude_switchboard_delivered_skips_socket(
    tmp_path: Path, monkeypatch
) -> None:
    """Claude peer that IS a live stream thread: the switchboard delivers the
    turn and the socket/MCP path is skipped entirely."""
    use_tmpdir(monkeypatch, tmp_path)

    from fno.agents.registry import AgentEntry, write_registry
    write_registry([
        AgentEntry(
            name="claude-stream",
            harness="claude",
            cwd="/tmp",
            log_path="/tmp/claude-stream.log",
            short_id="abcd1234",
            harness_session_id="11111111-2222-3333-4444-555555555555",
            status="live",
        ),
        AgentEntry(
            name="stream-sender",
            harness="claude",
            cwd="/tmp",
            log_path="/tmp/stream-sender.log",
            short_id="fno12345",
            harness_session_id="aaaaaaaa-2222-3333-4444-666666666666",
            status="live",
        ),
    ])

    rpc_calls: list = []
    from fno.agents import dispatch as dispatch_mod

    # Pin OBSERVED mode (auto off) so this stays a single-hop delivery test;
    # the A2A relay loop is exercised by the 4.1 tests below.
    monkeypatch.setattr(dispatch_mod, "_load_a2a_settings", lambda: (False, 6))

    def _mock_rpc(method: str, params: dict, **kwargs):
        rpc_calls.append({"method": method, "params": params})
        return {
            "delivered": True,
            "identity_verified": True,
            "transport": "switchboard",
            "reply": "ack",
            "mirrored": True,
        }

    monkeypatch.setattr(dispatch_mod, "_daemon_rpc", _mock_rpc)

    from fno.agents.dispatch import dispatch_send

    cwd = tmp_path / "work"
    cwd.mkdir()
    result = dispatch_send(from_name="stream-sender",
        name="claude-stream",
        message="hi via switchboard",
        provider=None,
        cwd=cwd,
    )

    assert result.delivery == "hosted"
    assert len(rpc_calls) == 1
    assert rpc_calls[0]["method"] == "agent.switchboard_v2"
    assert rpc_calls[0]["params"]["mirror"] is True, "observed mode mirrors into A"
    assert (
        rpc_calls[0]["params"]["recipient_identity"]["session_id"]
        == "11111111-2222-3333-4444-555555555555"
    )
    assert rpc_calls[0]["params"]["from_identity"]["short_id"] == "fno12345"
    # The old socket-path canary (send_to_session must never fire) is gone
    # with the ported ask adapters: there is no Python socket send left to
    # skip, so the switchboard contract above is the whole assertion.


# ---------------------------------------------------------------------------
# Group 2, Task 4.1: A2A relay loop + config.agents.a2a toggle/ceiling
# ---------------------------------------------------------------------------

def test_unanswered_confirmation_continues_as_observed_switchboard_hop(
    tmp_path, monkeypatch
) -> None:
    """A prompt timeout still reaches a terminal observed delivery."""
    use_tmpdir(monkeypatch, tmp_path)
    monkeypatch.delenv("FNO_A2A_NO_CONFIRM", raising=False)
    from fno.agents import dispatch as dispatch_mod

    read_fd, write_fd = os.pipe()

    class _UnansweredTTY:
        def isatty(self):
            return True

        def fileno(self):
            return read_fd

        def readline(self):
            return os.read(read_fd, 4096).decode()

    class _TTYErr:
        def isatty(self):
            return True

        def write(self, _text):
            return None

        def flush(self):
            return None

    monkeypatch.setattr(dispatch_mod.sys, "stdin", _UnansweredTTY())
    monkeypatch.setattr(dispatch_mod.sys, "stderr", _TTYErr())
    real_gate = dispatch_mod._a2a_first_use_gate
    monkeypatch.setattr(
        dispatch_mod,
        "_a2a_first_use_gate",
        lambda auto, ceiling: real_gate(
            auto,
            ceiling,
            confirm_timeout_seconds=0.05,
        ),
    )
    monkeypatch.setattr(dispatch_mod, "_load_a2a_settings", lambda: (True, 6))
    calls: list[dict] = []

    def _rpc(_method, params, **_kwargs):
        calls.append(params)
        return {"delivered": True, "identity_verified": True, "reply": "r1"}

    monkeypatch.setattr(dispatch_mod, "_daemon_rpc", _rpc)
    started = time.monotonic()
    try:
        delivered = dispatch_mod._switchboard_exchange(
            "B",
            "A",
            "msg",
            to_identity=_sb_identity("B"),
            from_identity=_sb_identity("A"),
        )
    finally:
        os.close(write_fd)
        os.close(read_fd)

    assert time.monotonic() - started < 0.5
    assert delivered is True
    assert len(calls) == 1
    assert calls[0]["mirror"] is True
    assert not (tmp_path / ".fno" / ".a2a-confirmed").exists()
    assert not (tmp_path / "config.toml").exists()


def test_switchboard_auto_is_nonblocking_kicks_off_detached_relay(monkeypatch) -> None:
    """ab-3bd520ab: auto=True drives B synchronously (hop 1 = the actual delivery)
    then KICKS OFF the relay in the background and returns immediately. The caller
    runs exactly ONE _daemon_rpc inline; the relay loop never runs in-process."""
    from fno.agents import dispatch as dispatch_mod

    monkeypatch.setattr(dispatch_mod, "_load_a2a_settings", lambda: (True, 6))
    calls: list = []

    def _rpc(method, params, **kw):
        calls.append(params)
        return {"delivered": True, "identity_verified": True, "reply": "r1"}

    kicked: list = []
    monkeypatch.setattr(dispatch_mod, "_daemon_rpc", _rpc)
    monkeypatch.setattr(
        dispatch_mod,
        "_kickoff_background_relay",
        lambda to_name, from_name, seed, ceiling, mail_ctxs=None, **kwargs: kicked.append(
            (to_name, from_name, seed, ceiling, mail_ctxs, kwargs)
        ),
    )
    assert dispatch_mod._switchboard_exchange(
        "B",
        "A",
        "msg",
        to_identity=_sb_identity("B"),
        from_identity=_sb_identity("A"),
    ) is True
    assert len(calls) == 1, "only the first hop (drive B) runs inline; the relay is detached"
    assert calls[0]["to"] == "B" and calls[0]["mirror"] is False
    # No mail ctxs on this bare _switchboard_exchange call -> chat-style raw relay.
    assert kicked == [
        (
            "B",
            "A",
            "r1",
            6,
            None,
            {"recipient_identities": _sb_identities("B", "A")},
        )
    ], "the relay is handed off with B's reply as the seed"


def test_background_relay_first_fork_failure_drops_continuation_without_inline_wait(
    monkeypatch,
) -> None:
    """A resource-exhausted launcher cannot run the relay before the receipt."""
    from fno.agents import dispatch as dispatch_mod

    monkeypatch.setattr(
        dispatch_mod.os,
        "fork",
        lambda: (_ for _ in ()).throw(OSError("fork unavailable")),
    )
    relay_calls: list[tuple] = []
    monkeypatch.setattr(
        dispatch_mod,
        "_run_relay_loop",
        lambda *args, **kwargs: relay_calls.append((args, kwargs)),
    )
    stopped: list[tuple] = []
    monkeypatch.setattr(
        dispatch_mod,
        "_emit_relay_stopped",
        lambda *args, **kwargs: stopped.append((args, kwargs)),
    )

    dispatch_mod._kickoff_background_relay(
        "B",
        "A",
        "reply",
        6,
        recipient_identities=_sb_identities("A", "B"),
    )

    assert relay_calls == []
    assert stopped[0][0][3] == "relay-detach-failed"


def _await_content(path: Path, timeout: float = 2.0) -> str:
    """Return the file's content once it is non-empty, or "" past the deadline."""
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        try:
            content = path.read_text()
        except OSError:
            content = ""
        if content:
            return content
        time.sleep(0.01)
    return ""


def test_real_relay_descendant_releases_captured_receipt_pipes(
    tmp_path, monkeypatch
) -> None:
    """A real double-forked relay cannot delay the hosted CLI-style receipt."""
    use_tmpdir(monkeypatch, tmp_path)
    from fno.agents.registry import AgentEntry, write_registry
    from tests.fixtures.bus_rows import read_rows, scan_unread

    write_registry(
        [
            AgentEntry(
                name="red",
                harness="claude",
                harness_session_id="aaaaaaaa-1111-7222-8333-4444abcd1234",
                cwd=str(tmp_path),
                log_path=str(tmp_path / "red.log"),
                short_id="abcd1234",
                status="live",
            )
        ]
    )
    relay_marker = tmp_path / "relay-started"
    relay_release = tmp_path / "relay-release"
    relay_finished = tmp_path / "relay-finished"
    script = """
import os
import time
from pathlib import Path
from fno.agents import dispatch

dispatch._load_a2a_settings = lambda: (True, 6)
dispatch._registered_family1_state = lambda _entry: "working"

def run_relay(*_args, **_kwargs):
    Path(os.environ["FNO_TEST_RELAY_MARKER"]).write_text("started")
    release = Path(os.environ["FNO_TEST_RELAY_RELEASE"])
    deadline = time.monotonic() + 8
    while not release.exists() and time.monotonic() < deadline:
        time.sleep(0.01)
    Path(os.environ["FNO_TEST_RELAY_FINISHED"]).write_text("finished")

dispatch._run_relay_loop = run_relay
dispatch._daemon_rpc = lambda *_args, **_kwargs: {
    "delivered": True,
    "identity_verified": True,
    "reply": "continue",
}

def deliver(entry, body, from_name, mail=None, sender_entry=None, reason_out=None, **_kw):
    return dispatch._switchboard_exchange(
        entry.name,
        from_name,
        body,
        to_identity={
            "harness": "claude",
            "session_id": entry.harness_session_id,
            "short_id": entry.short_id,
            "created_at": entry.created_at,
        },
        from_identity={
            "harness": "codex",
            "session_id": "sender-session",
            "short_id": "sender01",
            "created_at": "sender-created",
        },
    )

dispatch._deliver_live = deliver
result = dispatch.dispatch_send(from_name="lead", 
    name="red",
    message="hello",
    provider=None,
    cwd=Path.cwd(),
)
print(f"{result.msg_id} delivered ({result.delivery})")
"""
    env = os.environ.copy()
    env["FNO_TEST_RELAY_MARKER"] = str(relay_marker)
    env["FNO_TEST_RELAY_RELEASE"] = str(relay_release)
    env["FNO_TEST_RELAY_FINISHED"] = str(relay_finished)
    try:
        result = subprocess.run(
            [sys.executable, "-c", script],
            capture_output=True,
            text=True,
            env=env,
            timeout=8,
            check=False,
        )

        assert result.returncode == 0, result.stdout + result.stderr
        assert len(result.stdout.splitlines()) == 1
        assert result.stdout.strip().endswith("delivered (hosted)")
        audit_rows = read_rows()
        # The write-ahead stage row and the hosted receipt share one id.
        assert len(audit_rows) == 2
        staged_row, audit = audit_rows
        assert staged_row["id"] == audit["id"]
        assert staged_row.get("delivery") is None
        assert "hello" in staged_row.get("body", "")
        assert audit.get("delivery") == "hosted"
        assert audit["id"] in result.stdout
        assert "hello" in audit.get("body", "")
        assert scan_unread(audit["to"]) == []
        # Poll for the CONTENT, not the path. `exists()` goes true the instant the
        # file is created, so a poll on it can read an empty file the relay has not
        # finished writing, and the assert fails on '' instead of "started".
        assert _await_content(relay_marker) == "started"
        # The relay is still blocked on relay_release here - nothing has
        # touched it yet. Releasing it before this check (a bare
        # `finally: relay_release.touch()` around subprocess.run used to do
        # exactly that) races the still-running orphaned relay process
        # against this assert: its 10ms poll loop can see the touch and
        # write "finished" before this line runs, flaking the test under
        # load. Release only after this check, in the finally below.
        assert not relay_finished.exists(), "relay finished before the CLI receipt returned"
    finally:
        # Always release, even on assertion failure, so the orphaned relay
        # (bounded at an 8s internal deadline anyway) exits promptly.
        relay_release.touch()

    relay_release.write_text("release")
    assert _await_content(relay_finished) == "finished"


def test_relay_loop_bounded_by_ceiling(monkeypatch, capsys) -> None:
    """_run_relay_loop with an always-replying pair runs up to turn_ceiling total
    turns (the first hop already happened) and stops with 'loop ceiling reached',
    alternating A, B, A after the seed."""
    from fno.agents import dispatch as dispatch_mod

    calls: list = []

    def _rpc(method, params, **kw):
        calls.append(params)
        return {"delivered": True, "identity_verified": True, "reply": "more"}

    monkeypatch.setattr(dispatch_mod, "_daemon_rpc", _rpc)
    dispatch_mod._run_relay_loop(
        "B", "A", "r1", 4, recipient_identities=_sb_identities("A", "B")
    )
    # ceiling=4 total; the first hop (drive B) is the caller's, so the loop runs 3.
    assert [c["to"] for c in calls] == ["A", "B", "A"]
    assert all(c["mirror"] is False for c in calls)
    identities = _sb_identities("A", "B")
    assert all(c["recipient_identity"] == identities[c["to"]] for c in calls)
    assert "loop ceiling reached" in capsys.readouterr().err


def test_relay_loop_emits_stop_when_identity_proof_is_missing(monkeypatch) -> None:
    from fno.agents import dispatch as dispatch_mod

    monkeypatch.setattr(
        dispatch_mod,
        "_daemon_rpc",
        lambda *a, **k: {"delivered": True, "reply": "unverified"},
    )
    emitted: list[tuple[str, dict]] = []
    monkeypatch.setattr(
        dispatch_mod,
        "_emit_ev",
        lambda kind, **data: emitted.append((kind, data)),
    )

    turns = dispatch_mod._run_relay_loop(
        "B", "A", "r1", 6, recipient_identities=_sb_identities("A", "B")
    )

    assert turns == 1
    assert emitted == [
        (
            "agent_relay_stopped",
            {
                "target": "A",
                "peer": "B",
                "turn": 2,
                "turns_completed": 1,
                "reason": "identity-unverified",
            },
        )
    ]


def test_relay_loop_persists_stop_event(tmp_path: Path, monkeypatch) -> None:
    use_tmpdir(monkeypatch, tmp_path)
    from fno.agents import dispatch as dispatch_mod

    monkeypatch.setattr(
        dispatch_mod,
        "_daemon_rpc",
        lambda *a, **k: {"delivered": False, "reason": "recipient-identity-changed"},
    )

    dispatch_mod._run_relay_loop(
        "B", "A", "r1", 6, recipient_identities=_sb_identities("A", "B")
    )

    from tests._event_rows import event_rows

    records = event_rows(tmp_path / ".fno" / "events.jsonl")
    assert records[-1]["kind"] == "agent_relay_stopped"
    assert records[-1]["target"] == "A"
    assert records[-1]["turn"] == 2
    assert records[-1]["reason"] == "recipient-identity-changed"


def test_switchboard_demotes_success_without_identity_proof(monkeypatch) -> None:
    """A success without the current daemon's identity proof fails closed."""
    from fno.agents import dispatch as dispatch_mod

    monkeypatch.setattr(dispatch_mod, "_load_a2a_settings", lambda: (False, 6))
    monkeypatch.setattr(
        dispatch_mod,
        "_daemon_rpc",
        lambda *a, **k: {"delivered": True, "reply": "legacy daemon accepted unknown fields"},
    )
    assert dispatch_mod._switchboard_exchange(
        "B",
        "A",
        "msg",
        to_identity=_sb_identity("B"),
        from_identity=_sb_identity("A"),
    ) is None


# ---------------------------------------------------------------------------
# F7c: Param-contract pin - Python client emits shape Rust can deserialize
# ---------------------------------------------------------------------------

def test_daemon_rpc_params_contract_pin(monkeypatch) -> None:
    """F7c: pin the Python client's emitted params shape:
    {name: str, body: str, from_name: str} with method="agent.deliver" and
    id as int. Fails if either side renames a field."""
    import tempfile

    home_dir = Path(tempfile.mkdtemp(prefix="/tmp/fno"))
    sock_path = home_dir / "supervisor.sock"

    captured_params: list[dict] = []


    listening = threading.Event()

    def _capturing_daemon(path: Path) -> None:
        srv = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        srv.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        srv.bind(str(path))
        srv.listen(1)
        # Signal AFTER listen(): bind() already created the socket file, so a
        # client that waits on the file can connect into the gap before anyone
        # is listening and get refused - captured nothing, no error raised.
        listening.set()
        conn, _ = srv.accept()
        try:
            header = b""
            while len(header) < 4:
                chunk = conn.recv(4 - len(header))
                if not chunk:
                    return
                header += chunk
            (length,) = struct.unpack_from("<I", header)
            body = b""
            while len(body) < length:
                chunk = conn.recv(length - len(body))
                if not chunk:
                    return
                body += chunk
            req = json.loads(body.decode("utf-8"))
            captured_params.append(req)
            resp = {"id": req.get("id", 1), "result": {"delivered": True, "transport": "pty"}}
            payload = json.dumps(resp).encode("utf-8")
            conn.sendall(struct.pack("<I", len(payload)) + payload)
        except Exception:
            pass
        finally:
            conn.close()
            srv.close()

    t = threading.Thread(target=_capturing_daemon, args=(sock_path,), daemon=True)
    t.start()
    assert listening.wait(timeout=10.0), "capturing daemon never reached listen()"

    monkeypatch.setenv("FNO_AGENTS_HOME", str(home_dir))

    from fno.agents import dispatch as dispatch_mod
    dispatch_mod._daemon_rpc(
        "agent.deliver",
        {"name": "target-agent", "body": "hello world", "from_name": "orchestrator"},
    )
    t.join(timeout=2.0)

    import shutil
    shutil.rmtree(str(home_dir), ignore_errors=True)

    assert len(captured_params) == 1, f"Expected 1 request captured, got {len(captured_params)}"
    req = captured_params[0]

    # Pin the full contract shape that Rust's handle_deliver expects to deserialize.
    # Any field rename on either side breaks this test.
    assert isinstance(req.get("id"), int), f"id must be int, got {req.get('id')!r}"
    assert req.get("method") == "agent.deliver", f"method must be agent.deliver, got {req.get('method')!r}"
    params = req.get("params", {})
    # These three keys are the canonical Rust-side param names from handle_deliver.
    assert "name" in params, f"params must contain 'name'; got keys: {list(params)}"
    assert "body" in params, f"params must contain 'body'; got keys: {list(params)}"
    assert "from_name" in params, f"params must contain 'from_name'; got keys: {list(params)}"
    assert params["name"] == "target-agent"
    assert params["body"] == "hello world"
    assert params["from_name"] == "orchestrator"
    # No unexpected extra keys that would fail Rust's strict param parsing.
    expected_keys = {"name", "body", "from_name"}
    extra = set(params.keys()) - expected_keys
    assert not extra, f"params contains unexpected keys: {extra}"


# ---------------------------------------------------------------------------
# Gemini PR #459 round 1: malformed-response robustness (never raise)
# ---------------------------------------------------------------------------

def test_daemon_rpc_malformed_json_response_returns_none(monkeypatch) -> None:
    """Malformed JSON bytes from the daemon demote (None), never raise.

    json.JSONDecodeError is a ValueError subclass; the OSError-only catch
    crashed here before the gemini round-1 fix.
    """
    import struct as _struct
    import tempfile
    import threading as _threading

    home_dir = Path(tempfile.mkdtemp(prefix="/tmp/fno"))
    sock_path = home_dir / "supervisor.sock"

    def _raw_server() -> None:
        srv = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        srv.bind(str(sock_path))
        srv.listen(1)
        ready.set()
        conn, _ = srv.accept()
        try:
            _read_frame(conn)
            payload = b"{not json"
            conn.sendall(_struct.pack("<I", len(payload)) + payload)
        except Exception:
            pass
        finally:
            conn.close()
            srv.close()

    ready = _threading.Event()
    t = _threading.Thread(target=_raw_server, daemon=True)
    t.start()
    ready.wait(timeout=10)
    monkeypatch.setenv("FNO_AGENTS_HOME", str(home_dir))

    from fno.agents import dispatch as dispatch_mod
    result = dispatch_mod._daemon_rpc(
        "agent.deliver", {"name": "x", "body": "y", "from_name": "z"}
    )
    assert result is None

    import shutil
    shutil.rmtree(str(home_dir), ignore_errors=True)


# ---------------------------------------------------------------------------
# Codex PR #459 round 1
# ---------------------------------------------------------------------------

def test_deliver_live_mcp_row_delivers_via_control_sock(
    tmp_path: Path, monkeypatch
) -> None:
    """The redundant MCP fast lane is retired (US5): a row with an mcp_channel_id
    delivers over the confirming control.sock lane, never the fire-and-forget
    send_to_channel push (which reported hosted on bytes-written, banned by LD4).
    """
    use_tmpdir(monkeypatch, tmp_path)

    from fno.agents.registry import AgentEntry, write_registry
    write_registry([
        AgentEntry(
            name="claude-mcp",
            harness="claude",
            cwd="/tmp",
            log_path="/tmp/claude-mcp.log",
            short_id="abcd1234",
            status="live",
            mcp_channel_id="abcd1234",
        )
    ])

    from fno.agents import dispatch as dispatch_mod
    inject_calls: list = []

    def _ok_inject(recipient: str, text: str, **_k) -> bool:
        inject_calls.append(recipient)
        return True

    monkeypatch.setattr(dispatch_mod, "_mail_inject_claude", _ok_inject)

    from fno.agents.dispatch import dispatch_send

    cwd = tmp_path / "work"
    cwd.mkdir()
    result = dispatch_send(from_name="lead",
        name="claude-mcp", message="fyi built", provider=None, cwd=cwd
    )

    assert result.delivery == "hosted"
    assert inject_calls == ["abcd1234"], "delivery rides the confirming control.sock lane"


def test_deliver_live_mcp_channel_id_is_the_recipient_fallback(
    tmp_path: Path, monkeypatch
) -> None:
    """No former MCP recipient is stranded by the lane retirement: a live row
    whose plain short_id was cleared (x-3dac) and which carries no
    harness_session_id still resolves a control.sock recipient via its
    mcp_channel_id -- which is the original roster-resolvable short_id."""
    use_tmpdir(monkeypatch, tmp_path)

    from fno.agents.registry import AgentEntry, write_registry
    write_registry([
        AgentEntry(
            name="claude-mcp-only",
            harness="claude",
            cwd="/tmp",
            log_path="/tmp/claude-mcp-only.log",
            short_id="",                # cleared on a live row (x-3dac)
            status="live",
            mcp_channel_id="efgh5678",  # == the original short_id
        )
    ])

    from fno.agents import dispatch as dispatch_mod
    inject_calls: list = []

    def _ok_inject(recipient: str, text: str, **_k) -> bool:
        inject_calls.append(recipient)
        return True

    monkeypatch.setattr(dispatch_mod, "_mail_inject_claude", _ok_inject)

    from fno.agents.dispatch import dispatch_send

    cwd = tmp_path / "work"
    cwd.mkdir()
    result = dispatch_send(from_name="lead", 
        name="claude-mcp-only", message="fyi built", provider=None, cwd=cwd
    )

    assert result.delivery == "hosted"
    assert inject_calls == ["efgh5678"], "recipient falls back to mcp_channel_id"


# ---------------------------------------------------------------------------
# node x-1f23: the autonomous relay continuations carry <fno_mail>, not just
# the seed (codex P2). Chat (no mail ctxs) stays raw.
# ---------------------------------------------------------------------------

def test_relay_loop_wraps_continuations_with_mail_ctxs(monkeypatch) -> None:
    from fno.agents import dispatch as dispatch_mod
    from fno.agents.dispatch import _MailCtx, _run_relay_loop

    calls: list = []

    def _rpc(method, params, **kw):
        calls.append(params)
        return {
            "delivered": True,
            "identity_verified": True,
            "reply": "",
        }  # empty reply ends the loop

    monkeypatch.setattr(dispatch_mod, "_daemon_rpc", _rpc)

    ctxs = {
        "alice": _MailCtx(from_="aaaa1111", model="unknown", to="bbbb2222"),
        "bob": _MailCtx(from_="bbbb2222", model="unknown", to="aaaa1111"),
    }
    # seed = bob's reply; first continuation drives alice with bob's turn, so the
    # hop body is wrapped as BOB (the peer who just spoke).
    _run_relay_loop(
        "bob",
        "alice",
        "bob says hi",
        ceiling=3,
        mail_ctxs=ctxs,
        recipient_identities=_sb_identities("alice", "bob"),
    )
    assert len(calls) == 1
    body = calls[0]["body"]
    assert body.splitlines()[0].startswith("`@bbbb2222 · fmail-"), body
    assert "bob says hi" in body


def test_first_hop_read_budget_covers_the_daemon_drive_ceiling() -> None:
    """The client must never abandon a turn the daemon is still driving.

    `agent.switchboard_v2` does not ack and return: the daemon drives B's whole
    turn before answering. A read budget below that ceiling makes the client give
    up on a body B already received, and `_deliver_live` then injects it again.
    The two numbers live in different languages, so pin them against each other
    rather than against a literal.
    """
    import re
    from pathlib import Path

    from fno.agents.dispatch import _SWITCHBOARD_FIRST_HOP_READ_TIMEOUT

    daemon = Path(__file__).resolve().parents[3] / "crates/fno-agents/src/daemon.rs"
    src = daemon.read_text(encoding="utf-8")
    turn_ms = re.search(r"const SWITCHBOARD_TURN_TIMEOUT_MS: u64 = ([\d_]+);", src)
    grace_s = re.search(r"const SWITCHBOARD_DRIVE_GRACE_S: u64 = ([\d_]+);", src)
    assert turn_ms and grace_s, "the daemon's switchboard budget constants moved"

    ceiling = int(turn_ms.group(1).replace("_", "")) / 1000 + int(grace_s.group(1))
    assert _SWITCHBOARD_FIRST_HOP_READ_TIMEOUT >= ceiling, (
        f"first-hop read budget {_SWITCHBOARD_FIRST_HOP_READ_TIMEOUT}s is below the "
        f"daemon's own drive ceiling {ceiling}s; a send would be delivered twice"
    )
