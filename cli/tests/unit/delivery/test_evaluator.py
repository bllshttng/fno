from __future__ import annotations

from collections.abc import Callable
from datetime import datetime, timedelta, timezone

import pytest
from pydantic import ValidationError

from fno.company.contracts import (
    CompanyWorkRefs,
    DeliverableRef,
    EffectRef,
    EvidenceRef,
    EvidenceResult,
    EvidenceSubjectKind,
    FunctionRef,
    WorkOrderRef,
)
from fno.delivery.adapters import adapt_delivery_event
from fno.delivery import (
    DeliveryVerdict,
    DELIVERY_EVALUATOR_VERSION,
    DELIVERY_EVIDENCE_FACT_VERSION,
    DeliveryEvidenceFact,
    evaluate_delivery,
)

NOW = datetime(2026, 8, 2, 12, tzinfo=timezone.utc)
NODE_ID = "x-delivery"
ATTEMPT_ID = "attempt-1"
REQUIREMENTS = (
    ("artifact-ready", EvidenceSubjectKind.ARTIFACT, "artifact-1"),
    ("review-ready", EvidenceSubjectKind.REVIEW, "review-1"),
    ("approval-ready", EvidenceSubjectKind.APPROVAL, "approval-1"),
    ("probe-ready", EvidenceSubjectKind.PROBE, "probe-1"),
    ("ack-ready", EvidenceSubjectKind.ACKNOWLEDGMENT, "ack-1"),
)


def _company_work(function_name: str = "engineering") -> CompanyWorkRefs:
    return CompanyWorkRefs(
        function=FunctionRef(id=function_name),
        work_order=WorkOrderRef(node_id=NODE_ID, attempt_id=ATTEMPT_ID),
        deliverables=(
            DeliverableRef(
                id="release",
                kind="arbitrary-deliverable",
                work_order_id=NODE_ID,
                attempt_id=ATTEMPT_ID,
                required_evidence_ids=tuple(item[0] for item in REQUIREMENTS),
            ),
        ),
        evidence=tuple(
            EvidenceRef(
                id=evidence_id,
                work_order_id=NODE_ID,
                attempt_id=ATTEMPT_ID,
                subject_kind=subject_kind,
                subject_id=subject_id,
                result=EvidenceResult.PASSED,
            )
            for evidence_id, subject_kind, subject_id in REQUIREMENTS
        ),
    )


def _fact(
    evidence_id: str,
    *,
    result: EvidenceResult = EvidenceResult.PASSED,
    producer: str = "adapter:test",
    observed_at: datetime = NOW,
    fresh_until: datetime | None = None,
    attempt_id: str = ATTEMPT_ID,
    subject_kind: EvidenceSubjectKind | None = None,
    subject_id: str | None = None,
    fact_revision: str = "snapshot-7",
) -> DeliveryEvidenceFact:
    requirement = next(item for item in REQUIREMENTS if item[0] == evidence_id)
    return DeliveryEvidenceFact(
        version=DELIVERY_EVIDENCE_FACT_VERSION,
        evidence=EvidenceRef(
            id=evidence_id,
            work_order_id=NODE_ID,
            attempt_id=attempt_id,
            subject_kind=subject_kind or requirement[1],
            subject_id=subject_id or requirement[2],
            result=result,
        ),
        producer=producer,
        observed_at=observed_at,
        source_revision="source-sha-1",
        fresh_until=fresh_until or NOW + timedelta(minutes=5),
        adapter_version="test-adapter.v1",
        fact_revision=fact_revision,
    )


def _facts() -> tuple[DeliveryEvidenceFact, ...]:
    return tuple(_fact(item[0]) for item in REQUIREMENTS)


