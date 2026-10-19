"""The tier table: every id reachable, staleness detectable, omissions honest.

This table drifted a full model generation (gpt-5.5 / gpt-5.4 /
claude-opus-4-8) with nothing detecting it. The tests here are the tripwire:
AC5-HP walks the shipped tables asserting the unmapped-id list renders EMPTY
(listed, never an exit code trusted alone), and AC5-MARKER plants a dead id in
a copy and asserts the checker flags it BY NAME.
"""
from __future__ import annotations

import fno.adapters.providers.benchmarks as bm


def _all_tier_tables() -> dict:
    return {k: list(v) for k, v in bm.STATIC_TIERS.items()}


def test_a_planted_dead_id_is_flagged_by_name():
    """AC5-MARKER: plant, observe the flag, unplant, observe green."""
    planted = {"high": ["claude-opus", "gpt-dead-9.9"]}
    flagged = bm.unreachable_tier_ids(planted)
    assert flagged == ["gpt-dead-9.9"]
    # Unplant: the shipped table is green.
    assert bm.unreachable_tier_ids(_all_tier_tables()) == []


def test_every_band_serves_every_harness_with_a_row():
    """No band resolves to nothing on a harness the table serves at all."""
    assert bm.empty_bands_for_harness() == {}


def test_a_band_a_harness_cannot_serve_is_reported():
    planted = {"max": ["codex-sol"], "low": ["claude-haiku"]}
    holes = bm.empty_bands_for_harness(("claude", "codex"), tiers=planted)
    assert holes == {"claude": ["max"], "codex": ["low"]}
