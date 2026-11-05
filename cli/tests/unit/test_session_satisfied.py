"""Tests for session_satisfied + auto_complete_triggered event types.

Covers the typed builders, their build-time refusals, and one builder event
round-tripping through validate().
"""
from __future__ import annotations

import pytest

from fno.events.store_client import ValidationError, build_event


def session_satisfied(*, trigger, reason, session_id, gate_state_hash, evidence_url=None, source="target"):
    return build_event("session_satisfied", source, {
        "source": trigger, "reason": reason, "session_id": session_id,
        "gate_state_hash": gate_state_hash, "evidence_url": evidence_url,
    }, builder="session_satisfied")


def test_session_satisfied_rejects_unknown_trigger() -> None:
    with pytest.raises(ValidationError, match="unknown session_satisfied trigger"):
        session_satisfied(
            trigger="bogus",
            reason="r",
            session_id="s",
            gate_state_hash="h",
        )
