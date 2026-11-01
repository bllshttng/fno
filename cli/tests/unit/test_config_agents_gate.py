"""config.agents spawn-gate knobs (x-c5cc): max_live / min_free_gb / worker_qos.

All three coerce invalid values to defaults (fail-open, LD9): the gate is
protective infrastructure and a settings typo must never brick spawning.
"""
from fno.config import AgentsBlock


def test_provider_loader_reserved_keys_match_agents_schema():
    from fno.adapters.providers.loader import _AGENTS_RESERVED_KEYS

    # `max_lanes` is the one tolerated legacy spelling (renamed provider_limits,
    # x-3f84 W5): still parsed by the block's before-validator, so it stays
    # reserved against provider pins.
    assert _AGENTS_RESERVED_KEYS == set(AgentsBlock.model_fields) | {"max_lanes"}


def test_max_live_below_one_coerces_to_default():
    assert AgentsBlock(max_live=0).max_live == 3
    assert AgentsBlock(max_live=-2).max_live == 3
    assert AgentsBlock(max_live="banana").max_live == 3
    assert AgentsBlock(max_live=True).max_live == 3


def test_provider_limits_is_per_provider_and_invalid_shape_keeps_safe_default():
    # x-c703 widened the value from a bare lane count to a ProviderBudget; x-3f84
    # W5 renamed the key. The scalar spelling stays legal and reads as `lanes`;
    # zai's built-in `subagents = 1` rides along, because an install that
    # predates the dimension is exactly the one that must not fan out on a
    # shared account.
    lanes = {
        name: (b.lanes, b.subagents)
        for name, b in AgentsBlock(
            provider_limits={"zai": 2, "openai": 7}
        ).provider_limits.items()
    }
    assert lanes == {"zai": (2, 1), "openai": (7, None)}
    assert AgentsBlock(provider_limits={}).provider_limits == {}
    closed = AgentsBlock(provider_limits={"zai": 0, "anthropic": {"lanes": 0}}).provider_limits
    assert (closed["zai"].lanes, closed["zai"].subagents) == (0, 1)
    assert closed["anthropic"].lanes == 0
    for broken in ({"zai": -1}, {"zai": {"subagents": 0}}, "broken", {"zai": True}, {"zai": {"bogus": 1}}):
        restored = AgentsBlock(provider_limits=broken).provider_limits["zai"]
        assert (restored.lanes, restored.subagents) == (5, 1), broken


def test_min_free_gb_zero_is_valid_disable():
    # <= 0 means "guard disabled", NOT an invalid value.
    assert AgentsBlock(min_free_gb=0).min_free_gb == 0.0
    assert AgentsBlock(min_free_gb=-1).min_free_gb == -1.0


# --- x-7198: legacy agents.spawn_permission_mode migrates onto
# agents.defaults.permission_mode; the field itself is retired. ---------------
