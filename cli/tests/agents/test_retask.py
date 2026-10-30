"""The thread viewport door: the Python half the native front still shares
with mail --force. The retask transaction's own contracts live in the Rust
suite (crates/fno-agents/src/retask/front/tests.rs)."""

from __future__ import annotations

import json
from types import SimpleNamespace

from fno.agents import retask
from fno.agents.registry import AgentEntry


def _row(**overrides) -> AgentEntry:
    values = {
        "name": "bp-xbdb9-retask",
        "cwd": "/repo",
        "log_path": "",
        "harness": "codex",
        "provider": None,
        "model": "gpt-5.6-sol",
        "effort": "high",
        "harness_session_id": "old-session",
        "substrate": "pane",
        "mux": {"session": "main", "pane_id": 12},
    }
    values.update(overrides)
    return AgentEntry(**values)



def test_thread_viewport_reaches_by_registry_name_and_joins_the_opened_pane(
    monkeypatch,
) -> None:
    """AC1-HP: the door is keyed by the row name; the join still matches fno_id."""
    entry = _row(harness="claude", substrate="thread", mux=None, fno_id="F", name="bp-x")
    calls: list[list[str]] = []

    def run(argv, **_kwargs):
        calls.append(list(argv))
        if "thread" in argv:
            return SimpleNamespace(returncode=0, stdout="", stderr="")
        return SimpleNamespace(
            returncode=0,
            stdout=json.dumps([{"name": "bp-x", "fno_id": "F", "pane_id": 7}]),
            stderr="",
        )

    monkeypatch.setattr(retask, "resolve_mux_session", lambda *_args, **_kwargs: "main")
    monkeypatch.setattr(retask.subprocess, "run", run)

    assert retask.resolve_thread_viewport(entry) == ("main", 7)
    # The reach carries no baked placement: no flag tunes the row's open
    # portal, else the door's own portal 0 serves it.
    assert calls[0] == [
        "fno", "mux", "thread", "bp-x", "--server", "main",
    ]
