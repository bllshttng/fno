"""Tests for section ownership allowlist (Task 1.2)."""

import pytest
from fno.plan._ownership import (
    BLUEPRINT_WRITE_ALLOWLIST,
    OwnershipViolation,
    assert_blueprint_can_write,
    check_blueprint_can_write,
)


class TestBlueprintWriteAllowlist:
    def test_AC1_HP_allowlist_is_exactly_this_set(self):
        # Pins membership, not just size: a count passes when someone swaps one
        # entry for another, which is the silent widening this guards against.
        assert BLUEPRINT_WRITE_ALLOWLIST == frozenset(
            {
                "Execution Strategy",
                "File Ownership Map",
                "Five questions",
                "Patterns to Reuse",
                "kill_criteria",
                "execution_mode",
                "waves",
                "acceptance_contract",
                "consolidation",
                "difficulty",
                "join",
                "carveouts",
            }
        )


class TestAssertBlueprintCanWrite:
    def test_AC2_HP_execution_strategy_allowed(self):
        assert_blueprint_can_write("Execution Strategy")  # no exception

    def test_AC1_ERR_retired_orchestration_spelling_rejected(self):
        with pytest.raises(OwnershipViolation) as exc_info:
            assert_blueprint_can_write("orchestration")
        msg = str(exc_info.value)
        assert "orchestration" in msg
        assert "join" in msg

    def test_AC4_EDGE_lowercase_execution_strategy_rejected(self):
        with pytest.raises(OwnershipViolation):
            assert_blueprint_can_write("execution strategy")

    def test_AC5_HP_error_message_includes_allowlist(self):
        with pytest.raises(OwnershipViolation) as exc_info:
            assert_blueprint_can_write("Overview")
        msg = str(exc_info.value)
        # Allowlist entries (sorted) should appear in error
        assert "Execution Strategy" in msg
        assert "File Ownership Map" in msg
        assert "Patterns to Reuse" in msg
        assert "kill_criteria" in msg
        assert "execution_mode" in msg
        assert "waves" in msg


class TestCheckBlueprintCanWrite:
    def test_AC6_HP_returns_false_for_disallowed(self):
        assert check_blueprint_can_write("Overview") is False
        assert check_blueprint_can_write("Architecture") is False
        assert check_blueprint_can_write("") is False
