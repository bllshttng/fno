"""Integration tests for graph collision detection.

Covers:
- ``parse_files_to_modify`` for quick plans
- ``fno backlog supersede`` CLI command
- ``fno backlog triage health`` aggregate report
- ``superseded`` ``status`` derivation

The severity-scoring, threshold-loading and acknowledged-reconciliation
engine these sections once exercised is retired: its Rust owner is
``crates/fno-agents/src/backlog/collision.rs``, and the graph CLI's
collisions sub-app and triage health went native before the engine died.
"""
from __future__ import annotations
from tests.fixtures.graph_seed import seed_graph

import json
from pathlib import Path

import pytest
from typer.testing import CliRunner

from fno.cli import app

runner = CliRunner()

# supersede/unsupersede/defer are transports over the native patch
# door, so the module needs this checkout's fno-agents build pinned.
pytestmark = pytest.mark.usefixtures("native_backlog_door")


@pytest.fixture
def tmp_graph(tmp_path, monkeypatch) -> Path:
    """Fresh empty graph.json routed to tmp_path.

    Mirrors the fixture pattern used by test_defer.py so all the constants
    that point at ``~/.fno/graph.json`` redirect into the test sandbox.
    """
    g = tmp_path / "graph.json"
    seed_graph(g, '{"entries": []}\n')
    import fno.graph._constants as gc
    import fno.graph.store as gs

    monkeypatch.setattr(gc, "GRAPH_JSON", g)
    monkeypatch.setattr(gc, "GRAPH_MD", tmp_path / "graph.md")
    monkeypatch.setattr(gc, "GRAPH_ARCHIVE_JSON", tmp_path / "graph-archive.json")
    monkeypatch.setattr(gs, "GRAPH_JSON", g)
    # Seam readers resolve fno.paths.graph_json at call time; pin the
    # resolver to the same hermetic file (module-attr pins do not reach it).
    monkeypatch.setattr("fno.paths.graph_json", lambda: g)
    # The native triage door is a child process; it reads the store root from env.
    monkeypatch.setenv("FNO_STATE_DIR", str(tmp_path))
    return g


def _invoke(*args, input=None):
    return runner.invoke(app, list(args), input=input, catch_exceptions=False)


class _NativeReceipt:
    """The CliRunner face of a native run: exit_code plus merged streams."""

    def __init__(self, code: int, out: str):
        self.exit_code = code
        self.output = out


def _native_supersede(g: Path, *args: str) -> _NativeReceipt:
    """The supersede leaf answers natively; drive the dev binary over the
    same store the fixture seeded."""
    import os as _os
    import subprocess as _sp

    from fno.rust_binary import find_dev_binary, resolve_binary

    binary = find_dev_binary() or resolve_binary()
    if binary is None:
        pytest.skip("no fno-agents dev build (cargo build -p fno-agents)")
    proc = _sp.run(
        [str(binary), "backlog", "supersede", *args],
        capture_output=True,
        text=True,
        timeout=60,
        env={
            "PATH": _os.environ["PATH"],
            "HOME": str(g.parent),
            "FNO_STATE_DIR": str(g.parent),
            "FNO_TRACKER_BACKEND": "graph",
        },
        cwd=str(g.parent),
    )
    return _NativeReceipt(proc.returncode, proc.stdout + proc.stderr)


def _read_entries(g: Path) -> list[dict]:
    # The store owns state; graph.json is a frozen export, so read-backs
    # come from store rows.
    from fno.graph.store import read_graph_strict

    return read_graph_strict(g)


def _write_quick_plan(path: Path, files: list[str], title: str = "Test plan") -> Path:
    """Write a single-file quick plan with a Files to Modify table."""
    rows = "\n".join(f"| `{f}` | edit |" for f in files)
    path.write_text(
        f"""---
created: 2026-04-27
scope: feature
domain: code
---

# {title}

## Context

Some context.

## Files to Modify

| File | Action |
|------|--------|
{rows}
"""
    )
    return path


