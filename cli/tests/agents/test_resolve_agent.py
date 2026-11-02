"""Tests for the shared identifier resolver (x-1b1e US2).

`resolve_agent(token)` accepts one of three address forms — name/slug, full
harness_session_id, or an 8-hex short — for every session-connecting command.
Rust parity for the same matrix lives in crates/fno-agents (US4).
"""
from __future__ import annotations

from pathlib import Path

import pytest

from fno.agents.registry import (
    AgentEntry,
    AgentResolutionError,
    update_registry,
    write_registry,
)


def _claude(name: str, short: str, uuid: str) -> AgentEntry:
    return AgentEntry(
        name=name,
        cwd="/w",
        log_path=f"/tmp/{name}.log",
        short_id=short,
        harness="claude",
        harness_session_id=uuid,
    )


def _write(tmp_path: Path, *entries: AgentEntry) -> Path:
    reg = tmp_path / "registry.json"
    write_registry(list(entries), path=reg)
    return reg


UUID = "7c5dcf5d-c078-4b53-a8c9-7199b831eae4"


def test_update_registry_refuses_name_shadowing_existing_handle(tmp_path: Path) -> None:
    reg = _write(
        tmp_path,
        _claude("first", "transport1", "aaaaaaaa-0000-0000-0000-1111deadbeef"),
    )
    shadow = _claude(
        "deadbeef", "transport2", "bbbbbbbb-0000-0000-0000-222233334444"
    )

    with pytest.raises(AgentResolutionError, match="collides with row 'first'"):
        update_registry(lambda rows: [*rows, shadow], path=reg)


def _codex(name: str, uuid: str, short: str | None = None) -> AgentEntry:
    return AgentEntry(
        name=name,
        cwd="/w",
        log_path=f"/tmp/{name}.log",
        short_id=short or "",
        harness="codex",
        harness_session_id=uuid,
    )


_WINDOW_A = "01a0152f-45fd-78f0-b109-78f8dffdeeca"


def test_update_registry_refuses_second_row_claiming_same_full_session_id(
    tmp_path: Path,
) -> None:
    reg = _write(tmp_path, _codex("gql-codex", _WINDOW_A))
    duplicate = _codex("clone-codex", _WINDOW_A)

    with pytest.raises(AgentResolutionError, match="collides with row 'gql-codex'"):
        update_registry(lambda rows: [*rows, duplicate], path=reg)


