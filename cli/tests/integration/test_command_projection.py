"""Integration tests for graph-mutating commands repainting their linked docs.

Drives the REAL backlog `add` command through the native door against a temp
graph + temp plan doc. Covers the parent epic's doc repaint on a child add and
the create-path epic depth cap refusal.
"""
from __future__ import annotations
from tests.fixtures.graph_seed import seed_graph

import json
import re
from pathlib import Path

import pytest
import yaml

from tests.conftest import run_native_create


def _strify(v):
    """Scalars read back as the raw strings the line-based writer emits."""
    if isinstance(v, bool):
        return str(v)
    if isinstance(v, (int, float)):
        return str(v)
    if isinstance(v, list):
        return [_strify(i) for i in v]
    return v


def read_plan_file(path):
    """PyYAML-based stand-in for the retired Python codec reader."""
    text = Path(path).read_text(encoding="utf-8")
    m = re.match(r"^---\n(.*?)\n---(?:\n|$)", text, re.DOTALL)
    fields = yaml.safe_load(m.group(1)) if m else {}
    fields = {k: _strify(v) for k, v in (fields or {}).items()}
    return Path(path), fields, ""


@pytest.fixture
def tmp_graph(tmp_path, monkeypatch) -> Path:
    g = tmp_path / "graph.json"
    seed_graph(g, '{"entries": []}\n')
    import fno.graph._constants as gc
    import fno.graph.store as gs
    monkeypatch.setattr(gc, "GRAPH_JSON", g)
    monkeypatch.setattr(gc, "GRAPH_MD", tmp_path / "graph.md")
    monkeypatch.setattr(gs, "GRAPH_JSON", g)
    # Seam readers resolve fno.paths.graph_json at call time; pin the
    # resolver to the same hermetic file (module-attr pins do not reach it).
    monkeypatch.setattr("fno.paths.graph_json", lambda: g)
    return g


def _seed(g: Path, entries: list[dict]) -> None:
    seed_graph(g, json.dumps({"entries": entries}, indent=2) + "\n")


def _epic(nid, slug, parent=None):
    return {
        "id": nid, "slug": slug, "title": slug, "status": "ready",
        "domain": "code", "project": "fno", "type": "epic", "parent": parent,
    }


def test_add_epic_depth_cap_refused(tmp_graph, tmp_path):
    """AC3-ERR (create path): `add --type epic --parent <nested-epic>` is capped
    too, or the guard cmd_update applies would be bypassable at creation."""
    _seed(tmp_graph, [
        _epic("x-0a01", "mission"),
        _epic("x-0e02", "epic", parent="x-0a01"),
    ])
    res = run_native_create(tmp_graph, "add", "Third level epic", "--type", "epic", "--parent", "x-0e02")
    assert res.exit_code != 0
    assert "cap" in (res.output + res.stderr).lower()
    # No new node was appended.
    from fno.graph.store import read_graph_strict
    entries = read_graph_strict(tmp_graph)
    assert len(entries) == 2


_EPIC_DOC = """\
---
node: {nid}
status: ready
type: epic
---

# {nid} epic
"""


def _epic_with_plan(tmp_path, nid, slug, parent=None):
    p = tmp_path / f"{nid}.md"
    p.write_text(_EPIC_DOC.format(nid=nid), encoding="utf-8")
    e = _epic(nid, slug, parent)
    e["plan_path"] = str(p)
    return e, p


def test_add_child_repaints_parent_epic(tmp_graph, tmp_path):
    """Creating a child via `add --parent <epic>` repaints the epic."""
    epic, e_doc = _epic_with_plan(tmp_path, "x-0e0e", "epic")
    _seed(tmp_graph, [epic])
    res = run_native_create(tmp_graph, "add", "A child", "--parent", "x-0e0e")
    assert res.exit_code == 0, res.output
    _, fe, _ = read_plan_file(e_doc)
    assert fe["children_total"] == "1"
