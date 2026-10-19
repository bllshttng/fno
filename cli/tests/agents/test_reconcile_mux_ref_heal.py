"""Reconcile clears a structurally impossible mux ref, and says so.

A wrong value is worse than no value: 49 of 51 live registry rows carry
``mux: None`` and every reader handles that correctly, while the six rows
carrying ``{"session": "main", "pane_id": 0}`` were unreachable in every mux
path. So the heal deletes the ref rather than teaching each reader to
tolerate it. It clears only STRUCTURALLY impossible refs -- pane ids allocate
from a floor of 1 -- never a ref that names a real pane, and never one that
merely could not be probed.
"""
from __future__ import annotations

from pathlib import Path

import pytest

from fno.agents import events
from fno.agents.dispatch import reconcile_agents
from fno.agents.registry import AgentEntry, load_registry, update_registry

ZERO_REF = {"session": "main", "pane_id": 0}


@pytest.fixture
def isolated_registry(tmp_path: Path, monkeypatch) -> Path:
    """Point the registry at a clean tmp_path for each test."""
    from fno import paths

    registry_path = tmp_path / "registry.jsonl"
    monkeypatch.setattr(paths, "agents_registry_path", lambda: registry_path)
    return registry_path


def _seed_codex(name: str, *, mux, hsid: str) -> None:
    entry = AgentEntry(
        name=name,
        harness="codex",
        cwd=str(Path.cwd()),
        log_path=str(Path.cwd() / f"{name}.log"),
        harness_session_id=hsid,
        mux=mux,
        status="live",
        last_message_at="2026-05-21T00:00:00Z",
    )
    update_registry(lambda entries: entries + [entry])


def _patch_codex_known(monkeypatch, ids: set[str]) -> None:
    from fno.agents.harnesses import codex as codex_mod

    monkeypatch.setattr(codex_mod, "session_index_exists", lambda **_: True)
    monkeypatch.setattr(codex_mod, "load_known_session_ids", lambda **_: ids)


def test_a_pane_zero_ref_is_cleared_and_named(isolated_registry, monkeypatch) -> None:
    """AC6-HP: the damaged shape heals to mux None, one event per ref.

    No other field moves: status stays live, the recorded identity and pid
    are untouched. The event names exactly what was cleared so the heal is
    auditable from events.jsonl alone.
    """
    names = [f"t-zero-ref-{i}" for i in range(6)]
    hsids = [f"{i:08d}-aaaa" for i in range(6)]
    for name, hsid in zip(names, hsids):
        _seed_codex(name, mux=ZERO_REF, hsid=hsid)
    _patch_codex_known(monkeypatch, set(hsids))

    emitted: list[tuple] = []
    monkeypatch.setattr(events, "emit", lambda evt, **kw: emitted.append((evt, kw)))

    result = reconcile_agents()

    rows = {r.name: r for r in load_registry()}
    for name, hsid in zip(names, hsids):
        row = rows[name]
        assert row.mux is None, f"{name} must heal to a null ref"
        assert row.status == "live"
        assert row.harness_session_id == hsid
        assert row.pid is None
    assert {c["name"] for c in result.mux_cleared} == set(names)
    ref_events = [kw for evt, kw in emitted if evt == "mux_ref_cleared"]
    assert {kw["name"] for kw in ref_events} == set(names)
    assert all(kw["cleared_mux"] == ZERO_REF for kw in ref_events)
    assert len(result.errors) == 0


def test_a_ref_that_names_a_real_pane_is_never_cleared(
    isolated_registry, monkeypatch
) -> None:
    """AC8-INV: reconcile clears refs that are structurally impossible,
    never refs it merely could not probe."""
    real_ref = {"session": "main", "pane_id": 1005}
    _seed_codex("real-ref", mux=real_ref, hsid="22222222-aaaa")
    _patch_codex_known(monkeypatch, {"22222222-aaaa"})

    result = reconcile_agents()

    assert load_registry()[0].mux == real_ref
    assert result.mux_cleared == []


def test_a_failed_write_reports_the_heal_as_an_error(
    isolated_registry, monkeypatch
) -> None:
    """A heal that never committed must not claim it cleared anything."""
    from fno.agents import dispatch as dispatch_mod

    _seed_codex("unhealed", mux=ZERO_REF, hsid="33333333-aaaa")
    _patch_codex_known(monkeypatch, {"33333333-aaaa"})

    def failing_update(_):
        raise OSError("simulated disk-full")

    monkeypatch.setattr(dispatch_mod, "update_registry", failing_update)

    result = reconcile_agents()

    assert result.mux_cleared == []
    assert load_registry()[0].mux == ZERO_REF
    assert any(
        e["name"] == "unhealed" and "registry-write-failed" in e["reason"]
        for e in result.errors
    )
