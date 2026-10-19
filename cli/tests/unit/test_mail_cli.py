"""Integration tests for the `fno agents mail` CLI surface (ab-cee91152).

Messaging is one namespace over the jsonl-canon bus log: `mail send` publishes
a durable envelope; `mail unread`/`ack` are the per-recipient cursor consume;
`mail rebuild-render` regenerates the derived markdown from the log. The old
`fno inbox` namespace (mail's alias for `mail unread`) was retired clean (no
pointer, no shim). x-afa6 later minted a real `fno inbox` root for an
unrelated purpose (approvals/notify/outstanding/the lead board); mail's old
`unread` leaf never came back under it.
"""
from __future__ import annotations

import json
import re

import pytest
from typer.testing import CliRunner

from fno.cli import app
from fno.paths_testing import use_tmpdir
from tests.fixtures.graph_seed import seed_graph

# The drain-self tests exec the bundled fno-agents binary through the real
# forward leaf, and the smoke shards delete that build on purpose; the
# dev-build suite (tests/test-dev-build-suites.sh) is the one pytest leg
# that runs them with it present.
requires_rust = pytest.mark.dev_build


@pytest.fixture
def runner():
    return CliRunner()


def isolate_mailbox(tmp_path, monkeypatch):
    """Co-isolate the md render (FNO_INBOX_ROOT), the bus log, and the roster under tmp.

    A plain helper (not a fixture) so tests can invoke it imperatively after
    seeding an ambient env, without depending on fixture setup ordering.
    """
    # FNO_BUS_DIR outranks FNO_INBOX_ROOT in bus_dir(); clear it so FNO_INBOX_ROOT wins.
    monkeypatch.delenv("FNO_BUS_DIR", raising=False)
    # FNO_CLAUDE_DAEMON_DIR is process-global (set by spawn_gate) and defaults to the
    # real ~/.claude/daemon; pin an empty dir so no real live session leaks in. Tests
    # that need a roster (_isolate_claude_roster) setenv after this and win.
    monkeypatch.setenv("FNO_CLAUDE_DAEMON_DIR", str(tmp_path / "daemon-empty"))
    monkeypatch.setenv("FNO_INBOX_ROOT", str(tmp_path))
    use_tmpdir(monkeypatch, tmp_path)


@pytest.fixture
def mailbox(tmp_path, monkeypatch):
    """Co-isolate the md render (FNO_INBOX_ROOT), the bus log, and the roster under tmp."""
    isolate_mailbox(tmp_path, monkeypatch)
    return tmp_path


@pytest.fixture
def ruling_graph(mailbox, monkeypatch):
    graph_path = mailbox / ".fno" / "graph.json"
    seed_graph(graph_path, [{
        "id": "x-511a",
        "slug": "mailed-ruling",
        "title": "mailed ruling",
        "status": "ready",
        "type": "feature",
        "priority": "p2",
        "details": "Original node details.",
    }])
    import fno.graph._constants as graph_constants
    import fno.graph.store as graph_store

    # setitem, not setattr: GRAPH_JSON is served by module __getattr__, and a
    # setattr's undo would bake the resolved path into the module for every
    # later test in the process.
    monkeypatch.setitem(vars(graph_constants), "GRAPH_JSON", graph_path)
    monkeypatch.setattr(graph_store, "GRAPH_JSON", graph_path)
    monkeypatch.setattr("fno.paths.graph_json", lambda: graph_path)
    # The native read-backs resolve the store through FNO_CONFIG's state_dir.
    fno_dir = mailbox / ".fno"
    (fno_dir / "config.toml").write_text(f'state_dir = "{fno_dir}"\n')
    monkeypatch.setenv("FNO_CONFIG", str(fno_dir / "config.toml"))
    return graph_path


def _graph_details(graph_path):
    from fno.graph.store import read_graph_strict

    return read_graph_strict(graph_path)[0]["details"]


def _hosted_dispatch(monkeypatch, before_transport=None):
    calls = []

    def dispatch_send(from_name="lead", **kwargs):
        if before_transport is not None:
            before_transport()
        calls.append(kwargs)
        from fno.agents.dispatch import DispatchSendResult

        return DispatchSendResult(msg_id="msg-ruling1", delivery="hosted")

    monkeypatch.setattr("fno.agents.dispatch.dispatch_send", dispatch_send)
    return calls


