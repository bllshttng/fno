"""Unit tests for the transitive-children resolver (C2, ab-facfaade).

`descendants_of` powers the `--parent <epic-id>` epic-scope filter on
`fno backlog next`/`ready` and the epic-subtree megawalk. It returns the
set of node IDs that are transitive children of an epic via the `parent`
field, cycle-safe against a malformed graph.
"""
from fno.graph._intake import descendants_of


def test_transitive_grandchildren():
    entries = [
        {"id": "epic"},
        {"id": "c1", "parent": "epic"},
        {"id": "g1", "parent": "c1"},
        {"id": "g2", "parent": "c1"},
    ]
    assert descendants_of(entries, "epic") == {"c1", "g1", "g2"}


def test_cycle_back_to_parent_excludes_parent():
    # A malformed chain that loops back to the scoped parent must not add
    # the parent to its own descendant set (contract: parent excluded).
    entries = [
        {"id": "epic", "parent": "c2"},   # cycle: epic's parent is its own grandchild
        {"id": "c1", "parent": "epic"},
        {"id": "c2", "parent": "c1"},
    ]
    result = descendants_of(entries, "epic")
    assert "epic" not in result
    assert result == {"c1", "c2"}


def test_unhashable_parent_does_not_crash():
    # A corrupted row whose `parent` is a dict/list must be skipped, not
    # raise TypeError on the children-index insert.
    entries = [
        {"id": "epic"},
        {"id": "bad", "parent": {"nested": "dict"}},
        {"id": "bad2", "parent": ["list"]},
        {"id": "c1", "parent": "epic"},
    ]
    assert descendants_of(entries, "epic") == {"c1"}
