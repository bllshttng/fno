"""Integration tests for `fno backlog provenance <node-id>` (Task 2.3).

Uses the same tmp_graph fixture pattern as test_graph_cli.py.
The resolver's projects_root is injected via monkeypatch so no real
~/.claude is touched.
"""
from __future__ import annotations
from tests.fixtures.graph_seed import seed_graph

import json
from pathlib import Path

import pytest
from typer.testing import CliRunner

from fno.cli import app

runner = CliRunner()


# ---------------------------------------------------------------------------
# Fixtures
# ---------------------------------------------------------------------------

@pytest.fixture
def tmp_graph(tmp_path, monkeypatch) -> Path:
    """A fresh empty graph.json; monkeypatches fno.graph constants to use it."""
    g = tmp_path / "graph.json"
    seed_graph(g, '{"entries": []}\n')
    import fno.graph._constants as gc
    import fno.graph.store as gs
    monkeypatch.setattr(gc, "GRAPH_JSON", g)
    monkeypatch.setattr(gc, "GRAPH_MD", tmp_path / "graph.md")
    monkeypatch.setattr(gc, "GRAPH_HTML", tmp_path / "graph.html")
    monkeypatch.setattr(gc, "GRAPH_ARCHIVE_JSON", tmp_path / "graph-archive.json")
    monkeypatch.setattr(gs, "GRAPH_JSON", g)
    # Seam readers resolve fno.paths.graph_json at call time; pin the
    # resolver to the same hermetic file (module-attr pins do not reach it).
    monkeypatch.setattr("fno.paths.graph_json", lambda: g)
    return g


def _invoke(*args, input=None):
    return runner.invoke(app, list(args), input=input, catch_exceptions=False)


def _write_node(g: Path, node: dict) -> None:
    from fno.graph.store import read_graph_strict

    seed_graph(g, [*read_graph_strict(g), node])


def _store_state(g: Path):
    from fno.graph.store import read_graph_strict, store_export_status

    return read_graph_strict(g), store_export_status(g)


def _base_node(node_id: str, **overrides) -> dict:
    base = {
        "id": node_id,
        "title": "Test node",
        "status": "ready",
        "domain": "code",
        "project": "fno",
        "cwd": "/Users/bb16/code/footnote",
        "source_session_id": None,
        "source_harness": None,
        "source_node_id": None,
        "source_plan_path": None,
        "spawned_by_session": None,
        "spawned_by_harness": None,
        "spawned_by_cwd": None,
    }
    base.update(overrides)
    return base


# ---------------------------------------------------------------------------
# AC-HP: node-birth edge resolves via source_cwd
# ---------------------------------------------------------------------------


def test_node_birth_resolves_via_source_cwd_not_durable_cwd(tmp_graph, tmp_path, monkeypatch):
    """gemini review: a node filed from a worktree resolves via source_cwd.

    The node's durable `cwd` is the canonical project root; the transcript dir
    is slugged by the SESSION cwd (the worktree). Resolution must use source_cwd
    so the worktree case (the common mid-pipeline case) does not report not-found.
    """
    session_id = "abcd1234-0000-0000-0000-000000000000"
    session_cwd = "/Users/bb16/conductor/workspaces/footnote/wt-feature"  # worktree
    durable_cwd = "/Users/bb16/code/footnote"                              # canonical root

    slug = session_cwd.replace("/", "-").replace(".", "-")
    proj_dir = tmp_path / "projects" / slug
    proj_dir.mkdir(parents=True, exist_ok=True)
    transcript = proj_dir / f"{session_id}.jsonl"
    transcript.write_text('{"type":"summary"}\n')

    import fno.provenance.resolver as resolver_mod
    monkeypatch.setattr(resolver_mod, "_DEFAULT_PROJECTS_ROOT", tmp_path / "projects")

    node = _base_node(
        "ab-prov00wt",
        source_session_id=session_id,
        source_harness="claude",
        source_cwd=session_cwd,   # session/worktree cwd
        cwd=durable_cwd,          # durable project root (would resolve to nothing)
    )
    _write_node(tmp_graph, node)

    r = _invoke("backlog", "provenance", "ab-prov00wt")
    assert r.exit_code == 0, r.output
    assert str(transcript) in r.output  # resolved via source_cwd, not durable cwd


# ---------------------------------------------------------------------------
# AC-HP: spawned_by edge resolution uses spawned_by_cwd
# ---------------------------------------------------------------------------

