"""US9 (KFAD squad team): orchestrator role visibility.

A role is stamped on the spawned worker's registry row by the SPAWN path
(grantor derived from the spawning session, never self-declared), survives a
round-trip, and surfaces in `fno whoami` and `fno agents list`.
"""
from __future__ import annotations
from tests.fixtures.graph_seed import seed_graph

import json
import subprocess
from dataclasses import replace
from pathlib import Path

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
    from fno.paths import graph_json
    from fno.rust_binary import backlog_rows

    return backlog_rows(include_archived=True, path=graph_json())


def _mission_events() -> list[dict]:
    """The journal's mission_activated rows (the pinned journal also carries
    agent_* rows from the role telemetry, which share the tmp root)."""
    from fno.paths import project_events_json

    from tests._event_rows import event_rows

    return [
        e for e in event_rows(project_events_json())
        if e.get("type") == "mission_activated"
    ]


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


