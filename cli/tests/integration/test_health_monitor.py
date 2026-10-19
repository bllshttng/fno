"""Integration tests for the backlog health monitor (native door).

Covers:
- ``fno backlog triage health --check`` exit-code semantics
- ``fno backlog triage health --check --quiet`` loop-safe output
- ``fno backlog triage trend`` history readout
- the project_cwd_mismatch metric and the evals age line
"""
from __future__ import annotations
from tests.fixtures.graph_seed import seed_graph

import json
from datetime import datetime, timezone, timedelta
from pathlib import Path

import pytest
from typer.testing import CliRunner

from fno.cli import app

runner = CliRunner()


@pytest.fixture
def tmp_graph(tmp_path, monkeypatch):
    """Redirect ~/.fno/graph.json + the in-process global home dir to a
    temp tree so each test starts from an empty backlog and an empty home.

    Mirrors the fixture pattern in test_collision.py (its tmp_graph also
    flips internal module-level Path constants via
    fno.graph._constants).
    """
    import fno.graph._constants as gc

    home = tmp_path / "home"
    home.mkdir(parents=True, exist_ok=True)
    monkeypatch.setenv("HOME", str(home))

    fno_dir = home / ".fno"
    fno_dir.mkdir(parents=True, exist_ok=True)

    # GRAPH_JSON is computed at import time from Path.home(); monkeypatch
    # it to the temp path so reads/writes land in the test sandbox.
    monkeypatch.setattr(gc, "GRAPH_JSON", fno_dir / "graph.json")
    # Seam readers resolve fno.paths.graph_json at call time; pin the
    # resolver to the same hermetic file (module-attr pins do not reach it).
    monkeypatch.setattr("fno.paths.graph_json", lambda: fno_dir / "graph.json")

    return fno_dir


def _write_idea_nodes(graph_path: Path, n: int) -> None:
    """Seed the graph with N idea-status nodes (no plan_path = derives to idea)."""
    from datetime import datetime, timezone
    entries = []
    for i in range(n):
        entries.append(
            {
                "id": f"ab-test{i:04x}",
                "title": f"idea {i}",
                "type": "feature",
                "project": "test-proj",
                "cwd": str(graph_path.parent),
                "priority": "p2",
                "domain": "code",
                "blocked_by": [],
                "session_id": None,
                "claimed_at": None,
                "completed_at": None,
                "has_brief": False,
                "compacted": False,
                "plan_path": None,  # no plan_path -> derives to status: idea
                "pr_number": None,
                "pr_url": None,
                "merge_status": None,
                "created_at": datetime.now(timezone.utc).isoformat(),
                "status": "idea",
                "collisions_acknowledged": [],
                "supersedes": [],
                "superseded_by": None,
                "source_kind": "organic",
            }
        )
    graph_path.parent.mkdir(parents=True, exist_ok=True)
    seed_graph(graph_path, json.dumps({"entries": entries}, indent=2))


# ---------------------------------------------------------------------------
# CLI: fno backlog triage health --check
# ---------------------------------------------------------------------------


@pytest.mark.usefixtures("native_backlog_door")
def test_quiet_mode_no_output_when_healthy(tmp_graph):
    """--quiet on healthy backlog produces no stdout."""
    result = runner.invoke(app, ["backlog", "triage", "health", "--check", "--quiet"])
    assert result.exit_code == 0
    # No stdout when healthy + quiet (stderr is fine for "loaded config" diagnostics)
    assert result.stdout.strip() == ""