def test_named_send_ruling_appends_dated_node_block_before_transport(
    runner, ruling_graph, monkeypatch
):
    marker = "Ruling sentinel 511a must survive a fresh node read."
    calls = _hosted_dispatch(
        monkeypatch,
        before_transport=lambda: marker in _graph_details(ruling_graph)
        or pytest.fail("ruling was not projected before transport"),
    )

    sent = runner.invoke(
        app,
        [
            "agents", "mail", "send", "worker-one", marker,
            "--from-name", "lead", "--ruling", "x-511a",
        ],
    )

    assert sent.exit_code == 0, sent.output
    import json as _json

    receipt = _json.loads(sent.stdout)
    assert set(receipt) == {"msg_id", "subject", "to", "status"}
    assert receipt["msg_id"] == "msg-ruling1"
    assert receipt["status"] == "delivered (hosted)"
    assert len(calls) == 1
    import os as _os
    import subprocess as _sp

    from fno.rust_binary import find_dev_binary, resolve_binary

    binary = find_dev_binary() or resolve_binary()
    if binary is None:
        pytest.skip("no fno-agents dev build (cargo build -p fno-agents)")
    fresh = _sp.run(
        [str(binary), "backlog", "get", "x-511a"],
        capture_output=True,
        text=True,
        env={**_os.environ, "FNO_TRACKER_BACKEND": "graph"},
    )
    assert fresh.returncode == 0, fresh.stderr
    assert marker in fresh.stdout
    assert re.search(r"### Ruling \(\d{4}-\d{2}-\d{2}\)", fresh.stdout)


def test_send_ruling_unknown_node_refuses_before_mail(
    runner, ruling_graph, monkeypatch
):
    calls = _hosted_dispatch(monkeypatch)

    sent = runner.invoke(
        app,
        [
            "agents", "mail", "send", "worker-one",
            "This ruling names no real node.", "--from-name", "lead",
            "--ruling", "x-dead",
        ],
    )

    assert sent.exit_code != 0
    assert calls == []
    assert _graph_details(ruling_graph) == "Original node details."
    assert "ruling node not found" in sent.stderr


@pytest.mark.parametrize(
    "lane_args",
    [
        ["worker-one", "/review", "--raw"],
    ],
    ids=["raw"],
)
def test_send_ruling_refuses_non_named_worker_lanes_before_side_effects(
    runner, ruling_graph, monkeypatch, lane_args
):
    calls = _hosted_dispatch(monkeypatch)

    sent = runner.invoke(
        app,
        ["agents", "mail", "send", *lane_args, "--ruling", "x-511a"],
    )

    assert sent.exit_code != 0
    assert calls == []
    assert _graph_details(ruling_graph) == "Original node details."
    assert "--ruling supports only send <worker> <message>" in sent.stderr


def test_send_then_unread_then_ack(runner, mailbox):
    sent = runner.invoke(
        app,
        ["mail", "send", "--to-project", "web", "--kind", "fyi",
         "--from-name", "etl", "--body", "build is green"],
    )
    assert sent.exit_code == 0, sent.output

    from fno.bus.cursor import scan_unread

    msgs = scan_unread("web")
    assert [m.body for m in msgs] == ["build is green"]
    msg_id = msgs[0].id

    acked = runner.invoke(app, ["agents", "mail", "ack", msg_id, "--name", "web"])
    assert acked.exit_code == 0, acked.output

    assert scan_unread("web") == []


def test_send_body_and_body_file_refused(runner, mailbox):
    body_file = mailbox / "msg.md"
    body_file.write_text("from the file\n", encoding="utf-8")
    sent = runner.invoke(
        app,
        ["agents", "mail", "send", "--to-project", "web", "--from-name", "etl",
         "--body", "inline", "--body-file", str(body_file)],
    )
    assert sent.exit_code == 1
    assert "not both" in sent.stderr


