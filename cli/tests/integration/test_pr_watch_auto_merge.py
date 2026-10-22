"""Integration: a parked granted worker's PR merges through the watcher.

The journey this file proves (end to end through the REAL drain; the
authorized-merge door is faked at ``command_call``):

1. CI pending                              -> the door answers held and the
                                              drain emits the held phase; no
                                              second attempt is spent
2. CI turns green, the next drain runs     -> the door answers merged and
                                              the positive events name the
                                              actor, the grant session, the
                                              PR, and the node

The verdict arms themselves live in crates/fno-agents/src/merge_grant.rs;
these tests stub the door to a granted verdict and prove the drain.
"""
from __future__ import annotations

from pathlib import Path

import pytest

from fno.pr_watch._dispatch import run_execute_queue
from fno.pr_watch._discover import PrCandidate
from fno.pr_watch._state import WatermarkStore

NODE = "x-watch0001"
PR = 7
SLUG = "owner/repo"
GRANT = {"source": "config", "recorded_by": "spawner-session",
         "recorded_at": "2026-08-24T10:00:00Z"}


@pytest.fixture(autouse=True)
def _free_gh_budget(monkeypatch):
    """The drain's fleet-budget read must never answer from the real ledger
    inside a test."""
    import fno.pr_watch._dispatch as _dispatch_mod

    monkeypatch.setattr(_dispatch_mod, "_gh_budget_backoff_left", lambda: 0.0)


def _arm_world(
    monkeypatch,
    tmp_path: Path,
    *,
    checks_pending: bool,
) -> None:
    """The real drain over a stubbed authorized-merge door: pending checks
    hold, green authorizes and merges."""
    def _door(command, payload, timeout=None):
        if command == "authorized-merge" and payload.get("op") == "merge":
            if checks_pending:
                return {
                    "exit": 2,
                    "outcome": "held",
                    "reason": (
                        "checks are pending; require_checks_pass forbids "
                        "merging without green"
                    ),
                }
            return {"exit": 0, "outcome": "merged", "detail": "cafe" * 6,
                    "reason": ""}
        raise AssertionError(f"journey stub: unexpected command_call {command} {payload!r}")

    monkeypatch.setattr("fno.rust_binary.command_call", _door)


def _candidate(tmp_path: Path) -> PrCandidate:
    return PrCandidate(
        node_id=NODE,
        pr_number=PR,
        pr_url=f"https://github.com/{SLUG}/pull/{PR}",
        repo_dir=tmp_path,
        repo_slug=SLUG,
    )


class _NullTickClaim:
    def acquire_tick_lock(self, key, holder): pass

    def release_tick_lock(self, key, holder): pass

    def acquire_pr_lock(self, key, holder): pass

    def release_pr_lock(self, key, holder): pass

    def is_node_live(self, node_id): return False


def _drain(tmp_path: Path, store_path: Path) -> list[dict]:
    """The merge phase's half of the journey: one granted queue row, drained."""
    events: list[dict] = []
    def emit(kind, data):
        events.append({"type": kind, "data": data})

    run_execute_queue(
        [(_candidate(tmp_path), f"{SLUG}#{PR}", dict(GRANT))],
        store_path=store_path, emit=emit,
        notify=lambda *a, **k: None, max_retries=3,
        claim=_NullTickClaim(),
    )
    return events


def _seed_entry(tmp_path: Path) -> Path:
    store_path = tmp_path / "state.json"
    WatermarkStore(path=store_path).set(f"{SLUG}#{PR}", {
        "last_review_ts": None,
        "last_seen_state": "OPEN",
        "merge_dispatched": False,
        "retries": 0,
        "parked": None,
    })
    return store_path


def _grant_events(events: list[dict], phase: str) -> list[dict]:
    return [e for e in events
            if e["type"] == "merge_grant_execution" and e["data"]["phase"] == phase]


class TestParkedWorkerJourney:
    def test_granted_drain_holds_on_pending_checks_then_merges_green(
        self, tmp_path, monkeypatch
    ):
        # (1) CI pending: the door answers held - no merge is dispatched and
        # the held event carries the gate's reason.
        _arm_world(monkeypatch, tmp_path, checks_pending=True)
        store = _seed_entry(tmp_path)
        events = _drain(tmp_path, store)
        reserved = _grant_events(events, "reserved")
        held = _grant_events(events, "held")
        assert len(reserved) == 1 and len(held) == 1
        assert "checks are pending" in held[0]["data"]["reason"]
        assert not _grant_events(events, "executed")

        # (2) CI green, next drain: the door authorizes and the executed
        # events name actor, grant session, PR, and node.
        _arm_world(monkeypatch, tmp_path, checks_pending=False)
        events = _drain(tmp_path, store)
        executed = _grant_events(events, "executed")
        assert len(executed) == 1
        data = executed[0]["data"]
        assert data["actor"] == "pr-watch"
        assert data["pr"] == PR
        assert data["node_id"] == NODE
        assert data["recorded_by"] == "spawner-session"
