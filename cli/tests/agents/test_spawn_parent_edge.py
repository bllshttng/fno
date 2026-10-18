"""Task 2.2: spawn-time parent edge (spawned_by_session/harness/cwd).

Also covers x-42c5's spawn_trigger field: the CAUSE of a spawn (distinct from
the parent-edge WHO above), ambient-captured from FNO_SPAWN_TRIGGER the same
way spawned_by_* is captured from the session env vars.

Marker precedence and the disagreement rules (mixed families, two ids of one
family) belong to the identity resolver and are pinned in
tests/unit/test_harness_identity.py. This file pins what the spawn does with
the resolver's answer: the row and the event carry it, the codex lane stamps
the row, and an empty marker set falls back to the process walk.
"""
from __future__ import annotations

import os

import pytest

from fno.paths_testing import use_tmpdir
from fno.agents import events as events_mod
from fno.agents.registry import (
    load_registry,
)


# ---------------------------------------------------------------------------
# Fixtures
# ---------------------------------------------------------------------------


@pytest.fixture(autouse=True)
def _clear_parent_markers(monkeypatch):
    for marker in (
        "CODEX_THREAD_ID",
        "CLAUDE_CODE_SESSION_ID",
        "CODEX_SESSION_ID",
        "GEMINI_SESSION_ID",
    ):
        monkeypatch.delenv(marker, raising=False)


@pytest.fixture
def workdir_claude(tmp_path, monkeypatch):
    """Isolated fno home with a fake claude binary."""
    from tests.agents._fake_claude import install_fake_claude
    use_tmpdir(monkeypatch, tmp_path)
    bin_dir = tmp_path / "bin"
    install_fake_claude(bin_dir)
    monkeypatch.setenv("PATH", str(bin_dir))
    return tmp_path


@pytest.fixture
def captured_emits(monkeypatch):
    """Capture spawn births as (kind, data) tuples.

    Births write the daemon envelope via emit_spawned (x-8cd5 Wave 6), not the
    flat events.emit, so this intercepts the spawn seam the ACs assert on.
    """
    calls: list[tuple[str, dict]] = []

    def _capture(**data):
        calls.append(("agent_spawned", data))

    monkeypatch.setattr(events_mod, "emit_spawned", _capture)
    return calls


# ---------------------------------------------------------------------------
# AC-HP: claude spawn with CLAUDE_CODE_SESSION_ID set
# ---------------------------------------------------------------------------


def test_spawn_records_parent_edge_claude(workdir_claude, captured_emits, monkeypatch):
    """AC-HP: claude spawn -> registry row and agent_spawned event carry parent edge."""
    monkeypatch.setenv("CLAUDE_CODE_SESSION_ID", "parent-session-abc123")
    monkeypatch.setenv("PWD", "/parent/working/dir")
    monkeypatch.delenv("CODEX_SESSION_ID", raising=False)
    monkeypatch.delenv("GEMINI_SESSION_ID", raising=False)
    monkeypatch.delenv("FNO_SPAWN_TRIGGER", raising=False)

    from fno.agents.cli import agents_app
    from typer.testing import CliRunner

    runner = CliRunner()
    result = runner.invoke(
        agents_app,
        ["spawn", "--name", "test-parent-edge", "-H", "claude", "do something", "--substrate", "thread"],
        catch_exceptions=False,
    )

    assert result.exit_code == 0, f"expected exit 0, got {result.exit_code}\noutput: {result.output}"

    # Registry row must carry all three fields.
    entries = load_registry()
    entry = next((e for e in entries if e.name == "test-parent-edge"), None)
    assert entry is not None, "registry row must exist after claude spawn"
    assert entry.spawned_by_session == "parent-session-abc123", (
        f"expected spawned_by_session='parent-session-abc123', got {entry.spawned_by_session!r}"
    )
    assert entry.spawned_by_harness == "claude", (
        f"expected spawned_by_harness='claude', got {entry.spawned_by_harness!r}"
    )
    assert entry.spawned_by_cwd == "/parent/working/dir", (
        f"expected spawned_by_cwd='/parent/working/dir', got {entry.spawned_by_cwd!r}"
    )
    # A session that asked names no automated cause.
    assert entry.spawn_trigger is None

    # agent_spawned event must carry all three fields.
    spawned_events = [(k, d) for k, d in captured_emits if k == "agent_spawned"]
    assert len(spawned_events) == 1, (
        f"expected exactly 1 agent_spawned event, got {len(spawned_events)}: {spawned_events}"
    )
    ev_data = spawned_events[0][1]
    assert ev_data.get("spawned_by_session") == "parent-session-abc123"
    assert ev_data.get("spawned_by_harness") == "claude"
    assert ev_data.get("spawned_by_cwd") == "/parent/working/dir"
    assert ev_data.get("name") == "test-parent-edge"