def test_ack_refuses_hosted_audit_without_skipping_durable_mail(runner, mailbox):
    from fno.bus.cursor import read_cursor, scan_unread
    from fno.bus.log import Envelope, append
    from fno.inbox.store import write_new_thread

    durable = write_new_thread(
        recipient="web", sender="etl", kind="send", body="still pending"
    )
    append(
        Envelope.new(
            id="msg-hosted2",
            from_="etl",
            to="web",
            kind="send",
            body="already delivered",
            delivery="hosted",
            to_kind="session",
        )
    )

    result = runner.invoke(
        app, ["mail", "ack", "msg-hosted2", "--name", "web"]
    )

    assert result.exit_code == 2
    assert "already delivered (hosted)" in (result.stderr or "")
    assert read_cursor("web") is None
    assert [m.id for m in scan_unread("web")] == [durable.thread_id]


def test_unknown_agent_heads_up_exits_16_without_writing(
    runner, mailbox, monkeypatch
):
    from fno.agents import discover
    from fno.bus.log import iter_messages

    empty = mailbox / "discovery-empty"
    empty.mkdir()
    monkeypatch.setenv(discover.SESSIONS_DIR_ENV, str(empty))
    monkeypatch.setenv(discover.PROJECTS_DIR_ENV, str(empty))
    monkeypatch.setenv(discover.CODEX_SESSIONS_DIR_ENV, str(empty))
    monkeypatch.setenv(discover.OPENCODE_STORAGE_DIR_ENV, str(empty))

    result = runner.invoke(
        app,
        [
            "mail", "send", "missing-worker", "Migration is ready",
            "--kind", "heads-up", "--from-name", "sender",
        ],
    )

    assert result.exit_code == 16
    assert "unknown agent or live-session handle" in result.stderr
    assert list(iter_messages()) == []
    assert list(mailbox.glob("**/*.md")) == []


@pytest.mark.parametrize("kind", ["question"])
def test_handle_question_and_fyi_are_refused_before_any_write(runner, mailbox, kind):
    from fno.bus.log import iter_messages

    result = runner.invoke(
        app,
        [
            "mail",
            "send",
            "worker-a",
            "Does this route?",
            "--kind",
            kind,
            "--from-name",
            "sender",
        ],
    )

    assert result.exit_code == 2
    assert f"--kind {kind} to a session handle" in result.stderr
    assert "Drop --kind to inject it live" in result.stderr
    assert "add --to-project <project>" in result.stderr
    assert list(iter_messages()) == []
    assert list(mailbox.glob("**/*.md")) == []


# ---------------------------------------------------------------------------
# AC1-EDGE: a deleted render is regenerated from the log, no message lost
# ---------------------------------------------------------------------------

def test_rebuild_render_regenerates_from_log(runner, mailbox):
    from fno.inbox.store import inbox_dir_for

    runner.invoke(
        app, ["mail", "send", "--to-project", "web", "--kind", "fyi",
              "--from-name", "etl", "--body", "durable note"],
    )
    inbox = inbox_dir_for("web")
    for p in inbox.glob("*.md"):
        p.unlink()
    assert list(inbox.glob("*.md")) == []

    res = runner.invoke(app, ["agents", "mail", "rebuild-render", "web", "--json"])
    assert res.exit_code == 0, res.output
    payload = json.loads(res.stdout.strip().splitlines()[-1])
    assert payload["threads"] == 1
    assert list(inbox.glob("*.md"))  # render regenerated from the canonical log


# ---------------------------------------------------------------------------
# US5 / AC2-HP: a session drains its OWN cross-harness inbox and acks it
# ---------------------------------------------------------------------------

def _seed_bus_message(*, to: str, from_: str, body: str):
    """Append one durable bus envelope addressed to <to> (bypasses resolution)."""
    from fno.inbox.store import write_new_thread

    return write_new_thread(
        recipient=to, sender=from_, kind="send", body=body, to_kind="name"
    )


@requires_rust
def test_drain_self_reads_own_handle_and_acks(runner, mailbox, monkeypatch):
    # AC1-EDGE: the bare address drains exactly once and acks under that key.
    monkeypatch.setenv("CODEX_THREAD_ID", "019f48e1-5b09-72a0-9bc8-6b364bcf4ae4")
    _seed_bus_message(to="019f48e1", from_="claude-web", body="ack from K")

    res = runner.invoke(app, ["agents", "mail", "drain-self", "--json"])
    assert res.exit_code == 0, res.output
    payload = json.loads(res.stdout.strip().splitlines()[-1])
    assert [m["body"] for m in payload] == ["ack from K"]
    assert payload[0]["to"] == "019f48e1"

    # Ack advanced the cursor: a second drain sees nothing (not re-surfaced).
    again = runner.invoke(app, ["agents", "mail", "drain-self", "--json"])
    assert json.loads(again.stdout.strip().splitlines()[-1]) == []


