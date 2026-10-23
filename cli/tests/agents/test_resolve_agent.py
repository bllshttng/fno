"""Tests for the shared identifier resolver (x-1b1e US2).

`resolve_agent(token)` accepts one of three address forms — name/slug, full
harness_session_id, or an 8-hex short — for every session-connecting command.
Rust parity for the same matrix lives in crates/fno-agents (US4).
"""
from __future__ import annotations

import json
from pathlib import Path

import pytest

from fno.agents.registry import (
    AgentEntry,
    AgentResolutionError,
    resolve_agent,
    resolve_agent_in,
    update_registry,
    write_registry,
)
from tests._table_seed import seed_legacy_registry


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


def test_ac1_hp_all_three_forms_resolve_same_entry(tmp_path: Path) -> None:
    """AC1-HP: name, the row's fno_id, full uuid, and 8-hex short all resolve
    to one entry - either identity key finds the same row."""
    from dataclasses import replace

    fno_id = "0f6a4b2e-9c1d-4e5f-8a7b-3c2d1e0f9a8b"
    reg = _write(tmp_path, replace(_claude("billing", "7c5dcf5d", UUID), fno_id=fno_id))
    for token in ("billing", fno_id, UUID, "7c5dcf5d"):
        r = resolve_agent(token, path=reg)
        assert r.entry.name == "billing"
        assert r.worker_short_id == "7c5dcf5d"
    assert resolve_agent(UUID, path=reg).entry.fno_id == fno_id


def test_ac2_hp_daemon_short_and_canonical_handle_both_resolve(tmp_path: Path) -> None:
    """AC2-HP: a codex row resolves by its daemon short_id (name-derived,
    non-hex) AND by the canonical first-eight of its thread id."""
    codex_uuid = "a1b2c3d4-1111-2222-3333-444455556666"
    codex = AgentEntry(
        name="reviewer",
        cwd="/w",
        log_path="/tmp/r.log",
        short_id="billingf",  # daemon name-derived key (not hex)
        harness="codex",
        harness_session_id=codex_uuid,
    )
    reg = _write(tmp_path, codex)
    assert resolve_agent("billingf", path=reg).matched_by == "short_id"
    assert resolve_agent("a1b2c3d4", path=reg).matched_by == "canonical_handle"


def test_ac2_hp_opencode_canonical_handle_preserves_case() -> None:
    ses = "ses_7F3a9b2cAbCd1234"
    row = AgentEntry(
        name="oc", harness="opencode", harness_session_id=ses, cwd="/w", log_path="/l"
    )
    assert resolve_agent_in([row], "ses_7F3a").matched_by == "canonical_handle"
    with pytest.raises(AgentResolutionError):
        resolve_agent_in([row], "ses_7f3a")


def test_update_registry_allows_first_eight_overlap_between_sessions(
    tmp_path: Path,
) -> None:
    """Two rows whose sids share first-eight but name different sessions both
    land: the overlap (any harness, claude included) is read-side ambiguity,
    not a write collision. Codex hits this on every tiled same-window spawn."""
    reg = _write(
        tmp_path,
        _claude("first", "transport1", "aaaaaaaa-0000-0000-0000-111111111111"),
    )
    second = _claude(
        "second", "transport2", "aaaaaaaa-0000-0000-0000-222222222222"
    )

    persisted = update_registry(lambda rows: [*rows, second], path=reg)

    assert [entry.name for entry in persisted] == ["first", "second"]
    with pytest.raises(AgentResolutionError, match="ambiguous"):
        resolve_agent("aaaaaaaa", path=reg)
    by_full = resolve_agent(
        "aaaaaaaa-0000-0000-0000-222222222222", path=reg
    )
    assert by_full.entry.name == "second"


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


def test_ac1_edge_hex_shaped_name_and_short_id_are_ambiguous(tmp_path: Path) -> None:
    """AC1-EDGE: a name cannot silently displace another row's short id."""
    row_named = _claude("deadbeef", "aaaa0000", "aaaa0000-0000-0000-0000-000000000000")
    row_short = _claude("other", "deadbeef", "deadbeef-1111-1111-1111-111111111111")
    reg = seed_legacy_registry([row_named, row_short], tmp_path / "registry.json")
    with pytest.raises(AgentResolutionError, match="ambiguous"):
        resolve_agent("deadbeef", path=reg)


def test_same_row_matching_multiple_address_categories_is_not_ambiguous() -> None:
    row = _claude(
        "deadbeef", "deadbeef", "deadbeef-0000-0000-0000-0000deadbeef"
    )
    resolved = resolve_agent_in([row], "deadbeef")
    assert resolved.entry.name == "deadbeef"
    assert resolved.matched_by == "name"


def test_duplicate_name_rows_with_distinct_sessions_are_ambiguous() -> None:
    first = _claude(
        "same", "transport1", "aaaaaaaa-1111-7222-8333-4444deadbeef"
    )
    second = _claude(
        "same", "transport2", "bbbbbbbb-1111-7222-8333-4444cafefeed"
    )

    with pytest.raises(AgentResolutionError, match="ambiguous across 2 agents"):
        resolve_agent_in([first, second], "same")


def test_exact_full_session_id_wins_over_short_address_categories() -> None:
    full = _claude("full", "transport1", "deadbeef")
    named = _claude(
        "deadbeef", "transport2", "aaaaaaaa-0000-0000-0000-000000000000"
    )
    resolved = resolve_agent_in([named, full], "deadbeef")
    assert resolved.entry.name == "full"
    assert resolved.matched_by == "full_session_id"


def test_ac1_err_unknown_token_lists_accepted_forms(tmp_path: Path) -> None:
    reg = _write(tmp_path, _claude("billing", "7c5dcf5d", UUID))
    with pytest.raises(AgentResolutionError) as exc:
        resolve_agent("does-not-exist", path=reg)
    msg = str(exc.value)
    assert "does-not-exist" in msg
    assert "name" in msg and "short id" in msg and "session id" in msg
    assert exc.value.exit_code == 2


def test_ac3_err_unreadable_registry_degrades_cleanly(tmp_path: Path) -> None:
    """AC3-ERR: a malformed registry raises AgentResolutionError, never a
    traceback, and carries exit 2."""
    reg = tmp_path / "registry.json"
    reg.write_text("{not json", encoding="utf-8")
    with pytest.raises(AgentResolutionError) as exc:
        resolve_agent("billing", path=reg)
    assert exc.value.exit_code == 2


def test_registry_name_and_persisted_alias_share_one_namespace(
    tmp_path: Path, monkeypatch
) -> None:
    """A registry name cannot hide another session's persisted alias."""
    from fno.agents import discover

    registered = _claude("friendly", "transport", UUID)
    alias_sid = "aaaaaaaa-1111-7222-8333-444455556666"
    alias_map = tmp_path / "session-names.json"
    alias_map.write_text(
        json.dumps({alias_sid: "friendly"}),
        encoding="utf-8",
    )
    monkeypatch.setattr(discover, "default_name_map_path", lambda: alias_map)
    reg = _write(tmp_path, registered)

    with pytest.raises(AgentResolutionError, match=alias_sid):
        resolve_agent("friendly", path=reg)
