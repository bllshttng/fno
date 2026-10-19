"""Unit tests for the config.loops typed block (x-ce71).

A malformed level fails safe to "report" (observe only) rather than raising -
a standing loop must never silently upgrade its own autonomy from a config
typo.
"""
from __future__ import annotations

from fno.config import LoopEntry


def test_unknown_level_fails_safe_to_report():
    assert LoopEntry(level="banana").level == "report"