# ---------------------------------------------------------------------------
# x-42c5: spawn_trigger — the CAUSE of a spawn, distinct from the parent edge
# ---------------------------------------------------------------------------


def test_spawn_records_trigger_cause_when_dispatcher_sets_it(
    workdir_claude, captured_emits, monkeypatch
):
    """A dispatcher (e.g. think-spawn) sets FNO_SPAWN_TRIGGER before shelling
    out to `fno agents spawn`; the registry row it writes records the cause,
    answering "why was this spawned" as a field rather than a timestamp-gap
    inference against the filing node's created_at."""
    monkeypatch.setenv("CLAUDE_CODE_SESSION_ID", "parent-session-abc123")
    monkeypatch.setenv("FNO_SPAWN_TRIGGER", "think_spawn:work-start")

    from fno.agents.cli import agents_app
    from typer.testing import CliRunner

    runner = CliRunner()
    result = runner.invoke(
        agents_app,
        ["spawn", "--name", "test-spawn-trigger", "-H", "claude", "do something", "--substrate", "thread"],
        catch_exceptions=False,
    )

    assert result.exit_code == 0, f"exit {result.exit_code}\n{result.output}"

    entries = load_registry()
    entry = next((e for e in entries if e.name == "test-spawn-trigger"), None)
    assert entry is not None
    assert entry.spawn_trigger == "think_spawn:work-start"


def test_spawn_trigger_does_not_leak_into_this_process_environment(monkeypatch):
    """Code-review finding (x-42c5): _capture_spawn_trigger POPS the var, not just
    reads it. Left in place, it would ride into a spawned worker's own persistent
    environment via a later dict(os.environ) snapshot (bg_create builds the new
    worker's env that way, and scrub_ambient_identity does not know this marker),
    mislabeling that worker's OWN later spawns with this spawn's cause."""
    from fno.agents.dispatch import _capture_spawn_trigger

    monkeypatch.setenv("FNO_SPAWN_TRIGGER", "think_spawn:work-start")
    assert _capture_spawn_trigger() == "think_spawn:work-start"
    assert "FNO_SPAWN_TRIGGER" not in os.environ
    # A second read in the same process (e.g. a nested spawn) sees nothing left.
    assert _capture_spawn_trigger() is None


# --- x-132c: the gaps the fleet measurement found (0 of 30 rows stamped) ------

_PARENT_MARKERS = (
    "CODEX_THREAD_ID",
    "CLAUDE_CODE_SESSION_ID",
    "CODEX_SESSION_ID",
    "GEMINI_SESSION_ID",
    "OPENCODE_SESSION_ID",
)


def _only_marker(monkeypatch, var: str, value: str):
    """Clear every harness marker, then set exactly one, so the ambient
    resolver sees one unambiguous family regardless of the host session."""
    for m in _PARENT_MARKERS:
        monkeypatch.delenv(m, raising=False)
    monkeypatch.setenv(var, value)


def test_codex_create_path_stamps_the_row_not_just_the_event(
    workdir_claude, captured_emits, monkeypatch
):
    """The codex create path used to stamp the parent edge on the lifecycle
    EVENT only; the durable registry row - the surface every consumer reads -
    carried nothing. The row must name the same parent as the event, and a
    machine dispatch's cause, the same stamps the claude bg mint writes."""
    from unittest.mock import MagicMock

    from fno.agents.dispatch import DispatchAskResult, _codex_create_path
    from fno.agents.harnesses import codex as codex_mod
    from fno.agents.harnesses.codex import CodexResult

    class _Handle:
        def detach(self):
            pass

    monkeypatch.setattr(
        codex_mod,
        "create",
        MagicMock(return_value=CodexResult(
            exit_code=0, session_id="codex-sid-abc", last_msg="hello", duration_ms=42,
        )),
    )
    _only_marker(monkeypatch, "CODEX_SESSION_ID", "parent-cx-1")
    monkeypatch.setenv("FNO_SPAWN_TRIGGER", "dispatch:rd")
    result = _codex_create_path(
        name="codex-child",
        message="echo hello",
        cwd=os.getcwd(),
        from_name="fno",
        yolo=False,
        timeout_sec=10.0,
        lock_handle=_Handle(),
    )
    assert isinstance(result, DispatchAskResult)

    entries = load_registry()
    row = next(e for e in entries if e.name == "codex-child")
    assert row.spawned_by_session == "parent-cx-1"
    assert row.spawned_by_harness == "codex"
    assert row.spawned_by_cwd
    assert row.spawn_trigger == "dispatch:rd"


