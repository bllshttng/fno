"""`fno backlog note` delivery: who gets reached, and what a refusal must say.

The defect this closes is a write that succeeds while the communication fails
and nothing reports it. So the tests that matter assert a POSITIVE receipt:
every bound reader class produces a named line, nobody-bound REFUSES before
the append with every arm reading, and a run where no send confirms costs the
command its exit code, because a silent success is indistinguishable from
delivery from where the author stands.

Every prover is injected or patched. `claim_status` walks a real lockfile tree
and `resolve_to_lead` reads the live agent registry, so reading either for real
would make these tests pass or fail on what else is running on the machine.
Tests always pass `rows` to `note_readers`, so no test reads this machine's
registry.
"""
from __future__ import annotations

from pathlib import Path
from types import SimpleNamespace

import pytest

from fno.backlog import note_notify
from fno.backlog.note_notify import NoteReaders, Refused, note_readers, readers_before_append
from tests.fixtures.graph_seed import seed_graph


def _holders(**by_node: str):
    return lambda node_id: by_node.get(node_id)


def _graph(tmp_path: Path, entries: list[dict]) -> Path:
    """Rows the way the store writes them: the typed api drops a row the
    model cannot parse, so seeds carry the stamped fields."""
    path = tmp_path / "graph.json"
    complete = []
    for e in entries:
        row = {"type": "feature", "priority": "p2", "status": "idea", **e}
        row.setdefault("title", e.get("id", "node"))
        row.setdefault("slug", e.get("id", "node"))
        if row["status"] == "done" and not row.get("completed_at"):
            row["completed_at"] = "2026-09-01T00:00:00Z"
        complete.append(row)
    seed_graph(path, complete)
    return path


def _row(name: str, sid: str | None = None, status: str = "live", node: str | None = None):
    return SimpleNamespace(
        name=name,
        harness_session_id=sid,
        cc_session_id=None,
        session_id=None,
        status=status,
        node=node,
    )


def _free(monkeypatch) -> None:
    monkeypatch.setattr(
        "fno.claims.core.claim_status", lambda key: {"state": "free", "holder": None}
    )


# --- who gets reached --------------------------------------------------------


def test_holder_then_lead_each_once_and_in_order() -> None:
    got = note_readers(
        {"id": "x-0d08", "parent": "x-16b7"},
        index={"x-16b7": {"id": "x-16b7"}},
        rows=[],
        holder_of=_holders(**{"x-0d08": "sess-worker"}),
        leads_of=lambda scope: ["lead-a", "lead-a"],
    )
    assert got.recipients == [
        ("sess-worker", "holder of x-0d08"),
        ("lead-a", "lead of x-16b7"),
    ]


def test_a_graph_session_reaches_a_worker_the_claim_misses(monkeypatch) -> None:
    """AC1-HP: locked_by_harness_session binds where the claim does not."""
    _free(monkeypatch)
    rows = [_row("t-d211-selfkill", sid="sess-live")]
    got = note_readers(
        {"id": "x-d211", "locked_by_harness_session": "sess-live"},
        index={},
        rows=rows,
        leads_of=lambda scope: [],
    )
    assert got.recipients == [
        ("t-d211-selfkill", "session bound to x-d211 (graph locked_by_harness_session)")
    ]
    assert "claim node:x-d211: free" in got.readings
    assert "graph locked_by_harness_session: sess-live -> t-d211-selfkill" in got.readings


def test_the_registry_arm_reaches_a_worker_no_graph_field_names(monkeypatch) -> None:
    """AC1-EDGE: a graph session_id naming no live row falls to the registry arm."""
    _free(monkeypatch)
    rows = [_row("bp-2e1f-role-slot", sid="other", node="x-2e1f")]
    got = note_readers(
        {"id": "x-2e1f", "session_id": "sess-gone"},
        index={},
        rows=rows,
        leads_of=lambda scope: [],
    )
    assert got.recipients == [("bp-2e1f-role-slot", "worker on x-2e1f (registry node)")]
    assert "graph session_id: sess-gone names no live row" in got.readings


def test_a_parentless_node_walks_to_its_project_lead() -> None:
    got = note_readers(
        {"id": "x-1b2c", "project": "fno"},
        index={},
        rows=[],
        holder_of=_holders(),
        leads_of=lambda scope: ["lead-fno-g5"] if scope == "fno" else [],
    )
    assert got.recipients == [("lead-fno-g5", "lead of fno (project)")]
    assert "role fno (project): lead-fno-g5" in got.readings


