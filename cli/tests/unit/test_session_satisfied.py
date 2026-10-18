"""Tests for session_satisfied + auto_complete_triggered event types.

Covers the typed builders, their build-time refusals, and one builder event
round-tripping through validate().
"""

from __future__ import annotations

import pytest

from fno.events import (
    ValidationError,
    auto_complete_triggered,
    session_satisfied,
    validate,
)


def test_session_satisfied_happy_path() -> None:
    event = session_satisfied(
        trigger="check_pr",
        reason="external_review_approved",
        session_id="s1",
        gate_state_hash="abc123",
        evidence_url="https://github.com/o/r/pull/1",
    )
    assert event["type"] == "session_satisfied"
    assert event["source"] == "target"
    assert event["data"]["source"] == "check_pr"
    assert event["data"]["reason"] == "external_review_approved"
    assert event["data"]["session_id"] == "s1"
    assert event["data"]["gate_state_hash"] == "abc123"
    assert event["data"]["evidence_url"] == "https://github.com/o/r/pull/1"


def test_session_satisfied_rejects_unknown_trigger() -> None:
    with pytest.raises(ValidationError, match="unknown session_satisfied trigger"):
        session_satisfied(
            trigger="bogus",
            reason="r",
            session_id="s",
            gate_state_hash="h",
        )


def test_auto_complete_triggered_happy_path() -> None:
    event = auto_complete_triggered(trigger="check_pr", session_id="s1")
    assert event["type"] == "auto_complete_triggered"
    assert event["source"] == "hook"
    assert event["data"]["source"] == "check_pr"
    assert event["data"]["session_id"] == "s1"


def test_session_satisfied_rejects_empty_reason() -> None:
    """The audit-load-bearing reason field cannot be empty/whitespace."""
    with pytest.raises(ValidationError, match="reason cannot be empty"):
        session_satisfied(
            trigger="check_pr",
            reason="   ",
            session_id="s",
            gate_state_hash="h",
        )


def test_session_satisfied_delegated_validates() -> None:
    """AC1-HP: a delegated event round-trips through validate() cleanly."""
    event = session_satisfied(
        trigger="delegated",
        reason="handoff_helper_archived_manifest",
        session_id="s-delegated",
        gate_state_hash="abc123",
    )
    assert validate(event) is None
