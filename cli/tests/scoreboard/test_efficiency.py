"""AC coverage for `fno scoreboard --efficiency` (x-c284).

AC1-HP   efficiency view renders per-outcome-class buckets + distribution lines
         + a coverage dict from recorded telemetry.
AC2-HP   ci_reds counts red EPISODES (transitions into FAILURE), not red polls.
AC3-ERR  a corrupt events line never crashes the fold.
AC4-UI   no-data prints a state, and --efficiency pairs exclusively.
AC5-EDGE a session with no joined events -> None (not 0), excluded from dists.
AC6-FR   an unrecognized ci value surfaces as ci_unparsed, ci_reds None.
"""

from __future__ import annotations

import json
from datetime import datetime, timedelta, timezone

import typer
from typer.testing import CliRunner

from fno.scoreboard import cli as sb_cli
from fno.scoreboard.fold import build_efficiency

runner = CliRunner()
_RECENT_BASE = datetime.now(timezone.utc) - timedelta(days=1)


def _recent(delta_minutes: int = 0) -> str:
    """A `completed` timestamp inside the CLI's REAL 28-day window.

    Unit tests inject `now=`, so their fixture dates never expire. CLI-path tests
    invoke the real command and cannot inject anything, so a hardcoded fixture
    date is a time bomb: these were pinned to 2026-07-03 and started failing on
    2026-07-31, exactly 28 days later, with "no terminal sessions in window".
    Anchored to now so the window can never age out again.

    Built from a UTC base and emitted naive, matching how the pinned fixtures
    read (naive values were UTC wall time), so relative gaps are preserved.
    """
    return (_RECENT_BASE + timedelta(minutes=delta_minutes)).replace(
        tzinfo=None, microsecond=0
    ).isoformat()


def _recent_z(delta_minutes: int = 0) -> str:
    """`_recent` in the Z-suffixed UTC form the loop-event fixtures use."""
    return (_RECENT_BASE + timedelta(minutes=delta_minutes)).strftime("%Y-%m-%dT%H:%M:%SZ")
NOW = datetime(2026, 7, 3, 20, 0, 0)


def _app():
    app = typer.Typer()
    app.command()(sb_cli.scoreboard_command)
    return app


def _wire(monkeypatch, tmp_path, ledger_path, events=None):
    import fno.paths as paths

    monkeypatch.setattr(paths, "ledger_json", lambda: ledger_path)
    monkeypatch.setattr(paths, "graph_json", lambda: tmp_path / "graph.json")
    (ledger_path.parent / "events.jsonl").write_text("\n".join(events or []) + ("\n" if events else ""))


def _loop(sid: str, ci: str, ts: str) -> str:
    return json.dumps({"ts": ts, "type": "loop_check", "data": {"session_id": sid, "ci": ci}})


# --- AC1-HP ------------------------------------------------------------------
def test_hp_efficiency_view_shape():
    rows = [
        {"completed": "2026-07-03T10:00:00", "termination_reason": "DonePRGreen", "graph_node_id": "x-1",
         "cost_usd": 5.0, "tokens_total": 1000, "duration_minutes": 30, "sessions": ["s-a"]},
        {"completed": "2026-07-02T10:00:00", "termination_reason": "DonePRGreen", "graph_node_id": "x-2",
         "cost_usd": 8.0, "tokens_total": 4000, "duration_minutes": 90, "sessions": ["s-b"]},
    ]
    events = [
        {"ts": "2026-07-03T09:00:00Z", "type": "loop_check", "data": {"session_id": "s-a", "ci": "SUCCESS"}},
        {"ts": "2026-07-02T09:00:00Z", "type": "loop_check", "data": {"session_id": "s-b", "ci": "FAILURE:smoke"}},
        {"ts": "2026-07-02T09:05:00Z", "type": "loop_check", "data": {"session_id": "s-b", "ci": "SUCCESS"}},
    ]
    graph = [{"id": "x-1", "reverted": False, "merge_status": "merged", "completed_at": "2026-07-03T10:00:00"}, {"id": "x-2", "reverted": False, "merge_status": "merged", "completed_at": "2026-07-02T10:00:00"}]
    eff = build_efficiency(rows, events, graph, since_days=28, now=NOW, read_transcript=lambda sid: None)

    assert eff["state"] == "ok"
    cov = eff["coverage"]
    assert set(cov) >= {"rows", "loop_join_pct", "transcript_pct", "node_linkage_pct", "ci_unparsed"}
    assert cov["rows"] == 2 and cov["loop_join_pct"] == 100 and cov["node_linkage_pct"] == 100
    mc = eff["per_outcome_class"]["merged_clean"]
    assert mc["n"] == 2 and mc["spend_usd"] == 13.0 and mc["median_fires"] is not None
    dist = eff["distribution"]
    assert set(dist) == {"loop_fires", "ci_reds", "tokens_total", "duration_minutes"}
    for m in dist.values():
        assert set(m) == {"median", "p90", "n"}
    assert dist["ci_reds"]["median"] in (0, 1)  # s-a 0 reds, s-b 1 red


