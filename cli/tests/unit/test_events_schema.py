"""Schema registration for the ``operator_decision`` event.

The event log is schema-validated: an unregistered type is refused at emit,
so the schema row is the prerequisite every producer depends on.
"""
from __future__ import annotations

import pytest

from fno.events.store_client import ValidationError, validate


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


# ---------------------------------------------------------------------------
# The machine-scoped skip vocabulary
# ---------------------------------------------------------------------------
