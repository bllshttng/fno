"""A deliberate reopen outranks the automatic close that would undo it.

Four close paths read a reopen, and one deliberate command (``cmd_done``, with
its own --force plus --reason ladder) does not. The sweeps:

- `_cascade_close_parents` on a child close - guarded by
  `_reopen_outranks_child_closes`.
- `_sweep_close_done_epics` on reconcile - guarded through
  `_strandable_epic_ids`.
- reconcile's PR-merged close (`cmd_reconcile`'s closeable partition) and the
  contained merge cascade `_cascade_close_contained` - guarded by
  `_reopen_outranks_merge` / the same child-keyed predicate. The
  PR-merged leg reads no children, so the child-keyed guard alone never
  reached it: a container whose own PR shipped was re-closed on that evidence
  alone. Measured 2026-09-05 a node reopened with a written reason was
  re-closed by reconcile seven minutes later; measured 2026-09-09, twice
  inside two minutes on the PR-merged path.

`reopen` requires `--reason` because a close is evidenced by a merged PR while
a reopen is nothing but human judgment. An automatic sweep discarding that
judgment without a word is what these tests pin shut.

The cascade positive control asserts the SAME fixture closes once the reopen
is removed. Without it a green run cannot tell "the guard works" from "this
fixture never closed anyway".
"""
from __future__ import annotations


from fno.graph._reconcile import (
    _reopen_outranks_child_closes,
    _reopen_outranks_merge,
)
from fno.graph.cli import (
    _cascade_close_contained,
    _cascade_close_parents,
    _strandable_contained_ids,
    _strandable_epic_ids,
)

CHILD_CLOSE = "2026-09-04T20:35:52+00:00"
BEFORE_CLOSE = "2026-09-04T19:00:00+00:00"
AFTER_CLOSE = "2026-09-05T04:17:40+00:00"
MERGED_AT = "2026-08-30T12:00:00Z"
BEFORE_MERGE = "2026-08-29T00:00:00+00:00"
AFTER_MERGE = "2026-08-31T00:00:00+00:00"


def _pair(*, reopened_at=None, child_closed=CHILD_CLOSE, parent_closed=None):
    """A parent with one done child, matching the shape that misfired."""
    parent = {"id": "p", "status": "ready"}
    if reopened_at is not None:
        parent["reopened_at"] = reopened_at
    if parent_closed is not None:
        parent["completed_at"] = parent_closed
    child = {"id": "c", "parent": "p", "completed_at": child_closed}
    return parent, child


def test_reopen_after_every_child_close_holds():
    parent, child = _pair(reopened_at=AFTER_CLOSE)
    assert _reopen_outranks_child_closes(parent, [child]) is True


def test_reopen_before_the_last_child_close_is_stale():
    """It expires by itself: the parent genuinely completed after that call."""
    parent, child = _pair(reopened_at=BEFORE_CLOSE)
    assert _reopen_outranks_child_closes(parent, [child]) is False


def test_unreadable_reopen_stamp_protects():
    """Ambiguity favours the human; see the helper's docstring."""
    parent, child = _pair(reopened_at="not-a-timestamp")
    assert _reopen_outranks_child_closes(parent, [child]) is True


def test_sweep_skips_a_reopened_parent():
    parent, child = _pair(reopened_at=AFTER_CLOSE)
    assert _strandable_epic_ids([parent, child]) == set()


def test_cascade_positive_control_same_fixture_closes_without_the_reopen():
    parent, child = _pair()
    assert _cascade_close_parents([parent, child], "c") == ["p"]
    assert parent.get("completed_at")


def _plan_pair():
    """A parent carrying its own plan work with no PR of its own."""
    parent, child = _pair()
    parent["plan_path"] = "plans/p.md"
    return parent, child


def test_cascade_skips_a_parent_with_its_own_plan_work():
    parent, child = _plan_pair()
    assert _cascade_close_parents([parent, child], "c") == []
    assert parent.get("completed_at") is None


def _contained_pair(*, reopened_at=None):
    """A contained child whose delivery unit already closed."""
    owner = {"id": "unit", "status": "done", "completed_at": CHILD_CLOSE}
    child = {"id": "c", "status": "in_progress", "contained_in": "unit"}
    if reopened_at is not None:
        child["reopened_at"] = reopened_at
    return owner, child


def test_contained_sweep_skips_a_reopened_child():
    owner, child = _contained_pair(reopened_at=AFTER_CLOSE)
    assert _strandable_contained_ids([owner, child]) == set()


# ---------------------------------------------------------------------------
# `_reopen_outranks_merge`: the merge-keyed twin
# ---------------------------------------------------------------------------


def _merged_node(*, reopened_at=None):
    """A node closed on its own merged PR - the leg that reads no children."""
    node = {"id": "n", "status": "in_review", "pr_number": 42}
    if reopened_at is not None:
        node["reopened_at"] = reopened_at
    return node


def test_reopen_after_the_merge_holds():
    assert _reopen_outranks_merge(_merged_node(reopened_at=AFTER_MERGE), MERGED_AT) is True


def test_missing_merge_stamp_protects():
    """None on a reverse-mapped record when gh omits mergedAt - a real path."""
    assert _reopen_outranks_merge(_merged_node(reopened_at=BEFORE_MERGE), None) is True


# ---------------------------------------------------------------------------
# the contained merge cascade reads the same guard
# ---------------------------------------------------------------------------


def test_contained_cascade_reopen_between_merge_and_the_sweep_stamp_holds():
    """The owner's completed_at is reconcile's wall clock, not the close's
    evidence: a reopen made after the actual merge but before a delayed sweep
    holds when the merge stamp is the comparison basis."""
    owner = {"id": "unit", "status": "done", "completed_at": "2026-09-05T10:00:00+00:00"}
    child = {
        "id": "c",
        "status": "in_progress",
        "contained_in": "unit",
        "reopened_at": "2026-09-05T00:00:00+00:00",
    }
    assert _cascade_close_contained([owner, child], "unit", merged_at="2026-09-04T12:00:00Z") == []
    assert child.get("completed_at") is None


# ---------------------------------------------------------------------------
# end to end: `fno backlog reconcile` holds the reopened node
# ---------------------------------------------------------------------------