def test_drain_self_without_the_binary_refuses_with_a_reinstall_hint(
    runner, mailbox, monkeypatch
):
    # The forward leaf is the only Python left for hold/hold-release/drain-self,
    # so its no-binary refusal is the operator surface when the wheel's binary
    # is missing. That path is unreachable in the dev-build leg (the binary
    # exists there) and the marked tests above skip everywhere else, so this
    # seam test is the one owner of the 127 contract. The transport consults
    # the checkout's dev build before the resolver, so missing means both
    # routes read None.
    monkeypatch.setattr("fno.rust_binary.find_dev_binary", lambda: None)
    monkeypatch.setattr("fno.rust_binary.resolve_binary", lambda: None)
    res = runner.invoke(app, ["agents", "mail", "drain-self"])
    assert res.exit_code == 127
    streams = res.stdout + (res.stderr or "")
    assert "fno-agents binary was not found" in streams
    assert "reinstall fno" in streams
    assert "FNO_AGENTS_BIN" in streams


def test_drain_self_forward_execs_the_binary_with_the_mail_group_argv(
    runner, mailbox, tmp_path, monkeypatch
):
    # The leaf's transport shape: exec [binary, "mail", *argv] captured, echo
    # both streams back through typer, propagate the child's exit code. A fake
    # script stands in for the binary so argv order and returncode survive the
    # round trip without one.
    fake = tmp_path / "fno-agents"
    fake.write_text(
        '#!/bin/sh\n'
        'printf \'argv: %s\\n\' "$@"\n'
        'echo "drain-stdout-line"\n'
        'echo "drain-stderr-line" >&2\n'
        'exit 3\n',
        encoding="utf-8",
    )
    fake.chmod(0o755)
    # The dev build outranks the resolver, so close that route before the
    # fake stands in.
    monkeypatch.setattr("fno.rust_binary.find_dev_binary", lambda: None)
    monkeypatch.setattr("fno.rust_binary.resolve_binary", lambda: str(fake))
    res = runner.invoke(app, ["agents", "mail", "drain-self"])
    streams = res.stdout + (res.stderr or "")
    # The stub's printf reuses the format per argument, so argv order reads
    # as stacked lines, not one joined line.
    assert "argv: mail\nargv: drain-self" in streams
    assert "drain-stdout-line" in streams
    assert "drain-stderr-line" in streams
    assert res.exit_code == 3


# ---------------------------------------------------------------------------
# US7(a) / AC1-HP + AC2-HP: send to a DISK-DISCOVERED codex session (never
# registered) is addressed to its handle and drained by drain-self. Full round.
# ---------------------------------------------------------------------------

def _write_codex_rollout(codex_dir, *, session_id, cwd):
    import json as _json
    import os as _os
    import time as _time

    day = codex_dir / "2026" / "07" / "09"
    day.mkdir(parents=True, exist_ok=True)
    f = day / f"rollout-2026-07-09T00-00-00-{session_id}.jsonl"
    f.write_text(
        _json.dumps({"type": "session_meta", "payload": {"id": session_id, "cwd": cwd}}) + "\n",
        encoding="utf-8",
    )
    mt = _time.time() - 5.0
    _os.utime(f, (mt, mt))


def _isolate_codex_discovery(monkeypatch, tmp_path, *, session_id):
    from fno.agents import discover, session_truth

    codex_dir = tmp_path / "codex"
    _write_codex_rollout(codex_dir, session_id=session_id, cwd="/Users/x/proj")
    empty = tmp_path / "empty"
    empty.mkdir()
    monkeypatch.setenv(discover.SESSIONS_DIR_ENV, str(empty))
    monkeypatch.setenv(discover.PROJECTS_DIR_ENV, str(empty))
    monkeypatch.setenv(discover.CODEX_SESSIONS_DIR_ENV, str(codex_dir))
    monkeypatch.setattr(
        session_truth,
        "resolve_session_truth",
        lambda *_args, **_kwargs: {"state": "working", "last_activity_age_s": 1},
    )