@pytest.mark.parametrize("state", ["stale"])
def test_an_unowned_claim_reaches_nobody(state: str, monkeypatch) -> None:
    monkeypatch.setattr(
        "fno.claims.core.claim_status",
        lambda key: {"state": state, "holder": "sess-dead"},
    )
    assert note_notify.claim_holder("x-0d08") is None


# --- the author is named, never mailed ---------------------------------------


def test_the_author_is_never_mailed_its_own_note() -> None:
    got = note_readers(
        {"id": "x-0d08", "parent": "x-16b7"},
        index={},
        rows=[],
        holder_of=_holders(**{"x-0d08": "sess-me"}),
        leads_of=lambda scope: ["sess-me"],
        self_session="sess-me",
    )
    assert got.recipients == []


# --- role holders resolve once, where the address is born ---------------------


class _Row:
    name = "t-ae54-worker"
    harness_session_id = "01a08dab-7d3a"
    cc_session_id = None
    session_id = None


def test_holder_agent_name_resolves_all_three_role_prefixes() -> None:
    from fno.claims.core import (
        BLUEPRINT_HOLDER_PREFIX,
        TARGET_SESSION_HOLDER_PREFIX,
        holder_agent_name,
    )

    assert holder_agent_name("spawn-handover:t-ae54-worker", [_Row()]) == "t-ae54-worker"
    assert (
        holder_agent_name(f"{TARGET_SESSION_HOLDER_PREFIX}01a08dab-7d3a", [_Row()])
        == "t-ae54-worker"
    )
    assert (
        holder_agent_name(f"{BLUEPRINT_HOLDER_PREFIX}01a08dab-7d3a", [_Row()])
        == "t-ae54-worker"
    )
    assert holder_agent_name(f"{BLUEPRINT_HOLDER_PREFIX}gone", []) is None
    assert holder_agent_name("spawn-handover:bp-gone", []) is None
    assert holder_agent_name("sess-plain-holder", []) == "sess-plain-holder"
    assert holder_agent_name(None, []) is None


# --- the sender is a resolvable handle, never a literal ----------------------


def _fake_machine_mail(monkeypatch, receipt: str) -> list[list[str]]:
    import subprocess
    from pathlib import Path

    import fno.rust_binary as rust_binary

    calls: list[list[str]] = []

    def fake_run(argv, **kwargs):
        calls.append(list(argv))
        return SimpleNamespace(returncode=0, stdout=receipt, stderr="")

    monkeypatch.setattr(rust_binary, "resolve_binary", lambda: Path("/fake/fno-agents"))
    monkeypatch.setattr(subprocess, "run", fake_run)
    return calls


def test_send_pointer_forwards_to_rust_machine_mail_with_session(monkeypatch) -> None:
    session = "a1535d0b88424e4dbcafd733b8defc9c"
    monkeypatch.setattr(note_notify, "own_session", lambda: session)
    calls = _fake_machine_mail(monkeypatch, "hosted msg-abc12345")

    assert note_notify.send_pointer("sess-worker", "body") == "hosted msg-abc12345"
    assert calls == [[
        "/fake/fno-agents", "machine-mail-send", "--arm", "note-pointer",
        "--timeout-secs", "30", "--to", "sess-worker", "--", "body",
    ]]


# --- the refusal: resolution runs before the append ---------------------------


def _forbid_append(monkeypatch) -> None:
    def no_append(*a, **k):
        raise AssertionError("append_progress_note must not run on a refusal")

    monkeypatch.setattr("fno.graph.store.append_progress_note", no_append)


def test_nobody_bound_refuses_with_every_arm_reading(tmp_path, monkeypatch) -> None:
    """AC4-HP: silence is the defect; the refusal names every arm it read."""
    _free(monkeypatch)
    monkeypatch.setattr(note_notify, "own_session", lambda: "sess-me")
    monkeypatch.setattr(note_notify, "promoted_over", lambda scope: [])
    monkeypatch.setattr("fno.agents.registry.load_registry", lambda: [])
    _forbid_append(monkeypatch)
    got = readers_before_append(
        "x-d211", _graph(tmp_path, [{"id": "x-d211", "project": "fno"}])
    )
    assert isinstance(got, Refused)
    assert got.exit_code == 3
    assert got.message == (
        "note refused: nobody bound to x-d211 would be told, so nothing was written.\n"
        "  claim node:x-d211: free\n"
        "  graph locked_by_harness_session: none\n"
        "  graph session_id: none\n"
        "  graph locked_by: none\n"
        "  registry: no live row names x-d211\n"
        "  role fno (project): vacant\n"
        "Write it anyway with --quiet, or find a reader with fno agents team "
        "and mail them by name."
    )


