"""Scope growth per epic, and its refusal to report past its evidence (x-d157).

The metric answers "how much did this epic grow after decomposition" from the
source_node_id edge. At the capture rate it shipped against, a low growth figure
is indistinguishable from poor capture -- and errs low, which flatters the
process. So coverage travels with the number and the number is withheld below a
floor.
"""
from __future__ import annotations
from tests._native_door import run_native
from tests.fixtures.graph_seed import seed_graph

import json
from pathlib import Path

import pytest

EPIC_BORN = "2026-01-01T00:00:00+00:00"
LATER = "2026-02-01T00:00:00+00:00"


def _node(node_id: str, **over) -> dict:
    base = {
        "id": node_id,
        "title": f"Node {node_id}",
        "_status": "ready",
        "domain": "code",
        "project": "fno",
        "slug": f"node-{node_id}",
        "created_at": LATER,
    }
    base.update(over)
    return base


def _epic(node_id: str = "x-epic", **over) -> dict:
    return _node(node_id, type="epic", created_at=EPIC_BORN, **over)


# ---------------------------------------------------------------------------
# AC3-FR: the metric refuses to report past its evidence
# ---------------------------------------------------------------------------


def _window(with_origin: int, without_origin: int) -> list[dict]:
    """An epic with one child and one real follow-up, plus a window of the
    given capture ratio.

    The follow-up (x-grew, produced by the child) is what makes the reportable
    case have something to report; the x-w/x-n filler only moves coverage.
    """
    entries = [
        _epic(),
        _node("x-c1", parent="x-epic"),
        _node("x-seed", created_at=EPIC_BORN),
        _node("x-grew", source_node_id="x-c1"),
    ]
    for i in range(with_origin):
        entries.append(_node(f"x-w{i:03d}", source_node_id="x-seed"))
    for i in range(without_origin):
        entries.append(_node(f"x-n{i:03d}"))
    return entries


# ---------------------------------------------------------------------------
# The epic verb surfaces it
# ---------------------------------------------------------------------------


@pytest.fixture
def graph(tmp_path, monkeypatch, _plan_hermetic_events_journal):
    g = tmp_path / "graph.json"

    def seed(entries: list[dict]) -> Path:
        seed_graph(g, json.dumps({"entries": entries}, indent=2) + "\n")
        return g

    # The verb answers natively: the binary reads the store through the tmp
    # state root, and its events reads sit under the pinned HOME (the global
    # mirror is HOME/.fno/events.jsonl, absent here; FNO_EVENTS_PATH is the
    # conftest journal pin).
    monkeypatch.setenv("HOME", str(tmp_path))
    monkeypatch.setenv("FNO_STATE_DIR", str(tmp_path))
    return seed


def test_epic_status_states_why_a_figure_is_withheld(graph):
    graph(_window(with_origin=1, without_origin=9))
    code, out, err = run_native("backlog", "epic", "status", "x-epic")
    assert code == 0, out + err
    assert "scope growth: withheld" in out
    assert "below the" in out and "floor" in out


def test_epic_status_json_never_emits_a_count_it_cannot_support(graph):
    graph(_window(with_origin=1, without_origin=9))
    code, out, err = run_native("backlog", "epic", "status", "x-epic", "--json")
    assert code == 0, out + err
    payload = json.loads(out)
    sg = payload["scope_growth"]
    assert sg["reportable"] is False
    assert sg["follow_ups"] is None, "a suppressed figure must be null, not 0"
    # Coverage evidence ships regardless, so the null explains itself.
    assert sg["window_total"] > 0


def test_epic_status_renders_the_reportable_line(graph):
    """The happy-path line is user-facing surface and needs rendering at least once."""
    graph(_window(with_origin=9, without_origin=1))
    code, out, err = run_native("backlog", "epic", "status", "x-epic")
    assert code == 0, out + err
    assert "scope growth: 1 follow-ups" in out
    assert "origin capture" in out
    assert "realized" in out


def test_scope_growth_reads_through_the_archive(graph, tmp_path, monkeypatch):
    """A swept child must not shrink the epic's realized cost.

    A metric that quietly changes when unrelated grooming runs is exactly the
    kind of number this feature refuses to print.
    """
    swept = _node("x-swept", parent="x-epic", pr_number=99)
    swept["archived_at"] = "2026-01-01T00:00:00Z"
    graph([_epic(), _node("x-c1", parent="x-epic", pr_number=1), swept])

    code, out, err = run_native("backlog", "epic", "status", "x-epic", "--json")
    assert code == 0, out + err
    payload = json.loads(out)
    assert payload["scope_growth"]["realized_nodes"] == 2
    assert payload["scope_growth"]["realized_prs"] == 2
