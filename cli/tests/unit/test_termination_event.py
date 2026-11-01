from __future__ import annotations

import pytest

from fno.events.store_client import ValidationError, validate


def test_unknown_termination_reason_fails_loud() -> None:
    event = {
        "ts": "2026-08-23T06:00:00Z",
        "type": "termination",
        "source": "hook",
        "data": {"session_id": "session-1", "reason": "DoneTypo", "message": "done"},
    }
    with pytest.raises(ValidationError, match="unknown termination data.reason"):
        validate(event)
