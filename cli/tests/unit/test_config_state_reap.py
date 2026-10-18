"""State-file reap retention defaults and visible fail-safe degradation."""

from __future__ import annotations

import pytest

from fno.config import AgentsBlock
from fno.config._sweeps import DEGRADED

DEFAULTS = {
    "enabled": True,
    "locks_retain_days": 7,
    "expired_claims_retain_days": 30,
    "pr_status_cache_retain_days": 14,
}


def test_state_reap_enabled_requires_an_actual_bool() -> None:
    # The string "false" is truthy; reading it as a bool would invert the knob.
    DEGRADED.clear()
    block = AgentsBlock(state_reap={"enabled": "false"}).state_reap
    assert block.enabled is True
    assert set(DEGRADED) == {"agents.state_reap.enabled"}


@pytest.mark.parametrize(
    "configured",
    [
        {
            "locks_retain_days": "weekly",
            "expired_claims_retain_days": True,
            "pr_status_cache_retain_days": None,
        },
        {
            "locks_retain_days": 0,
            "expired_claims_retain_days": -1,
            "pr_status_cache_retain_days": -14,
        },
    ],
    ids=("invalid", "nonpositive"),
)
def test_bad_state_reap_days_degrade_and_name_exact_keys(
    configured: dict[str, object],
) -> None:
    DEGRADED.clear()
    block = AgentsBlock(state_reap=configured).state_reap
    assert block.model_dump() == DEFAULTS
    assert set(DEGRADED) == {f"agents.state_reap.{key}" for key in configured}
