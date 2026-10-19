"""Python-seam tests for the worked overlay: the fleet read, the
reachability pass, and the degradation contracts. The join itself (seat
records, role exclusion, provenance, ship rows, closed receipts) is the
Rust `fno-agents worked-nodes` verb; its contracts are tested in Rust
(crates/fno-agents/src/worked_nodes.rs)."""
from __future__ import annotations

import pytest

from fno.claims.roster import RosterReading
from fno.graph.statuses import live_worked_node_ids


@pytest.fixture(autouse=True)
def _no_worked_reply(monkeypatch):
    """Hermetic overlay: the verb never runs unless a test provides a reply."""
    monkeypatch.setattr(
        "fno.graph.statuses._worked_nodes_reply", lambda rows: {}
    )


def _entry(node_id: str, *, status: str = "ready") -> dict:
    return {
        "id": node_id,
        "status": status,
        "sessions": [
            {
                "phase": "blueprint",
                "harness": "claude",
                "session_id": "session-1",
                "started_at": "2026-09-09T00:00:00Z",
            }
        ],
    }


def _reading(state: str, node: str = "ac1-node") -> RosterReading:
    row = {
        "name": "bp-worker",
        "state": state,
        "cwd": "/worktrees/ac1-node",
        "row_id": "session-1",
        "node": node,
    }
    return RosterReading(True, 1, {}, "", {"session-1": row}, 0, ())


def test_ac8_edge_degrades_loudly_when_roster_is_unreadable(monkeypatch, capsys):
    monkeypatch.setattr("fno.graph.store.read_graph_strict", lambda *_a, **_kw: [_entry("ac1-node")])
    monkeypatch.setattr(
        "fno.claims.roster.read_roster",
        lambda **_kw: RosterReading(False, 0, {}, "roster timeout"),
    )

    assert live_worked_node_ids() == {}
    # The degrade names the reader that failed: read_roster prefixes the raise.
    assert (
        "worked overlay degraded: read_roster: roster timeout"
        in capsys.readouterr().err
    )
    with pytest.raises(RuntimeError, match="roster timeout"):
        live_worked_node_ids(strict=True)


def test_a_stopped_worker_drops_out_of_the_payload(monkeypatch):
    """Liveness stays a Python contract: the transcript/stopped-at predicate
    filters the payload, so a killed worker never reaches the verb."""
    captured: list[list[dict]] = []

    def fake_reply(rows):
        captured.append(rows)
        return {"ac1-node": ["bp-worker"]} if rows else {}

    monkeypatch.setattr("fno.graph.statuses._worked_nodes_reply", fake_reply)
    state = ["working"]
    monkeypatch.setattr(
        "fno.graph.store.read_graph_strict", lambda *_a, **_kw: [_entry("ac1-node")]
    )
    monkeypatch.setattr(
        "fno.claims.roster.read_roster", lambda **_kw: _reading(state[0])
    )

    assert live_worked_node_ids() == {"ac1-node": ["bp-worker"]}
    state[0] = "killed"
    assert live_worked_node_ids() == {}
    # The killed worker never reaches the verb: its second payload is empty,
    # and the verb's empty-rows answer reads as no worked nodes.
    assert len(captured) == 2
    assert [r["name"] for r in captured[0]] == ["bp-worker"]
    assert captured[1] == []


def test_the_payload_passes_attribution_and_marks_unmeasured(monkeypatch):
    """The delegate's one Python-side contract: the rows it sends the verb
    carry each row's node attribution (attributed rows survive the dedupe a
    seat-only pass would lose) and an unmeasured row rides its marked label."""
    captured: list[list[dict]] = []

    def fake_reply(rows):
        captured.append(rows)
        return {"x-node": ["t-worker"]}

    monkeypatch.setattr("fno.graph.statuses._worked_nodes_reply", fake_reply)
    entries = [{"id": "x-node", "status": "in_progress", "sessions": []}]
    reading = RosterReading(
        True, 2,
        {"x-node": [{"name": "t-worker", "state": "working",
                     "cwd": "/worktrees/x-node", "row_id": "s-1"}]},
        "",
        {"s-2": {"name": "t-drifter", "state": "working",
                 "cwd": "/elsewhere", "row_id": "s-2"}},
        0, (),
        {"x-node": ("bp-nosid",)},
    )
    assert live_worked_node_ids(strict=True, entries=entries, reading=reading) == {
        "x-node": ["t-worker"]
    }

    rows = captured[0]
    by_name = {r["name"]: r for r in rows}
    assert by_name["t-worker"]["node"] == "x-node"
    assert by_name["t-worker"]["session"] == "s-1"
    assert by_name["t-drifter"]["node"] is None
    assert "unmeasurable" in by_name["bp-nosid"]["label"]
    assert by_name["bp-nosid"]["session"] == ""
