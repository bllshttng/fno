"""Integration tests for the `fno agents mail` CLI surface (ab-cee91152).

Messaging is one namespace over the jsonl-canon bus log: `mail send` publishes
a durable envelope; `mail unread`/`ack` are the per-recipient cursor consume.
The old
`fno inbox` namespace (mail's alias for `mail unread`) was retired clean (no
pointer, no shim). x-afa6 later minted a real `fno inbox` root for an
unrelated purpose (approvals/notify/outstanding/the lead board); mail's old
`unread` leaf never came back under it.
"""
from __future__ import annotations

import json

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


def _hosted_dispatch(monkeypatch):
    calls = []

    def dispatch_send(from_name="lead", **kwargs):
        calls.append(kwargs)
        from fno.agents.dispatch import DispatchSendResult

        return DispatchSendResult(msg_id="msg-ruling1", delivery="hosted")

    monkeypatch.setattr("fno.agents.dispatch.dispatch_send", dispatch_send)
    return calls


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


# ---------------------------------------------------------------------------
# US4/AC3-HP: the envelope carries the invoking session's real from + model.
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


# ---------------------------------------------------------------------------
# Dead-letterbox (x-730d): a project send with no live peer queues durably and
# fails loud on stderr so the sender knows delivery deferred (exit stays 0).
# ---------------------------------------------------------------------------


