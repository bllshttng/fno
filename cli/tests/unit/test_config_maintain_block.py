"""`backlog.maintain.*` and `backlog.staleness_days`: defaults, overrides, and
the positive-int floor every knob enforces."""

from __future__ import annotations

import pytest
from pydantic import ValidationError

from fno.config import BacklogBlock, MaintainBlock

_DEFAULTS = {
    "staleness_days": 60,
    "max_failed_attempts": 3,
    "abandoned_do_row_hours": 24,
    "budget_seconds": 300,
}


def test_maintain_block_rejects_non_positive():
    for key in _DEFAULTS:
        with pytest.raises(ValidationError):
            MaintainBlock(**{key: 0})
    with pytest.raises(ValidationError):
        BacklogBlock(staleness_days=0)
