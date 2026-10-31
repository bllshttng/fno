"""`fno agents spawn --promote --substrate bg`: the role rides the bg substrate.

A role is three registry fields (`role_level` / `role_scope` / `role_grantor`)
and nothing in it needs a PTY. The substrate axis it actually cares about is
TERM LENGTH: a lead must outlive the grant. `bg` qualifies - a bg worker is a
full persistent conversation in claude's agent view, attachable and resumable,
differing from a pane only in who draws it. `headless` does not: it answers once
and exits, so its role is orphaned at birth.

These tests exercise the END-TO-END CLI path (`spawn --promote --substrate bg`),
not `_claude_create_path` in isolation. That is deliberate: the original defect
was a refusal at the CLI seam sitting in front of unplumbed params, so a test
that called the helper directly would have passed against the broken build.
"""
from __future__ import annotations

from pathlib import Path

import pytest
from typer.testing import CliRunner

from fno.agents.registry import AgentEntry, load_registry, update_registry
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


def _spawn(*args: str):
    from fno.agents.cli import agents_app

    return CliRunner().invoke(agents_app, list(args), catch_exceptions=False)


def _row(name: str) -> AgentEntry:
    entry = next((e for e in load_registry() if e.name == name), None)
    assert entry is not None, f"no registry row named {name!r}"
    return entry


# --- the role lands on bg ---------------------------------------------------


# --- one live role per scope, enforced on bg too ----------------------------


# --- headless stays refused --------------------------------------------------


# --- in-process callers get the same guards ----------------------------------


# --- role values are validated on every writer, not just the CLI ------------
#
# The CLI parses `--promote` through _parse_role, but both spawn dispatchers take
# (level, scope) directly from in-process callers. A value that skips validation
# is written to the SHARED registry: Rust's role_level is Option<u32>
# (crates/fno-agents/src/state.rs), so a negative or boolean level breaks
# registry reads for every reader, not just the caller that wrote it.


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
