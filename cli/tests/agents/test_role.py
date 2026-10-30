"""US9 (KFAD squad team): orchestrator role visibility.

A role is stamped on the spawned worker's registry row by the SPAWN path
(grantor derived from the spawning session, never self-declared), survives a
round-trip, and surfaces in `fno whoami` and `fno agents list`.
"""
from __future__ import annotations
from tests.fixtures.graph_seed import seed_graph

import json
import subprocess
from dataclasses import asdict, replace
from pathlib import Path
from typing import Optional

import pytest

from fno.paths_testing import use_tmpdir


def _read_manifest(path) -> dict:
    """The manifest's fields through the native lead `read` op."""
    from fno.agents.role import lead_manifest_call

    return lead_manifest_call("read", path=str(path))["fields"]


def _write_manifest(path, **fields) -> None:
    """Write one manifest through the native spawn-team `manifest` op."""
    from fno.agents.spawn_overlay_client import spawn_overlay_call

    answer = spawn_overlay_call({"kind": "spawn-team", "op": "manifest", "path": str(path), **fields})
    assert not answer.get("error"), answer


# --- the stored scope encoding ----------------------------------------------
#
# There is no `--promote` spec parser any more. The flag takes scopes directly and
# the rung is derived (see crates/fno-agents/tests/role_ladder_parity.rs), so what remains to pin
# here is the ENCODING: a portfolio has to reduce to one canonical string,
# because the one-live-role guard compares scopes by equality and would
# otherwise miss a duplicate spelled in a different order.


def test_a_portfolio_canonicalizes_regardless_of_spelling() -> None:
    from fno.agents.role import canonical_scope, split_scope

    assert canonical_scope(["web", "etl"]) == canonical_scope(["etl", "web"])
    assert canonical_scope(["etl", "web", "etl"]) == "etl,web"
    assert canonical_scope([" etl ", "web"]) == "etl,web"
    assert split_scope(canonical_scope(["web", "etl"])) == ["etl", "web"]
    # A corrupted row's non-string scope degrades instead of crashing a read.
    assert split_scope(5) == []  # type: ignore[arg-type]


# --- spawn stamps the role, grantor is provenance not self-declared ---------


def _space_manifest(repo: Path, scope: str) -> Path:
    """Manifest path as the product resolves it for a spawn whose cwd is repo.

    The role writes key the space on the SPAWN cwd's canonical root, so a
    test sandbox (a non-git tmp dir) hashes its own path; read the manifest
    through the same resolution instead of a hand-built checkout path.
    """
    from fno.paths import space_dir

    return space_dir(repo) / "leads" / f"{scope}.md"


class _FakeRunner:
    def __init__(self) -> None:
        self.calls: list[list[str]] = []

    def __call__(self, argv, **kwargs):
        self.calls.append(list(argv))
        if argv[1:4] == ["mux", "pane", "run"]:
            return subprocess.CompletedProcess(argv, 0, "7\n", "")
        if argv[1:4] == ["mux", "pane", "ls"]:
            out = json.dumps(
                [{"pane_id": 7, "squad_id": 1, "tab_id": 1, "cwd": "/w", "child_pid": 4242}]
            )
            return subprocess.CompletedProcess(argv, 0, out, "")
        if argv[1:4] == ["mux", "pane", "wait"]:
            return subprocess.CompletedProcess(argv, 11, "", "")
        if argv[1:4] == ["mux", "pane", "read"]:
            return subprocess.CompletedProcess(argv, 0, "", "")
        raise AssertionError(f"unexpected invocation: {argv}")


def _spawn_promoted(monkeypatch, tmp_path, *, grantor_env: Optional[str], **role):
    use_tmpdir(monkeypatch, tmp_path)
    monkeypatch.delenv("FNO_SESSION", raising=False)
    for var in ("CODEX_SESSION_ID", "GEMINI_SESSION_ID"):
        monkeypatch.delenv(var, raising=False)
    if grantor_env is None:
        monkeypatch.delenv("CLAUDE_CODE_SESSION_ID", raising=False)
    else:
        monkeypatch.setenv("CLAUDE_CODE_SESSION_ID", grantor_env)
    if role.get("succession"):
        # The machine names successors: seal the pending draw so the typed
        # name counts as accepting it.
        from tests.agents._name_draw_seed import seed_pending_draw

        seed_pending_draw("Avery", role["role_scope"])

    from fno.agents.mux_spawn import dispatch_spawn_pane

    return dispatch_spawn_pane(
        name="Avery",
        message="term",
        provider="claude",
        cwd=tmp_path,
        runner=_FakeRunner(),
        **role,
    )


