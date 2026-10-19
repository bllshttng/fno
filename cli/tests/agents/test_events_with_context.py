"""Tests for emit_with_context — Task 1.2 from observability spec.

Locks in:
- emit_with_context flattens all 13 EventContext fields into the record
- ts/kind are last-write-wins (cannot be overridden by ctx or data)
- Open **data kwargs land on top of ctx fields (Locked Decision #1)
"""
from __future__ import annotations

import re
from pathlib import Path
from typing import Any

import pytest

from fno.paths_testing import use_tmpdir
from tests._event_rows import event_rows


EVENT_CONTEXT_FIELD_NAMES = {
    "from_name", "from_provider", "from_session_id", "from_cwd", "from_pid",
    "caller_kind",
    "to_name", "to_provider", "to_cwd", "to_session_id", "transport",
    "request_id", "target_session_id",
}


def _make_ctx(**overrides: Any):
    """Build a baseline EventContext, optionally overriding fields."""
    from fno.agents.context import EventContext

    base = dict(
        from_name="fno",
        from_provider=None,
        from_session_id=None,
        from_cwd="/cwd",
        from_pid=42,
        caller_kind="human_cli",
        to_name="recv",
        to_provider="codex",
        to_cwd=None,
        to_session_id=None,
        transport="direct-cli",
        request_id="abcd" * 8,  # 32 hex chars
        target_session_id=None,
    )
    base.update(overrides)
    return EventContext(**base)


def test_emit_with_context_includes_all_13_context_fields(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """All 13 EventContext fields appear flattened on the JSONL record."""
    use_tmpdir(monkeypatch, tmp_path)
    from fno.agents.events import emit_with_context

    events_path = tmp_path / ".fno" / "events.jsonl"
    ctx = _make_ctx(
        from_name="parent-agent",
        from_provider="claude",
        from_session_id="sess-abc",
        caller_kind="nested_agent",
        to_session_id="recv-sess",
        transport="mcp",
        target_session_id="target-xyz",
    )
    emit_with_context(ctx, "agent_ask_done", path=events_path)

    record = event_rows(events_path)[0]
    for name in EVENT_CONTEXT_FIELD_NAMES:
        assert name in record, f"missing flattened field: {name}"
    assert record["from_name"] == "parent-agent"
    assert record["caller_kind"] == "nested_agent"
    assert record["target_session_id"] == "target-xyz"
    assert record["transport"] == "mcp"


def test_emit_with_context_kwargs_override_context_fields(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """Locked Decision #1: **data lands on top of ctx fields (last-write-wins)."""
    use_tmpdir(monkeypatch, tmp_path)
    from fno.agents.events import emit_with_context

    events_path = tmp_path / ".fno" / "events.jsonl"
    ctx = _make_ctx(to_session_id="from-ctx")
    emit_with_context(
        ctx, "agent_ask_done", path=events_path, to_session_id="from-kwargs"
    )
    record = event_rows(events_path)[0]
    assert record["to_session_id"] == "from-kwargs"


def test_emit_with_context_ts_cannot_be_overridden_via_data(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """``ts`` is last-write-wins; passing ``ts`` in **data cannot shadow it.

    (``kind`` is a named parameter — Python's TypeError on duplicate
    kwargs is the structural defense there; this only covers ``ts``.)
    """
    use_tmpdir(monkeypatch, tmp_path)
    from fno.agents.events import emit_with_context

    events_path = tmp_path / ".fno" / "events.jsonl"
    ctx = _make_ctx()
    emit_with_context(ctx, "agent_ask_done", path=events_path, ts="HIJACK")
    record = event_rows(events_path)[0]
    assert record["kind"] == "agent_ask_done"
    assert record["ts"] != "HIJACK"
    assert re.match(r"^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}Z$", record["ts"])


