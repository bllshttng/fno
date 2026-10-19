"""Tests for the batched update_registry write in reconcile_agents.

reconcile_agents accumulates status flips and applies them all via one
``update_registry`` call, so N flips cost one atomic write cycle.

- AC3-HP: ``update_registry`` is called at most ONCE per reconcile.
- AC3-ERR: registry-write failure routes every queued name to errors
  rather than splitting orphaned/recovered/errors.
"""
from __future__ import annotations

from dataclasses import replace
from pathlib import Path
from typing import Callable

import pytest

from fno.agents.dispatch import reconcile_agents
from fno.agents.registry import (
    AgentEntry,
    update_registry,
)


@pytest.fixture
def isolated_registry(tmp_path: Path, monkeypatch) -> Path:
    """Point the registry at a clean tmp_path for each test."""
    from fno import paths
    registry_path = tmp_path / "registry.jsonl"
    monkeypatch.setattr(paths, "agents_registry_path", lambda: registry_path)
    return registry_path


def _seed_codex(name: str, *, status: str, session_id: str) -> AgentEntry:
    entry = AgentEntry(
        name=name,
        harness="codex",
        cwd=str(Path.cwd()),
        log_path=str(Path.cwd() / f"{name}.log"),
        harness_session_id=session_id,
        status=status,
        last_message_at="2026-05-21T00:00:00Z",
    )
    update_registry(lambda entries: entries + [entry])
    return entry


def _patch_codex_known(monkeypatch, ids: set[str]) -> None:
    """Stub codex's session index loader to return ``ids``."""
    from fno.agents.harnesses import codex as codex_mod
    monkeypatch.setattr(codex_mod, "session_index_exists", lambda **_: True)
    monkeypatch.setattr(
        codex_mod, "load_known_session_ids", lambda **_: ids
    )


def _count_update_calls(monkeypatch) -> Callable[[], int]:
    """Patch ``update_registry`` to count its invocations.

    Returns a callable that yields the current count.
    """
    from fno.agents import dispatch as dispatch_mod
    real = dispatch_mod.update_registry
    counter = {"n": 0}

    def counted(updater):
        counter["n"] += 1
        return real(updater)

    monkeypatch.setattr(dispatch_mod, "update_registry", counted)
    return lambda: counter["n"]


def test_mixed_orphan_and_recovered_writes_once(
    isolated_registry: Path, monkeypatch
) -> None:
    """A registry with both directions queued still writes only once."""
    # Start in opposite-of-truth states: live ones that are unreachable,
    # orphaned ones that ARE reachable.
    _seed_codex("alive-but-orphan", status="orphaned",
                session_id="11111111-1111-1111-1111-111111111111")  # reachable
    _seed_codex("dead-but-live", status="live",
                session_id="22222222-2222-2222-2222-222222222222")  # NOT reachable

    _patch_codex_known(
        monkeypatch, {"11111111-1111-1111-1111-111111111111"}
    )
    count = _count_update_calls(monkeypatch)

    result = reconcile_agents()
    assert count() == 1
    assert len(result.orphaned) == 1
    assert len(result.recovered) == 1


def test_status_flip_is_compare_and_set_against_same_name_replacement(
    isolated_registry: Path, monkeypatch
) -> None:
    """A probed row cannot overwrite a replacement that reused its name."""
    original = _seed_codex(
        "same", status="live",
        session_id="11111111-1111-1111-1111-111111111111",
    )
    _patch_codex_known(monkeypatch, set())

    from fno.agents import dispatch as dispatch_mod
    from fno.agents.registry import load_registry

    real_update = dispatch_mod.update_registry
    injected = False

    def replace_then_apply(updater):
        nonlocal injected
        if not injected:
            injected = True
            real_update(lambda _rows: [replace(
                original, harness="claude", harness_session_id="replacement", status="exited"
            )])
        return real_update(updater)

    monkeypatch.setattr(dispatch_mod, "update_registry", replace_then_apply)
    result = reconcile_agents()

    row = load_registry()[0]
    assert row.harness == "claude"
    assert row.status == "exited"
    assert result.orphaned == []
    assert any(e["reason"] == "registry-status-update-raced" for e in result.errors)


def test_write_failure_routes_every_queued_name_to_errors(
    isolated_registry: Path, monkeypatch
) -> None:
    """AC3-ERR (write-failure leg): if the single batched write fails,
    all queued changes show up in ``errors`` not ``orphaned``/``recovered``.

    The atomicity contract is: ALL pending updates commit, or NONE.
    A write failure means none committed, so the caller MUST see them
    as errors so the operator does not act on stale state.
    """
    _seed_codex("x", status="live",
                session_id="11111111-1111-1111-1111-111111111111")  # would orphan
    _seed_codex("y", status="orphaned",
                session_id="22222222-2222-2222-2222-222222222222")  # would recover

    _patch_codex_known(
        monkeypatch, {"22222222-2222-2222-2222-222222222222"}
    )

    # Stub update_registry to raise OSError on call.
    from fno.agents import dispatch as dispatch_mod

    def failing_update(_):
        raise OSError("simulated disk-full")

    monkeypatch.setattr(dispatch_mod, "update_registry", failing_update)

    result = reconcile_agents()
    assert result.orphaned == [], "failed write must not appear in orphaned"
    assert result.recovered == [], "failed write must not appear in recovered"
    assert len(result.errors) == 2
    error_names = {e["name"] for e in result.errors}
    assert error_names == {"x", "y"}
    for err in result.errors:
        assert "registry-write-failed" in err["reason"]
        assert "simulated disk-full" in err["reason"]