def test_ac_hp_spawn_edge_uses_spawned_by_cwd(tmp_graph, tmp_path, monkeypatch):
    """AC-HP: spawned_by_session uses spawned_by_cwd for slug resolution."""
    session_id = "deadbeef-cafe-babe-0000-000000000001"
    spawn_cwd = "/Users/bb16/code/regready"

    slug = spawn_cwd.replace("/", "-").replace(".", "-")
    proj_dir = tmp_path / "projects" / slug
    proj_dir.mkdir(parents=True, exist_ok=True)
    transcript = proj_dir / f"{session_id}.jsonl"
    transcript.write_text("{}\n")
    projects_root = tmp_path / "projects"

    import fno.provenance.resolver as resolver_mod
    monkeypatch.setattr(resolver_mod, "_DEFAULT_PROJECTS_ROOT", projects_root)

    node = _base_node(
        "ab-prov0004",
        spawned_by_session=session_id,
        spawned_by_harness="claude",
        spawned_by_cwd=spawn_cwd,
    )
    _write_node(tmp_graph, node)

    r = _invoke("backlog", "provenance", "ab-prov0004")
    assert r.exit_code == 0, r.output
    assert session_id in r.output
    assert str(transcript) in r.output


# ---------------------------------------------------------------------------
# AC-EDGE: node not found -> non-zero exit
# ---------------------------------------------------------------------------

def test_ac_edge_node_not_found(tmp_graph):
    """AC-EDGE: unknown node id -> non-zero exit, no crash."""
    r = _invoke("backlog", "provenance", "ab-nosuchnode")
    assert r.exit_code != 0


# ---------------------------------------------------------------------------
# Read-only: no mutation to the graph store
# ---------------------------------------------------------------------------

def test_ac_verify_read_only(tmp_graph, tmp_path, monkeypatch):
    """AC-VERIFY: provenance command never mutates the graph store."""
    import fno.provenance.resolver as resolver_mod
    monkeypatch.setattr(resolver_mod, "_DEFAULT_PROJECTS_ROOT", tmp_path / "empty")

    node = _base_node("ab-prov0006", source_session_id="sid-xyz", source_harness="claude")
    _write_node(tmp_graph, node)

    before = _store_state(tmp_graph)
    _invoke("backlog", "provenance", "ab-prov0006")
    after = _store_state(tmp_graph)

    assert before == after


# ---------------------------------------------------------------------------
# Lifecycle `sessions` rows in provenance output
# ---------------------------------------------------------------------------

_SESSIONS = [
    {"phase": "think", "harness": "claude", "session_id": "S1", "at": "2026-07-12T01:00:00Z"},
    {"phase": "blueprint", "harness": "claude", "session_id": "S1", "at": "2026-07-12T02:00:00Z"},
    {"phase": "ship", "harness": "codex", "session_id": "S2", "at": "2026-07-12T03:00:00Z"},
]


def test_ac3_ui_lifecycle_rows_json(tmp_graph, tmp_path, monkeypatch):
    """AC3-UI (JSON): --json carries the sessions list in append order alongside edges."""
    import fno.provenance.resolver as resolver_mod
    monkeypatch.setattr(resolver_mod, "_DEFAULT_PROJECTS_ROOT", tmp_path / "empty")

    _write_node(tmp_graph, _base_node("ab-life0002", sessions=list(_SESSIONS)))

    r = _invoke("backlog", "provenance", "ab-life0002", "--json")
    assert r.exit_code == 0, r.output
    data = json.loads(r.output)
    # Store rows normalize sessions to the typed shape (started_at/ended_at
    # fields, nulls materialized), so compare the lifecycle identity, not
    # the raw seeded dicts.
    assert [
        (s["phase"], s["harness"], s["session_id"]) for s in data["sessions"]
    ] == [(s["phase"], s["harness"], s["session_id"]) for s in _SESSIONS]
    assert "edges" in data  # existing edges preserved


# ---------------------------------------------------------------------------
# One-call answer: the PR block + per-session registry liveness
# ---------------------------------------------------------------------------