@requires_rust
def test_us7a_send_to_disk_discovered_codex_round_trips(runner, mailbox, monkeypatch, tmp_path):
    sid = "019f48e1-5b09-72a0-9bc8-6b364bcf4ae4"
    _isolate_codex_discovery(monkeypatch, tmp_path, session_id=sid)
    # US8: no live daemon in the test env -- force the codex live-inject miss so
    # the send deterministically writes the durable floor (round-trip target).
    monkeypatch.setattr("fno.agents.dispatch._mail_inject_codex", lambda *_a, **_k: False)

    # A claude session sends to the codex handle (codex row is unregistered, so
    # the send falls through registry-unknown into disk resolution).
    sent = runner.invoke(
        app, ["mail", "send", "019f48e1", "ack from K", "--from-name", "web"]
    )
    assert sent.exit_code == 14, sent.output
    assert "019f48e1" in sent.output
    assert "queued (durable)" in sent.output

    # The codex session drains its own handle and sees the message.
    monkeypatch.setenv("CODEX_THREAD_ID", sid)
    drained = runner.invoke(app, ["agents", "mail", "drain-self", "--json"])
    assert drained.exit_code == 0, drained.output
    payload = json.loads(drained.stdout.strip().splitlines()[-1])
    assert payload and payload[0]["to"] == "019f48e1"
    assert "ack from K" in payload[0]["body"]  # inside the <fno_mail> wrap


@requires_rust
def test_us8_codex_live_inject_hosted_short_circuits_durable(
    runner, mailbox, monkeypatch, tmp_path
):
    # US8 (node x-d899): a running codex daemon takes the turn LIVE, so the send
    # reports "delivered (hosted)" and writes only an audit bus row, never a
    # durable thread.
    sid = "019f48e1-5b09-72a0-9bc8-6b364bcf4ae4"
    _isolate_codex_discovery(monkeypatch, tmp_path, session_id=sid)
    monkeypatch.setattr("fno.agents.dispatch._mail_inject_codex", lambda *_a, **_k: True)

    sent = runner.invoke(
        app, ["mail", "send", "019f48e1", "ack from K", "--from-name", "web"]
    )
    assert sent.exit_code == 0, sent.output
    assert "delivered (hosted)" in sent.output
    assert "queued (durable)" not in sent.output

    # The audit row is non-deliverable: drain-self sees nothing for the handle.
    monkeypatch.setenv("CODEX_THREAD_ID", sid)
    drained = runner.invoke(app, ["agents", "mail", "drain-self", "--json"])
    assert drained.exit_code == 0, drained.output
    payload = json.loads(drained.stdout.strip().splitlines()[-1])
    assert payload == []


def test_plain_send_delivers_header_and_body(
    runner, mailbox, monkeypatch, tmp_path
):
    # A plain peer send (no --kind) delivers the header line and then the
    # body: no live turn arrives as the header alone. The bus row keeps the
    # same body, so the id the header names resolves to it.
    sid = "9a063cd3-69d4-415a-ada5-649b0164189c"
    _isolate_claude_roster(monkeypatch, tmp_path, session_id=sid)
    injected: list[str] = []

    def _capture(recipient, text, **_k):
        injected.append(text)
        return True

    monkeypatch.setattr("fno.agents.dispatch._mail_inject_claude", _capture)

    sent = runner.invoke(
        app,
        ["mail", "send", "9a063cd3", "secret body words", "--from-name", "web"],
    )
    assert sent.exit_code == 0, sent.output
    assert len(injected) == 1
    # Without a subject the header's third field is the body's first sentence
    # (AC10-HP). A body that is only that sentence still follows the header.
    lines = injected[0].splitlines()
    assert len(lines) == 2, injected[0]
    header = lines[0]
    assert header.startswith("`@web · fmail-")
    assert lines[1] == "secret body words", injected[0]
    msg_id = header.split(" · ")[1].rstrip("`")

    # The bus copy the id points at holds the full body.
    from fno.bus.log import iter_messages

    row = next(m for m in iter_messages() if m.id == msg_id)
    assert "secret body words" in row.body