# --- AC2-HP ------------------------------------------------------------------
def test_hp_ci_reds_counts_episodes_not_polls():
    rows = [{"completed": "2026-07-03T10:00:00", "termination_reason": "DonePRGreen", "graph_node_id": "x-1",
             "cost_usd": 1.0, "tokens_total": 1, "duration_minutes": 1, "sessions": ["s-a"]}]
    # SUCCESS, FAILURE:smoke, FAILURE:smoke, SUCCESS -> one red episode.
    events = [
        _loop("s-a", "SUCCESS", "2026-07-03T09:00:00Z"),
        _loop("s-a", "FAILURE:smoke", "2026-07-03T09:01:00Z"),
        _loop("s-a", "FAILURE:smoke", "2026-07-03T09:02:00Z"),
        _loop("s-a", "SUCCESS", "2026-07-03T09:03:00Z"),
    ]
    events = [json.loads(e) for e in events]
    eff = build_efficiency(rows, events, [{"id": "x-1", "reverted": False, "merge_status": "merged", "completed_at": "2026-07-03T10:00:00"}],
                           since_days=28, now=NOW, read_transcript=lambda sid: None)
    assert eff["distribution"]["ci_reds"]["median"] == 1


# --- AC3-ERR -----------------------------------------------------------------
def test_err_corrupt_events_line_never_crashes(tmp_path, monkeypatch):
    ledger = tmp_path / "ledger.json"
    ledger.write_text(json.dumps({"entries": [
        {"completed": _recent(), "termination_reason": "DonePRGreen",
         "graph_node_id": "x-1", "cost_usd": 1.0, "sessions": ["s-a"]},
    ]}))
    events = [
        _loop("s-a", "FAILURE:smoke", _recent_z(-60)),
        "{ this is not valid json",  # corrupt line between valid ones
        _loop("s-a", "SUCCESS", _recent_z(-59)),
    ]
    _wire(monkeypatch, tmp_path, ledger, events)
    res = runner.invoke(_app(), ["--efficiency", "--json"])
    assert res.exit_code == 0, res.output
    d = json.loads(res.output)
    assert d["state"] == "ok"
    assert d["coverage"]["loop_join_pct"] == 100  # both valid events counted


# --- AC4-UI ------------------------------------------------------------------
def test_ui_efficiency_conflicts_with_calibration(tmp_path, monkeypatch):
    ledger = tmp_path / "ledger.json"
    ledger.write_text(json.dumps({"entries": []}))
    _wire(monkeypatch, tmp_path, ledger)
    res = runner.invoke(_app(), ["--efficiency", "--calibration"])
    assert res.exit_code != 0
    assert "mutually exclusive" in res.output


# --- AC5-EDGE ----------------------------------------------------------------
def test_edge_no_events_is_none_not_zero():
    rows = [{"completed": "2026-07-03T10:00:00", "termination_reason": "DonePRGreen", "graph_node_id": "x-1",
             "cost_usd": 1.0, "tokens_total": 5, "duration_minutes": 3, "sessions": ["s-none"]}]
    eff = build_efficiency(rows, [], [{"id": "x-1", "reverted": False, "merge_status": "merged", "completed_at": "2026-07-03T10:00:00"}],
                           since_days=28, now=NOW, read_transcript=lambda sid: None)
    # the row joined no loop_check event: excluded from every distribution,
    # coverage reflects the miss.
    assert eff["coverage"]["loop_join_pct"] == 0
    assert eff["distribution"]["loop_fires"] == {"median": None, "p90": None, "n": 0}
    assert eff["distribution"]["ci_reds"]["n"] == 0
    # but the row still lands in its outcome bucket (spend reconciles).
    assert eff["per_outcome_class"]["merged_clean"]["n"] == 1
    assert eff["per_outcome_class"]["merged_clean"]["median_fires"] is None


