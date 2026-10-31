"""Schema registration for the ``operator_decision`` event.

The event log is schema-validated: an unregistered type is refused at emit,
so the schema row is the prerequisite every producer depends on.
"""
from __future__ import annotations

import pytest

from fno.events.store_client import ValidationError, build_event, validate


def test_validate_rejects_missing_required_field() -> None:
    event = {
        "ts": "2026-08-14T00:00:00Z",
        "type": "operator_decision",
        "source": "target",
        "data": {"decision_id": "d-ab12cd34"},
    }
    with pytest.raises(ValidationError, match="decision"):
        validate(event)

    # decision_span: span_kind and trace are the required pair; span_kind,
    # route and class are enum-checked at validate (the same chokepoint the
    # termination reason uses) so a typo'd hop cannot land as an
    # unrecognized bucket the eval grouping cannot read.
    def _span(**data) -> dict:
        base = {
            "ts": "2026-10-02T19:00:00Z",
            "type": "decision_span",
            "source": "target",
            "data": {
                "span_kind": "ask",
                "trace": {
                    "trace_id": "x-98cf",
                    "span_id": "s-1a2b3c4d",
                    "actor_kind": "worker",
                    "comms": "mail",
                },
            },
        }
        base["data"].update(data)
        return base

    validate(_span())  # the required pair plus a legal span_kind passes
    with pytest.raises(ValidationError, match="missing required data field: span_kind"):
        validate(
            {
                "ts": "2026-10-02T19:00:00Z",
                "type": "decision_span",
                "source": "target",
                "data": {
                    "trace": {"trace_id": "x-98cf", "span_id": "s-1a2b3c4d", "actor_kind": "worker", "comms": "mail"},
                },
            }
        )
    with pytest.raises(ValidationError, match="span_kind"):
        validate(_span(span_kind="vibe"))
    with pytest.raises(ValidationError, match="data.class"):
        validate(_span(span_kind="route", route="escalate", **{"class": "cheap"}))


def test_decision_retracted_is_registered_and_carries_target() -> None:
    event = build_event("decision_retracted", "target", {
        "retraction_id": "r-test", "target_decision_id": "d-ab12cd34",
        "subject": "x-7d94", "reason": "the coordination window ended",
        "retracted_by": "worker-1", "authority_source": "agent",
    })
    validate(event)
    assert event["type"] == "decision_retracted"
    assert event["data"]["target_decision_id"] == "d-ab12cd34"
    assert event["data"]["reason"] == "the coordination window ended"


class TestReviewAttestationFindingRecord:
    """The optional finding record on review_attestation.

    Optional as a whole, but a PRESENT key must be the right shape: this is
    the record the coverage gate re-derives blocking from, and a silently
    malformed one is a forged count one level down.
    """

    @staticmethod
    def _event(**data):
        base = {
            "ts": "2026-08-25T23:00:00Z",
            "type": "review_attestation",
            "source": "hook",
            "data": {"reviewer": "code-review", "head_sha": "a" * 40, "verdict": "fail", "session_id": "s-1"},
        }
        base["data"].update(data)
        return base

    def test_record_carrying_event_validates(self) -> None:
        validate(
            self._event(
                findings_blocking=1,
                findings_nonblocking=1,
                findings=[
                    {
                        "category": "correctness",
                        "verdict": None,
                        "blocking": True,
                        "has_required_fields": True,
                        "finding_key": "a.py:3:correctness",
                    }
                ],
                review_round=2,
                dispositions=[
                    {"finding_key": "a.py:3:correctness", "disposition": "declined", "reason": "not applicable"}
                ],
            )
        )

    def test_disposition_without_reason_refused(self) -> None:
        with pytest.raises(ValidationError):
            validate(
                self._event(
                    dispositions=[
                        {"finding_key": "a.py:3:correctness", "disposition": "declined", "reason": "  "}
                    ]
                )
            )


# ---------------------------------------------------------------------------
# The machine-scoped skip vocabulary
# ---------------------------------------------------------------------------


def _advance_skipped(reason: str, **extra) -> dict:
    return build_event("advance_skipped", "backlog", {"reason": reason, **extra})


def test_advance_skipped_accepts_machine_scoped_reasons() -> None:
    validate(_advance_skipped("capacity-refused", exit_code=79, detail="spawn-gate: load"))
    validate(_advance_skipped("gate-unavailable", exit_code=81, detail="spawn-gate: registry"))
    validate(_advance_skipped("sandbox-unreachable", exit_code=82, detail="sandbox-probe: gh is unreachable"))
    # Historic emission, declared late: the enum admits rows already in journals.
    validate(_advance_skipped("slot-queue-deferred", retry_at=1234.0))