@pytest.mark.usefixtures("native_backlog_door")
def test_check_to_history_to_throttle_to_trend_journey(tmp_graph, tmp_path, monkeypatch):
    """End-to-end wiring: --check breach -> history append -> throttle write
    -> repeat --check still breaches but throttle suppresses dispatch ->
    trend reads both entries.

    Catches wiring bugs the per-function tests miss: e.g. cmd_health
    forgetting to pass throttle_path or skipping append_history.
    """
    # Seed graph so --check breaches.
    _write_idea_nodes(tmp_graph / "graph.json", n=30)

    history_path = tmp_path / "hist.jsonl"
    monkeypatch.setenv("FNO_HEALTH_HISTORY", str(history_path))

    # Point the throttle/alert paths at the temp tree so the journey is
    # observable. We seed a project settings.yaml that turns on log_only
    # so the alert log is the dispatch sink we can read back.
    settings = tmp_graph / "settings.yaml"
    settings.write_text(
        f"config:\n"
        f"  health_monitor:\n"
        f"    enabled: true\n"
        f"    notifications:\n"
        f"      surfaces: [log_only]\n"
        f"    history:\n"
        f"      enabled: true\n"
        f"      path: {history_path}\n"
        f"      retain_days: 90\n"
    )
    # The CLI loads project settings from `.fno/settings.yaml` in
    # CWD; switch to the tmp home so that resolves correctly.
    monkeypatch.chdir(tmp_graph.parent)
    (tmp_graph.parent / ".fno").mkdir(exist_ok=True)
    (tmp_graph.parent / ".fno" / "settings.yaml").write_text(settings.read_text())

    # First check: breach, history written, alert dispatched.
    r1 = runner.invoke(app, ["backlog", "triage", "health", "--check", "--all"])
    assert r1.exit_code == 4, r1.output
    assert history_path.exists()
    history_lines_1 = history_path.read_text().strip().splitlines()
    assert len(history_lines_1) == 1
    entry1 = json.loads(history_lines_1[0])
    assert entry1["report"]["idea_pile_depth"] >= 30
    assert any(b["key"] == "idea_pile_depth" for b in entry1["breaches"])

    # Second check: still breach. History appends a second line. The
    # alert log (from log_only) should NOT have grown - throttle
    # suppresses the second info-severity dispatch.
    alert_log = Path.home() / ".fno" / "logs" / "health-alerts.log"
    log_size_before = alert_log.stat().st_size if alert_log.exists() else 0
    r2 = runner.invoke(app, ["backlog", "triage", "health", "--check", "--all"])
    assert r2.exit_code == 4, r2.output
    history_lines_2 = history_path.read_text().strip().splitlines()
    assert len(history_lines_2) == 2  # history is not throttled
    if alert_log.exists():
        # Throttle should keep the log file from growing on the second
        # info-severity dispatch within the throttle window.
        assert alert_log.stat().st_size == log_size_before

    # Trend: reads the populated history.
    r3 = runner.invoke(app, ["backlog", "triage", "trend", "--days", "1"])
    assert r3.exit_code == 0
    assert "idea_pile_depth" in r3.output


# ---------------------------------------------------------------------------
# project_cwd_mismatch health metric (US3, Task 1.3)
# ---------------------------------------------------------------------------


def _make_pending_node(
    node_id: str,
    *,
    project: str,
    cwd: str,
    completed_at=None,
) -> dict:
    """Build a minimal pending (or done if completed_at set) backlog node."""
    from datetime import datetime, timezone

    return {
        "id": node_id,
        "title": f"node {node_id}",
        "type": "feature",
        "project": project,
        "cwd": cwd,
        "priority": "p2",
        "domain": "code",
        "blocked_by": [],
        "session_id": None,
        "claimed_at": None,
        "completed_at": completed_at,
        "has_brief": False,
        "compacted": False,
        "plan_path": "internal/fno/plans/test-plan.md",
        "pr_number": None,
        "pr_url": None,
        "merge_status": None,
        "created_at": datetime.now(timezone.utc).isoformat(),
        "status": "done" if completed_at else "ready",
        "collisions_acknowledged": [],
        "supersedes": [],
        "superseded_by": None,
        "source_kind": "organic",
    }


