"""Archive read surfaces: `get` / `find` read-through, `album`, and the
archive-inclusive merge. The sweep itself is native
(`crates/fno-agents/src/backlog/maintenance_archive.rs`).

Every command here rides the keeper, so the module needs the compiled
runtime and skips whole where the smoke harness deleted the worker binary
(the parity-test convention).
"""
from __future__ import annotations
from tests.fixtures.graph_seed import seed_graph

import pytest


requires_rust = pytest.mark.dev_build

pytestmark = requires_rust

import json  # noqa: E402
from pathlib import Path  # noqa: E402





def _route(tmp_path, monkeypatch) -> tuple[Path, Path]:
    import fno.graph._constants as gc
    import fno.graph.store as gs

    g = tmp_path / "graph.json"
    seed_graph(g, '{"entries": []}\n')
    # _constants serves these names through module __getattr__, so they are
    # NOT real attributes. monkeypatch.setattr would save the resolved value
    # and its undo would setattr it back, BAKING a frozen path into the module
    # for every later test in the process (the closure-release tests failed on
    # exactly that). setitem undoes by deleting the key, so the lazy
    # __getattr__ keeps answering after this test.
    monkeypatch.setitem(vars(gc), "GRAPH_JSON", g)
    monkeypatch.setitem(vars(gc), "GRAPH_MD", tmp_path / "graph.md")
    monkeypatch.setitem(vars(gc), "GRAPH_ARCHIVE_JSON", tmp_path / "graph-archive.json")
    monkeypatch.setattr(gs, "GRAPH_JSON", g)
    # Seam readers (guarded metadata/display reads) resolve paths.graph_json
    # at call time; pin the resolver to the same hermetic file.
    monkeypatch.setattr("fno.paths.graph_json", lambda: g)
    # Route paths.graph_archive_json (used by cmd_get read-through) to the temp.
    import fno.paths as p
    monkeypatch.setattr(p, "graph_json", lambda: g)
    # The native binary (which the get/find read-backs exec) resolves the
    # store through FNO_CONFIG's state_dir; point it at this same tmp dir.
    (tmp_path / "config.toml").write_text(f'state_dir = "{tmp_path}"\n')
    monkeypatch.setenv("FNO_CONFIG", str(tmp_path / "config.toml"))
    return g, tmp_path / "graph-archive.json"


def _native_backlog(*args) -> tuple[int, str, str]:
    """Run the native binary's backlog verb against the routed store.
    Returns (code, stdout, stderr)."""
    import os as _os
    import subprocess as _sp

    from fno.rust_binary import find_dev_binary, resolve_binary

    binary = find_dev_binary() or resolve_binary()
    if binary is None:
        pytest.skip("no fno-agents dev build (cargo build -p fno-agents)")
    proc = _sp.run(
        [str(binary), "backlog", *args],
        capture_output=True,
        text=True,
        env={**_os.environ, "FNO_TRACKER_BACKEND": "graph"},
    )
    return proc.returncode, proc.stdout, proc.stderr


def _seed(g: Path, entries: list[dict]) -> None:
    seed_graph(g, json.dumps({"entries": entries}) + "\n")


def test_get_read_through_resolves_archived_node(tmp_path, monkeypatch):
    g, archive = _route(tmp_path, monkeypatch)
    _seed(g, [])  # working graph empty
    archive.write_text(json.dumps({"entries": [
        {"id": "ab-arch0001", "slug": "archived-node", "title": "Old", "completed_at": "2026-01-01T00:00:00Z"}
    ]}) + "\n")

    code, stdout, stderr = _native_backlog("get", "ab-arch0001")
    assert code == 0, stderr
    out = json.loads(stdout)
    assert out["id"] == "ab-arch0001"
    assert out["_archived"] is True


def test_get_missing_everywhere_exits_1(tmp_path, monkeypatch):
    g, _archive = _route(tmp_path, monkeypatch)
    _seed(g, [])
    code, _, _ = _native_backlog("get", "ab-nope0001")
    assert code == 1


def test_find_read_through_resolves_archived_node(tmp_path, monkeypatch):
    """AC1-UI: the dedup path must still surface an archived node (stamped
    _archived), or archiving done nodes silently destroys /think + /blueprint
    recall against everything ever shipped."""
    g, archive = _route(tmp_path, monkeypatch)
    _seed(g, [])  # working graph empty
    archive.write_text(json.dumps({"entries": [
        {"id": "ab-arch0001", "slug": "old-archived-feature",
         "title": "Old Archived Feature", "domain": "code",
         "completed_at": "2026-01-01T00:00:00Z"}
    ]}) + "\n")

    code, stdout, stderr = _native_backlog("find", "Archived Feature", "--json")
    assert code == 0, stderr
    hits = json.loads(stdout)
    assert [h["id"] for h in hits] == ["ab-arch0001"]
    assert hits[0]["_archived"] is True


def test_find_corrupt_archive_is_miss_not_crash(tmp_path, monkeypatch):
    """AC1-ERR: a corrupt graph-archive.json is a miss (fall through to exit-1),
    never a crash propagated to the caller."""
    g, archive = _route(tmp_path, monkeypatch)
    _seed(g, [])  # working graph empty
    archive.write_text("{not json at all")

    code, _, _ = _native_backlog("find", "anything", "--json")
    assert code == 1


def test_album_project_filter(tmp_path, monkeypatch):
    g, archive = _route(tmp_path, monkeypatch)
    _seed(g, [])
    archive.write_text(json.dumps({"entries": [
        {"id": "ab-p1", "title": "P1", "status": "done",
         "completed_at": "2026-01-01T00:00:00Z", "project": "alpha"},
        {"id": "ab-p2", "title": "P2", "status": "done",
         "completed_at": "2026-01-02T00:00:00Z", "project": "beta"},
    ]}) + "\n")
    code, out, err = _native_backlog("album", "--project", "alpha")
    assert code == 0, err
    assert "ab-p1" in out
    assert "ab-p2" not in out


def test_album_reports_overflow_count(tmp_path, monkeypatch):
    g, archive = _route(tmp_path, monkeypatch)
    _seed(g, [])
    archive.write_text(json.dumps({"entries": [
        {"id": f"ab-{i:04d}", "status": "done",
         "completed_at": f"2026-01-{i:02d}T00:00:00Z"}
        for i in range(1, 6)
    ]}) + "\n")
    code, out, err = _native_backlog("album", "--limit", "2")
    assert code == 0, err
    assert "3 more" in out


def test_entries_with_archive_never_returns_duplicate_ids(tmp_path, monkeypatch):
    from fno.graph.store import entries_with_archive
    import fno.paths as p

    archive_path = tmp_path / "graph-archive.json"
    archive_path.write_text(json.dumps({"entries": [
        {"id": "x-dup", "title": "archived version"}
    ]}) + "\n")
    monkeypatch.setattr(p, "graph_archive_json", lambda: archive_path)

    working = [{"id": "x-dup", "title": "live version"}]
    merged = entries_with_archive(working)
    ids = [e["id"] for e in merged]
    assert ids.count("x-dup") == 1  # working entry wins; archived duplicate dropped
    assert next(e for e in merged if e["id"] == "x-dup")["title"] == "live version"
