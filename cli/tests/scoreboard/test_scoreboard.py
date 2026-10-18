"""AC coverage for `fno scoreboard` (Wave 5, x-e7c4).

AC5-HP  stop-cause + spend + coverage print; autonomy/survival on W4 signals.
AC5-ERR corrupt ledger -> file+offset on one line, exit 1.
AC5-UI  coverage <100% -> caveat on the same screen as any rate.
AC5-EDGE empty window -> explicit no-data, exit 0.
AC5-FR  mid-append partial -> single retry recovers rather than crashing.
"""

from __future__ import annotations
from tests.fixtures.graph_seed import seed_graph

import json
from datetime import datetime

import pytest
import typer
from typer.testing import CliRunner

from fno.scoreboard import cli as sb_cli
from fno.scoreboard.fold import build_scoreboard, load_ledger_rows

runner = CliRunner()
NOW = datetime(2026, 7, 3, 20, 0, 0)


def _ledger(tmp_path, rows):
    p = tmp_path / "ledger.json"
    p.write_text(json.dumps({"entries": rows}))
    return p


def _app():
    app = typer.Typer()
    app.command()(sb_cli.scoreboard_command)
    return app


# Fixture timestamps are RELATIVE to now, not hardcoded (x-e957 fix-on-discovery).
# The scoreboard windows on "last 28d", so the literal 2026-07-02/03 rows these
# tests used aged out of the window on 2026-07-30T10:00Z and reddened CI on
# every PR from that moment. It passed locally only because the timestamps are
# naive and a UTC-behind developer clock still placed them inside the window -
# a failure that arrives on a wall-clock date and depends on the runner's
# timezone is the worst shape of flake to debug from a red PR.
def _days_ago(n: int, hour: int = 10) -> str:
    from datetime import datetime, timedelta

    return (datetime.now() - timedelta(days=n)).replace(
        hour=hour, minute=0, second=0, microsecond=0
    ).isoformat(timespec="seconds")


# Named for the role each plays in the window, so a reader sees the intent
# rather than an arithmetic puzzle.
_RECENT = _days_ago(1)
_OLDER = _days_ago(2)


def _wire(monkeypatch, tmp_path, ledger_path, *, hermetic_graph=True):
    import fno.paths as paths

    monkeypatch.setattr(paths, "ledger_json", lambda: ledger_path)
    monkeypatch.setattr(paths, "graph_json", lambda: tmp_path / "graph.json")
    if hermetic_graph:
        # A graph fixture that parses sends read_graph_nodes down the
        # _apply_graph_defaults seam, which needs the fno-agents-worker
        # runtime (absent on CI's changed-smoke and pip-only boxes). These
        # tests judge the fold and the render, not the migration pass, so
        # identity keeps them hermetic.
        import fno.graph.store as graph_store

        monkeypatch.setattr(graph_store, "_apply_graph_defaults", lambda entries, **k: entries)


# --- AC5-HP -----------------------------------------------------------------
def test_hp_prints_core_metrics(tmp_path, monkeypatch):
    rows = [
        {"completed": _RECENT, "termination_reason": "DonePRGreen", "graph_node_id": "x-1", "cost_usd": 5.0},
        {"completed": _OLDER, "termination_reason": "NoProgress", "graph_node_id": "x-2", "cost_usd": 2.0},
    ]
    _wire(monkeypatch, tmp_path, _ledger(tmp_path, rows))
    res = runner.invoke(_app(), [])
    assert res.exit_code == 0, res.output
    assert "Stop-cause distribution" in res.output
    assert "DonePRGreen" in res.output
    assert "Spend split" in res.output
    assert "ship-terminal:   $5.00" in res.output
    assert "wedge-terminal:  $2.00" in res.output


