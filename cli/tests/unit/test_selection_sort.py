"""Unit tests for epics-first selection precedence (C3, ab-82e65b72).

`make_selection_sort_key` is the single source of truth for the
"epics-first, then flat priority" precedence (Locked Decision 7). It is
used by both `fno backlog next`/`ready` and the parallel walker's
`_select_ready_nodes`.
"""

from fno.graph._intake import make_effective_priority, make_selection_sort_key


def _ids_sorted(entries, candidates):
    key = make_selection_sort_key(entries)
    return [e["id"] for e in sorted(candidates, key=key)]


def test_epic_child_outranks_higher_priority_loose_node():
    # A p3 epic child must come before a p0 loose node: epics-first beats
    # raw priority (Locked Decision 7).
    epic = {"id": "epic1", "type": "epic", "priority": "p2", "created_at": "2026-01-01"}
    child = {"id": "child1", "parent": "epic1", "priority": "p3", "created_at": "2026-01-02"}
    loose = {"id": "loose1", "priority": "p0", "created_at": "2026-01-03"}
    entries = [epic, child, loose]
    assert _ids_sorted(entries, [loose, child]) == ["child1", "loose1"]


def test_in_progress_epic_preferred_over_unstarted_same_priority():
    # Two same-priority epics; epicX has a done child (in progress), epicY
    # does not. Stay focused: drain the in-progress epic first.
    epic_x = {"id": "epicX", "type": "epic", "priority": "p2", "created_at": "2026-01-01"}
    epic_y = {"id": "epicY", "type": "epic", "priority": "p2", "created_at": "2026-01-01"}
    done_child = {
        "id": "xdone",
        "parent": "epicX",
        "priority": "p2",
        "status": "done",
        "created_at": "2026-02-01",
    }
    ready_x = {"id": "xr", "parent": "epicX", "priority": "p2", "created_at": "2026-02-02"}
    ready_y = {"id": "yr", "parent": "epicY", "priority": "p2", "created_at": "2026-02-02"}
    entries = [epic_x, epic_y, done_child, ready_x, ready_y]
    assert _ids_sorted(entries, [ready_y, ready_x]) == ["xr", "yr"]


# -- Curated rank drives selection (AC1-*) ----------------------------------
# The selection key prepends the SAME `_rank_band` term the board uses, so a
# `fno backlog rank --top` node is worked next, not just floated on the board.


def test_ranked_node_selected_before_unranked():
    # AC1-HP: a ranked node (even at the lowest priority) is selected ahead of
    # higher-priority unranked nodes in the same project.
    ranked = {"id": "r", "priority": "p3", "created_at": "2026-03-01", "rank": 1.0}
    loose_a = {"id": "a", "priority": "p0", "created_at": "2026-01-01"}
    loose_b = {"id": "b", "priority": "p1", "created_at": "2026-01-02"}
    entries = [ranked, loose_a, loose_b]
    assert _ids_sorted(entries, [loose_a, loose_b, ranked])[0] == "r"


def test_poisoned_rank_degrades_to_unranked():
    # AC1-ERR: NaN / inf / bool ranks degrade to unranked via the shared
    # `_rank_band` guard - no crash, and a high-priority unranked node wins.
    for bad in (float("nan"), float("inf"), float("-inf"), True):
        poisoned = {"id": "p", "priority": "p3", "created_at": "2026-03-01", "rank": bad}
        loose = {"id": "l", "priority": "p0", "created_at": "2026-01-01"}
        entries = [poisoned, loose]
        assert _ids_sorted(entries, [poisoned, loose])[0] == "l"


# -- Child rank is scoped to its live epic (x-4628) -------------------------
# The epic's own rank band leads a child's key; the child's rank orders it
# only among its live-epic siblings and never floats its epic group.


def test_child_rank_orders_only_within_its_epic_group():
    # AC1-HP: epicA is ranked (band 0), epicB is not. A ranked child of
    # epicB must not jump epicA's group - the EPIC's band decides position.
    epic_a = {
        "id": "epicA",
        "type": "epic",
        "priority": "p2",
        "created_at": "2026-01-01",
        "rank": 1.0,
    }
    epic_b = {"id": "epicB", "type": "epic", "priority": "p2", "created_at": "2026-01-01"}
    child_a = {"id": "ca", "parent": "epicA", "priority": "p3", "created_at": "2026-02-01"}
    ranked_child_b = {
        "id": "rb",
        "parent": "epicB",
        "priority": "p3",
        "created_at": "2026-03-01",
        "rank": -50.0,
    }
    entries = [epic_a, epic_b, child_a, ranked_child_b]
    assert _ids_sorted(entries, [ranked_child_b, child_a]) == ["ca", "rb"]


def test_terminal_epic_child_sorts_exactly_like_a_loose_node():
    for status in ("done", "superseded", "deferred"):
        epic = {
            "id": "epic",
            "type": "epic",
            "status": status,
            "priority": "p0",
            "created_at": "2026-01-01",
        }
        child = {
            "id": "child",
            "parent": "epic",
            "priority": "p2",
            "created_at": "2026-02-01",
        }
        loose_equivalent = {**child, "id": "loose", "parent": None}
        key = make_selection_sort_key([epic, child, loose_equivalent])
        assert key(child) == key(loose_equivalent), status


def test_malformed_epic_enrichment_degrades_to_safe_defaults():
    epic = {
        "id": "epic",
        "type": "epic",
        "status": [],
        "priority": [],
        "created_at": [],
    }
    child = {
        "id": "child",
        "parent": "epic",
        "priority": [],
        "created_at": [],
    }
    loose = {
        "id": "loose",
        "parent": ["not", "hashable"],
        "priority": "p2",
        "created_at": "",
    }

    key = make_selection_sort_key([epic, child, loose])

    assert key(child) == key(loose)
    assert make_effective_priority([epic, child])(child) == "p2"
