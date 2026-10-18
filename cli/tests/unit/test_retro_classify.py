"""Unit tests for retro classify (Wave 3.2, US3)."""
from __future__ import annotations

from fno.retro.classify import (
    BODY_CAP,
    classify,
    classify_item,
)
from fno.retro.types import (
    KIND_CARVEOUT,
    KIND_REVIEW,
    TIER_NODE,
    RawItem,
)


def test_ac3_hp_verbatim_body_and_real_title():
    """AC3-HP: body contains the reviewer's comment verbatim + source cite; title is a real summary."""
    item = RawItem(
        kind=KIND_REVIEW,
        text="![high] The retry loop never resets `attempts`, so it spins forever on a transient error.",
        source_pr=343,
        source_id="556677",
        severity="high",
        url="https://github.com/o/r/pull/343#discussion_r556677",
        reviewer="gemini[bot]",
    )
    c = classify_item(item)
    assert c.uncited is False
    assert c.tier == TIER_NODE
    assert c.priority == "p1"
    # Verbatim reasoning preserved in the body.
    assert "retry loop never resets `attempts`" in c.body
    # Source cite present.
    assert "PR #343" in c.body
    assert "556677" in c.body or "discussion_r556677" in c.body
    # Title is a real one-line summary, not a generic stub.
    assert c.title and "address review feedback" not in c.title.lower()
    assert "retry loop never resets" in c.title


def test_ac3_err_uncited_candidate_rejected():
    """AC3-ERR: a candidate with no source_pr/source id is marked uncited (no node)."""
    item = RawItem(kind=KIND_REVIEW, text="some floating finding", source_pr=None, source_id="")
    c = classify_item(item)
    assert c.uncited is True
    cited, uncited = classify([item])
    assert cited == []
    assert len(uncited) == 1


def test_ac3_fr_oversize_body_truncated_with_marker():
    """AC3-FR: a body exceeding the cap is truncated with a marker + link, never dropped."""
    huge = "x" * 20_000
    item = RawItem(
        kind=KIND_REVIEW,
        text=huge,
        source_pr=10,
        source_id="1",
        severity="medium",
        url="http://c/1",
    )
    c = classify_item(item)
    assert len(c.body) <= BODY_CAP
    assert "truncated" in c.body
    assert "http://c/1" in c.body  # link back to source preserved


def test_a_carveout_is_cited_by_its_ledger_id_with_no_pr():
    """The PR-independent sweep has no source_pr. Requiring one rejected every
    swept carve-out as uncited, so the ledger could not be harvested at all."""
    item = RawItem(
        kind=KIND_CARVEOUT,
        text="declared scope that did not ship",
        source_pr=None,
        source_id="cv-ghi",
        subkind="deferred",
    )

    c = classify_item(item)
    assert not c.uncited
    assert "cv-ghi" in c.body  # the cite is the ledger id itself