def test_hp_autonomy_survival_activate_with_w4(tmp_path, monkeypatch):
    # W4 signals present: a human_touch event + a graph node carrying a causal
    # field. The ship is 20 days old, so it has completed the observation
    # window and survival may judge it.
    rows = [{"completed": _days_ago(20), "termination_reason": "DonePRGreen", "graph_node_id": "x-00000001", "cost_usd": 5.0}]
    (tmp_path / "events.jsonl").write_text(json.dumps({"type": "human_touch", "ts": _days_ago(1, hour=9)}) + "\n")
    seed_graph(tmp_path / "graph.json", json.dumps({"entries": [{"id": "x-00000001", "reverted": False, "merge_status": "merged", "completed_at": _days_ago(20)}]}))
    _wire(monkeypatch, tmp_path, _ledger(tmp_path, rows))
    res = runner.invoke(_app(), ["--json"])
    sb = json.loads(res.output)
    assert sb["autonomy"]["available"] is True
    assert sb["survival"]["available"] is True and sb["survival"]["survived"] == 1


# --- AC5-ERR ----------------------------------------------------------------
def test_err_corrupt_ledger_exit_1(tmp_path, monkeypatch):
    p = tmp_path / "ledger.json"
    p.write_text('{"entries": [ {"a": 1}, {corrupt')  # invalid JSON
    _wire(monkeypatch, tmp_path, p)
    res = runner.invoke(_app(), [])
    assert res.exit_code == 1
    assert "ledger.json" in res.output and "byte" in res.output


# --- AC5-UI -----------------------------------------------------------------
def test_ui_partial_coverage_shows_caveat(tmp_path, monkeypatch):
    rows = [
        {"completed": _RECENT, "termination_reason": "DonePRGreen", "graph_node_id": "x-1", "cost_usd": 5.0},
        {"completed": _OLDER, "cost_usd": 1.0},  # no termination_reason -> <100%
    ]
    _wire(monkeypatch, tmp_path, _ledger(tmp_path, rows))
    res = runner.invoke(_app(), [])
    assert "a partial window is not a trend" in res.output  # caveat present
    assert "%" in res.output


# --- AC5-EDGE ---------------------------------------------------------------
def test_edge_empty_window_exit_0(tmp_path, monkeypatch):
    rows = [{"completed": "2020-01-01T00:00:00", "termination_reason": "DonePRGreen", "cost_usd": 5.0}]
    _wire(monkeypatch, tmp_path, _ledger(tmp_path, rows))
    res = runner.invoke(_app(), [])
    assert res.exit_code == 0
    assert "no terminal sessions in window" in res.output


# --- review hardening (gemini PR #186) --------------------------------------
def test_survival_ignores_fix_predating_ship(tmp_path, monkeypatch):
    # A fix-node created BEFORE the ship is not a follow-up to it -> node survives.
    # Fixed dates on purpose: this test injects an explicit `now=`, so it is
    # deterministic and must NOT follow the wall clock. The ship sits 20 days
    # before `now`, so it has completed the observation window.
    rows = [{"completed": "2026-06-15T10:00:00", "termination_reason": "DonePRGreen", "graph_node_id": "x-1", "cost_usd": 1.0}]
    graph = [
        {"id": "x-1", "reverted": False, "merge_status": "merged", "completed_at": "2026-06-15T10:00:00"},
        {"id": "x-fix", "caused_by": "x-1", "created_at": "2026-06-10T00:00:00"},  # 5 days BEFORE ship
    ]
    sb = build_scoreboard(rows, [], graph, since_days=28, now=datetime(2026, 7, 5, 20, 0, 0))
    assert sb["survival"]["available"] is True and sb["survival"]["survived"] == 1

    # a fix AFTER the ship, within 14 days, counts against survival
    graph[1]["created_at"] = "2026-06-25T00:00:00"
    sb2 = build_scoreboard(rows, [], graph, since_days=28, now=datetime(2026, 7, 5, 20, 0, 0))
    assert sb2["survival"]["survived"] == 0


# --- AC3: integrity, cohorts, emission failures -----------------------------
def test_quality_denominator_only_mature_deliveries(tmp_path, monkeypatch):
    # One delivery observed past the follow-up window, one 1 day old: only the
    # mature one is judged; the young one is pending, never in the denominator.
    rows = [
        {"completed": _days_ago(20), "termination_reason": "DonePRGreen", "graph_node_id": "x-old", "cost_usd": 1.0},
        {"completed": _days_ago(1), "termination_reason": "DonePRGreen", "graph_node_id": "x-new", "cost_usd": 1.0},
    ]
    graph = [
        {"id": "x-old", "reverted": False, "merge_status": "merged", "completed_at": _days_ago(20)},
        {"id": "x-new", "reverted": False, "merge_status": "merged", "completed_at": _days_ago(1)},
    ]
    sb = build_scoreboard(rows, [], graph, since_days=28, now=datetime.now())
    su = sb["survival"]
    assert su["available"] is True
    assert su["shipped_nodes"] == 1
    assert su["survived"] == 1
    assert su["pending"] == 1


