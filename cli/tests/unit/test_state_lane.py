from __future__ import annotations


import pytest

from fno.test_cmd import (
    _is_state_control,
    _parse_smoke_args,
    _state_both_exit,
)


def test_state_mode_parser_rejects_unknown_shapes():
    with pytest.raises(ValueError, match="--state 'invalid'.*clean/populated/both"):
        _parse_smoke_args(["--state", "invalid"])


def test_state_both_exit_green_only_when_control_fires_and_diff_is_canary_only():
    canary = "tests.unit.test_state_canary::test_state_canary_detects_populated_state"
    other = "tests.unit.test_state::reads_live_graph"

    # Healthy run: control fired, only the canary changed. The populated lane
    # itself exited red on the canary, so the raw populated rc must NOT decide.
    assert _state_both_exit(0, [canary], "passed", "failed") == 0
    # A real leak: some testcase other than the canary changed verdict.
    assert _state_both_exit(0, [canary, other], "passed", "failed") == 1
    # Control disarmed or never ran: never green.
    assert _state_both_exit(0, [canary], "passed", "passed") == 1
    assert _state_both_exit(0, [canary], None, "failed") == 1
    # Empty verdict set is unmeasurable, never green.
    assert _state_both_exit(0, [], "passed", "failed") == 1
    # A red clean lane is red, whatever the diff says.
    assert _state_both_exit(3, [canary], "passed", "failed") == 3
    # The filesystem canary refused in a lane. That lane may be the populated
    # one, whose rc is discarded by design, so the flag is the only carrier.
    assert _state_both_exit(0, [canary], "passed", "failed", True) == 1


def test_state_both_exit_is_red_when_a_lane_never_took_a_baseline():
    canary = "tests.unit.test_state_canary::test_state_canary_detects_populated_state"

    # A plant that failed is deliberately NOT reported as a leak, so the leak
    # flag stays False. In the populated lane the exit code is discarded, so
    # without its own carrier the broken instrument reaches the verdict as
    # nothing at all and an otherwise-healthy comparison reads green.
    assert _state_both_exit(0, [canary], "passed", "failed", False, True) == 1
    # The two signals stay independent: neither one implies the other.
    assert _state_both_exit(0, [canary], "passed", "failed", True, False) == 1
    assert _state_both_exit(0, [canary], "passed", "failed", False, False) == 0


def test_the_control_matcher_does_not_accept_a_wiring_testcase():
    control = "tests.unit.test_state_canary::test_state_canary_detects_populated_state"
    wiring = "tests.unit.test_state_canary_wiring::test_the_verb_reaches_the_script_verbatim"

    assert _is_state_control(control)
    # A substring match on "test_state_canary" accepts this, and under parallel
    # execution its passing verdict is then read as the control's.
    assert not _is_state_control(wiring)
    assert _is_state_control(control + "[param]")