@pytest.mark.dev_build
def test_role_stamped_grantor_is_the_spawning_session(
    tmp_path: Path, monkeypatch, native_backlog_door
) -> None:
    from fno.agents.registry import AgentEntry, load_registry, update_registry
    import fno.agents.role as lead_state

    use_tmpdir(monkeypatch, tmp_path)
    monkeypatch.setattr(lead_state, "lead_loop_enabled", lambda: True)
    # Seat the grantor as a registered lead over epic-x; the spawn is then a
    # succession. An agent identity with no registry row is now refused at the
    # grantor check, so the agent must be seated - the corrected opposite of the
    # fail-open these tests rode. Reuse _spawn_promoted so the provider axis
    # binding stays on its baselined line rather than adding a new one inline.
    update_registry(
        lambda rows: rows
        + [
            AgentEntry(
                name="parent",
                cwd="/w",
                log_path="",
                harness="claude",
                harness_session_id="parent-sess-abc",
                short_id="parent",
                status="busy",
                role_level=1,
                role_scope="epic-x",
                role_grantor="human",
            )
        ]
    )
    _spawn_promoted(
        monkeypatch, tmp_path,
        grantor_env="parent-sess-abc",
        role_level=1, role_scope="epic-x", succession=True,
    )
    successor = next(e for e in load_registry() if e.name == "Avery")
    assert successor.role_level == 1
    assert successor.role_scope == "epic-x"
    # Provenance, not self-declared: the grantor is who actually spawned it.
    assert successor.role_grantor == "parent-sess-abc"
    assert successor.role_label == "L1 epic-x"
    manifest = _space_manifest(tmp_path, "epic-x")
    assert _read_manifest(manifest)["harness_session_id"] == successor.harness_session_id


def _role_row(name: str, *, status: str = "busy", scope="epic-x"):
    from fno.agents.registry import AgentEntry

    return AgentEntry(
        name=name,
        cwd="/w",
        log_path="",
        harness="claude",
        harness_session_id=f"{name}-sess",
        status=status,
        role_level=2 if scope else None,
        role_scope=scope,
        role_grantor="human" if scope else None,
    )


