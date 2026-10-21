"""The keeper roster tuple stays in step with the capability rows."""
from __future__ import annotations

from fno.agents.rust_runtime import _KEEPER_THREAD_HARNESSES


def test_keeper_roster_matches_the_capability_lane() -> None:
    """The literal tuple and the capability contract must agree exactly, so a
    harness whose thread lane is the keeper always reaches its Python arm."""
    from fno.agents.harness_map import thread_lane
    from fno.harness_names import SPAWN_HARNESSES

    keeper_roster = {h for h in SPAWN_HARNESSES if thread_lane(h) == "keeper"}
    assert keeper_roster == set(_KEEPER_THREAD_HARNESSES)
    assert "opencode" not in keeper_roster, "opencode's row regressed to keeper"
