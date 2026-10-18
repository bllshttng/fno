"""Tests for fno.cost - ledger + budget integration."""
from __future__ import annotations
from tests.fixtures.graph_seed import seed_graph

import json
import subprocess
from pathlib import Path
from unittest.mock import MagicMock, patch


# ---- Helpers ----

def _make_ledger(tmp_path: Path) -> Path:
    """Create a minimal ledger.json."""
    fno_dir = tmp_path / ".fno"
    fno_dir.mkdir(parents=True, exist_ok=True)
    ledger_path = fno_dir / "ledger.json"
    ledger_path.write_text("[]")
    return ledger_path


def _make_graph(tmp_path: Path, node_id: str = "ab-12345678") -> Path:
    """Create a minimal graph.json with one node in the canonical {'entries': [...]} envelope."""
    fno_dir = tmp_path / ".fno"
    fno_dir.mkdir(parents=True, exist_ok=True)
    graph_path = fno_dir / "graph.json"
    node = {
        "id": node_id,
        "title": "Test Feature",
        "cost_usd": None,
        "cost_sessions": [],
    }
    seed_graph(graph_path, json.dumps({"entries": [node]}))
    return graph_path


# ---- Ledger reader ----

def _read_entries(ledger_path: Path) -> list[dict]:
    """Read ledger entries from either the canonical `{"entries": [...]}`
    envelope or a legacy flat-list ledger. Mirrors `_append_to_ledger`'s
    read-side tolerance so the tests don't bind to one shape.
    """
    raw = json.loads(ledger_path.read_text())
    if isinstance(raw, list):
        return raw
    if isinstance(raw, dict):
        entries = raw.get("entries", [])
        return entries if isinstance(entries, list) else []
    return []


# ---- AC1-HP: cost.update appends to graph node's cost_sessions ----

def test_ac1_hp_cost_update_appends_to_graph_node(tmp_path):
    """cost.update() appends to the graph node's cost_sessions[] when node_id provided."""
    ledger_path = _make_ledger(tmp_path)
    graph_path = _make_graph(tmp_path, "ab-12345678")

    from fno.cost import update as cost_update

    cost_update(
        session_id="sess-graph-test",
        tokens=2000,
        cost_usd=1.00,
        ledger_path=ledger_path,
        graph_path=graph_path,
        node_id="ab-12345678",
    )

    from fno.graph.store import read_graph_strict

    node = read_graph_strict(graph_path)[0]
    assert len(node["cost_sessions"]) == 1
    assert node["cost_sessions"][0]["session_id"] == "sess-graph-test"
    assert node["cost_sessions"][0]["cost_usd"] == 1.00
    # Cumulative cost_usd updated
    assert node["cost_usd"] == 1.00
    assert [e["session_id"] for e in _read_entries(ledger_path)] == ["sess-graph-test"]


# ---- AC2-HP: budget cap check ----

def test_ac2_hp_budget_cap_at_boundary(tmp_path):
    """check_budget() returns True when total + estimated exactly equals cap."""
    from fno.cost import check_budget

    # 24.50 + 0.50 = 25.00 = cap -> over cap
    assert check_budget(total_cost_usd=24.50, budget_cap_usd=25.00, estimated_phase_cost=0.50) is True


# ---- subprocess failure surface ----

def _cost_probe_run(result):
    """subprocess.run stand-in: the cost probe gets *result*; every other
    caller (the events validator's door subprocess) runs for real."""
    real_run = subprocess.run

    def _run(argv, *a, **kw):
        if isinstance(argv, list) and "fno.cost._session_cost" in map(str, argv):
            return result
        return real_run(argv, *a, **kw)

    return _run

def _make_failed_result(returncode: int, stderr: str) -> subprocess.CompletedProcess:
    """Build a fake subprocess.CompletedProcess representing script failure."""
    r = MagicMock(spec=subprocess.CompletedProcess)
    r.returncode = returncode
    r.stderr = stderr
    r.stdout = ""
    return r


