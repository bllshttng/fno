"""`backlog.maintain.*` and `backlog.staleness_days`: defaults, overrides, and
the positive-int floor every knob enforces."""

from __future__ import annotations

import pytest
from pydantic import ValidationError

from fno.config import BacklogBlock, ConfigBlock, MaintainBlock

_DEFAULTS = {
    "staleness_days": 30,
    "max_failed_attempts": 3,
    "abandoned_do_row_hours": 24,
    "budget_seconds": 300,
}


def test_maintain_block_defaults():
    maintain = ConfigBlock().backlog.maintain
    for key, want in _DEFAULTS.items():
        assert getattr(maintain, key) == want, key
    assert BacklogBlock().staleness_days == 21


def test_maintain_block_takes_positive_overrides():
    for key, default in _DEFAULTS.items():
        block = BacklogBlock(maintain={key: default + 1})
        assert getattr(block.maintain, key) == default + 1, key


def test_maintain_block_rejects_non_positive():
    for key in _DEFAULTS:
        with pytest.raises(ValidationError):
            MaintainBlock(**{key: 0})
    with pytest.raises(ValidationError):
        BacklogBlock(staleness_days=0)