def _seed_node(graph: list[dict], *, id_: str, plan_path: str, created_at: str = "2026-04-27T00:00:00+00:00") -> dict:
    node = {
        "id": id_,
        "parent": None,
        "title": "seed node",
        "type": "feature",
        "project": "fno",
        "cwd": "/repo",
        "priority": "p2",
        "domain": "code",
        "blocked_by": [],
        "session_id": None,
        "claimed_at": None,
        "completed_at": None,
        "status": "ready",
        "has_brief": False,
        "compacted": False,
        "plan_path": plan_path,
        "pr_number": None,
        "pr_url": None,
        "merge_status": None,
        "created_at": created_at,
    }
    graph.append(node)
    return node


# ---------------------------------------------------------------------------
# parse_files_to_modify
# ---------------------------------------------------------------------------


def test_parse_strips_parentheticals_and_line_suffixes(tmp_path):
    from fno.graph.collision import parse_files_to_modify

    plan = tmp_path / "p.md"
    plan.write_text(
        """# t

## Files to Modify

| File | Action |
|------|--------|
| `path/to/file.py:42` | edit |
| `~/.fno/settings.yaml` (template) | edit |
| `cli/src/fno/setup/` | edit |
"""
    )
    out = parse_files_to_modify(plan)
    assert "path/to/file.py" in out
    assert "~/.fno/settings.yaml" in out
    assert "cli/src/fno/setup/" in out
    # Line suffix should be stripped, not preserved
    assert "path/to/file.py:42" not in out


# ---------------------------------------------------------------------------
# CLI: supersede
# ---------------------------------------------------------------------------


def test_supersede_requires_cause_and_surface_before_mutation(tmp_graph, tmp_path):
    entries = _read_entries(tmp_graph)
    _seed_node(entries, id_="ab-old", plan_path=str(_write_quick_plan(tmp_path / "old.md", ["x.py"])))
    _seed_node(entries, id_="ab-new", plan_path=str(_write_quick_plan(tmp_path / "new.md", ["x.py"])))
    seed_graph(tmp_graph, json.dumps({"entries": entries}, indent=2))

    result = _native_supersede(
        tmp_graph, "ab-new", "--replaces", "ab-old",
        "--reason", "consolidated",
    )

    assert result.exit_code != 0
    assert "cause" in result.output.lower()
    assert {e["id"]: e for e in _read_entries(tmp_graph)}["ab-old"].get(
        "superseded_by"
    ) is None


def test_supersede_records_pending_evidence_and_terminals_status(tmp_graph, tmp_path):
    """The supersede edge is the terminal fact for status (x-e8f3): the
    evidence record is still written unverified, but the old row persists
    superseded instead of reading as live held work."""
    entries = _read_entries(tmp_graph)
    _seed_node(entries, id_="ab-old", plan_path=str(_write_quick_plan(tmp_path / "old.md", ["src/old.py"])))
    _seed_node(entries, id_="ab-new", plan_path=str(_write_quick_plan(tmp_path / "new.md", ["src/new.py"])))
    seed_graph(tmp_graph, json.dumps({"entries": entries}, indent=2))

    result = _native_supersede(
        tmp_graph, "ab-new", "--replaces", "ab-old",
        "--cause", "old implementation replaced", "--surface", "src/old.py",
        "--reason", "rewritten", "--covers", "ab-new replaces src/old.py whole",
    )

    assert result.exit_code == 0, result.output
    old = {e["id"]: e for e in _read_entries(tmp_graph)}["ab-old"]
    assert old["superseded_by"] == "ab-new"
    # Store rows omit null fields, so the empty slots read as absent keys.
    assert old["supersession"] == {
        "successor": "ab-new",
        "cause": "old implementation replaced",
        "reason": "rewritten; covers: ab-new replaces src/old.py whole",
        "surfaces": ["src/old.py"],
        "matched_surfaces": [],
    }
    assert old["supersession"].get("verified_at") is None
    assert old["supersession"].get("evidence_pr") is None
    assert old.get("deferred_at") is None
    assert old["status"] == "superseded"


