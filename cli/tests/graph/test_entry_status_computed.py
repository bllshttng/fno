"""Tests for Entry.status as a Pydantic 2 computed_field.

Entry.status must be derived from the entry's
own fields rather than being a settable plain str field.

Precedence (single-entry, matching recompute_statuses):
  completed_at set    -> "done"
  superseded_by set   -> "superseded"
  deferred_at set     -> "deferred"
  non-empty blocked_by -> "blocked"
  session_id set      -> "in_progress"
  no plan_path        -> "idea"
  else                -> "ready"
"""
from __future__ import annotations


import pytest

from fno.graph.statuses import VALID_STATUSES
from fno.graph.types import Node as Entry


# ---------------------------------------------------------------------------
# AC1-EDGE: Parameterized coverage of all VALID_STATUSES derivation cases
# ---------------------------------------------------------------------------

EDGE_CASES = [
    # (status, kwargs)
    ("done", {"plan_path": "/p", "completed_at": "2026-05-15T00:00:00Z"}),
    ("superseded", {"plan_path": "/p", "superseded_by": "ab-x"}),
    ("deferred", {"plan_path": "/p", "deferred_at": "2026-05-15T00:00:00Z"}),
    ("blocked", {"plan_path": "/p", "blocked_by": ["ab-x"]}),
    ("in_progress", {"plan_path": "/p", "session_id": "sess-abc"}),
    ("idea", {}),
    ("ready", {"plan_path": "/p"}),
]


@pytest.mark.parametrize("expected_status,kwargs", EDGE_CASES)
def test_ac1_edge_all_valid_statuses_covered(expected_status, kwargs):
    """Each VALID_STATUSES member has a derivation test case."""
    assert expected_status in VALID_STATUSES, f"{expected_status!r} not in VALID_STATUSES"
    entry = Entry(id="ab-edge", **kwargs)
    assert entry.status == expected_status


# ---------------------------------------------------------------------------
# AC1-FR: Legacy graph.json with stale status -> computed wins + drift event
# ---------------------------------------------------------------------------


def test_ac1_fr_legacy_stale_status_computed_wins(tmp_path):
    """model_validate with stale status: computed value wins.

    AC1-FR: Given an on-disk entry with {completed_at set, status: "ready"},
    model_validate must return an entry with status == "done".
    """
    raw = {
        "id": "ab-fr",
        "completed_at": "2026-05-15T00:00:00Z",
        "plan_path": "/p",
        "status": "ready",  # stale / impossible state
    }
    entry = Entry.model_validate(raw)
    assert entry.status == "done", (
        f"Expected computed 'done', got {entry.status!r}"
    )


# ---------------------------------------------------------------------------
# AC1-UI: model_dump round-trip preserves status key
# ---------------------------------------------------------------------------


def test_ac1_ui_model_dump_includes_status_key():
    """entry.model_dump() must include 'status' key so graph.json readers don't break."""
    entry = Entry(id="ab-ui", plan_path="/some/plan")
    dumped = entry.model_dump()
    assert "status" in dumped, f"'status' not in model_dump(): {list(dumped.keys())}"
    assert dumped["status"] == "ready"


# ---------------------------------------------------------------------------
# Status enum stays in sync with VALID_STATUSES
# ---------------------------------------------------------------------------


def test_ac1_status_enum_covers_all_valid_statuses():
    """The Status enum must cover exactly VALID_STATUSES, keeping them in sync."""
    from fno.graph.types import Status
    enum_values = {s.value for s in Status}
    assert enum_values == VALID_STATUSES, (
        f"Status enum values {enum_values} != VALID_STATUSES {VALID_STATUSES}. "
        "Add new enum member or update VALID_STATUSES."
    )


# ---------------------------------------------------------------------------
# Entry.rank validator rejects bool / inf / NaN at the boundary
# ---------------------------------------------------------------------------


@pytest.mark.parametrize(
    "bad",
    [True, float("inf")],
)
def test_rank_rejects_bool_and_non_finite(bad):
    """A bool or non-finite rank fails loudly at model construction."""
    with pytest.raises(Exception):  # pydantic ValidationError wraps the ValueError
        Entry(id="ab-rank0004", rank=bad)