def _map_projects(fno_dir: Path, monkeypatch, roots: dict[str, str]) -> None:
    """Seed the work map the native door reads: the global config.toml under
    the tmp HOME. The run starts from that HOME so no project config found
    from the checkout outranks it."""
    rows = [
        f'[[work.workspaces.test.projects]]\nname = "{name}"\npath = "{root}"\n'
        for name, root in roots.items()
    ]
    (fno_dir / "config.toml").write_text("\n".join(rows))
    monkeypatch.chdir(fno_dir.parent)


def _write_nodes(graph_path: Path, nodes: list[dict]) -> None:
    graph_path.parent.mkdir(parents=True, exist_ok=True)
    seed_graph(graph_path, json.dumps({"entries": nodes}, indent=2))


@pytest.mark.usefixtures("native_backlog_door")
def test_health_mismatch_ac3_hp(tmp_graph, monkeypatch):
    """AC3-HP: pending node with mapped project and cwd != work-map root is
    counted in project_cwd_mismatch and listed in project_cwd_mismatch_nodes."""
    mapped_root = "/real/project/root"
    wrong_cwd = "/wrong/cwd"

    node = _make_pending_node("ab-mismatch01", project="my-proj", cwd=wrong_cwd)
    _write_nodes(tmp_graph / "graph.json", [node])

    _map_projects(tmp_graph, monkeypatch, {"my-proj": mapped_root})

    result = runner.invoke(app, ["backlog", "triage", "health", "--json", "--all"])
    assert result.exit_code == 0, result.output
    data = json.loads(result.output)
    assert data["project_cwd_mismatch"] >= 1
    assert "ab-mismatch01" in data["project_cwd_mismatch_nodes"]


@pytest.mark.usefixtures("native_backlog_door")
def test_health_mismatch_ac3_err_unmapped_not_counted(tmp_graph, monkeypatch):
    """AC3-ERR: pending node whose project has no work-map entry is not counted."""
    node = _make_pending_node(
        "ab-unmapped01", project="unknown-proj", cwd="/some/cwd"
    )
    _write_nodes(tmp_graph / "graph.json", [node])

    _map_projects(tmp_graph, monkeypatch, {})

    result = runner.invoke(app, ["backlog", "triage", "health", "--json", "--all"])
    assert result.exit_code == 0, result.output
    data = json.loads(result.output)
    assert data["project_cwd_mismatch"] == 0
    assert data["project_cwd_mismatch_nodes"] == []


@pytest.mark.usefixtures("native_backlog_door")
def test_health_mismatch_ac3_edge_done_not_counted(tmp_graph, monkeypatch):
    """AC3-EDGE: a done node (completed_at set) with historical cwd mismatch
    is NOT counted in project_cwd_mismatch."""
    from datetime import datetime, timezone

    mapped_root = "/real/root"
    wrong_cwd = "/totally/wrong"

    done_node = _make_pending_node(
        "ab-done01",
        project="my-proj",
        cwd=wrong_cwd,
        completed_at=datetime.now(timezone.utc).isoformat(),
    )
    _write_nodes(tmp_graph / "graph.json", [done_node])

    _map_projects(tmp_graph, monkeypatch, {"my-proj": mapped_root})

    result = runner.invoke(app, ["backlog", "triage", "health", "--json", "--all"])
    assert result.exit_code == 0, result.output
    data = json.loads(result.output)
    assert data["project_cwd_mismatch"] == 0


@pytest.mark.usefixtures("native_backlog_door")
def test_health_mismatch_normalization_tilde(tmp_graph, monkeypatch):
    """Normalization edge: cwd stored as ~/x matches abspath expanded root."""
    import os

    home = tmp_graph.parent  # monkeypatched HOME from the tmp_graph fixture
    mapped_root = os.path.join(str(home), "myproject")
    tilde_cwd = "~/myproject"

    node = _make_pending_node("ab-tilde01", project="tilde-proj", cwd=tilde_cwd)
    _write_nodes(tmp_graph / "graph.json", [node])

    # Override HOME so expanduser resolves correctly
    monkeypatch.setenv("HOME", str(home))
    _map_projects(tmp_graph, monkeypatch, {"tilde-proj": mapped_root})

    result = runner.invoke(app, ["backlog", "triage", "health", "--json", "--all"])
    assert result.exit_code == 0, result.output
    data = json.loads(result.output)
    # ~/myproject normalized == mapped_root -> no mismatch
    assert data["project_cwd_mismatch"] == 0