# --- AC6-FR ------------------------------------------------------------------
def test_fr_unrecognized_ci_surfaces_as_unparsed_not_silence():
    rows = [{"completed": "2026-07-03T10:00:00", "termination_reason": "DonePRGreen", "graph_node_id": "x-1",
             "cost_usd": 1.0, "sessions": ["s-a"]}]
    events = [
        json.loads(_loop("s-a", "SUCCESS", "2026-07-03T09:00:00Z")),
        json.loads(_loop("s-a", "WEIRD_NEW_SHAPE", "2026-07-03T09:01:00Z")),
    ]
    eff = build_efficiency(rows, events, [{"id": "x-1", "reverted": False, "merge_status": "merged", "completed_at": "2026-07-03T10:00:00"}],
                           since_days=28, now=NOW, read_transcript=lambda sid: None)
    assert eff["coverage"]["ci_unparsed"] == 1
    # the affected session's ci_reds is None (excluded from the distribution),
    # never a fabricated count.
    assert eff["distribution"]["ci_reds"]["n"] == 0


# --- join semantics ----------------------------------------------------------
def test_shipped_without_causal_telemetry_is_untracked_not_clean():
    # no node carries reverted/caused_by anywhere -> a shipped row must not
    # silently claim merged_clean (mirrors _survival's w4 gate).
    rows = [{"completed": "2026-07-03T10:00:00", "termination_reason": "DonePRGreen", "graph_node_id": "x-1",
             "cost_usd": 1.0, "sessions": ["s-a"]}]
    events = [json.loads(_loop("s-a", "SUCCESS", "2026-07-03T09:00:00Z"))]
    eff = build_efficiency(rows, events, [{"id": "x-1", "merge_status": "merged", "completed_at": "2026-07-03T10:00:00"}],  # exists, no causal fields
                           since_days=28, now=NOW, read_transcript=lambda sid: None)
    assert "merged_clean" not in eff["per_outcome_class"]
    assert eff["per_outcome_class"]["shipped_untracked"]["n"] == 1
    assert eff["coverage"]["outcome_tracked_pct"] == 0


# --- planned bucket (x-c3d6c136 / x-68d3) -------------------------------------
def _rt(sid):  # no-transcript stub for planned-bucket tests
    return None


def test_idea_thread_think_plan_folds_planned():
    """A /target idea thread that ran think+plan and stopped is `planned`, not `unshipped`."""
    rows = [{"completed": "2026-07-03T10:00:00", "termination_reason": "NoWork",
             "phases_completed": ["think", "plan"], "graph_node_id": "x-1", "cost_usd": 3.0}]
    eff = build_efficiency(rows, [], [], since_days=28, now=NOW, read_transcript=_rt)
    assert "planned" in eff["per_outcome_class"]
    assert "unshipped" not in eff["per_outcome_class"]
    assert eff["per_outcome_class"]["planned"]["n"] == 1


def test_plan_vs_build_cost_per_node():
    """Plan-thread cost and build-thread cost sum per node, only for nodes with a planned row."""
    rows = [
        {"completed": "2026-07-03T10:00:00", "termination_reason": "NoWork",
         "phases_completed": ["think", "plan"], "graph_node_id": "x-1", "cost_usd": 3.0},
        {"completed": "2026-07-03T11:00:00", "termination_reason": "DonePRGreen",
         "phases_completed": ["do", "ship"], "graph_node_id": "x-1", "cost_usd": 7.0},
        {"completed": "2026-07-03T12:00:00", "termination_reason": "DonePRGreen",
         "phases_completed": ["do", "ship"], "graph_node_id": "x-2", "cost_usd": 5.0},
    ]
    eff = build_efficiency(rows, [], [], since_days=28, now=NOW, read_transcript=_rt)
    pvb = eff["plan_vs_build_cost"]
    assert pvb == {"x-1": {"plan_usd": 3.0, "build_usd": 7.0}}  # x-2 has no planned row -> excluded
