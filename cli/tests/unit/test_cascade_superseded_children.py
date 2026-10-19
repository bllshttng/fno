"""A parent closes when its superseded children were replaced elsewhere.

``supersede`` never stamps ``completed_at``, so the old all-children-carry-
``completed_at`` read kept any parent holding a superseded child open
forever. The replacement, ``children_all_closed``, asks three things: every
child terminal, no child superseded BY the parent itself (a child the parent
absorbed means the parent's own work is still open), and at least one child
that really shipped. Each shape runs against both close paths:
``_cascade_close_parents`` (child-close event) and ``_strandable_epic_ids``
(reconcile sweep).

Filter: ``fno doctor test cli/tests/unit/test_cascade_superseded_children.py``
"""
from __future__ import annotations

from fno.graph.cli import _cascade_close_parents, _strandable_epic_ids

CHILD_CLOSE = "2026-09-04T20:35:52+00:00"


def _world(*, status=None, superseded_by=None):
    """A parent with one done child and one caller-shaped second child."""
    parent = {"id": "p", "status": "ready"}
    done = {"id": "c1", "parent": "p", "completed_at": CHILD_CLOSE}
    second = {"id": "c2", "parent": "p"}
    if status is not None:
        second["status"] = status
    if superseded_by is not None:
        second["superseded_by"] = superseded_by
    return parent, done, second


# --- replaced elsewhere: terminal child pointing at a sibling -------------


def test_cascade_closes_when_the_only_other_child_was_replaced_elsewhere():
    parent, done, second = _world(status="superseded", superseded_by="other")
    assert _cascade_close_parents([parent, done, second], "c1") == ["p"]
    assert parent.get("completed_at")


def test_sweep_names_the_parent_when_the_only_other_child_was_replaced_elsewhere():
    parent, done, second = _world(status="superseded", superseded_by="other")
    assert _strandable_epic_ids([parent, done, second]) == {"p"}


# --- absorbed by the parent: the child points BACK at this parent ---------


def test_absorbed_child_keeps_the_parent_open_on_the_cascade():
    parent, done, second = _world(status="superseded", superseded_by="p")
    assert _cascade_close_parents([parent, done, second], "c1") == []
    assert parent.get("completed_at") is None


# --- nothing shipped: every child replaced, nothing ever completed --------


def test_a_parent_whose_every_child_was_replaced_elsewhere_stays_open():
    parent, done, second = _world(status="superseded", superseded_by="other")
    done["status"] = "superseded"
    done["superseded_by"] = "other2"
    done.pop("completed_at")
    assert _cascade_close_parents([parent, done, second], "c1") == []
    assert _strandable_epic_ids([parent, done, second]) == set()


# --- reopen still outranks the sweep on the new predicate -----------------