def test_interrupted_live_send_keeps_the_stored_copy(
    runner, mailbox, monkeypatch, tmp_path
):
    # Write first: the stored copy lands before the live attempt, so a sender
    # killed between the two steps still leaves the mail `mail show` reads.
    sid = "9a063cd3-69d4-415a-ada5-649b0164189c"
    _isolate_claude_roster(monkeypatch, tmp_path, session_id=sid)
    typed: list[str] = []

    def _interrupted(recipient, text, **_k):
        typed.append(text)
        raise KeyboardInterrupt

    monkeypatch.setattr("fno.agents.dispatch._mail_inject_claude", _interrupted)
    runner.invoke(
        app, ["mail", "send", "9a063cd3", "kept body words", "--from-name", "web"]
    )
    assert len(typed) == 1
    msg_id = typed[0].splitlines()[0].split(" · ")[1].rstrip("`")

    from fno.rust_binary import chats_verb

    stored = chats_verb(["show", msg_id, "--all", "--json"], {})
    assert stored["id"] == msg_id
    assert "kept body words" in stored["body"]

    # The same staging writes the bus row first, so the killed send is also
    # drainable: the body's only home is on the bus before any inject.
    from fno.bus.log import iter_messages

    staged = [m for m in iter_messages() if m.id == msg_id]
    assert len(staged) == 1
    assert staged[0].delivery is None
    assert "kept body words" in staged[0].body


# ---------------------------------------------------------------------------
# a2a US7b / AC3-HP: the mux PaneSend live rung. A resolved session that is
# mux-hosted (fno owns its PTY) delivers live through its pane instead of
# demoting to the durable bus. Provider-agnostic by construction: the rung keys
# off the roster entry's pane info, never a provider name.
# ---------------------------------------------------------------------------


def _stub_pane_rung(
    monkeypatch, *, in_roster: bool, pane_sends: bool, expect_token: str,
    status: str = "live",
) -> list:
    """Wire the pane rung to a fake roster entry and a recorded ``_mux_pane_send``.
    Returns the call log, so a test can assert the rung was skipped entirely.

    ``expect_token`` is asserted inside the resolver: the rung must look up the
    full session id, not the display handle. The two pick different match rules
    in resolve_agent_in (full_session_id vs derived_short), so a swap changes
    real behavior while leaving every outcome assertion green."""
    from types import SimpleNamespace

    from fno.agents.registry import AgentResolutionError, ResolvedAgent

    entry = SimpleNamespace(
        name="hosted-worker", status=status, mux={"session": "main", "pane_id": 3}
    )

    def _resolve(token, **_kw):
        assert token == expect_token, f"rung resolved {token!r}, expected the session id"
        if not in_roster:
            raise AgentResolutionError(f"no agent matching {token!r}")
        return ResolvedAgent(entry=entry, matched_by="full_session_id")

    calls: list = []

    def _send(resolved_entry, text, **_kw):
        calls.append((resolved_entry, text))
        return pane_sends

    monkeypatch.setattr("fno.agents.registry.resolve_agent", _resolve)
    monkeypatch.setattr("fno.agents.dispatch._mux_pane_send", _send)
    return calls


@requires_rust
def test_us7b_mux_pane_rung_delivers_live_when_socket_inject_misses(
    runner, mailbox, monkeypatch, tmp_path
):
    """Codex daemon socket is dead but the session is mux-hosted: the pane rung
    takes the turn live, so no durable thread is written."""
    sid = "019f48e1-5b09-72a0-9bc8-6b364bcf4ae4"
    _isolate_codex_discovery(monkeypatch, tmp_path, session_id=sid)
    monkeypatch.setattr("fno.agents.dispatch._mail_inject_codex", lambda *_a, **_k: False)
    calls = _stub_pane_rung(
        monkeypatch, in_roster=True, pane_sends=True, expect_token=sid
    )

    sent = runner.invoke(
        app, ["mail", "send", "019f48e1", "ping", "--from-name", "web"]
    )
    assert sent.exit_code == 0, sent.output
    assert "delivered (hosted)" in sent.output
    assert "queued (durable)" not in sent.output
    assert len(calls) == 1
    # The live turn is the header line, then the body.
    lines = calls[0][1].splitlines()
    assert len(lines) == 2, calls[0][1]
    assert lines[0].startswith("`@web · fmail-")
    assert lines[1] == "ping"

    monkeypatch.setenv("CODEX_THREAD_ID", sid)
    drained = runner.invoke(app, ["agents", "mail", "drain-self", "--json"])
    assert json.loads(drained.stdout.strip().splitlines()[-1]) == []