def test_settle_spawn_role_outcomes(tmp_path: Path, monkeypatch, native_backlog_door) -> None:
    """Exercise the Rust plan and apply stages against planted registry rows."""
    from dataclasses import replace

    from fno.agents.role import plan_spawn_role, settle_spawn_role
    from fno.agents.registry import update_registry
    from fno.paths_testing import use_tmpdir

    use_tmpdir(monkeypatch, tmp_path)

    def plan_for(rows, succession=False):
        update_registry(lambda _: rows)
        if succession:
            from tests.agents._name_draw_seed import seed_pending_draw

            seed_pending_draw("Avery", "epic-x")
        refusal, plan = plan_spawn_role("epic-x", None, succession, proposed_name="Avery" if succession else None)
        assert refusal is None
        assert plan is not None
        return plan

    unpromoted = _role_row("a", scope=None)
    granted_plan = plan_for([unpromoted])
    _, outcome, vacated = settle_spawn_role([unpromoted], scope="epic-x", plan=granted_plan)
    assert outcome == "granted"
    assert vacated == []

    caller = _role_row("caller")
    # The predecessor's live team: one child whose current owner names the
    # caller's session. Succession re-homes it in the same write that moves
    # the role; the birth edge stays history.
    child = replace(
        _role_row("w5", scope=None),
        spawned_by_session="caller-sess",
        spawn_provenance={
            "origin": {"kind": "session", "parent": {"harness": "claude", "session_id": "caller-sess", "cwd": "/w"}, "invocation": None},
            "owner": {"kind": "session", "harness": "claude", "session_id": "caller-sess", "cwd": "/w"},
        },
    )
    succeeded_plan = plan_for([caller, child], succession=True)
    rows, outcome, vacated = settle_spawn_role(
        [caller, child], scope="epic-x", plan=succeeded_plan,
        successor="Avery", successor_harness="codex", successor_session="successor-sess", successor_cwd="/w",
    )
    assert outcome == "succeeded"
    assert [r.role_scope for r in rows] == [None, None]
    assert [(r.name, cause) for r, cause in vacated] == [("caller", "succession"), ("w5", "reowned")]
    assert rows[1].spawn_provenance["owner"]["session_id"] == "successor-sess"
    assert rows[1].spawned_by_session == "caller-sess", "the birth edge stays history"

    # The race backstop: the plan was computed against "caller" holding the
    # scope, but the write sees "stranger" instead - declines rather than
    # applying a plan for a holder that is no longer there.
    stranger = _role_row("stranger")
    race_plan = plan_for([caller, child], succession=True)
    rows, outcome, vacated = settle_spawn_role(
        [stranger, child], scope="epic-x", plan=race_plan, successor="Avery", successor_harness="codex", successor_session="successor-sess", successor_cwd="/w",
    )
    assert outcome == "declined"
    assert rows[0].role_scope == "epic-x", "a declined spawn leaves the holder alone"

    # AC8: a take-over ask rides the payload as take_over.reason and the
    # plan answer carries it back with the acting caller and the old
    # holder's row fields at the moment of the move.
    alder = _role_row("alder")
    update_registry(lambda _: [alder])
    refusal, plan = plan_spawn_role("epic-x", None, False, proposed_name="Maya", take_over="Alder paused since 10:00")
    assert refusal is None
    assert plan is not None
    assert plan["outcome"] == "succeeded"
    assert plan["vacate"] == ["alder"]
    assert plan["take_over"]["by"] == "the user"
    assert plan["take_over"]["reason"] == "Alder paused since 10:00"
    assert [p["name"] for p in plan["take_over"]["predecessors"]] == ["alder"]
    assert plan["take_over"]["predecessors"][0]["harness_session_id"] == "alder-sess"
    assert rows[1].spawn_provenance["owner"]["session_id"] == "caller-sess", (
        "a declined succession re-homes nobody"
    )
    assert vacated == []

    dead = _role_row("dead", status="exited")
    granted_plan = plan_for([dead])
    rows, outcome, vacated = settle_spawn_role([dead], scope="epic-x", plan=granted_plan)
    assert outcome == "granted"
    assert [(r.name, cause) for r, cause in vacated] == [("dead", "holder_terminal")]
    assert rows[0].role_scope is None

    rebound_plan = plan_for([caller, child], succession=True)
    rebound = replace(caller, harness_session_id="caller-sess-2")
    rows, outcome, vacated = settle_spawn_role(
        [rebound, child], scope="epic-x", plan=rebound_plan, successor="Avery", successor_harness="codex", successor_session="successor-sess", successor_cwd="/w",
    )
    assert outcome == "declined"
    assert rows[0].role_scope == "epic-x"
    assert vacated == []


def test_settle_spawn_role_declines_when_rust_is_unavailable(
    tmp_path: Path, monkeypatch, native_backlog_door,
) -> None:

    from fno.agents import spawn_overlay_client
    from fno.agents.role import plan_spawn_role, settle_spawn_role
    from fno.agents.registry import update_registry
    from fno.paths_testing import use_tmpdir

    use_tmpdir(monkeypatch, tmp_path)
    caller = _role_row("caller")
    update_registry(lambda _: [caller])
    from tests.agents._name_draw_seed import seed_pending_draw

    seed_pending_draw("Avery", "epic-x")
    refusal, plan = plan_spawn_role("epic-x", None, True, proposed_name="Avery")
    assert refusal is None
    assert plan is not None
    before = asdict(caller)

    def unavailable(*args, **kwargs):
        raise spawn_overlay_client.SpawnOverlayUnavailable("not built")

    monkeypatch.setattr(spawn_overlay_client, "spawn_overlay_call", unavailable)
    rows, outcome, vacated = settle_spawn_role([caller], scope="epic-x", plan=plan)
    assert (outcome, vacated) == ("declined", [])
    assert asdict(rows[0]) == before


# --- registry round-trip (write -> read preserves the role) -----------------


# --- whoami surfaces the role -----------------------------------------------


# --- list marks promoted rows -------------------------------------------------


def test_list_serialize_marks_the_role() -> None:
    from fno.agents.format import serialize_entry
    from fno.agents.registry import AgentEntry

    promoted = AgentEntry(
        name="lead-epic", cwd="/w", log_path="", harness="claude", short_id="a",
        role_level=1, role_scope="epic-x", role_grantor="human",
    )
    plain = AgentEntry(name="worker", cwd="/w", log_path="", harness="claude", short_id="b")

    js = serialize_entry(promoted, None)
    assert js["role"] == "L1 epic-x"
    assert js["role_level"] == 1 and js["role_grantor"] == "human"
    assert serialize_entry(plain, None)["role"] is None


# --- attended in-place role promotion --------------------------------------