def test_supersede_done_node_rejected(tmp_graph, tmp_path):
    """Superseding a shipped node would erase its completed_at and destroy
    forensic history. Refuse the mutation; user opens a follow-up instead."""
    entries = _read_entries(tmp_graph)
    _seed_node(entries, id_="ab-shipped", plan_path=str(_write_quick_plan(tmp_path / "old.md", ["x.py"])))
    _seed_node(entries, id_="ab-new", plan_path=str(_write_quick_plan(tmp_path / "new.md", ["y.py"])))
    entries[0]["completed_at"] = "2026-04-30T12:00:00+00:00"
    entries[0]["status"] = "done"
    seed_graph(tmp_graph, json.dumps({"entries": entries}, indent=2))

    res = _native_supersede(tmp_graph, "ab-new", "--replaces", "ab-shipped", "--cause", "test", "--surface", "x.py")
    assert res.exit_code != 0
    assert "already shipped" in res.output.lower() or "status=done" in res.output

    # Confirm no mutation happened: completed_at preserved, no superseded_by written
    entries = _read_entries(tmp_graph)
    by_id = {e["id"]: e for e in entries}
    assert by_id["ab-shipped"]["completed_at"] == "2026-04-30T12:00:00+00:00"
    assert by_id["ab-shipped"].get("superseded_by") is None


def test_supersede_already_superseded_rejected(tmp_graph, tmp_path):
    """Chaining supersede on an already-superseded node would corrupt the
    chain. Refuse and ask the user to resolve the existing chain."""
    entries = _read_entries(tmp_graph)
    _seed_node(entries, id_="ab-old", plan_path=str(_write_quick_plan(tmp_path / "old.md", ["x.py"])))
    _seed_node(entries, id_="ab-mid", plan_path=str(_write_quick_plan(tmp_path / "mid.md", ["y.py"])))
    _seed_node(entries, id_="ab-new", plan_path=str(_write_quick_plan(tmp_path / "new.md", ["z.py"])))
    entries[0]["superseded_by"] = "ab-mid"
    seed_graph(tmp_graph, json.dumps({"entries": entries}, indent=2))

    res = _native_supersede(tmp_graph, "ab-new", "--replaces", "ab-old", "--cause", "test", "--surface", "x.py")
    assert res.exit_code != 0
    assert "already superseded" in res.output.lower()


def test_supersede_self_rejected(tmp_graph, tmp_path):
    entries = _read_entries(tmp_graph)
    _seed_node(entries, id_="ab-x", plan_path=str(_write_quick_plan(tmp_path / "x.md", ["x.py"])))
    seed_graph(tmp_graph, json.dumps({"entries": entries}, indent=2))

    res = _native_supersede(tmp_graph, "ab-x", "--replaces", "ab-x", "--cause", "test", "--surface", "x.py")
    assert res.exit_code != 0
    assert "supersede self" in res.output


def test_supersede_live_children_rejected(tmp_graph, tmp_path):
    """Superseding a unit with live children strands them under a dead unit
    (dead-ancestor guard + inherited priority). Refuse, name the children,
    and leave the graph untouched. Gates on liveness, not type."""
    entries = _read_entries(tmp_graph)
    _seed_node(entries, id_="ab-old", plan_path=str(_write_quick_plan(tmp_path / "old.md", ["x.py"])))
    _seed_node(entries, id_="ab-new", plan_path=str(_write_quick_plan(tmp_path / "new.md", ["y.py"])))
    for kid in ("ab-k1", "ab-k2"):
        node = _seed_node(entries, id_=kid, plan_path=str(_write_quick_plan(tmp_path / f"{kid}.md", ["z.py"])))
        node["parent"] = "ab-old"
    seed_graph(tmp_graph, json.dumps({"entries": entries}, indent=2))

    res = _native_supersede(tmp_graph, "ab-new", "--replaces", "ab-old", "--cause", "fold", "--surface", "x.py")
    assert res.exit_code != 0
    assert "live child" in res.output.lower()
    assert "ab-k1" in res.output and "ab-k2" in res.output
    assert "--force" in res.output

    # No mutation happened.
    entries = _read_entries(tmp_graph)
    by_id = {e["id"]: e for e in entries}
    assert by_id["ab-old"].get("superseded_by") is None
    assert by_id["ab-k1"]["parent"] == "ab-old"


