"""The silent failover names its give-up.

Run: cd cli && uv run pytest tests/test_quota_refusal_and_failover.py -v

The refusal-deadline half of this contract is the Rust taxonomy's now:
crates/fno-agents/tests/golden/accounts_error_class replays the recorded
429 corpus and the window-name rules.
"""
from __future__ import annotations

import pytest


class TestFailoverNamesItsGiveUp:
    """AC21-ERR. Twenty recorded rotations, twenty abandonments, nineteen mute."""

    def test_an_abandonment_without_a_reason_is_refused_at_the_writer(self) -> None:
        from fno.events.store_client import ValidationError, build_event as _build

        with pytest.raises(ValidationError):
            _build(
                "failover_swapped",
                "daemon",
                {"short_id": "79ebf387", "redispatched": False},
            )
