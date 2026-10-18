"""Unit tests for fno.graph.statuses - recompute_statuses and is_stale_lock."""
from __future__ import annotations

from datetime import datetime, timezone, timedelta

import pytest

from fno.graph.statuses import (
    is_terminal_entry,
    is_stale_lock,
    live_claimed_node_ids,
    live_worked_node_ids,
    recompute_statuses,
)


def _entry(eid: str, **kwargs) -> dict:
    base = {
        "id": eid,
        "title": eid,
        "completed_at": None,
        "session_id": None,
        "locked_at": None,
        "blocked_by": [],
        # A stub plan_path so the default fixture exercises the ready/blocked/
        # claimed/done branches; tests for the idea derivation explicitly omit
        # plan_path or set it to None.
        "plan_path": f"plans/{eid}.md",
        "status": "ready",
    }
    base.update(kwargs)
    return base


@pytest.mark.parametrize("kind", ["wont_do"])
def test_is_terminal_entry_closes_decision_deferrals(kind):
    assert is_terminal_entry(_entry("ab-terminal", status="deferred", deferred_kind=kind))


@pytest.mark.parametrize("kind", ["later"])
def test_is_terminal_entry_keeps_other_deferrals_open(kind):
    assert not is_terminal_entry(
        _entry("ab-open", status="deferred", deferred_kind=kind)
    )


# -- is_stale_lock --


def test_ac1_hp_is_stale_lock_old_claim():
    """AC1-HP: claim older than TTL is stale."""
    old = (datetime.now(timezone.utc) - timedelta(hours=3)).isoformat()
    e = _entry("ab-33333333", session_id="sess-001", locked_at=old)
    assert is_stale_lock(e) is True


def test_ac2_err_is_stale_lock_bad_timestamp():
    """AC2-ERR: unparseable timestamp is treated as stale."""
    e = _entry("ab-44444444", session_id="sess-001", locked_at="not-a-date")
    assert is_stale_lock(e) is True


def test_live_claim_read_can_fail_open_for_display_or_raise_for_mutation(monkeypatch):
    def unavailable(*args, **kwargs):
        raise OSError("claims unavailable")

    monkeypatch.setattr("fno.claims.core.list_claims", unavailable)

    assert live_claimed_node_ids() == set()
    with pytest.raises(OSError, match="claims unavailable"):
        live_claimed_node_ids(strict=True)


# -- recompute_statuses --


def test_old_lock_on_in_progress_node_is_unknown_and_preserved():
    """Age alone cannot prove that a long-running worker is dead."""
    old = (datetime.now(timezone.utc) - timedelta(hours=5)).isoformat()
    entry = _entry(
        "ab-livework1",
        status="in_progress",
        locked_by="worker-unverified",
        session_id="worker-unverified",
        locked_at=old,
    )

    result = recompute_statuses([entry])

    assert result[0]["status"] == "in_progress"
    assert result[0]["ownership_defect"] == {
        "kind": "stale-active-owner-unverified",
        "node_id": "ab-livework1",
        "holder": "worker-unverified",
        "liveness": "unverified",
    }
    assert entry["locked_by"] == "worker-unverified"
    assert entry["session_id"] == "worker-unverified"
    assert entry["locked_at"] == old


# -- in_review: node with an open, unmerged PR is held out of dispatch --


def test_in_review_survives_stale_claim():
    """The stampede case: builder session died (stale lock) but PR is still
    open. Status must stay in_review, NOT revert to ready and re-enter the
    dispatch pool."""
    old = (datetime.now(timezone.utc) - timedelta(hours=48)).isoformat()
    e = _entry("ab-prreview2", pr_number=358, session_id="dead-sess", locked_at=old)
    result = recompute_statuses([e])
    assert result[0]["status"] == "in_review"
    assert result[0]["session_id"] == "dead-sess"
    assert result[0]["locked_at"] == old
    assert result[0]["locked_by"] == "dead-sess"
    assert result[0]["ownership_defect"]["liveness"] == "unverified"


def test_in_review_reachable_through_typed_entry():
    """The Entry.status computed field must also derive in_review, so a typed
    round-trip (model_dump) does not silently emit ready/idea/blocked."""
    from fno.graph.types import Node as Entry

    entry = Entry(id="ab-prreview5", title="t", pr_number=358)
    assert entry.status == "in_review"


# ---------------------------------------------------------------------------
# Rung-derived status (x-3571 wave 1)
# ---------------------------------------------------------------------------


@pytest.mark.parametrize(
    "status,derived",
    [("stub", "idea")],
)
def test_AC2_EDGE_a_linked_pre_design_plan_never_derives_ready(
    tmp_path, status, derived
):
    """The defect this wave exists to fix - the `stub` rows fail against main.

    `stub` was in no vocabulary: `is_design_stage` read False for it and the
    final `else` assigned `ready`, so a node pointing at an unfilled decompose
    scaffold was fully dispatchable. `idea` is the same rung under a word every
    gate can see.
    """
    from fno.graph.ladder import is_dispatchable

    plan = tmp_path / f"{status}.md"
    plan.write_text(f"---\nstatus: {status}\n---\n\n# Child\n")
    e = _entry("ab-predsgn1", plan_path=str(plan))

    assert recompute_statuses([e])[0]["status"] == derived
    assert derived != "ready"
    assert is_dispatchable(e) is False



def test_an_unconsulted_roster_raises_naming_the_reader():
    """A not-consulted roster read raises with the reader prefixed, so the
    spawn-guard receipt points at read_roster instead of implying the node is
    occupied. The function arrives via the module-level import: the conftest
    autouse stub rebinds the module attribute at setup, and a test-body
    import would bind the stub, not the real gate."""
    from fno.claims.roster import RosterReading

    with pytest.raises(RuntimeError, match=r"^read_roster: boom$"):
        live_worked_node_ids(
            strict=True,
            entries=[{"status": "ready"}],
            reading=RosterReading(False, 0, {}, "boom"),
        )