def test_register_adopted_row_stamps_the_adopting_session(tmp_path, monkeypatch):
    """An adopted row records the session that vouched for it on
    ``adopted_by_session`` (x-5283 LD3) and keeps ``spawned_by_*`` empty:
    vouching is not spawning, and the share is charged on the spawn edge."""
    from fno.agents.registry import register_existing_session

    _only_marker(monkeypatch, "CLAUDE_CODE_SESSION_ID", "adopter-session-1")
    entry = register_existing_session(
        provider="claude",
        session_id="found-orphan-id",
        cwd=str(tmp_path),
        origin="adopted",
        registry_path=tmp_path / "registry.json",
    )
    assert entry.spawned_by_session is None
    assert entry.spawned_by_harness is None
    assert entry.adopted_by_session == "adopter-session-1"


def test_register_self_registration_never_stamps_its_own_id(tmp_path, monkeypatch):
    """A caller that registers the session it runs in (a mail hold, any
    self-join) passes no operator origin; the identity guard must refuse the
    stamp anyway - a row naming itself as parent is a cycle the renderer has
    to defend against, and the registry can simply never write it."""
    from fno.agents.registry import register_existing_session

    _only_marker(monkeypatch, "CLAUDE_CODE_SESSION_ID", "own-session-id")
    for origin in (None, "adopted"):
        entry = register_existing_session(
            provider="claude",
            session_id="own-session-id",
            cwd=str(tmp_path),
            origin=origin,
            registry_path=tmp_path / f"reg-{origin}.json",
        )
        assert entry.spawned_by_session is None, f"origin={origin}"
        assert entry.spawned_by_cwd is None, f"origin={origin}"


# ---------------------------------------------------------------------------
# AC4-HP (x-5283): a birth with no recordable requester names WHY in the event
# ---------------------------------------------------------------------------


def test_spawn_with_no_requester_carries_lineage_reason_in_the_event(
    workdir_claude, captured_emits, monkeypatch
):
    """The event carries either a session id or a reason, never both empty.
    An absent spawned_by_session has three explanations and only one of them
    is "identity was genuinely ambiguous"; the positive marker is the reason
    string itself, captured at spawn time into the durable journal."""
    monkeypatch.delenv("OPENCODE_SESSION_ID", raising=False)
    monkeypatch.setenv("PWD", "/parent/working/dir")
    import fno.claims.session_pid as session_pid

    monkeypatch.setattr(session_pid, "resolve_session_harness", lambda: None)

    from fno.agents.cli import agents_app
    from typer.testing import CliRunner

    runner = CliRunner()
    result = runner.invoke(
        agents_app,
        ["spawn", "--name", "test-lineage-reason", "-H", "claude", "do something", "--substrate", "thread"],
        catch_exceptions=False,
    )

    assert result.exit_code == 0, f"expected exit 0, got {result.exit_code}\noutput: {result.output}"

    spawned_events = [(k, d) for k, d in captured_emits if k == "agent_spawned"]
    assert len(spawned_events) == 1, (
        f"expected exactly 1 agent_spawned event, got {len(spawned_events)}: {spawned_events}"
    )
    ev_data = spawned_events[0][1]
    assert ev_data.get("spawned_by_session") is None
    reason = ev_data.get("lineage_reason")
    assert isinstance(reason, str) and reason.strip(), (
        f"lineage_reason must be a non-empty reason, got {reason!r}"
    )
    assert "identity disposition=" in reason


# ---------------------------------------------------------------------------
# x-5c25: a null parent must say which of its three meanings it carries
# ---------------------------------------------------------------------------


def test_ac5_hp_marker_less_spawn_still_names_the_harness(monkeypatch):
    """AC5-HP: no marker in the env, but the process walk names claude.

    Measured on 2026-09-08: 24 registry rows carried spawned_by_cwd and no
    session, and every one of their agent_spawned events said
    "identity disposition=empty, markers=no markers". The row could not be told
    apart from a human-run spawn. The walk answers the family question when the
    id cannot be proved, so the harness half is recorded and the id stays null.
    """
    import fno.claims.self_identity as self_identity
    import fno.claims.session_pid as session_pid
    from fno.agents.dispatch import _capture_parent_edge

    class _Empty:
        harness = None
        session_id = None
        disposition = "empty"
        markers_present = ()

    monkeypatch.setattr(self_identity, "resolve_self_identity", lambda: _Empty())
    monkeypatch.setattr(session_pid, "resolve_session_harness", lambda: "claude")
    monkeypatch.setenv("PWD", "/parent/working/dir")

    assert _capture_parent_edge() == (None, "claude", "/parent/working/dir")