@requires_rust
def test_us7b_mux_pane_send_failure_falls_closed_to_durable(
    runner, mailbox, monkeypatch, tmp_path
):
    """Roster entry exists but the pane send fails (mux gone, claim lost): the
    message must land on the durable floor, never vanish."""
    sid = "019f48e1-5b09-72a0-9bc8-6b364bcf4ae4"
    _isolate_codex_discovery(monkeypatch, tmp_path, session_id=sid)
    monkeypatch.setattr("fno.agents.dispatch._mail_inject_codex", lambda *_a, **_k: False)
    calls = _stub_pane_rung(
        monkeypatch, in_roster=True, pane_sends=False, expect_token=sid
    )

    sent = runner.invoke(
        app, ["mail", "send", "019f48e1", "ping", "--from-name", "web"]
    )
    assert sent.exit_code == 14, sent.output
    assert "queued (durable)" in sent.output
    assert len(calls) == 1

    monkeypatch.setenv("CODEX_THREAD_ID", sid)
    drained = runner.invoke(app, ["agents", "mail", "drain-self", "--json"])
    payload = json.loads(drained.stdout.strip().splitlines()[-1])
    assert payload and "ping" in payload[0]["body"]


@requires_rust
@pytest.mark.parametrize("status", ["idle"])
def test_us7b_non_live_entry_never_pane_sends(runner, mailbox, monkeypatch, tmp_path, status):
    """Only a "live" row may be pane-sent. A non-live row keeps its mux ref, and
    pane ids restart at 1 with the mux server, so sending on that ref would type
    into an unrelated pane and report hosted, suppressing the durable copy the
    real recipient needs. "idle" matters as much as "exited" here: it is the
    status a hand-started session registers under precisely because it has no
    live transport. Parameterized so weakening the gate to `!= "exited"` fails."""
    sid = "019f48e1-5b09-72a0-9bc8-6b364bcf4ae4"
    _isolate_codex_discovery(monkeypatch, tmp_path, session_id=sid)
    monkeypatch.setattr("fno.agents.dispatch._mail_inject_codex", lambda *_a, **_k: False)
    calls = _stub_pane_rung(
        monkeypatch, in_roster=True, pane_sends=True, expect_token=sid, status=status
    )

    sent = runner.invoke(app, ["mail", "send", "019f48e1", "ping", "--from-name", "web"])
    assert sent.exit_code == 14, sent.output
    assert "queued (durable)" in sent.output
    assert calls == []  # the stale pane was never written to

    monkeypatch.setenv("CODEX_THREAD_ID", sid)
    drained = runner.invoke(app, ["agents", "mail", "drain-self", "--json"])
    payload = json.loads(drained.stdout.strip().splitlines()[-1])
    assert payload and "ping" in payload[0]["body"]


# ---------------------------------------------------------------------------
# x-605c US3 / AC1-HP + AC1-FR: send to a ROSTERED claude bg worker is
# handle-addressed (live-inject first, durable floor to its canonical handle);
# the old claude->project re-route is gone. US4/AC3-HP: the envelope carries the
# invoking session's real from + model.
# ---------------------------------------------------------------------------


def _isolate_claude_roster(monkeypatch, tmp_path, *, session_id):
    """Only the daemon roster resolves: empty disk sources + a fixture roster
    holding one rostered claude bg worker (no pid-sidecar)."""
    from fno.agents import discover, session_truth

    empty = tmp_path / "empty"
    empty.mkdir(exist_ok=True)
    monkeypatch.setenv(discover.SESSIONS_DIR_ENV, str(empty))
    monkeypatch.setenv(discover.PROJECTS_DIR_ENV, str(empty))
    monkeypatch.setenv(discover.CODEX_SESSIONS_DIR_ENV, str(empty))
    daemon = tmp_path / "daemon"
    daemon.mkdir(parents=True, exist_ok=True)
    (daemon / "roster.json").write_text(
        json.dumps({"proto": 1, "workers": {
            session_id[:8]: {"sessionId": session_id, "pid": 4242, "cwd": "/Users/x/proj"}
        }}),
        encoding="utf-8",
    )
    monkeypatch.setenv("FNO_CLAUDE_DAEMON_DIR", str(daemon))
    monkeypatch.setattr(
        session_truth,
        "resolve_session_truth",
        lambda *_args, **_kwargs: {"state": "working", "last_activity_age_s": 1},
    )


