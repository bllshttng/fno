from __future__ import annotations

from pathlib import Path

import pytest


def test_cursor_agent_refuses_native_worktree_passthrough():
    from fno.agents.mux_spawn import DispatchAskError, build_pane_argv

    chat_id = "fadad56b-8008-45f5-b809-f9fab7074534"
    with pytest.raises(DispatchAskError, match="native worktree"):
        build_pane_argv(
            "cursor-agent",
            "",
            Path("/tmp/worktree"),
            False,
            chat_id,
            passthrough=["-w", "native"],
        )


def test_cursor_agent_permission_mappings_fail_closed():
    from fno.agents.mux_spawn import DispatchAskError, permission_pane_tokens

    assert permission_pane_tokens("cursor-agent", "force") == ["--force"]
    with pytest.raises(DispatchAskError, match="unmappable"):
        permission_pane_tokens("cursor-agent", "auto")