def test_supersede_force_orphans_live_children(tmp_graph, tmp_path):
    """--force overrides the guard; live children have parent cleared so they
    stay dispatchable instead of stranding under the dead unit."""
    entries = _read_entries(tmp_graph)
    _seed_node(entries, id_="ab-old", plan_path=str(_write_quick_plan(tmp_path / "old.md", ["x.py"])))
    _seed_node(entries, id_="ab-new", plan_path=str(_write_quick_plan(tmp_path / "new.md", ["y.py"])))
    node = _seed_node(entries, id_="ab-k1", plan_path=str(_write_quick_plan(tmp_path / "k1.md", ["z.py"])))
    node["parent"] = "ab-old"
    seed_graph(tmp_graph, json.dumps({"entries": entries}, indent=2))

    res = _native_supersede(
        tmp_graph, "ab-new", "--replaces", "ab-old", "--cause", "fold", "--surface", "x.py", "--force",
        "--reason", "folded", "--covers", "ab-new carries ab-old whole",
    )
    assert res.exit_code == 0, res.output
    assert "Cleared parent" in res.output and "ab-k1" in res.output

    entries = _read_entries(tmp_graph)
    by_id = {e["id"]: e for e in entries}
    assert by_id["ab-old"]["superseded_by"] == "ab-new"
    assert by_id["ab-k1"]["parent"] is None


def test_supersede_guard_not_bypassed_by_abbreviated_id(tmp_graph, tmp_path):
    """The guard must compare against the canonical resolved id, not the raw
    --replaces argument: an abbreviated id resolves via _find_node but never
    equals a child's full canonical parent, which would silently bypass it."""
    entries = _read_entries(tmp_graph)
    _seed_node(entries, id_="ab-aabbccdd", plan_path=str(_write_quick_plan(tmp_path / "old.md", ["x.py"])))
    _seed_node(entries, id_="ab-eeffffff", plan_path=str(_write_quick_plan(tmp_path / "new.md", ["y.py"])))
    kid = _seed_node(entries, id_="ab-11223344", plan_path=str(_write_quick_plan(tmp_path / "kid.md", ["z.py"])))
    kid["parent"] = "ab-aabbccdd"
    seed_graph(tmp_graph, json.dumps({"entries": entries}, indent=2))

    # Abbreviated --replaces (resolves to ab-aabbccdd); the live child must
    # still trip the guard.
    res = _native_supersede(tmp_graph, "ab-eeffffff", "--replaces", "ab-aabbcc", "--cause", "fold", "--surface", "x.py")
    assert res.exit_code != 0
    assert "live child" in res.output.lower()
    assert "ab-11223344" in res.output


# ---------------------------------------------------------------------------
# CLI: triage health
# ---------------------------------------------------------------------------


def test_triage_health_reports_collisions(tmp_graph, tmp_path):
    """Two pending plans touching the same files surface as a collision."""
    entries = _read_entries(tmp_graph)
    _seed_node(entries, id_="ab-a", plan_path=str(_write_quick_plan(tmp_path / "a.md", ["src/a.py", "src/b.py"])))
    _seed_node(entries, id_="ab-b", plan_path=str(_write_quick_plan(tmp_path / "b.md", ["src/a.py", "src/b.py", "src/c.py"])))
    seed_graph(tmp_graph, json.dumps({"entries": entries}, indent=2))

    res = _invoke("backlog", "triage", "health", "--all", "--json")
    assert res.exit_code == 0, res.output
    report = json.loads(res.output)
    assert report["totals"]["collisions"] >= 1
    pair = sorted(report["collisions"][0]["between"])
    assert pair == ["ab-a", "ab-b"]


def test_triage_health_failure_prone(tmp_graph, tmp_path):
    """Multi-attempt nodes with no PR show as failure-prone."""
    entries = _read_entries(tmp_graph)
    _seed_node(entries, id_="ab-burn", plan_path=str(_write_quick_plan(tmp_path / "p.md", ["x.py"])))
    entries[-1]["cost_sessions"] = [
        {"cost_usd": 5.0},
        {"cost_usd": 8.0},
    ]
    entries[-1]["pr_number"] = None
    seed_graph(tmp_graph, json.dumps({"entries": entries}, indent=2))

    res = _invoke("backlog", "triage", "health", "--all", "--json")
    assert res.exit_code == 0, res.output
    report = json.loads(res.output)
    assert any(n["id"] == "ab-burn" for n in report["failure_prone_nodes"])
    assert report["failure_prone_nodes"][0]["burned_usd"] == 13.0


