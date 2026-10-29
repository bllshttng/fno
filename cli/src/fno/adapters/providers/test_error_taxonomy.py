"""Construction guards of the provider error taxonomy's Python leg.

Run: cd cli && uv run pytest src/fno/adapters/providers/test_error_taxonomy.py -v

The classification, the cooldown rules and the reset-stamp harvest are pinned
by the Rust parity goldens (crates/fno-agents/tests/accounts_error_class_parity.rs),
captured from this module. What stays here is Python-only: the dataclass
guards that refuse a hand-built value the Rust types cannot express.
"""
from __future__ import annotations

import pytest

from fno.adapters.providers.error_taxonomy import (
    ErrorClass,
    ErrorRule,
    NormalizedError,
)


def test_inconsistent_triggers_swap_raises() -> None:
    with pytest.raises(ValueError, match="triggers_swap"):
        NormalizedError(
            error_class=ErrorClass.PROVIDER_5XX,
            raw_status=529,
            raw_exit_code=None,
            body_excerpt="",
            triggers_swap=False,
        )


def test_rejects_empty_model_string() -> None:
    with pytest.raises(ValueError, match="non-empty"):
        NormalizedError(
            error_class=ErrorClass.PROVIDER_4XX_QUOTA,
            raw_status=429,
            raw_exit_code=None,
            body_excerpt="",
            triggers_swap=True,
            model="",
        )


def test_error_rule_rejects_both_text_and_status() -> None:
    with pytest.raises(ValueError, match="text or status"):
        ErrorRule(text="rate limit", status=429, backoff=True)