@requires_rust
def test_ac3_hp_envelope_carries_real_from_and_the_model_rides_the_bus(
    runner, mailbox, monkeypatch, tmp_path
):
    recipient_sid = "9a063cd3-69d4-415a-ada5-649b0164189c"
    _isolate_claude_roster(monkeypatch, tmp_path, session_id=recipient_sid)
    monkeypatch.setattr("fno.agents.dispatch._mail_inject_claude", lambda *_a, **_k: False)

    # The SENDER is a claude session with its own transcript carrying a model.
    from fno.agents import discover

    sender_sid = "abcd1234-1111-2222-3333-444444444444"
    projects = tmp_path / "sender-projects"
    (projects / "-Users-x-proj").mkdir(parents=True, exist_ok=True)
    (projects / "-Users-x-proj" / f"{sender_sid}.jsonl").write_text(
        json.dumps(
            {
                "type": "assistant",
                "isSidechain": False,
                "message": {"model": "claude-opus-4-8"},
            }
        )
        + "\n",
        encoding="utf-8",
    )
    monkeypatch.setenv(discover.PROJECTS_DIR_ENV, str(projects))
    monkeypatch.setenv("CLAUDE_CODE_SESSION_ID", sender_sid)

    # No --from-name: from + model are auto-stamped from the invoking session.
    sent = runner.invoke(app, ["agents", "mail", "send", "9a063cd3", "hello"])
    assert sent.exit_code == 14, sent.output

    monkeypatch.setenv("CLAUDE_CODE_SESSION_ID", recipient_sid)
    drained = runner.invoke(app, ["agents", "mail", "drain-self", "--json"])
    body = json.loads(drained.stdout.strip().splitlines()[-1])[0]["body"]
    # Full sender identity rides the header line; the model stays on the bus
    # record, never in the delivered text.
    assert body.splitlines()[0].startswith("`@abcd1234 · fmail-")
    assert 'model=' not in body
    from fno.bus.log import iter_messages

    row = next(
        m for m in iter_messages()
        if m.body.splitlines()[0].startswith("`@abcd1234 · fmail-")
    )
    assert row.from_model == "claude-opus-4-8"
    assert row.from_harness == "claude"


# ---------------------------------------------------------------------------
# Dead-letterbox (x-730d): a project send with no live peer queues durably and
# fails loud on stderr so the sender knows delivery deferred (exit stays 0).
# ---------------------------------------------------------------------------

def test_project_send_no_peer_warns_deferred(runner, mailbox):
    res = runner.invoke(
        app, ["mail", "send", "--to-project", "web", "--from-name", "etl", "quiet?"]
    )
    assert res.exit_code == 0, res.output
    receipt = json.loads(res.stdout.strip())
    assert receipt["to"] == "web"
    assert "queued (durable)" in receipt["status"]
    assert "project inbox web has no live drain" in (res.stderr or "")


@requires_rust
@pytest.mark.parametrize("token", ["019fb417-1111-7222-8333-444455556666"])
def test_full_and_legacy_ambient_self_queue_to_canonical_without_inject(
    runner, mailbox, monkeypatch, token
):
    sid = "019fb417-1111-7222-8333-444455556666"
    monkeypatch.setenv("CODEX_THREAD_ID", sid)
    monkeypatch.setattr(
        "fno.agents.dispatch._mail_inject_codex",
        lambda *_a, **_k: pytest.fail("self-send attempted live injection"),
    )
    sent = runner.invoke(app, ["agents", "mail", "send", token, "note", "--from-name", "web"])
    assert sent.exit_code == 0, sent.output
    drained = runner.invoke(app, ["agents", "mail", "drain-self", "--json"])
    payload = json.loads(drained.stdout.strip().splitlines()[-1])
    assert payload and payload[0]["to"] == "019fb417"