def test_an_unreadable_registry_refuses_before_the_append(tmp_path, monkeypatch) -> None:
    """AC4-ERR: a fault cannot prove anyone would be told, so nothing writes."""
    from fno.agents.registry import RegistryVersionError

    def boom():
        raise RegistryVersionError("registry at /tmp/r.json is malformed JSON")

    monkeypatch.setattr("fno.agents.registry.load_registry", boom)
    _forbid_append(monkeypatch)
    got = readers_before_append("x-d211", _graph(tmp_path, [{"id": "x-d211"}]))
    assert isinstance(got, Refused)
    assert got.exit_code == 3
    assert got.message.startswith(
        "note refused: could not read who is bound to x-d211 "
        "(registry at /tmp/r.json is malformed JSON), so nothing was written."
    )
    assert got.message.endswith(
        "Write it anyway with --quiet, or find a reader with fno agents team "
        "and mail them by name."
    )


def test_an_unknown_node_refuses_with_the_command_text(tmp_path) -> None:
    got = readers_before_append("x-ffff", _graph(tmp_path, [{"id": "x-0d08"}]))
    assert isinstance(got, Refused)
    assert got.message == "Error: no node resolves to 'x-ffff'"
    assert got.exit_code == 1


def test_a_wedged_send_is_bounded_and_reported_unconfirmed(monkeypatch) -> None:
    """A live inject can outlast any writer's patience; the note command cannot."""
    import threading

    monkeypatch.setattr(note_notify, "_SEND_TIMEOUT_SECONDS", 0.2)
    started = threading.Event()

    def wedge(address: str, body: str) -> str:
        started.set()
        threading.Event().wait(30)  # never returns within the bound
        return "hosted msg-never"

    monkeypatch.setattr(note_notify, "send_pointer", wedge)
    readers = NoteReaders("x-0d08", [("sess-worker", "holder of x-0d08")], None, [])
    receipts = note_notify.send_note(readers, "the finding")
    assert started.is_set()
    assert receipts == [
        ("notify UNCONFIRMED sess-worker (holder of x-0d08): no answer in 0s", True)
    ]


def test_no_confirmed_delivery_costs_the_exit_code(monkeypatch, capsys) -> None:
    """AC5-ERR: all receipts unconfirmed or failed, exit 4, summary on stderr."""
    def boom(address: str, body: str) -> str:
        raise RuntimeError("pair budget spent")

    monkeypatch.setattr(note_notify, "send_pointer", boom)
    readers = NoteReaders(
        "x-0d08",
        [("sess-a", "holder of x-0d08"), ("sess-b", "lead of x-16b7")],
        None,
        [],
    )
    assert note_notify.deliver(readers, "the finding", json_output=False) == 4
    err = capsys.readouterr().err
    assert err.count("notify FAILED") == 2
    assert err.strip().splitlines()[-1] == (
        "notify: x-0d08 is noted, but no reader confirmed delivery "
        "(0 UNCONFIRMED, 2 FAILED). An UNCONFIRMED send may still land, "
        "so check before you re-send."
    )


# --- the command: refuse before the append, deliver by default --------------------


def _stubbed_receipt(node_id: str) -> dict:
    """The native receipt shape the bridge relays verbatim."""
    return {
        "status": "ok",
        "routed": "state",
        "node_id": node_id,
        "revision": 1,
        "line": (
            f"noted {node_id}: revision 1, 11 chars\n"
            f"replaced nothing: {node_id} had no current state"
        ),
    }


