"""Machine-readable live-worker evidence for claim status."""
from __future__ import annotations


import time
from datetime import datetime, timezone

import pytest

from fno.agents.reachability import REACHABLE, UNREACHABLE
from fno.claims.roster import _worker_reachability


@pytest.fixture(autouse=True)
def _no_worked_reply(monkeypatch):
    """Hermetic overlay: the command never runs unless a test provides a reply."""
    monkeypatch.setattr(
        "fno.graph.statuses._worked_nodes_reply", lambda rows: {}
    )


def _stop_row(stopped_at=None, state="working") -> dict:
    return {
        "name": "stop-worker",
        "state": state,
        "cwd": "/worktrees/ac1-node",
        "row_id": "session-stop",
        "stopped_at": stopped_at,
    }


def _fresh_tail(monkeypatch, last_event_epoch: float) -> None:
    from fno.agents.watchdog import TailFacts

    monkeypatch.setattr(
        "fno.agents.watchdog.tail_facts",
        lambda *_a, **_kw: TailFacts(
            records=None,
            last_event_epoch=last_event_epoch,
            tail_text="progress update",
            last_role="assistant",
            last_text="progress update",
        ),
    )
    monkeypatch.setattr(
        "fno.agents.watchdog.harness_for_session", lambda *_a, **_kw: "claude"
    )


def test_a_stop_newer_than_the_tail_falsifies_liveness(monkeypatch):
    """A claude row fno just stopped keeps a seconds-old tail; the stop stamp
    must outrank the fresh-tail cancel so the node reads free at once."""
    now = time.time()
    _fresh_tail(monkeypatch, now - 30)
    stamp = datetime.fromtimestamp(now - 10, timezone.utc).isoformat()

    verdict = _worker_reachability(_stop_row(stopped_at=stamp))

    assert verdict.verdict == UNREACHABLE
    assert verdict.basis.startswith("stopped:")


def test_a_tail_event_after_the_stop_still_reads_reachable(monkeypatch):
    """A tail event strictly after the stop is a resumed session; the stop
    stamp does not condemn it."""
    now = time.time()
    _fresh_tail(monkeypatch, now - 5)
    stamp = datetime.fromtimestamp(now - 10, timezone.utc).isoformat()

    verdict = _worker_reachability(_stop_row(stopped_at=stamp))

    assert verdict.verdict == REACHABLE
