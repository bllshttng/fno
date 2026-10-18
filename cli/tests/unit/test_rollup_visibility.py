"""The three orphan visibility surfaces: board flag, ordering tiebreaker, metric.

All three read one predicate (`rollup.is_orphan`), so these lock in that they
agree about exemptions and that the tiebreaker stays in-band.
"""

from __future__ import annotations

from fno.graph._intake import make_selection_sort_key
from fno.graph.rollup import orphan_ids


def node(nid, **kw):
    base = {
        "id": nid,
        "type": "feature",
        "title": nid,
        "priority": "p2",
        "project": "fno",
        "created_at": "2026-01-01T00:00:00+00:00",
    }
    base.update(kw)
    return base


# -- ordering tiebreaker --


def _order(entries):
    orphans = orphan_ids(entries)
    key = make_selection_sort_key(entries, orphans, swimlane=True)
    return [e["id"] for e in sorted(entries, key=key)]


def test_orphan_sorts_after_linked_peer_in_the_same_band():
    """AC5: same lane, same rank band, same priority -> mission-linked first."""
    entries = [
        node("x-epic", type="epic", title="mission"),
        node("x-orphan", created_at="2026-01-01T00:00:00+00:00"),
        node("x-linked", parent="x-epic", created_at="2026-06-01T00:00:00+00:00"),
    ]
    order = [i for i in _order(entries) if i != "x-epic"]
    assert order == ["x-linked", "x-orphan"], (
        "the older orphan must still sort after its mission-linked peer"
    )


# -- health metric shape --


def test_orphan_rate_threshold_default_never_breaches():
    """A backlog starts near-total orphan; day one must be quiet."""
    from fno.health_monitor import DEFAULT_CONFIG, evaluate_thresholds

    report = {"orphan_feature_rate": 1.0}
    breaches = evaluate_thresholds(report, config=DEFAULT_CONFIG)
    assert [b for b in breaches if b.key == "orphan_feature_rate"] == []


# -- review findings: per-project scoping --


def test_per_project_board_does_not_invent_orphans():
    """Orphanhood is a whole-graph property.

    A feature whose parent epic lives in another project has no reachable
    ancestor inside a project-scoped slice; computing orphans from that subset
    invents orphans. Every scoped consumer must be handed the whole-graph set.
    The dashboard renderer carries the same rule through its context_entries
    argument, covered by test_render_html's cross-project relationship test.
    """
    entries = [
        node("x-epic", type="epic", title="mission", project="other"),
        node("x-child", parent="x-epic", project="fno"),
    ]
    all_orphans = orphan_ids(entries)
    assert all_orphans == frozenset(), "child reaches its epic in the full graph"

    proj_entries = [e for e in entries if e["project"] == "fno"]
    # Without the whole-graph set the subset would call x-child an orphan.
    assert orphan_ids(proj_entries) == {"x-child"}


def test_done_cards_are_not_flagged_as_orphans():
    """The metric excludes closed work, so the board must too."""
    assert orphan_ids([node("x-1", status="done")]) == frozenset()


# -- codex review: work order must match board order --


def _selection_order(entries):
    from fno.graph._intake import make_selection_sort_key

    return [e["id"] for e in sorted(entries, key=make_selection_sort_key(entries))]


def test_next_demotes_an_orphan_under_a_non_epic_parent():
    """board == work order.

    An orphan parented to a plain feature is an epic-child by the selection
    key's tier test but an orphan by the rollup predicate, so before this the
    board showed the linked child first while `next` picked the orphan.
    """
    entries = [
        node("x-epic", type="epic", title="mission", priority="p1"),
        node("x-host", title="plain parent", priority="p1"),
        node("x-orphan", parent="x-host", priority="p1", created_at="2026-01-01T00:00:00+00:00"),
        node("x-linked", parent="x-epic", priority="p1", created_at="2026-06-01T00:00:00+00:00"),
    ]
    order = [i for i in _selection_order(entries) if i.startswith(("x-orphan", "x-linked"))]
    assert order == ["x-linked", "x-orphan"]


def test_selection_priority_still_outranks_the_orphan_term():
    """The orphan term sits AFTER child priority, so it only breaks ties.

    Compared within one tier: both nodes are loose, so the pre-existing
    epics-first tier does not decide it. `orphan_ok` makes one exempt.
    """
    entries = [
        node("x-p0-orphan", priority="p0"),
        node("x-p1-exempt", priority="p1", orphan_ok="infra"),
    ]
    assert _selection_order(entries) == ["x-p0-orphan", "x-p1-exempt"]


def test_epics_first_tier_is_unchanged():
    """Pre-existing contract: an epic child outranks a loose node by tier.

    Documented in make_selection_sort_key and untouched here - recorded so a
    future change to the orphan term does not silently alter it.
    """
    entries = [
        node("x-epic", type="epic", title="mission"),
        node("x-p0-loose", priority="p0"),
        node("x-p1-child", parent="x-epic", priority="p1"),
    ]
    order = [i for i in _selection_order(entries) if i != "x-epic"]
    assert order == ["x-p1-child", "x-p0-loose"]


def test_selection_key_fails_open_when_rollup_raises(monkeypatch):
    """Selection must never break because the ordering signal failed."""
    import fno.graph.rollup as rollup
    from fno.graph._intake import make_selection_sort_key

    monkeypatch.setattr(
        rollup,
        "orphan_ids",
        lambda e: (_ for _ in ()).throw(RuntimeError("boom")),
    )
    entries = [node("x-b", priority="p1"), node("x-a", priority="p0")]
    assert [e["id"] for e in sorted(entries, key=make_selection_sort_key(entries))] == [
        "x-a",
        "x-b",
    ]