def test_ac_d1_hp_complete_five_slot_coverage_passes_in_declaration_order() -> None:
    verdict = evaluate_delivery(_company_work(), _facts(), evaluated_at=NOW)

    assert verdict.evaluator_version == DELIVERY_EVALUATOR_VERSION
    assert verdict.work_order_node_id == NODE_ID
    assert verdict.attempt_id == ATTEMPT_ID
    assert verdict.fact_revision == "snapshot-7"
    assert verdict.aggregate is EvidenceResult.PASSED
    assert [row.evidence_id for row in verdict.requirements] == [item[0] for item in REQUIREMENTS]
    assert {row.result for row in verdict.requirements} == {EvidenceResult.PASSED}


@pytest.mark.parametrize(
    "mutate",
    [
        lambda payload: payload.update({"aggregate": "failed"}),
    ],
)
def test_passed_verdict_public_contract_rejects_contradictions(mutate) -> None:
    payload = evaluate_delivery(
        _company_work(), _facts(), evaluated_at=NOW
    ).model_dump(mode="json")
    mutate(payload)

    with pytest.raises(ValidationError):
        DeliveryVerdict.model_validate(payload)


def test_ac_d1_err_projection_only_passed_values_are_not_runtime_evidence() -> None:
    verdict = evaluate_delivery(_company_work(), (), evaluated_at=NOW)

    assert verdict.aggregate is EvidenceResult.UNKNOWN
    assert all(row.result is EvidenceResult.UNKNOWN for row in verdict.requirements)
    assert "artifact-ready" in verdict.requirements[0].diagnostics[0]
    assert "missing runtime fact" in verdict.requirements[0].diagnostics[0]


@pytest.mark.parametrize(
    ("facts", "evidence_id", "diagnostic"),
    [
        (lambda: _facts()[1:], "artifact-ready", "missing runtime fact"),
        (
            lambda: (
                (
                    _fact(
                        "artifact-ready",
                        observed_at=NOW - timedelta(minutes=10),
                        fresh_until=NOW - timedelta(seconds=1),
                    ),
                )
                + _facts()[1:]
            ),
            "artifact-ready",
            "stale",
        ),
        (
            lambda: (_fact("artifact-ready", attempt_id="attempt-old"),) + _facts()[1:],
            "artifact-ready",
            "attempt-old",
        ),
    ],
)
def test_ac_d2_err_missing_stale_mismatched_and_conflicting_facts_are_unknown(
    facts: Callable[[], tuple[DeliveryEvidenceFact, ...]],
    evidence_id: str,
    diagnostic: str,
) -> None:
    verdict = evaluate_delivery(_company_work(), facts(), evaluated_at=NOW)
    row = next(item for item in verdict.requirements if item.evidence_id == evidence_id)

    assert row.result is EvidenceResult.UNKNOWN
    assert verdict.aggregate is EvidenceResult.UNKNOWN
    assert evidence_id in " ".join(row.diagnostics)
    assert diagnostic in " ".join(row.diagnostics)


def test_ac_d3_err_precedence_is_failed_then_blocked_then_unknown_with_all_rows() -> None:
    facts = (
        _fact("artifact-ready", result=EvidenceResult.FAILED),
        _fact("review-ready", result=EvidenceResult.BLOCKED),
    )

    verdict = evaluate_delivery(_company_work(), facts, evaluated_at=NOW)

    assert verdict.aggregate is EvidenceResult.FAILED
    assert [row.result for row in verdict.requirements] == [
        EvidenceResult.FAILED,
        EvidenceResult.BLOCKED,
        EvidenceResult.UNKNOWN,
        EvidenceResult.UNKNOWN,
        EvidenceResult.UNKNOWN,
    ]


def test_ac_d8_inv_observation_facts_are_excluded_and_irrelevant() -> None:
    passing = evaluate_delivery(_company_work(), _facts(), evaluated_at=NOW)
    observation = _fact(
        "artifact-ready",
        result=EvidenceResult.FAILED,
        producer="adapter:observation",
        subject_kind=EvidenceSubjectKind.OBSERVATION,
        subject_id="later-business-metric",
    )
    with_observation = evaluate_delivery(
        _company_work(), _facts() + (observation,), evaluated_at=NOW
    )

    assert passing == with_observation
    assert with_observation.aggregate is EvidenceResult.PASSED