def _entry(name: str, **kw):
    from fno.agents.registry import AgentEntry
    harness = kw.pop("harness", "claude")
    return AgentEntry(name=name, cwd="/w", log_path="", harness=harness, **kw)


def _seed(monkeypatch, tmp_path, rows) -> None:
    use_tmpdir(monkeypatch, tmp_path)
    from fno.agents.registry import write_registry
    write_registry(rows)


def _prepare_role_cli(monkeypatch, tmp_path, rows) -> None:
    from fno.harness_identity import AMBIENT_IDENTITY_ENV

    # The lock-time identity match runs through Rust's role-identity kind
    # (the lock-time identity match runs through Rust now), so pin this
    # checkout's dev build like native_backlog_door
    # does; the smoke pytest legs skip by design when it has none.
    from fno.rust_binary import find_dev_binary

    binary = find_dev_binary()
    if binary is None:
        pytest.skip("no fno-agents dev build (cargo build -p fno-agents)")
    monkeypatch.setenv("FNO_AGENTS_BIN", str(binary))
    # The binary-side kinds that read the agents home (team-rescope) must
    # never resolve the ambient fleet store from a test.
    monkeypatch.setenv("FNO_AGENTS_HOME", str(tmp_path / ".agents-home"))
    _seed(monkeypatch, tmp_path, [replace(row, cwd=str(tmp_path)) for row in rows])
    for name in AMBIENT_IDENTITY_ENV:
        monkeypatch.delenv(name, raising=False)
    # The project resolver is native (the project-family port): _canonical_project
    # answers through one fno-agents round trip, so the fixture registry
    # rides a stub binary pinned over FNO_AGENTS_BIN (env outranks the dev
    # build) instead of a pinned settings path. $4 is the name to resolve;
    # every other spawn-overlay shape execs the real binary (the ladder
    # door rides the same env pin).
    stub = tmp_path / "agents-stub"
    stub.write_text(
        "#!/bin/sh\n"
        'if [ "$2" = "project" ]; then\n'
        '  if [ "$3" = "map" ]; then\n'
        "    echo '{\"alpha\":\"alpha\",\"a\":\"alpha\",\"beta\":\"beta\"}'\n"
        "    exit 0\n"
        "  fi\n"
        '  case "$4" in\n'
        "    alpha|a) echo alpha; exit 0;;\n"
        "    beta) echo beta; exit 0;;\n"
        "    *) exit 1;;\n"
        "  esac\n"
        "fi\n"
        f'exec "{binary}" "$@"\n',
        encoding="utf-8",
    )
    stub.chmod(0o755)
    monkeypatch.setenv("FNO_AGENTS_BIN", str(stub))



WIDEN_LEAD_SESSION = "aaaaaaaa-1111-4aaa-8aaa-aaaaaaaaaaaa"
WIDEN_OTHER_SESSION = "bbbbbbbb-2222-4bbb-8bbb-bbbbbbbbbbbb"



def test_spawn_role_refuses_before_launch_when_scope_already_occupied(
    tmp_path: Path, monkeypatch, native_backlog_door
) -> None:
    """A role-bearing spawn at an already-occupied scope, with no --succeed,
    refuses BEFORE launch rather than spawning unpromoted: launching a successor
    that holds no role is exactly the failure the pre-launch occupancy check
    exists to prevent."""
    from fno.agents.dispatch import DispatchAskError
    from fno.agents.registry import AgentEntry, load_registry, write_registry

    use_tmpdir(monkeypatch, tmp_path)
    # Pre-seed an existing promoted row over scope "epic-x"
    write_registry([AgentEntry(
        name="incumbent", harness="claude", cwd="/w", log_path="",
        harness_session_id="sess-incumbent",  # x-7bcd: needs a resolvable handle
        short_id="inc", status="live",
        role_level=1, role_scope="epic-x", role_grantor="human",
    )])
    # Spawn a new worker with --promote level=1,scope=epic-x (same scope). The
    # caller is an attended human (no agent identity), so it is authorized to
    # attempt the grant; the pre-launch check still refuses because the
    # incumbent holds the scope and no --succeed named the transfer.
    with pytest.raises(DispatchAskError, match="--hand-off"):
        _spawn_promoted(
            monkeypatch, tmp_path,
            grantor_env=None,
            role_level=1, role_scope="epic-x",
        )
    rows = load_registry()
    assert not [r for r in rows if r.name == "Avery"], "a refused role must launch nothing"
    # The incumbent's role is untouched
    inc = next(r for r in rows if r.name == "incumbent")
    assert inc.role_level == 1
    assert inc.role_scope == "epic-x"
    assert not _space_manifest(tmp_path, "epic-x").exists()


