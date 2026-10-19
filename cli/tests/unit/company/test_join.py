from __future__ import annotations


from fno.company.contracts import EvidenceResult
from fno.company.join import JoinBranch, evaluate_join


def _branch(
    result: EvidenceResult, *, work_order_id: str = "wo-1", attempt_id: str = "att-1"
) -> JoinBranch:
    return JoinBranch(work_order_id=work_order_id, attempt_id=attempt_id, result=result)


def test_ac6_one_failed_branch_aggregates_failed_and_lists_all_rows() -> None:
    evaluation = evaluate_join(
        [
            _branch(EvidenceResult.PASSED, work_order_id="wo-1"),
            _branch(EvidenceResult.FAILED, work_order_id="wo-2"),
            _branch(EvidenceResult.PASSED, work_order_id="wo-3"),
        ]
    )
    assert evaluation.aggregate is EvidenceResult.FAILED
    assert len(evaluation.branches) == 3
    assert {row.work_order_id for row in evaluation.branches} == {"wo-1", "wo-2", "wo-3"}


def test_ac6_one_branch_with_no_evidence_aggregates_unknown() -> None:
    evaluation = evaluate_join(
        [
            _branch(EvidenceResult.PASSED, work_order_id="wo-1"),
            _branch(EvidenceResult.UNKNOWN, work_order_id="wo-2"),
            _branch(EvidenceResult.PASSED, work_order_id="wo-3"),
        ]
    )
    assert evaluation.aggregate is EvidenceResult.UNKNOWN


def test_all_passed_aggregates_passed() -> None:
    evaluation = evaluate_join(
        [
            _branch(EvidenceResult.PASSED, work_order_id="wo-1"),
            _branch(EvidenceResult.PASSED, work_order_id="wo-2"),
        ]
    )
    assert evaluation.aggregate is EvidenceResult.PASSED


def test_blocked_outranks_unknown() -> None:
    evaluation = evaluate_join(
        [
            _branch(EvidenceResult.BLOCKED, work_order_id="wo-1"),
            _branch(EvidenceResult.UNKNOWN, work_order_id="wo-2"),
        ]
    )
    assert evaluation.aggregate is EvidenceResult.BLOCKED


def test_empty_join_is_unknown_matching_delivery() -> None:
    # Mirrors delivery's "not results -> unknown": never claim passed on absent
    # evidence.
    evaluation = evaluate_join([])
    assert evaluation.aggregate is EvidenceResult.UNKNOWN
    assert evaluation.branches == ()
