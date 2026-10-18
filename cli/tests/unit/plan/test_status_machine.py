"""Tests for frontmatter status state machine (Task 1.2)."""

import pytest
from fno.plan._status import (
    STATUS_PROGRESSION,
    StatusTransitionError,
    coerce_status_from_yaml,
    validate_transition,
)


class TestStatusProgression:
    def test_progression_contains_all_expected_statuses(self):
        # reviewing/shipping pruned (x-f34f): zero consumers, no graph state.
        # `idea` sits below design: a decompose scaffold is a plan doc at a
        # pre-design rung, which the old axis had no word for.
        assert STATUS_PROGRESSION == (
            "idea",
            "design",
            "ready",
            "in_progress",
            "in_review",
        )


class TestValidateTransition:
    # Forward transitions - all must pass
    def test_AC1_HP_design_to_in_review_extreme_skip(self):
        validate_transition("design", "in_review")

    # Identity transitions - must raise
    def test_AC3_ERR_identity_ready_to_ready_rejected(self):
        with pytest.raises(StatusTransitionError):
            validate_transition("ready", "ready")

    # Unknown statuses - must raise
    def test_AC4_ERR_unknown_new_status_rejected(self):
        with pytest.raises(StatusTransitionError):
            validate_transition("ready", "wip")

    def test_AC4_EDGE_error_message_is_informative(self):
        with pytest.raises(StatusTransitionError) as exc_info:
            validate_transition("in_review", "ready")
        assert "in_review" in str(exc_info.value)
        assert "ready" in str(exc_info.value)


class TestCoerceStatusFromYaml:
    # YAML bool coercion (per feedback_literal_string_rejects_yaml_bool)
    def test_AC5_HP_true_bool_coerces_to_string_then_rejected(self):
        # True -> "true" which is not in STATUS_PROGRESSION -> StatusTransitionError
        with pytest.raises(StatusTransitionError):
            coerce_status_from_yaml(True)


class TestRetiredSpellings:
    """x-3ad5: `shipped`/`archived` are retired as status VALUES and accepted on
    read, so a vault doc stamped under the old vocabulary keeps working.
    """

    def test_AC2_FR_alias_resolves_to_the_surviving_spelling(self):
        from fno.plan._status import canonical_status

        assert canonical_status("shipped") == "in_review"
        assert canonical_status("archived") == "superseded"
        # Quoting and casing are normalized the same way as any other status.
        assert canonical_status('  "Shipped" ') == "in_review"

    def test_AC2_FR_transition_off_the_old_spelling_reads_as_its_survivor(self):
        # `shipped` ranks where `in_review` ranks, so a backward move still raises.
        with pytest.raises(StatusTransitionError):
            validate_transition("shipped", "ready")
        # ...and an identity move through the alias is still an identity move.
        with pytest.raises(StatusTransitionError):
            validate_transition("shipped", "in_review")