def _run(monkeypatch, argv: list[str], readers=None, refused=None, send=None):
    """Run `fno backlog note` with resolution and the native write stubbed.

    `refused` short-circuits resolution; `readers` stands in for a resolved
    binding. The write stub records the node id it was handed.
    """
    from typer.testing import CliRunner

    from fno.graph import cli as graph_cli
    from fno.graph import note_cli as note_bridge

    monkeypatch.setattr(graph_cli, "_graph_path", lambda *a, **k: Path("graph.json"))
    written: list[str] = []

    def fake_write(node_id, text, *, quiet, session_id, graph_path, reads=None, kind=None):
        written.append(node_id)
        return 0, _stubbed_receipt(node_id)

    monkeypatch.setattr(note_bridge, "_write_state", fake_write)

    def fake_readers(task_id, graph_path):
        if refused is not None:
            return refused
        return readers

    monkeypatch.setattr(note_notify, "readers_before_append", fake_readers)
    if send is not None:
        monkeypatch.setattr(note_notify, "send_pointer", send)
    return CliRunner().invoke(graph_cli.cli, argv), written


def test_the_command_delivers_by_default(monkeypatch) -> None:
    readers = NoteReaders("x-0d08", [("sess-worker", "holder of x-0d08")], None, [])
    result, appended = _run(
        monkeypatch,
        ["note", "x-0d08", "the finding"],
        readers=readers,
        send=lambda a, b: "hosted msg-abc",
    )
    assert result.exit_code == 0
    assert appended == ["x-0d08"]
    assert "noted x-0d08: revision 1, 11 chars" in result.stdout
    assert "notified sess-worker" in result.stdout


def test_a_flag_shaped_body_refuses_before_the_write(monkeypatch) -> None:
    """`note <id> --list` is a mistyped read command, never a note body."""
    result, appended = _run(monkeypatch, ["note", "x-0d08", "--list", "--quiet"])
    assert result.exit_code != 0
    assert appended == []
    assert "--list" in result.stderr
    assert "fno backlog notes history x-0d08" in result.stderr


def test_quiet_writes_the_note_and_resolves_nobody(monkeypatch) -> None:
    """AC4-EDGE: --quiet skips resolution entirely and writes."""
    from typer.testing import CliRunner

    from fno.graph import cli as graph_cli
    from fno.graph import note_cli as note_bridge

    monkeypatch.setattr(graph_cli, "_graph_path", lambda *a, **k: Path("graph.json"))
    monkeypatch.setattr(
        note_bridge,
        "_write_state",
        lambda node_id, text, *, quiet, session_id, graph_path, reads=None, kind=None: (
            0,
            _stubbed_receipt(node_id),
        ),
    )

    def no_resolve(task_id, graph_path):
        raise AssertionError("the resolver must not run under --quiet")

    monkeypatch.setattr(note_notify, "readers_before_append", no_resolve)
    result = CliRunner().invoke(
        graph_cli.cli, ["note", "x-0d08", "the finding", "--quiet"]
    )
    assert result.exit_code == 0
    assert "noted x-0d08: revision 1, 11 chars" in result.stdout
    assert "notif" not in result.stdout


def _run_note_quiet(monkeypatch, node_id: str, live_rows: list[dict]):
    """Run `fno backlog note <id> <text> --quiet` with the archive and the
    live read stubbed, so the archive pre-check is the only store contact."""
    from typer.testing import CliRunner

    from fno.cli import app
    from fno.graph import cli as graph_cli
    from fno.graph import note_cli as note_bridge

    monkeypatch.setattr(graph_cli, "_graph_path", lambda *a, **k: Path("graph.json"))
    monkeypatch.setattr(
        "fno.graph.api.wire_rows", lambda *, path=None: list(live_rows)
    )
    monkeypatch.setattr(
        "fno.backlog.note_notify.archived_entry",
        lambda nid: {"id": node_id} if nid == node_id else None,
    )
    monkeypatch.setattr(
        note_bridge,
        "_write_state",
        lambda nid, text, *, quiet, session_id, graph_path, reads=None, kind=None: (
            0,
            _stubbed_receipt(nid),
        ),
    )
    return CliRunner().invoke(app, ["backlog", "note", node_id, "the finding", "--quiet"])


def test_a_live_node_with_an_archive_twin_still_notes(monkeypatch) -> None:
    """The x-12d1 deadlock: an archive row shares a live id, note refused and
    named an unarchive that reported nothing to restore. Live first, then the
    archive: a node the update door accepts, the note door accepts too."""
    result = _run_note_quiet(monkeypatch, "x-12d1", [{"id": "x-12d1", "type": "feature"}])
    assert result.exit_code == 0, result.stderr
    assert "noted x-12d1" in result.stdout
    assert "is archived" not in result.stderr