@pytest.mark.usefixtures("native_backlog_door")
def test_health_mismatch_check_exit4(tmp_graph, monkeypatch):
    """--check exits 4 when project_cwd_mismatch breaches threshold=0."""
    mapped_root = "/real/root"
    wrong_cwd = "/wrong/cwd"

    node = _make_pending_node("ab-check01", project="check-proj", cwd=wrong_cwd)
    _write_nodes(tmp_graph / "graph.json", [node])

    _map_projects(tmp_graph, monkeypatch, {"check-proj": mapped_root})
    config = tmp_graph / "config.toml"
    config.write_text(
        config.read_text()
        + "\n[health_monitor]\nenabled = true\n"
        + "[health_monitor.thresholds]\nproject_cwd_mismatch = 0\n"
        + '[health_monitor.notifications]\nsurfaces = ["log_only"]\n'
    )

    result = runner.invoke(
        app, ["backlog", "triage", "health", "--check", "--all"]
    )
    assert result.exit_code == 4, (
        f"expected exit 4 on project_cwd_mismatch breach, got {result.exit_code}\n"
        f"stdout: {result.output}"
    )


# ---------------------------------------------------------------------------
# Evals demand (x-ab72): the health line carries the regression run's age
# ---------------------------------------------------------------------------


def _append_eval_row(path, row):
    with open(path, "a", encoding="utf-8") as fh:
        fh.write(json.dumps(row) + "\n")


@pytest.mark.usefixtures("native_backlog_door")
def test_health_evals_line_carries_age_and_stale(tmp_graph, monkeypatch):
    """A 9-day-old newest regression run reads 'age 9d STALE', not a healthy
    100%: the line demands a fresh run instead of only displaying history."""
    # The native door reads the history under the store's state root.
    hist = tmp_graph / "history" / "evals-history.jsonl"
    hist.parent.mkdir(parents=True, exist_ok=True)
    now = datetime.now(timezone.utc)
    _append_eval_row(hist, {
        "ts": (now - timedelta(days=9)).isoformat().replace("+00:00", "Z"),
        "task_id": "regression-cli-help-smoke", "tier": "regression",
        "pass": True, "reason": "", "duration_s": 1.0, "repeat_index": 0,
        "bank_rev": None, "worker_provider": None,
    })
    monkeypatch.setattr("fno.paths.evals_history", lambda: hist)

    result = runner.invoke(app, ["backlog", "triage", "health", "--all"])
    assert result.exit_code == 0, result.output
    assert "age 9d STALE" in result.stdout


@pytest.mark.usefixtures("native_backlog_door")
def test_health_evals_line_fresh_has_no_stale_marker(tmp_graph, monkeypatch):
    # The native door reads the history under the store's state root.
    hist = tmp_graph / "history" / "evals-history.jsonl"
    hist.parent.mkdir(parents=True, exist_ok=True)
    now = datetime.now(timezone.utc)
    _append_eval_row(hist, {
        "ts": (now - timedelta(days=2)).isoformat().replace("+00:00", "Z"),
        "task_id": "regression-cli-help-smoke", "tier": "regression",
        "pass": True, "reason": "", "duration_s": 1.0, "repeat_index": 0,
        "bank_rev": None, "worker_provider": None,
    })
    monkeypatch.setattr("fno.paths.evals_history", lambda: hist)

    result = runner.invoke(app, ["backlog", "triage", "health", "--all"])
    assert result.exit_code == 0, result.output
    assert "evals:" in result.stdout
    assert "STALE" not in result.stdout