# --- the role types the command (x-7b36 change 11): raw mail names the delivery



# --- a role grant arms the epic's mission -----------------------------------
#
# Operator rule 2026-09-09: every epic with an owner is a mission. The drain
# keys one loop per epic with mission_active=true, so a role that left the
# flag unset ruled a territory no drain loop could see.


def _seed_role_graph(monkeypatch, tmp_path: Path, epics: list[dict]) -> None:
    # Pin the events journal into the tmp root: use_tmpdir redirects
    # state_dir but not project_events_json, and an unpinned journal is
    # shared by every test in the process.
    monkeypatch.setenv(
        "FNO_EVENTS_PATH", str(tmp_path / ".fno" / "events.jsonl")
    )
    from fno.paths import graph_json

    graph_path = graph_json()
    graph_path.parent.mkdir(parents=True, exist_ok=True)
    seed_graph(graph_path, json.dumps({"entries": epics}))


def _graph_entries() -> list[dict]:
    from fno.graph.store import read_graph_strict
    from fno.paths import graph_json

    return read_graph_strict(graph_json())


def _mission_events() -> list[dict]:
    """The journal's mission_activated rows (the pinned journal also carries
    agent_* rows from the role telemetry, which share the tmp root)."""
    from fno.paths import project_events_json

    from tests._event_rows import event_rows

    return [
        e for e in event_rows(project_events_json())
        if e.get("type") == "mission_activated"
    ]


def test_a_spawn_grant_over_an_unflagged_epic_arms_its_mission(
    tmp_path: Path, monkeypatch
) -> None:
    from fno.agents.role import journal_spawn_role

    _prepare_role_cli(monkeypatch, tmp_path, [])
    _seed_role_graph(
        monkeypatch,
        tmp_path,
        [
            {"id": "e-1", "type": "epic", "project": "alpha", "status": "in_progress"},
            {"id": "c-1", "type": "task", "parent": "e-1"},
        ],
    )

    journal_spawn_role(
        "granted", [], name="w", level=2, scope="e-1", grantor="human"
    )

    entry = _graph_entries()[0]
    assert entry["mission_active"] is True
    events = [
        e for e in _mission_events() if e.get("type") == "mission_activated"
    ]
    assert len(events) == 1
    assert events[0]["data"] == {"epic_id": "e-1", "source": "role"}


# --- the manifest arms when the successor identifies --------------------------
#
# Spawn-time succession carries the role in the registry while the successor
# child still has no harness session id, so the spawn lane's arm attempt
# refuses and the manifest keeps naming the stepping_down session. The
# SessionStart observation is the first moment the successor is addressable;
# that is where the manifest must rewrite.

SUCCESSOR_ID = "22222222-2222-4222-8222-222222222222"


def _promoted_row(tmp_path, monkeypatch, name="successor", **kw):
    from fno.agents.registry import AgentEntry, write_registry
    from fno.paths_testing import use_tmpdir

    use_tmpdir(monkeypatch, tmp_path)
    write_registry(
        [
            AgentEntry(
                name=name,
                cwd=str(tmp_path),
                log_path=str(tmp_path / f"{name}.log"),
                harness="claude",
                short_id=kw.pop("short_id", "deadbeef"),
                role_level=2,
                role_scope="x-epic",
                **kw,
            )
        ]
    )


def _arm_enabled(monkeypatch):
    import fno.agents.role as lead_state

    monkeypatch.setattr(lead_state, "lead_loop_enabled", lambda: True)


def _scope_manifest(tmp_path):
    from fno.paths import space_dir

    return space_dir(tmp_path) / "leads" / "x-epic.md"


@pytest.mark.dev_build
def test_spawn_succession_arms_the_manifest_when_the_successor_identifies(
    tmp_path, monkeypatch
) -> None:
    from fno.agents.registry import record_session_observation

    _promoted_row(tmp_path, monkeypatch, harness_session_id="")
    _arm_enabled(monkeypatch)

    entry, outcome = record_session_observation(
        name="successor", harness="claude", session_id=SUCCESSOR_ID
    )

    assert outcome == "primary"
    manifest = _scope_manifest(tmp_path)
    assert manifest.exists(), "the successor's first id must arm the wake gate"
    assert _read_manifest(manifest)["harness_session_id"] == SUCCESSOR_ID


