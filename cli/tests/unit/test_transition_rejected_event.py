from __future__ import annotations

import pytest

from fno.events.store_client import ValidationError, validate


def test_transition_rejected_rejects_unknown_enum_values() -> None:
    event = {
        "ts": "2026-08-23T06:00:00Z",
        "type": "transition_rejected",
        "source": "hook",
        "data": {
            "session_id": "20260823T060900Z-cx73523-e04109",
            "kind": "not-a-real-value",
            "event": "terminal_decided",
            "from": "working",
            "error": "invalid",
        },
    }
    with pytest.raises(ValidationError, match="unknown transition_rejected data.kind"):
        validate(event)