def test_triage_health_resolves_relative_plan_paths(tmp_graph, tmp_path, monkeypatch):
    """When graph entries store repo-relative plan_paths and triage health is
    invoked from a non-repo-root cwd, the health report must resolve candidate
    paths against the repo root, otherwise the comparison silently yields
    zero collisions (false negatives).

    Simulates the scenario by using relative plan_paths in the graph and
    running from a fake repo root (a `.git` marker), so the test does not
    depend on the test runner's actual cwd.
    """
    # Create plans inside a fake repo root.
    repo = tmp_path / "fakerepo"
    repo.mkdir()
    (repo / ".git").mkdir()
    (repo / "plans").mkdir()
    plan_a = repo / "plans" / "a.md"
    plan_b = repo / "plans" / "b.md"
    _write_quick_plan(plan_a, ["src/a.py", "src/b.py", "src/c.py"])
    _write_quick_plan(plan_b, ["src/a.py", "src/b.py", "src/c.py"])

    # The native door resolves relative plans against the cwd's repo root.
    monkeypatch.chdir(repo)

    entries = _read_entries(tmp_graph)
    # Store relative plan_paths the way intake does on the live graph.
    _seed_node(entries, id_="ab-rel1", plan_path="plans/a.md")
    _seed_node(entries, id_="ab-rel2", plan_path="plans/b.md")
    seed_graph(tmp_graph, json.dumps({"entries": entries}, indent=2))

    res = _invoke("backlog", "triage", "health", "--all", "--json")
    assert res.exit_code == 0, res.output
    report = json.loads(res.output)
    # Without the fix, collisions == 0 (false negative).
    assert report["totals"]["collisions"] >= 1, (
        f"expected at least one collision pair from relative-path plans, got: {report}"
    )


# ---------------------------------------------------------------------------
# Unevaluated surface: "nothing to compare" != "compared, clean"
# ---------------------------------------------------------------------------


def _write_ownership_map_plan(path: Path, files: list[str]) -> Path:
    rows = "\n".join(f"| `{f}` | modify | /blueprint |" for f in files)
    path.write_text(
        "# Plan\n\n## File Ownership Map\n\n"
        f"| File | Action | Owner |\n|---|---|---|\n{rows}\n"
    )
    return path


def test_file_ownership_map_is_a_parseable_surface(tmp_path):
    """/blueprint writes File Ownership Map, not Files to Modify; the parser
    must read it or every blueprint-generated plan is invisible to collisions."""
    from fno.graph.collision import parse_files_to_modify

    p = _write_ownership_map_plan(tmp_path / "p.md", ["cli/src/fno/graph/cli.py"])

    assert parse_files_to_modify(p) == {"cli/src/fno/graph/cli.py"}


def test_has_file_surface_distinguishes_empty_from_clean(tmp_path):
    from fno.graph.collision import has_file_surface

    empty = tmp_path / "empty.md"
    empty.write_text("# Plan\n\n## Context\n\nNo file table here.\n")
    populated = _write_ownership_map_plan(tmp_path / "full.md", ["a.py"])

    assert has_file_surface(empty) is False
    assert has_file_surface(populated) is True


def test_parse_folder_plan_scans_index_and_phase_files(tmp_path):
    """Reading a directory raises IsADirectoryError, which degrades to an empty
    set - indistinguishable from a plan stating no surface, so every folder plan
    bypassed the gate."""
    from fno.graph.collision import has_file_surface, parse_files_to_modify

    folder = tmp_path / "plan"
    folder.mkdir()
    _write_quick_plan(folder / "00-INDEX.md", ["src/index.py"])
    _write_quick_plan(folder / "01-phase.md", ["src/phase.py"])

    assert parse_files_to_modify(folder) == {"src/index.py", "src/phase.py"}
    assert has_file_surface(folder) is True