def _pin_registry(monkeypatch, tmp_path, agents):
    """Point the registry reader at a hermetic file with the given agent rows.

    Every row gains the required-but-irrelevant cwd/log_path fields so the
    strict AgentEntry parse accepts the fixture."""
    filled = [
        {"cwd": "/tmp/wt", "log_path": "/tmp/wt/log", **a}
        for a in agents
    ]
    p = tmp_path / "registry.json"
    p.write_text(
        json.dumps({"schema_version": 19, "agents": filled}), encoding="utf-8"
    )
    monkeypatch.setattr("fno.paths.agents_registry_path", lambda: p)


def test_ac1_hp_pr_renders_human_and_json(tmp_graph, tmp_path, monkeypatch):
    """AC1-HP: an entry with pr_number/pr_url renders a pr: line and JSON keys."""
    import fno.provenance.resolver as resolver_mod
    monkeypatch.setattr(resolver_mod, "_DEFAULT_PROJECTS_ROOT", tmp_path / "empty")

    _write_node(tmp_graph, _base_node(
        "ab-prv00001",
        pr_number=1234,
        pr_url="https://github.com/bllshttng/fno/pull/1234",
    ))

    r = _invoke("backlog", "provenance", "ab-prv00001")
    assert r.exit_code == 0, r.output
    assert "pr: #1234" in r.output
    assert "https://github.com/bllshttng/fno/pull/1234" in r.output

    rj = _invoke("backlog", "provenance", "ab-prv00001", "--json")
    data = json.loads(rj.output)
    assert data["pr"]["number"] == 1234
    assert data["pr"]["url"] == "https://github.com/bllshttng/fno/pull/1234"


def test_ac3_hp_registry_status_live_and_reaped(tmp_graph, tmp_path, monkeypatch):
    """AC3-HP: a row whose session id has a registry entry renders that status;
    a row with no entry renders reaped."""
    import fno.provenance.resolver as resolver_mod
    monkeypatch.setattr(resolver_mod, "_DEFAULT_PROJECTS_ROOT", tmp_path / "empty")

    _pin_registry(monkeypatch, tmp_path, [
        {"name": "worker-live", "harness": "claude",
         "harness_session_id": "sess-live-0001", "status": "live"},
        {"name": "worker-done", "harness": "claude",
         "harness_session_id": "sess-exited-01", "status": "exited"},
    ])
    _write_node(tmp_graph, _base_node("ab-liv00001", sessions=[
        {"phase": "do", "harness": "claude", "session_id": "sess-live-0001",
         "started_at": "2026-09-14T01:00:00Z"},
        {"phase": "do", "harness": "claude", "session_id": "sess-exited-01",
         "started_at": "2026-09-14T02:00:00Z"},
        {"phase": "ship", "harness": "claude", "session_id": "sess-gone-0001",
         "started_at": "2026-09-14T03:00:00Z"},
    ]))

    r = _invoke("backlog", "provenance", "ab-liv00001")
    assert r.exit_code == 0, r.output
    assert "sess-live-0001" in r.output and "[live]" in r.output
    assert "sess-exited-01" in r.output and "[exited]" in r.output
    assert "sess-gone-0001" in r.output and "[reaped]" in r.output

    data = json.loads(_invoke("backlog", "provenance", "ab-liv00001", "--json").output)
    by_sid = {row["session_id"]: row.get("registry_status")
              for row in data["lifecycle"]["phases"] if row.get("recorded")}
    assert by_sid["sess-live-0001"] == "live"
    assert by_sid["sess-exited-01"] == "exited"
    assert by_sid["sess-gone-0001"] == "reaped"


def test_ac4_edge_registry_read_failure_omits_annotations(tmp_graph, tmp_path, monkeypatch):
    """AC4-EDGE: an unreadable registry omits liveness annotations, never fails."""
    import fno.provenance.resolver as resolver_mod
    monkeypatch.setattr(resolver_mod, "_DEFAULT_PROJECTS_ROOT", tmp_path / "empty")

    import fno.agents.registry as registry_mod
    def _boom():
        raise RuntimeError("registry wedged")
    monkeypatch.setattr(registry_mod, "load_registry", _boom)
    _write_node(tmp_graph, _base_node("ab-fail0001", sessions=[
        {"phase": "do", "harness": "claude", "session_id": "sess-fail-0001",
         "started_at": "2026-09-14T01:00:00Z"},
    ]))

    r = _invoke("backlog", "provenance", "ab-fail0001")
    assert r.exit_code == 0, r.output
    assert "sess-fail-0001" in r.output
    assert "[reaped]" not in r.output
    assert "[live]" not in r.output