def test_a_present_marker_that_resolves_to_nothing_stays_nothing(monkeypatch):
    """The fallback is gated on an EMPTY marker set, not on a missing harness.
    A marker that IS present and resolved to nothing is the contradiction
    x-b57a / x-0992 rule must attribute nothing, and the walk must not
    overturn it."""
    import fno.claims.self_identity as self_identity
    import fno.claims.session_pid as session_pid
    from fno.agents.dispatch import _capture_parent_edge

    class _Contradicted:
        harness = None
        session_id = None
        disposition = "contradiction"
        markers_present = (("CODEX_THREAD_ID", "codex", "t-1"),)

    monkeypatch.setattr(self_identity, "resolve_self_identity", lambda: _Contradicted())
    monkeypatch.setattr(session_pid, "resolve_session_harness", lambda: "claude")
    monkeypatch.setenv("PWD", "/parent/working/dir")

    assert _capture_parent_edge() == (None, None, "/parent/working/dir")


def test_the_launch_edge_stamp_is_wired_into_the_spawn(workdir_claude, monkeypatch):
    """x-5c25 AC1-HP, wiring half: a real spawn that names a node reaches
    _stamp_launch_edge with that node. The helper's own behavior is covered in
    test_spawn_graph_launch_edge.py; this proves the call site exists."""
    import fno.agents.cli as agents_cli

    seen: list = []
    monkeypatch.setattr(agents_cli, "_stamp_launch_edge", seen.append)
    import fno.agents.mux_spawn as mux_spawn

    monkeypatch.setattr(
        mux_spawn, "resolve_provenance", lambda *a, **k: {"FNO_NODE": "x-1234"}
    )
    monkeypatch.setenv("CLAUDE_CODE_SESSION_ID", "parent-session-abc123")

    from typer.testing import CliRunner

    result = CliRunner().invoke(
        agents_cli.agents_app,
        ["spawn", "--name", "test-launch-wired", "-H", "claude", "--node", "x-1234",
         "/fix something", "--substrate", "thread"],
        catch_exceptions=False,
    )

    assert result.exit_code == 0, f"exit {result.exit_code}\n{result.output}"
    assert seen == ["x-1234"], f"expected the stamp to see the node, got {seen!r}"
@pytest.mark.dev_build
def test_resume_spawn_without_markers_inherits_the_source_rows_parent_edge(
    workdir_claude, captured_emits, monkeypatch
):
    """A relaunch (`spawn --resume`) keeps the SOURCE row's identity: the
    original spawner keeps the row in its orphan check even when a different
    session ran the relaunch, and the source fno_id rides along, so the
    ledger join never breaks."""
    from fno.agents.registry import AgentEntry, update_registry

    source_uuid = "11111111-2222-3333-4444-555555555555"
    update_registry(
        lambda entries: entries
        + [
            AgentEntry(
                name="source-row",
                harness="claude",
                cwd="/tmp",
                log_path="/tmp/source-row.log",
                harness_session_id=source_uuid,
                short_id="11111111",
                model="claude-sonnet-5-5",
                status="exited",
                fno_id="f-source-1111",
                spawned_by_session="original-spawner-1",
                spawned_by_harness="claude",
                spawned_by_cwd="/original/spawner/dir",
            )
        ]
    )

    # A DIFFERENT session runs the relaunch: its ambient marker names a
    # stranger as parent, and the carried edge must beat it.
    monkeypatch.setenv("CLAUDE_CODE_SESSION_ID", "22222222-abba-4aba-8aba-333333333333")
    monkeypatch.setenv("PWD", "/relaunch/runner/dir")

    from fno.agents.cli import agents_app
    from typer.testing import CliRunner

    result = CliRunner().invoke(
        agents_app,
        ["spawn", "--name", "relaunch-worker", "-H", "claude", "--here",
         "--resume", source_uuid, "--substrate", "thread", "continue"],
        catch_exceptions=False,
    )
    assert result.exit_code == 0, f"exit {result.exit_code}\n{result.output}"

    entries = load_registry()
    entry = next((e for e in entries if e.name == "relaunch-worker"), None)
    assert entry is not None, "registry row must exist after the relaunch"
    assert entry.spawned_by_session == "original-spawner-1", (
        f"expected the source row's spawner, got {entry.spawned_by_session!r}"
    )
    assert entry.spawned_by_harness == "claude"
    assert entry.spawned_by_cwd == "/original/spawner/dir"
    assert entry.fno_id == "f-source-1111", (
        f"expected the source row's fno_id, got {entry.fno_id!r}"
    )