def test_malformed_journal_states_its_omission(tmp_path, monkeypatch):
    # A malformed line in the touch journal is an omission the report states,
    # never a silent zero in the rates that read that journal.
    rows = [{"completed": _RECENT, "termination_reason": "DonePRGreen", "graph_node_id": "x-1", "cost_usd": 1.0}]
    (tmp_path / "events.jsonl").write_text("{not json\n")
    _wire(monkeypatch, tmp_path, _ledger(tmp_path, rows))
    res = runner.invoke(_app(), ["--json"])
    sb = json.loads(res.output)
    assert sb["event_coverage"]["complete"] is False
    assert sb["event_coverage"]["malformed_lines"] == 1
    res2 = runner.invoke(_app(), [])
    assert "event journals incomplete" in res2.output


def test_emission_failures_unknown_when_server_unreachable(tmp_path, monkeypatch):
    # AC3-ERR: an unavailable counter renders as Unknown with its reason, not
    # as a measured zero.
    import fno.scoreboard.cli as cli_mod

    monkeypatch.setattr(
        cli_mod,
        "emission_failures_snapshot",
        lambda: {"available": False, "reason": "connection refused"},
    )
    rows = [{"completed": _RECENT, "termination_reason": "DonePRGreen", "graph_node_id": "x-1", "cost_usd": 1.0}]
    _wire(monkeypatch, tmp_path, _ledger(tmp_path, rows))
    res = runner.invoke(_app(), [])
    assert "touch emission failures: unknown (connection refused)" in res.output


# --- AC5-FR -----------------------------------------------------------------
def test_fr_single_retry_recovers(tmp_path, monkeypatch):
    """First read sees a truncated file (mid-append), the retry sees it whole."""
    p = tmp_path / "ledger.json"
    good = {"entries": [{"completed": _RECENT, "termination_reason": "DonePRGreen", "cost_usd": 1.0}]}
    p.write_text('{"entries": [ {"completed"')  # truncated

    state = {"n": 0}
    real_sleep = __import__("time").sleep

    def fake_sleep(_):
        state["n"] += 1
        p.write_text(json.dumps(good))  # writer finishes during the backoff

    monkeypatch.setattr("fno.scoreboard.fold.time.sleep", fake_sleep)
    rows = load_ledger_rows(p)
    assert state["n"] == 1  # retried exactly once
    assert len(rows) == 1
    _ = real_sleep  # keep reference; no real sleeping in the test


# --- x-b6bd: shipped is the merge, the terminal is a breakdown ---------------
def test_merged_node_with_backstop_row_counts_shipped():
    # AC1-HP: a node whose PR merged ships even when its only ledger row is a
    # reconcile-backstop (the session's terminal never said Done*), and its
    # spend is ship spend. The second merged node has no ledger row at all -
    # the population the ledger never saw, reported separately.
    rows = [
        {"completed": _RECENT, "termination_reason": "reconcile-backstop", "graph_node_id": "x-m", "cost_usd": 3.0}
    ]
    graph = [
        {"id": "x-m", "merge_status": "merged", "completed_at": _RECENT, "reverted": False},
        {"id": "x-orphan", "merge_status": "merged", "completed_at": _RECENT},
        {"merge_status": "merged", "completed_at": _RECENT},  # junk row: no id, never a crash
    ]
    sb = build_scoreboard(rows, [], graph, since_days=28, now=datetime.now())
    assert sb["shipped_nodes"] == 2
    assert sb["merged_nodes_without_ledger_row"] == 1
    assert sb["spend"]["ship_terminal_usd"] == 3.0
    assert sb["spend"]["wedge_terminal_usd"] == 0.0