def test_an_archive_only_id_still_refuses_after_a_live_miss(monkeypatch) -> None:
    """The pre-check keeps its teeth: no live row, archive row present, the
    exact PR 1871 remedy stands."""
    result = _run_note_quiet(monkeypatch, "x-3a64", [])
    assert result.exit_code == 1
    assert (
        "Error: node x-3a64 is archived; run `fno backlog unarchive x-3a64`"
        " to restore it before updating."
    ) in result.stderr


def test_a_refusal_writes_nothing_and_exits_three(monkeypatch) -> None:
    def no_send(address: str, body: str) -> str:
        raise AssertionError("a refused note must not send")

    refused = Refused(
        "note refused: nobody bound to x-d211 would be told, so nothing was written.\n"
        "Write it anyway with --quiet, or find a reader with fno agents team "
        "and mail them by name.",
        3,
    )
    result, appended = _run(
        monkeypatch, ["note", "x-d211", "the finding"], refused=refused, send=no_send
    )
    assert result.exit_code == 3
    assert appended == []
    assert result.stdout == ""
    assert "note refused: nobody bound to x-d211" in result.stderr


def test_the_author_only_reader_prints_the_binding_line(monkeypatch) -> None:
    def no_send(address: str, body: str) -> str:
        raise AssertionError("nothing to send")

    readers = NoteReaders("x-a792", [], "lead of x-a792", [])
    result, _ = _run(
        monkeypatch, ["note", "x-a792", "the ruling"], readers=readers, send=no_send
    )
    assert result.exit_code == 0
    assert (
        "notify: you are the only reader bound to x-a792 (lead of x-a792)"
        in result.stdout
    )


def test_blocking_note_rides_ctx_args_straight_to_rust(monkeypatch) -> None:
    """The finding flags never touch the note machinery in Python: the bridge
    is a passthrough and Rust owns routing, delivery and the receipt."""
    from typer.testing import CliRunner

    from fno.graph import cli as graph_cli
    from fno.graph import note_cli as note_bridge

    monkeypatch.setattr(graph_cli, "_graph_path", lambda *a, **k: Path("graph.json"))
    calls: list[list[str]] = []

    class Proc:
        returncode = 0

    def fake_run(argv, check=False):
        calls.append(argv)
        return Proc()

    monkeypatch.setattr("fno.rust_binary.resolve_binary", lambda: "/fake/fno-agents")
    monkeypatch.setattr(note_bridge.subprocess, "run", fake_run)

    def must_not_run(*a, **k):
        raise AssertionError("the note machinery must not run for a blocking finding")

    monkeypatch.setattr("fno.backlog.note_notify.readers_before_append", must_not_run)
    monkeypatch.setattr(note_bridge, "_write_state", must_not_run)
    result = CliRunner().invoke(
        graph_cli.cli, ["note", "x-5a62", "the gate leak", "--blocking"]
    )
    assert result.exit_code == 0, result.output
    assert calls, "the native action must run"
    assert calls[0][1:3] == ["backlog", "note"]
    assert calls[0][-1] == "--blocking"
    assert "x-5a62" in calls[0] and "the gate leak" in calls[0], (
        "the positionals ride through verbatim"
    )


@pytest.mark.parametrize(
    "tail",
    [
        ["--resolve", "04aa7f57"],
    ],
)
def test_the_stop_hook_remedy_reaches_rust_as_a_resolve(monkeypatch, tail) -> None:
    """The remedy the open-finding gate prints (`fno backlog note --resolve
    <id>`) must reach the native parser as a flag. Typer leaves a leading
    unknown option in the positional slots, so it once read as a node id
    ("no node resolves to '--resolve'") and a held builder could not end its turn."""
    from typer.testing import CliRunner

    from fno.graph import cli as graph_cli
    from fno.graph import note_cli as note_bridge

    monkeypatch.setattr(graph_cli, "_graph_path", lambda *a, **k: Path("graph.json"))
    calls: list[list[str]] = []

    class Proc:
        returncode = 0

    def fake_run(argv, check=False):
        calls.append(argv)
        return Proc()

    monkeypatch.setattr("fno.rust_binary.resolve_binary", lambda: "/fake/fno-agents")
    monkeypatch.setattr(note_bridge.subprocess, "run", fake_run)
    result = CliRunner().invoke(graph_cli.cli, ["note", *tail])
    assert result.exit_code == 0, result.output
    assert calls, "the native action must run"
    argv = calls[0]
    assert argv[1:3] == ["backlog", "note"]
    assert argv[argv.index("--resolve") + 1] == "04aa7f57"
