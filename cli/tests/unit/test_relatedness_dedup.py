"""Filing-time dedup scorer: relatedness.similar_nodes (plan x-6ac7).

The dedup twin of ``epic_candidates``. Fixtures use controlled token sets so a
score is an exact inter/union ratio, not natural-language guesswork; ids are
obviously synthetic (no live node ids).
"""

from __future__ import annotations

from fno.graph.relatedness import _score, _tokens, similar_nodes


def node(nid, **kw):
    base = {"id": nid, "type": "feature", "title": nid, "domain": "code", "details": ""}
    base.update(kw)
    return base


# -- the specimen classes (AC7 + the caught dups) --


def test_similar_nodes_excludes_epic_bonus_so_siblings_stay_silent():
    # Same epic + domain, moderate overlap. WITH the epic bonus this pair scores
    # ~0.52 (would warn); the dedup path excludes the bonus, so it stays ~0.27
    # and is silent. This is the x-6ac7/x-a7ab sibling class.
    sib_a = node("aaa", title="alpha bravo charlie delta", domain="code", parent="epic1")
    sib_b = node("bbb", title="alpha echo golf", domain="code", parent="epic1")
    assert similar_nodes(sib_b, [sib_a]) == []
    with_epic = _score(sib_a, sib_b, _tokens(sib_a), _tokens(sib_b), include_epic=True)[0]
    assert with_epic >= 0.30  # proves the exclusion is load-bearing, not a no-op


# -- exclusion rules --


def test_similar_nodes_excludes_superseded():
    sup = node("sup", title="alpha bravo charlie delta", domain="code", status="superseded")
    new = node("new", title="alpha bravo charlie", domain="code")
    assert similar_nodes(new, [sup]) == []


def test_similar_nodes_includes_done_and_in_review_states():
    # A shipped `done` node is the answer to a duplicate filing; in_review is
    # in-flight work. Both are candidates (the old idea-state-only net missed them).
    done = node("don", title="alpha bravo charlie delta", domain="code", status="done")
    rev = node("rev", title="bravo charlie echo foxtrot", domain="code", status="in_review")
    new = node("new", title="alpha bravo charlie", domain="code")
    assert {r[0] for r in similar_nodes(new, [done, rev])} == {"don", "rev"}


# -- threshold boundary --


def test_similar_nodes_threshold_floor_inclusive_drops_below():
    # inter=1, union=5, +domain => exactly 0.30 (the floor): included.
    at_floor = node("ccc", title="alpha bravo charlie", domain="code")
    new_floor = node("ddd", title="alpha delta echo", domain="code")
    assert [r[0] for r in similar_nodes(new_floor, [at_floor])] == ["ccc"]

    # inter=1, union=6, +domain => 0.267: below the floor but above _MIN_SCORE,
    # so _score returns it and similar_nodes drops it (the 0.30 gate bites).
    below = node("eee", title="alpha bravo charlie delta", domain="code")
    new_below = node("fff", title="alpha echo golf", domain="code")
    assert similar_nodes(new_below, [below]) == []


# -- ordering / caps --


def test_similar_nodes_excludes_grandparent_via_chain():
    # The lineage walk follows the parent chain, not just the direct parent.
    mission = node("miss1", title="database migration program", type="roadmap")
    epic = node("epic1", title="database migration safety", type="epic", parent="miss1")
    child = node("kid", title="database migration safety checks", parent="epic1")
    ids = {r[0] for r in similar_nodes(child, [mission, epic, child])}
    assert "epic1" not in ids  # direct parent
    assert "miss1" not in ids  # grandparent reached via the chain
