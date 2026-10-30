"""A role refused at the pane door never reaches the runner.

A role is three registry fields (`role_level` / `role_scope` / `role_grantor`).
One live role per scope holds at every in-process spawn door, so a duplicate
refuses before anything launches.
"""
from __future__ import annotations

from pathlib import Path

import pytest

from fno.agents.registry import AgentEntry, update_registry
from fno.paths_testing import use_tmpdir


@pytest.fixture(autouse=True)
def _clear_parent_markers(monkeypatch):
    for marker in (
        "FNO_SESSION",
        "CODEX_THREAD_ID",
        "CLAUDE_CODE_SESSION_ID",
        "CODEX_SESSION_ID",
        "GEMINI_SESSION_ID",
    ):
        monkeypatch.delenv(marker, raising=False)


def test_dispatch_spawn_pane_refuses_a_duplicate_role_before_launch(
    tmp_path: Path, monkeypatch, native_backlog_door
) -> None:
    """Same guard as the bg door: a live holder over the requested scope means
    the runner must never be reached, whether the launch was refused for a bad
    value or for a scope another live row already holds."""
    use_tmpdir(monkeypatch, tmp_path)
    from fno.agents.dispatch import DispatchAskError
    from fno.agents.mux_spawn import dispatch_spawn_pane

    update_registry(
        lambda rows: rows
        + [
            AgentEntry(
                name="sitting-lead",
                cwd=str(tmp_path),
                log_path="",
                harness="claude",
                harness_session_id="sess-sitting-lead",
                status="busy",
                role_level=2,
                role_scope="epic-x",
                role_grantor="human",
            )
        ]
    )

    def _explode(*a, **k):
        raise AssertionError("a refused role must not reach the pane runner")

    with pytest.raises(DispatchAskError) as exc:
        dispatch_spawn_pane(
            name="pretender",
            message="term",
            provider="claude",
            cwd=tmp_path,
            runner=_explode,
            role_level=2,
            role_scope="epic-x",
        )
    assert exc.value.exit_code == 2
    assert "--hand-off" in str(exc.value)
