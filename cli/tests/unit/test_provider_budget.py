"""The per-provider budget, and the review route that consumes it (x-c703).

Every assertion here names a POSITIVE marker: a reason string, a resolved
route, a number. None of them asserts that a panel was absent. An absence has
two explanations, the real outcome and "the instrument never ran", and a test
built on one cannot tell them apart - which is the failure that let an
uncapped fan-out read as a pass on the night this node was filed.
"""

from __future__ import annotations

import pytest

from fno.agents.model_routing import ROUTE_PROVIDER_ENV
from fno.config import (
    AgentsBlock,
    assert_subagent_budget,
)


def _budget(**kwargs) -> tuple[int | None, int | None]:
    b = AgentsBlock(**kwargs).provider_limits.get("zai")
    return (None, None) if b is None else (b.lanes, b.subagents)


def test_legacy_scalar_reads_as_lanes_and_keeps_the_builtin_subagents():
    # AC3-HP. An install written before the record widened must still get the
    # protection, or shipping a built-in for a shared account buys nothing.
    assert _budget(max_lanes={"zai": 5}) == (5, 1)  # legacy kwarg: exercises the alias


@pytest.mark.parametrize("written", [{"zai": 0}, "broken"])
def test_a_malformed_table_restores_the_safe_builtin(written):
    # Restoring the whole built-in table, rather than dropping one entry, is
    # what keeps a typo from making a shared provider unlimited.
    assert _budget(max_lanes=written) == (5, 1)


# --- assert_subagent_budget: the relocated refusing consumer (x-25a7 w5) ---
#
# The panel-width refusal outlived the module that hosted it: the gate lives
# again here, fan-out-neutral: any site that DECLARES
# a width in skill text calls this before dispatching (Locked Decision 7).


def test_a_width_over_budget_is_refused_with_all_three_names():
    # AC3b-MARKER. The refusal must name the provider, the declared width and
    # the budget, or a wedged session cannot tell what to shrink.
    check = assert_subagent_budget(4, "zai")
    assert check.permitted is False
    assert "zai" in check.reason
    assert "4" in check.reason
    assert "budget of 1" in check.reason


def test_every_unreadable_input_permits_and_records_why():
    # AC3b-ERR. Unreadable width, no-budget provider, unknown provider: all
    # three permit, none silently. Fail-open here is the documented posture of
    # provider_subagent_budget; refusing over an unreadable config would wedge
    # every session on the machine.
    unreadable_width = assert_subagent_budget(None, "zai")
    assert unreadable_width.permitted is True and "unreadable" in unreadable_width.reason
    no_entry = assert_subagent_budget(4, "anthropic")
    assert no_entry.permitted is True and "no budget entry" in no_entry.reason
    unknown = assert_subagent_budget(4, "unknown")
    assert unknown.permitted is True


def test_the_env_stamp_drives_the_check_when_no_provider_is_passed(monkeypatch):
    monkeypatch.setenv("FNO_ROUTE_PROVIDER", "zai")
    check = assert_subagent_budget(4)
    assert check.permitted is False
    assert "zai" in check.reason


# --- route resolution -------------------------------------------------------
#
# The panel whose width this section measured is retired, and the resolver
# verdicts it pinned (the budget-1 provider resolving the default reviewer to a
# literal `satisfiable`, retired sigma refusing with the replacement named)
# are golden-covered in crates/fno-agents/tests/golden/review_capability/.
# The provider-stamp tests above delete with their module: the env read they
# pinned now lives in the native detect_session, golden-covered.


# --- the stamp is set or cleared, never inherited ---------------------------


def test_a_stamp_alone_does_not_count_as_a_recorded_route(tmp_path):
    # A settings file holding the scrub floor plus a provider name records no
    # route, and relaunching from it would bill the default account in silence.
    import json

    from fno.agents.model_routing import RouteRestoreError, read_route_settings

    path = tmp_path / "route.json"
    path.write_text(
        json.dumps({"env": {"ANTHROPIC_API_KEY": "", ROUTE_PROVIDER_ENV: "zai"}}),
        encoding="utf-8",
    )
    with pytest.raises(RouteRestoreError, match="records no route"):
        read_route_settings(str(path))
