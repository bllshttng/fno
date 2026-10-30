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


def test_send_has_no_ruling_option_and_touches_no_node(
    runner, ruling_graph, monkeypatch
):
    # `fno backlog note` is the one lane that puts a correction on a node.
    calls = _hosted_dispatch(monkeypatch)

    sent = runner.invoke(
        app,
        ["agents", "mail", "send", "w", "msg", "--ruling", "x-511a"],
    )

    assert sent.exit_code == 2
    assert "No such option" in sent.output and "--ruling" in sent.output
    assert calls == []
    assert _graph_details(ruling_graph) == "Original node details."


def test_send_then_unread_then_ack(runner, mailbox):
    sent = runner.invoke(
        app,
        ["mail", "send", "--to-project", "web", "--kind", "fyi",
         "--from-name", "etl", "--body", "build is green"],
    )
    assert sent.exit_code == 0, sent.output

    from tests.fixtures.bus_rows import scan_unread

    msgs = scan_unread("web")
    assert [m.get("body", "") for m in msgs] == ["build is green"]
    msg_id = msgs[0]["id"]

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
    from tests.fixtures.bus_rows import append_row, read_cursor, scan_unread
    from fno.inbox.store import write_new_thread

    durable = write_new_thread(
        recipient="web", sender="etl", kind="send", body="still pending"
    )
    append_row(
        {
            "id": "msg-hosted2",
            "from": "etl",
            "to": "web",
            "kind": "send",
            "body": "already delivered",
            "delivery": "hosted",
            "to_kind": "session",
        }
    )

    result = runner.invoke(
        app, ["mail", "ack", "msg-hosted2", "--name", "web"]
    )

    assert result.exit_code == 2
    assert "already reached the recipient's harness (hosted)" in (result.stderr or "")
    assert read_cursor("web") is None
    assert [m["id"] for m in scan_unread("web")] == [durable]


def test_unknown_agent_heads_up_exits_16_without_writing(
    runner, mailbox, monkeypatch
):
    from fno.agents import discover
    from tests.fixtures.bus_rows import read_rows

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
    assert read_rows() == []
    assert list(mailbox.glob("**/*.md")) == []


@pytest.mark.parametrize("kind", ["question"])
def test_handle_question_and_fyi_are_refused_before_any_write(runner, mailbox, kind):
    from tests.fixtures.bus_rows import read_rows

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
    assert read_rows() == []
    assert list(mailbox.glob("**/*.md")) == []


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
    from tests.fixtures.bus_rows import read_rows

    row = next(
        m for m in read_rows()
        if m.get("body", "").splitlines()[0].startswith("`@abcd1234 · fmail-")
    )
    assert row.get("from_model") == "claude-opus-4-8"
    assert row.get("from_harness") == "claude"


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



@pytest.mark.parametrize(
    ("name", "replacement"),
    [
        ("list", "fno agents mail unread"),
        ("drain", "fno agents mail drain-self"),
        ("status", "fno agents mail unread"),
        ("triage", "fno agents mail show <id>"),
    ],
)
def test_retired_markdown_readers_exit_1_and_name_their_replacement(
    runner, name, replacement
):
    from fno.mail.cli import mail_app

    res = runner.invoke(mail_app, [name, "--any", "args"])
    assert res.exit_code == 1
    assert f"mail {name} is retired" in res.stderr
    assert replacement in res.stderr
