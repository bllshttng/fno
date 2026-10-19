"""The archive-inclusive merge: the working graph wins a reused id.

The archive reads ride the keeper, so the module needs the compiled runtime
and skips whole where the smoke harness deleted the worker binary.
"""
from __future__ import annotations

import json

import pytest

pytestmark = pytest.mark.dev_build


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
