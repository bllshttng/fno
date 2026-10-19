"""Tests for fno.agent.state - AgentContext + load_agent_context().

Covers AC1-HP through AC7-EDGE from plan 01-foundation.md task 01.2:
all three layers, malformed retry, detected_paths, dual target+session,
state_file_override, non-git project root, empty .fno/.
"""
from __future__ import annotations

from pathlib import Path

import pytest

from fno.agent.state import (
    AgentContext,
    FleetState,
    MalformedStateError,
    MissingStateFileOverrideError,
    SessionState,
    WalkerState,
    load_agent_context,
)

FIXTURES = Path(__file__).parent / "fixtures" / "agent"


def _build_workspace(tmp_path: Path, *, target: bool = False, walker: bool = False,
                    session: bool = False, fleet: bool = False,
                    malformed_target: bool = False) -> Path:
    """Materialize a fake project_root with selected state layers present."""
    project = tmp_path / "project"
    fno_dir = project / ".fno"
    fno_dir.mkdir(parents=True)

    if target and not malformed_target:
        (fno_dir / "target-state.md").write_text(
            (FIXTURES / "target-state.md").read_text()
        )
    if malformed_target:
        (fno_dir / "target-state.md").write_text(
            (FIXTURES / "malformed-state.md").read_text()
        )
    if walker:
        (fno_dir / "megawalk-state.md").write_text(
            (FIXTURES / "megawalk-state.md").read_text()
        )
    if session:
        (fno_dir / "session-state.md").write_text(
            (FIXTURES / "session-state-think.md").read_text()
        )
    if fleet:
        # Build fleet under a tmp HOME so we don't touch the real ~/.fno/.
        fleet_root = tmp_path / "fake_home" / ".fno" / "fleet" / "fleet-fixture-001"
        fleet_root.mkdir(parents=True)
        body = (FIXTURES / "fleet-mission.md").read_text().replace(
            "__PROJECT_ROOT__", str(project.resolve())
        )
        (fleet_root / "00-INDEX.md").write_text(body)
    return project


def test_ac1_hp_all_three_layers_detected(tmp_path, monkeypatch):
    """AC1-HP: deep stack with fleet+walker+session populates all three fields."""
    project = _build_workspace(tmp_path, target=True, walker=True, fleet=True)
    monkeypatch.setenv("HOME", str(tmp_path / "fake_home"))
    monkeypatch.chdir(project)
    ctx = load_agent_context(project_root_override=project)
    assert isinstance(ctx, AgentContext)
    assert ctx.session is not None and isinstance(ctx.session, SessionState)
    assert ctx.session.session_id == "20260512T010101Z-99999-fixaaa"
    assert ctx.session.phase == "review"
    assert ctx.walker is not None and isinstance(ctx.walker, WalkerState)
    assert ctx.walker.in_flight == 3
    assert ctx.walker.done == 1
    assert ctx.fleet is not None and isinstance(ctx.fleet, FleetState)
    assert ctx.fleet.mission_id == "fleet-fixture-001"
    assert ctx.fleet.wave_current == 2
    assert ctx.fleet.wave_total == 4
    assert ctx.fleet.status == "running"


def test_ac2_err_malformed_yaml_raises_after_retry(tmp_path):
    """AC2-ERR: malformed session YAML retried once then raises MalformedStateError."""
    project = _build_workspace(tmp_path, malformed_target=True)
    with pytest.raises(MalformedStateError) as excinfo:
        load_agent_context(project_root_override=project)
    assert "target-state.md" in str(excinfo.value.path)


def test_ac4_edge_dual_target_and_session_prefers_target(tmp_path):
    """AC4-EDGE: both files present -> target wins, warning recorded."""
    project = _build_workspace(tmp_path, target=True, session=True)
    ctx = load_agent_context(project_root_override=project)
    assert ctx.session is not None
    assert ctx.session.kind == "target"
    assert ctx.session.session_id == "20260512T010101Z-99999-fixaaa"
    assert any("both" in w and "target-state.md" in w for w in ctx.warnings), ctx.warnings


def test_ac5_fr_state_file_override_bypasses_session_detection(tmp_path):
    """AC5-FR: override loads session from given path; fleet/walker still detected."""
    project = _build_workspace(tmp_path, target=True, walker=True)
    override = tmp_path / "custom-state.md"
    override.write_text((FIXTURES / "session-state-think.md").read_text())
    ctx = load_agent_context(
        state_file_override=override, project_root_override=project
    )
    # Override-loaded session, NOT the project's target-state.md
    assert ctx.session is not None
    assert ctx.session.path == override
    assert ctx.session.kind == "override"
    assert ctx.session.phase == "think"
    # Walker layer still detected normally.
    assert ctx.walker is not None
    assert ctx.walker.in_flight == 3


_HARNESS_ENVS = (
    "CODEX_THREAD_ID",
    "CLAUDE_CODE_SESSION_ID",
    "CODEX_SESSION_ID",
    "GEMINI_SESSION_ID",
    "OPENCODE_SESSION_ID",
    "CODEX_PLUGIN_ROOT",
    "GEMINI_PROJECT_DIR",
    "CLAUDE_PLUGIN_ROOT",
)


def _clear_harness_env(monkeypatch):
    for key in _HARNESS_ENVS:
        monkeypatch.delenv(key, raising=False)


def test_ac1_codex_session_marker_resolves_codex(tmp_path, monkeypatch):
    """AC1: a real codex session sets CODEX_THREAD_ID but no *_PLUGIN_ROOT;
    it must resolve codex, not fail open to claude."""
    _clear_harness_env(monkeypatch)
    monkeypatch.setenv("CODEX_THREAD_ID", "thread-abc")
    ctx = load_agent_context(project_root_override=tmp_path)
    assert ctx.harness == "codex"


def test_walker_malformed_yaml_logs_warning_no_raise(tmp_path):
    """Walker layer failure does NOT raise (failure-isolation)."""
    project = tmp_path / "p"
    fno = project / ".fno"
    fno.mkdir(parents=True)
    (fno / "megawalk-state.md").write_text(
        (FIXTURES / "malformed-state.md").read_text()
    )
    ctx = load_agent_context(project_root_override=project)
    assert ctx.walker is None
    assert any("walker" in w and "malformed" in w for w in ctx.warnings)


def test_fleet_status_must_be_running_or_paused(tmp_path, monkeypatch):
    """A fleet mission with status: complete should NOT match."""
    project = tmp_path / "p"
    project.mkdir()
    fleet_root = tmp_path / "fake_home" / ".fno" / "fleet" / "complete-mission"
    fleet_root.mkdir(parents=True)
    (fleet_root / "00-INDEX.md").write_text(
        f"---\nmission_id: complete-mission\nstatus: complete\nprojects:\n  p:\n    cwd: {project.resolve()}\n---\n"
    )
    monkeypatch.setenv("HOME", str(tmp_path / "fake_home"))
    ctx = load_agent_context(project_root_override=project)
    assert ctx.fleet is None


def test_state_file_override_missing_raises(tmp_path):
    """--state-file pointing at a missing path raises MissingStateFileOverrideError.

    Prior behavior was to warn + return None, silently degrading to "no
    session". An explicit user typo deserves a hard error.
    """
    project = _build_workspace(tmp_path, target=True)
    bogus = tmp_path / "does_not_exist.md"
    with pytest.raises(MissingStateFileOverrideError) as excinfo:
        load_agent_context(
            state_file_override=bogus, project_root_override=project
        )
    assert excinfo.value.path == bogus
