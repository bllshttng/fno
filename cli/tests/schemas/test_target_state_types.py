"""Tests for TargetState invariant tightening (FOLLOW-UPS #15, #16, #17).

Covers bundled Pydantic invariant changes on cli/src/fno/schemas/target.py:
  #15  clean_passed / goal_verification_passed / browser_testing_passed: Optional[str] -> bool
       with a legacy-coercion field_validator and a once-per-session DeprecationWarning
  #16  merged_prs: List[Any] -> List[int]
       merge_failed, conflicts_resolved: List[Any] -> List[MergeFailureRecord|ConflictResolutionRecord]
  #17  provenance_nonce: new Optional[str] = Field(pattern=r"^[a-f0-9]{16}$")

AC2-UI (type-design-analyzer dispatch in the review phase) is a review-phase obligation
per Locked Decision #10 in the design doc. It is NOT a unit-test invariant and therefore
has no direct test case in this file. The reviewer must dispatch type-design-analyzer
during /review.
"""

import warnings

import pytest
from pydantic import ValidationError

from fno.schemas.target import TargetState


# ---------------------------------------------------------------------------
# #15 -- gate fields bool coercion + deprecation warning
# ---------------------------------------------------------------------------


class TestGateBoolCoercion:
    """AC2-HP, AC2-FR for clean_passed / goal_verification_passed / browser_testing_passed."""

    def test_ac2_fr_passed_string_coerces_to_true_with_warning(self):
        """AC2-FR (#15): 'passed' coerces to True and emits DeprecationWarning."""
        with pytest.warns(DeprecationWarning):
            state = TargetState.model_validate({"clean_passed": "passed"})
        assert state.clean_passed is True

    def test_ac2_fr_skipped_string_coerces_to_false_with_warning(self):
        """AC2-FR (#15): 'skipped' coerces to False and emits DeprecationWarning."""
        with pytest.warns(DeprecationWarning):
            state = TargetState.model_validate({"clean_passed": "skipped"})
        assert state.clean_passed is False

    def test_ac2_fr_warning_fires_exactly_once_per_field_value(self):
        """AC2-FR (#15): for a given (field, value) pair, exactly 1 DeprecationWarning fires
        across 10 model_validate calls (module-level seen-set deduplification).

        We temporarily clear the seen-set to guarantee this (field, value) pair is
        fresh for the loop, then restore the seen-set so other tests are unaffected.
        """
        import fno.schemas.target as target_module

        pair = ("goal_verification_passed", "passed")
        # Save and clear the seen-set so this pair is definitely fresh.
        original_seen = target_module._GATE_COERCION_WARNED.copy()
        target_module._GATE_COERCION_WARNED.discard(pair)

        try:
            with warnings.catch_warnings(record=True) as caught:
                warnings.simplefilter("always")
                for _ in range(10):
                    TargetState.model_validate({"goal_verification_passed": "passed"})
        finally:
            # Restore original seen-set state
            target_module._GATE_COERCION_WARNED.clear()
            target_module._GATE_COERCION_WARNED.update(original_seen)

        deprecation_warnings = [
            w for w in caught
            if issubclass(w.category, DeprecationWarning)
            and "goal_verification_passed" in str(w.message)
        ]
        # Exactly 1 warning for this (field, value) pair across 10 validations
        assert len(deprecation_warnings) == 1, (
            f"Expected exactly 1 DeprecationWarning for (goal_verification_passed, 'passed'), "
            f"got {len(deprecation_warnings)}"
        )


# ---------------------------------------------------------------------------
# #16 -- typed list fields
# ---------------------------------------------------------------------------

class TestTypedListFields:
    """AC2-HP, AC2-ERR, AC2-FR for merged_prs, merge_failed,
    conflicts_resolved."""

    def test_ac2_err_merged_prs_rejects_non_int(self):
        """AC2-ERR (#16): non-int elements in merged_prs raise ValidationError."""
        with pytest.raises(ValidationError):
            TargetState(merged_prs=["not-int"])

    def test_ac2_err_merge_failed_rejects_missing_required_fields(self):
        """AC2-ERR (#16): merge_failed record must have 'pr' (int) and 'reason' (str)."""
        with pytest.raises(ValidationError):
            TargetState(merge_failed=[{"reason": "no pr field"}])

    def test_ac2_fr_conflicts_resolved_extra_keys_tolerated(self):
        """AC2-FR (#16): ConflictResolutionRecord has extra='allow'."""
        state = TargetState(conflicts_resolved=[{"files": ["a.py", "b.py"], "extra": "ok"}])
        assert state.conflicts_resolved[0].pr is None


# ---------------------------------------------------------------------------
# #17 -- provenance_nonce regex
# ---------------------------------------------------------------------------

class TestProvenanceNonce:
    """AC2-HP, AC2-EDGE, AC2-FR for provenance_nonce field."""

    def test_ac2_edge_uppercase_rejected(self):
        """AC2-EDGE (#17): uppercase hex chars rejected (only lowercase allowed)."""
        with pytest.raises(ValidationError):
            TargetState(provenance_nonce="FF22B603121DDDF7")  # uppercase

    def test_ac2_fr_backward_compat_nonce_not_in_model_extra(self):
        """AC2-FR (#17): state dicts that previously stored nonce via extra='allow' now
        load it into the modeled field. The value must NOT appear in model_extra."""
        state = TargetState.model_validate(
            {"provenance_nonce": "ff22b603121dddf7", "other_extra": "x"}
        )
        assert state.provenance_nonce == "ff22b603121dddf7"
        # nonce is now a modeled field; it must NOT appear in model_extra
        assert "provenance_nonce" not in state.model_extra
        # other_extra falls through to model_extra (extra="allow" on TargetState)
        assert state.model_extra.get("other_extra") == "x"


# ---------------------------------------------------------------------------
# Fix 3: _coerce_gate_field accepts bare int (0/1)
# ---------------------------------------------------------------------------


class TestGateBoolIntCoercion:
    """Fix 3: _coerce_gate_field must accept bare int (0/1) as bool."""

    def test_ac2_fr_int_coerces_to_bool(self):
        """AC2-FR (Fix 3): bare int 1 coerces to True; 0 coerces to False.

        A YAML writer emitting clean_passed: 1 (bare int, not bool) must not
        crash validation. No DeprecationWarning fires for bare int -- it is
        not a legacy-string shape, just an alternate writer representation.
        """
        import warnings as _warnings
        with _warnings.catch_warnings(record=True) as caught:
            _warnings.simplefilter("always")
            state_one = TargetState(clean_passed=1)
            state_zero = TargetState(clean_passed=0)

        assert state_one.clean_passed is True
        assert state_zero.clean_passed is False

        # No DeprecationWarning should fire for int input
        dep_warnings = [w for w in caught if issubclass(w.category, DeprecationWarning)]
        assert len(dep_warnings) == 0, (
            f"Unexpected DeprecationWarning(s) for int gate input: {dep_warnings}"
        )