# --- x-e159: undated events never enter a timed window ----------------------
def test_undated_touch_never_inflates_autonomy():
    from datetime import timedelta

    from fno.scoreboard.fold import _autonomy

    now = datetime.now()
    cutoff = now - timedelta(days=28)
    events = [{"kind": "human_touch"}, {"kind": "human_touch", "ts": _RECENT}]
    out = _autonomy(events, {"x-1"}, cutoff, now)
    assert out["available"] is True
    assert out["touches"] == 1


# --- AC1: one delivery classification across the main view ------------------
def test_in_review_node_does_not_ship_and_document_stays_delivered():
    # AC1-HP: a DonePRGreen terminal on a known unmerged code node is a
    # session stop, not a delivery; an explicitly delivered doc stays one.
    rows = [
        {"completed": _RECENT, "termination_reason": "DonePRGreen", "graph_node_id": "x-code", "cost_usd": 1.0},
        {"completed": _RECENT, "termination_reason": "DoneAdvisory", "graph_node_id": "x-doc", "cost_usd": 1.0},
    ]
    graph = [
        {"id": "x-code", "merge_status": None, "completed_at": _RECENT},
        {"id": "x-doc", "merge_status": None, "completed_at": _RECENT},
    ]
    sb = build_scoreboard(rows, [], graph, since_days=28, now=datetime.now())
    assert sb["shipped_nodes"] == 1
    assert sb["delivery_classes"] == {"delivered_doc": 1}


# --- AC2: the denominator is scoped once ------------------------------------
def test_project_scope_counts_once_and_keeps_unattributed_out(tmp_path, monkeypatch):
    graph = [
        {"id": "x-1", "project": "p1", "merge_status": "merged", "completed_at": _RECENT},
        {"id": "x-2", "project": "p2", "merge_status": "merged", "completed_at": _RECENT},
    ]
    rows = [
        # p1: a retry, then the delivering run - one delivered node.
        {"completed": _RECENT, "termination_reason": "NoProgress", "graph_node_id": "x-1", "cost_usd": 1.0, "project": "p1", "provider_id": "zai"},
        {"completed": _RECENT, "termination_reason": "DonePRGreen", "graph_node_id": "x-1", "cost_usd": 2.0, "project": "p1", "provider_id": "zai"},
        {"completed": _RECENT, "termination_reason": "DonePRGreen", "graph_node_id": "x-1", "cost_usd": 3.0, "project": "p1", "provider_id": "claude"},
        # p2 must not leak into p1; the unattributed row is counted, not copied.
        {"completed": _RECENT, "termination_reason": "DonePRGreen", "graph_node_id": "x-2", "cost_usd": 9.0, "project": "p2"},
        {"completed": _RECENT, "termination_reason": "DonePRGreen", "cost_usd": 5.0},
    ]
    _wire(monkeypatch, tmp_path, _ledger(tmp_path, rows))
    seed_graph(tmp_path / "graph.json", json.dumps({"entries": graph}))
    res = runner.invoke(_app(), ["--project", "p1", "--json"])
    sb = json.loads(res.output)
    assert sb["shipped_nodes"] == 1
    assert sb["project_scope"]["project"] == "p1"
    assert sb["project_scope"]["unattributed_rows"] == 1
    assert sb["project_scope"]["other_project_rows"] == 1
    assert sb["spend"]["ship_terminal_usd"] == 6.0


# --- x-fe4d: the JSON stream stays strict (no NaN/Infinity tokens) ----------
def test_scoreboard_json_stream_is_strict(tmp_path, monkeypatch):
    rows = [
        {
            "completed": _RECENT,
            "termination_reason": "DonePRGreen",
            "graph_node_id": "x-1",
            "cost_usd": float("inf"),
        }
    ]
    _wire(monkeypatch, tmp_path, _ledger(tmp_path, rows))
    res = runner.invoke(_app(), ["--json"])
    assert res.exit_code == 0, res.output
    import json as _json

    parsed = _json.loads(res.output, parse_constant=lambda c: pytest.fail(f"non-finite {c}"))
    assert isinstance(parsed, dict)
