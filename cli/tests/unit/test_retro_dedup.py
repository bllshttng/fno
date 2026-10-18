"""Unit tests for retro dedup (Wave 3.3, US5)."""

from __future__ import annotations

from fno.retro import dedup
from fno.retro.types import TIER_NODE, Candidate


def _cand(text: str, *, pr: int = 343, sid: str = "c1") -> Candidate:
    return Candidate(
        title="t",
        body=text,
        tier=TIER_NODE,
        priority="p1",
        source_pr=pr,
        source_id=sid,
        extra={"finding_text": text},
    )


def test_ac5_edge_two_reviewers_same_issue_one_node():
    """AC5-EDGE: two reviewers flag the same issue -> one candidate kept."""
    c1 = _cand("![high] the lock never releases on error", sid="gem-1")
    c2 = _cand("![P1 Badge] the lock never releases on error", sid="cdx-2")
    kept, skipped = dedup.dedup_candidates([c1, c2])
    assert len(kept) == 1
    assert len(skipped) == 1


def test_idempotent_round_trip_via_trailer():
    """A landed node's trailer makes a re-run skip the same candidate (AC5-HP idempotency)."""
    c = _cand("a genuine bug in the parser")
    [c_hashed] = dedup.assign_hashes([_cand("a genuine bug in the parser")])
    landed_node = {
        "id": "ab-new",
        "details": f"body\n\n{dedup.trailer(c_hashed.source_pr, c_hashed.content_hash)}",
    }
    existing = dedup.existing_keys_from_nodes([landed_node])
    kept, skipped = dedup.dedup_candidates([c], existing_keys=existing)
    assert kept == []
    assert len(skipped) == 1


def test_different_pr_not_deduped():
    """Same text on a different PR is a distinct key (source_pr is part of the key)."""
    c1 = _cand("same text", pr=1)
    c2 = _cand("same text", pr=2)
    kept, _ = dedup.dedup_candidates([c1, c2])
    assert len(kept) == 2
