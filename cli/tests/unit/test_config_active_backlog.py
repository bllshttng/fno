"""Unit tests for the config.active_backlog typed block (node x-c070).

The always-on backlog dispatcher daemon's config gate. Posture parity target:
config.auto_continue / config.target.blast - default OFF, a malformed block
degrades to disabled rather than raising out of the whole settings load, a bad
scalar field is dropped to its default (never raised), and an invalid interval
fails CLOSED (the feature disables) rather than spinning a 0-sleep hot loop.
"""
from __future__ import annotations

from fno.config import ActiveBacklogConfig, ConfigBlock


def test_defaults_are_off_and_serial():
    b = ActiveBacklogConfig()
    assert b.enabled is False
    assert b.interval == "5m"
    assert b.failure_limit == 3
    assert b.max_concurrent == 1
    assert b.mission is None
    assert b.interval_seconds() == 300
    assert b.any_enabled() is False
    assert b.is_enabled_for("footnote") is False


def test_enabled_per_project_map():
    b = ActiveBacklogConfig(enabled={"footnote": True, "readyrule": False})
    assert b.is_enabled_for("footnote") is True
    assert b.is_enabled_for("readyrule") is False
    # a project absent from the map is not enabled
    assert b.is_enabled_for("other") is False
    # a None project can never match a map
    assert b.is_enabled_for(None) is False
    assert b.any_enabled() is True
    assert b.enabled_projects() == ["footnote"]


def test_enabled_scalar_typo_fails_safe_to_disabled():
    # false-enabled is the dangerous direction for an autonomous-dispatch opt-in.
    assert ActiveBacklogConfig(enabled="banana").enabled is False
    assert ActiveBacklogConfig(enabled=[1, 2]).enabled is False
    assert ActiveBacklogConfig(enabled=None).enabled is False
    # affirmative scalars still enable
    assert ActiveBacklogConfig(enabled="true").enabled is True
    assert ActiveBacklogConfig(enabled="on").enabled is True


def test_invalid_interval_fails_closed_not_raises():
    # zero, negative, and unparseable intervals disable the feature rather than
    # spinning a 0-sleep hot loop (Boundaries) - and must NOT raise.
    # NB: a bare positive digit string like "5" is now valid (5 seconds); only
    # zero/negative/non-numeric/unit-less-non-digit forms fail closed.
    for bad in ("0s", "0m", "banana", "-5m", "", "m", "5x"):
        b = ActiveBacklogConfig(enabled=True, interval=bad)
        assert b.interval_seconds() is None, bad
        assert b.is_enabled_for("footnote") is False, bad
        assert b.any_enabled() is False, bad


def test_max_concurrent_bad_scalar_dropped_to_default():
    assert ActiveBacklogConfig(max_concurrent=0).max_concurrent == 1
    assert ActiveBacklogConfig(max_concurrent="banana").max_concurrent == 1
    assert ActiveBacklogConfig(max_concurrent=4).max_concurrent == 4


def test_max_live_per_territory_bad_scalar_dropped_to_default():
    from fno.config import AgentsBlock

    assert AgentsBlock(max_live_per_territory=0).max_live_per_territory == 4
    assert AgentsBlock(max_live_per_territory=-2).max_live_per_territory == 4
    assert AgentsBlock(max_live_per_territory="banana").max_live_per_territory == 4
    assert AgentsBlock(max_live_per_territory=True).max_live_per_territory == 4
    # valid values pass through (incl. numeric strings)
    assert AgentsBlock(max_live_per_territory=2).max_live_per_territory == 2
    assert AgentsBlock(max_live_per_territory="6").max_live_per_territory == 6
    assert ActiveBacklogConfig().mission is None


def test_malformed_block_degrades_to_defaults_via_configblock():
    # A scalar/list/null active_backlog: cannot build the block; the parent
    # coercer must fall back to a default disabled block, not raise.
    for bad in (42, "banana", [1, 2], None):
        cfg = ConfigBlock(active_backlog=bad)
        assert isinstance(cfg.active_backlog, ActiveBacklogConfig)
        assert cfg.active_backlog.any_enabled() is False


# ---------------------------------------------------------------------------
# x-7f1f task 2.2: active_backlog.mission is IGNORED. The mission-scoped drain
# resolves one target per ACTIVE mission (an epic with mission_active=true);
# the config key selects nothing and survives parseable for one release, with
# `fno config doctor` naming the live axis when it is set.
# ---------------------------------------------------------------------------