def test_ac_d10_con_mixed_fact_revisions_fail_closed() -> None:
    facts = (_fact("artifact-ready", fact_revision="snapshot-6"),) + _facts()[1:]

    verdict = evaluate_delivery(_company_work(), facts, evaluated_at=NOW)

    assert verdict.aggregate is EvidenceResult.UNKNOWN
    assert verdict.fact_revision is None
    assert all(row.result is EvidenceResult.UNKNOWN for row in verdict.requirements)
    assert "snapshot-6" in " ".join(verdict.diagnostics)
    assert "snapshot-7" in " ".join(verdict.diagnostics)


def _producer_company_work(*, duplicate_approval_slot: bool = False) -> CompanyWorkRefs:
    evidence = [
        EvidenceRef(
            id="approval-ready",
            work_order_id=NODE_ID,
            attempt_id=ATTEMPT_ID,
            subject_kind=EvidenceSubjectKind.APPROVAL,
            subject_id="request-digest-1",
            result=EvidenceResult.UNKNOWN,
        ),
        EvidenceRef(
            id="effect-acknowledged",
            work_order_id=NODE_ID,
            attempt_id=ATTEMPT_ID,
            subject_kind=EvidenceSubjectKind.EFFECT,
            subject_id="effect-1",
            result=EvidenceResult.UNKNOWN,
        ),
        EvidenceRef(
            id="destination-acknowledged",
            work_order_id=NODE_ID,
            attempt_id=ATTEMPT_ID,
            subject_kind=EvidenceSubjectKind.ACKNOWLEDGMENT,
            subject_id="effect-1",
            result=EvidenceResult.UNKNOWN,
        ),
    ]
    if duplicate_approval_slot:
        evidence.append(evidence[0].model_copy(update={"id": "approval-ready-duplicate"}))
    return CompanyWorkRefs(
        work_order=WorkOrderRef(node_id=NODE_ID, attempt_id=ATTEMPT_ID),
        deliverables=(
            DeliverableRef(
                id="external-send",
                kind="arbitrary-external-effect",
                work_order_id=NODE_ID,
                attempt_id=ATTEMPT_ID,
                required_evidence_ids=tuple(item.id for item in evidence),
                effect_id="effect-1",
            ),
        ),
        effects=(
            EffectRef(
                id="effect-1",
                work_order_id=NODE_ID,
                attempt_id=ATTEMPT_ID,
                deliverable_id="external-send",
                effect_class="communication.external",
                destination="email:customer@example.com",
                idempotency_key="effect-key-1",
                approval_id="request-digest-1",
            ),
        ),
        evidence=tuple(evidence),
    )


def _producer_event(event_type: str, **changes: object) -> dict[str, object]:
    data: dict[str, object]
    if event_type == "approval_requested":
        data = {
            "request_digest": "request-digest-1",
            "request_id": "request-1",
            "principal_id": "principal-1",
            "work_order_id": NODE_ID,
            "attempt_id": ATTEMPT_ID,
            "effect_id": "effect-1",
            "effect_class": "communication.external",
            "destination": "email:customer@example.com",
            "action_digest": "action-digest-1",
            "expires_at": "2026-08-02T13:00:00Z",
        }
    elif event_type == "approval_decided":
        data = {
            "request_digest": "request-digest-1",
            "decision": "approved",
            "deciding_principal_id": "principal-1",
            "work_order_id": NODE_ID,
            "attempt_id": ATTEMPT_ID,
            "effect_id": "effect-1",
        }
    else:
        data = {
            "idempotency_key": "effect-key-1",
            "state": "acknowledged",
            "previous_state": "executing",
            "request_digest": "request-digest-1",
            "work_order_id": NODE_ID,
            "attempt_id": ATTEMPT_ID,
            "effect_id": "effect-1",
            "external_ref": "message-42",
            "reconciliation_ref": None,
        }
    data.update(changes)
    return {
        "ts": NOW.isoformat().replace("+00:00", "Z"),
        "type": event_type,
        "source": "approvals",
        "data": data,
    }


