"""orchestrator.emit_status_event - the do-phase task_done/blocked boundary emit.

The orchestrator shells `fno doctor event emit` (skills stay self-contained, never
import repo code). These tests cover the argv it builds and the non-fatal
contract: an emit failure logs and returns False, never raising.
"""
from __future__ import annotations

import importlib.util
from pathlib import Path

REPO = Path(__file__).resolve().parents[3]


def _load_orch():
    spec = importlib.util.spec_from_file_location(
        "do_orchestrator", REPO / "skills/execute/orchestrator.py"
    )
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


class _Result:
    def __init__(self, rc: int = 0) -> None:
        self.returncode = rc
        self.stderr = b""


def test_emit_status_event_builds_argv(monkeypatch) -> None:
    orch = _load_orch()
    captured: dict = {}

    def fake_run(argv, **kw):
        captured["argv"] = argv
        return _Result(0)

    monkeypatch.setattr(orch.subprocess, "run", fake_run)
    ok = orch.emit_status_event(
        "task_done", run="R1", node="prj-0001", task="2.1", outcome="SUCCESS",
        data={"commit": "abc"},
    )
    assert ok is True
    argv = captured["argv"]
    assert argv[:6] == ["fno", "doctor", "event", "emit", "-t", "task_done"]
    for flag, val in (("--run", "R1"), ("--node", "prj-0001"), ("--task", "2.1"), ("--outcome", "SUCCESS")):
        assert flag in argv and val in argv


def test_emit_status_event_nonfatal_when_fno_missing(monkeypatch) -> None:
    orch = _load_orch()

    def boom(*a, **k):
        raise FileNotFoundError()

    monkeypatch.setattr(orch.subprocess, "run", boom)
    assert orch.emit_status_event("blocked", run="R1", data={"reason": "x"}) is False


# -- task claim settlement at the boundary (x-09d7 group 3) --


def test_boundary_success_outcome_releases_done(monkeypatch) -> None:
    """task_done + SUCCESS/DONE_WITH_CONCERNS settles the claim to done."""
    orch = _load_orch()
    calls: list[list[str]] = []

    def fake_run(argv, **kw):
        calls.append(list(argv))
        return _Result(0)

    monkeypatch.setattr(orch.subprocess, "run", fake_run)
    assert orch.release_task_claim_at_boundary("x-t1", "1.1", "SUCCESS") is True
    assert orch.release_task_claim_at_boundary("x-t1", "1.1", "DONE_WITH_CONCERNS") is True
    for argv in calls:
        assert argv[:6] == ["fno", "backlog", "task", "update", "x-t1", "1.1"]
        assert argv[-2:] == ["--status", "done"]


def test_boundary_failed_or_blocked_gives_back_pending(monkeypatch) -> None:
    """FAILED and BLOCKED outcomes give the task back to pending so the next
    ready worker can claim it."""
    orch = _load_orch()
    calls: list[list[str]] = []

    def fake_run(argv, **kw):
        calls.append(list(argv))
        return _Result(0)

    monkeypatch.setattr(orch.subprocess, "run", fake_run)
    assert orch.release_task_claim_at_boundary("x-t1", "2.2", "FAILED") is True
    assert orch.release_task_claim_at_boundary("x-t1", "2.2", "BLOCKED") is True
    for argv in calls:
        assert argv[-2:] == ["--status", "pending"]


def test_unknown_outcome_settles_nothing(monkeypatch) -> None:
    """The outcome vocabulary is closed: an unrecognized spelling (lowercase
    success, PARTIAL) runs NO subprocess - mapping strays to the give-back
    would release a finished task's claim mid-flight."""
    orch = _load_orch()
    called: list[list[str]] = []

    def fake_run(argv, **kw):
        called.append(list(argv))
        return _Result(0)

    monkeypatch.setattr(orch.subprocess, "run", fake_run)
    assert orch.release_task_claim_at_boundary("x-t1", "1.1", "PARTIAL") is False
    assert orch.release_task_claim_at_boundary("x-t1", "1.1", "success") is False
    assert called == [], "an unknown outcome must not touch the claim"
    # Positive control: a known outcome through the same path runs.
    assert orch.release_task_claim_at_boundary("x-t1", "1.1", "SUCCESS") is True
    assert called and called[0][-2:] == ["--status", "done"]


def test_the_manifest_walk_never_leaves_the_repo(tmp_path, monkeypatch) -> None:
    """An unbounded walk finds a STRANGER's manifest.

    Task ids like "1.1" exist in nearly every plan, so settling against the
    first `.fno/target-state.md` above cwd marks a task done on an unrelated
    node. With no repo between cwd and the filesystem root there is no node to
    settle, and saying so is the only safe answer.
    """
    orch = _load_orch()
    monkeypatch.delenv("FNO_REPO_ROOT", raising=False)

    outer = tmp_path / "outer"
    (outer / ".fno").mkdir(parents=True)
    (outer / ".fno" / "target-state.md").write_text(
        "graph_node_id: x-STRANGER\n", encoding="utf-8"
    )
    deep = outer / "inner" / "deep"
    deep.mkdir(parents=True)

    monkeypatch.chdir(deep)
    assert orch.manifest_graph_node_id() == "", (
        "no .git between cwd and / means no repo, so no node"
    )

    (outer / ".git").mkdir()
    assert orch.manifest_graph_node_id() == "x-STRANGER", (
        "positive control: once outer IS a repo, its manifest is the right one"
    )