def test_ac1_err_subprocess_failure_surfaces_stderr_and_event(tmp_path, capsys):
    """AC1-ERR: rc!=0 with stderr 'boom' -> stderr printed, event emitted, ledger updated."""
    ledger_path = _make_ledger(tmp_path)

    from fno.cost import update as cost_update

    failed_result = _make_failed_result(returncode=2, stderr="boom")

    captured_events = []

    def fake_append(event, path=None, **kw):
        captured_events.append(event)

    with patch("subprocess.run", side_effect=_cost_probe_run(failed_result)), \
         patch("fno.cost.append_event", side_effect=fake_append):

        cost_update(
            session_id="sess-err-001",
            tokens=200,
            cost_usd=0.10,
            ledger_path=ledger_path,
        )

    captured = capsys.readouterr()
    assert "cost.py: subprocess failed:" in captured.err, \
        f"Expected 'cost.py: subprocess failed:' in stderr, got: {captured.err!r}"
    assert "boom" in captured.err, \
        f"Expected 'boom' in stderr output, got: {captured.err!r}"

    assert len(captured_events) == 1, f"Expected 1 cost_subprocess_failed event, got: {captured_events}"
    evt = captured_events[0]
    assert evt["type"] == "cost_subprocess_failed"
    assert evt["data"]["fallback_succeeded"] is True
    assert evt["data"]["returncode"] == 2
    assert evt["data"]["stderr_snippet"] == "boom"

    entries = _read_entries(ledger_path)
    assert len(entries) == 1, f"Expected 1 ledger entry (direct fallback), got: {entries}"
    assert entries[0]["session_id"] == "sess-err-001"


# ---------------------------------------------------------------------------
# AC1-FR-OSE: subprocess.run raises OSError before completing.
# The ledger fallback MUST still write and a forensic event MUST be emitted
# with returncode=-1 and the exception text as stderr_snippet. Without the
# try/except around subprocess.run, the OSError propagates out of update()
# and the fallback never runs (silent failure - the very class this sweep
# is meant to eliminate).
# ---------------------------------------------------------------------------

def test_ac1_fr_subprocess_oserror_still_writes_ledger_and_event(tmp_path, capsys):
    """AC1-FR-OSE: when subprocess.run raises OSError (text-file-busy,
    permission denied on sys.executable, etc.), the ledger fallback still
    runs and a cost_subprocess_failed event is emitted with returncode=-1."""
    ledger_path = _make_ledger(tmp_path)

    from fno.cost import update as cost_update

    captured_events: list[dict] = []

    def fake_append(event, path=None, **kw):
        captured_events.append(event)

    real_run = subprocess.run

    def _raise_oserror(argv, *args, **kwargs):
        if isinstance(argv, list) and "fno.cost._session_cost" in map(str, argv):
            raise OSError("text file busy")
        return real_run(argv, *args, **kwargs)

    with patch("subprocess.run", side_effect=_raise_oserror), \
         patch("fno.cost.append_event", side_effect=fake_append):

        cost_update(
            session_id="sess-ose-001",
            tokens=10,
            cost_usd=0.002,
            ledger_path=ledger_path,
        )

    # Ledger fallback ran despite OSError on subprocess.run.
    entries = _read_entries(ledger_path)
    assert len(entries) == 1, f"Expected 1 ledger entry from fallback, got: {entries}"
    assert entries[0]["session_id"] == "sess-ose-001"

    # Forensic event was emitted with the OSError text in stderr_snippet.
    cost_events = [e for e in captured_events if e.get("type") == "cost_subprocess_failed"]
    assert len(cost_events) == 1, f"Expected 1 cost_subprocess_failed event, got: {cost_events}"
    assert cost_events[0]["data"]["returncode"] == -1
    assert "text file busy" in cost_events[0]["data"]["stderr_snippet"]
    assert cost_events[0]["data"]["fallback_succeeded"] is True