@pytest.mark.parametrize(
    ("event_type", "changes", "expected"),
    [
        ("approval_decided", {}, {"approval-ready": EvidenceResult.PASSED}),
        (
            "approval_decided",
            {"decision": "declined"},
            {"approval-ready": EvidenceResult.BLOCKED},
        ),
        (
            "effect_state_changed",
            {},
            {
                "effect-acknowledged": EvidenceResult.PASSED,
                "destination-acknowledged": EvidenceResult.PASSED,
            },
        ),
    ],
)
def test_ac_d2_hp_producer_events_normalize_only_their_declared_source_fact(
    event_type: str,
    changes: dict[str, object],
    expected: dict[str, EvidenceResult],
) -> None:
    facts = adapt_delivery_event(
        _producer_company_work(),
        _producer_event(event_type, **changes),
        fresh_until=NOW + timedelta(minutes=5),
        fact_revision="event-snapshot-1",
        approval_request_event=(
            _producer_event("approval_requested")
            if event_type == "effect_state_changed"
            else None
        ),
    )

    assert {fact.evidence.id: fact.evidence.result for fact in facts} == expected
    assert {fact.evidence.work_order_id for fact in facts} == {NODE_ID}
    assert {fact.evidence.attempt_id for fact in facts} == {ATTEMPT_ID}
    assert {fact.producer for fact in facts} == {f"event:approvals:{event_type}"}
    assert {fact.fact_revision for fact in facts} == {"event-snapshot-1"}


def test_ac_d2_err_effect_event_without_request_metadata_is_not_evidence() -> None:
    assert (
        adapt_delivery_event(
            _producer_company_work(),
            _producer_event("effect_state_changed"),
            fresh_until=NOW + timedelta(minutes=5),
            fact_revision="event-snapshot-1",
        )
        == ()
    )


@pytest.mark.parametrize(
    "event",
    [
        {"type": "approval_decided", "source": "approvals", "data": {}},
        _producer_event("approval_requested", destination="email:attacker@example.com"),
        {**_producer_event("approval_decided"), "source": "test"},
    ],
)
def test_ac_d2_err_malformed_unknown_and_mismatched_events_produce_no_fact(
    event: dict[str, object],
) -> None:
    assert (
        adapt_delivery_event(
            _producer_company_work(),
            event,
            fresh_until=NOW + timedelta(minutes=5),
            fact_revision="event-snapshot-1",
        )
        == ()
    )


def test_ac_d2_err_ambiguous_declared_slot_match_produces_no_fact() -> None:
    facts = adapt_delivery_event(
        _producer_company_work(duplicate_approval_slot=True),
        _producer_event("approval_decided"),
        fresh_until=NOW + timedelta(minutes=5),
        fact_revision="event-snapshot-1",
    )

    assert facts == ()


def test_ac_d3_err_conflicting_producer_events_never_resolve_by_last_writer() -> None:
    company_work = _producer_company_work()
    facts = tuple(
        fact
        for decision in ("approved", "declined")
        for fact in adapt_delivery_event(
            company_work,
            _producer_event("approval_decided", decision=decision),
            fresh_until=NOW + timedelta(minutes=5),
            fact_revision="event-snapshot-1",
        )
    )

    verdict = evaluate_delivery(company_work, facts, evaluated_at=NOW)

    approval = next(row for row in verdict.requirements if row.evidence_id == "approval-ready")
    assert approval.result is EvidenceResult.UNKNOWN
    assert verdict.aggregate is EvidenceResult.UNKNOWN
    assert "conflicting duplicate facts" in " ".join(approval.diagnostics)
