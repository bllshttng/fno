"""Context-to-outcome trace contracts for x-2e3c Task 1.2."""

from __future__ import annotations
from tests.fixtures.graph_seed import seed_graph

import hashlib
import json
import os
import subprocess
import sys
from datetime import datetime
from pathlib import Path

import pytest

from fno.events.log import normalize_event, read_events
from fno.scoreboard import fold
from fno.scoreboard.fold import (
    build_context_outcome_trace,
    evaluate_context_comparison,
    read_jsonl_events_with_coverage,
)
from tests._delivery_reference import delivered_stub, reference_deliveries


@pytest.fixture(autouse=True)
def hermetic_deliveries(monkeypatch):
    # The fold tests here run the real graph reads in a subprocess CLI or
    # none at all; the classifier seam stays hermetic either way.
    monkeypatch.setattr(fold, "classify_deliveries", reference_deliveries)
    monkeypatch.setattr("fno.rust_binary.delivered_terminals", delivered_stub)


def _snapshot_event(session_id: str, context_bytes: int, ts: str) -> dict:
    source_hash = f"source-{context_bytes}"
    return {
        "ts": ts,
        "type": "context_snapshot",
        "source": "hook",
        "data": {
            "session_id": session_id,
            "harness": "codex",
            "entry_state": "startup",
            "context_bytes": context_bytes,
            "estimated_tokens": (context_bytes + 3) // 4,
            "context_hash": hashlib.sha256(source_hash.encode()).hexdigest(),
            "source_hashes": [source_hash],
            "source_manifest": [
                {
                    "source_id": "fixture",
                    "status": "observed",
                    "bytes": context_bytes,
                    "content_hash": source_hash,
                }
            ],
            "measurement_complete": True,
            "measurement_errors": [],
        },
    }


def test_event_normalizer_preserves_canonical_and_legacy_identity(
    tmp_path: Path,
) -> None:
    canonical = {
        "ts": "2026-07-03T10:00:00Z",
        "type": "loop_check",
        "source": "hook",
        "data": {
            "session_id": "session-a",
            "node_id": "x-1",
            "pr_number": 42,
            "ci": "SUCCESS",
        },
    }
    legacy = {
        "ts": "2026-07-03T10:01:00Z",
        "type": "context_census",
        "session_id": "session-a",
        "payload": {"graph_node_id": "x-1", "pr_number": "42"},
    }

    assert normalize_event(canonical) == {
        "type": "loop_check",
        "ts": "2026-07-03T10:00:00Z",
        "source": "hook",
        "session_id": "session-a",
        "holder_session_id": None,
        "node_id": "x-1",
        "pr_number": 42,
        "short_id": None,
        "head_sha": None,
        "data": canonical["data"],
        "raw": canonical,
    }
    assert normalize_event(legacy)["data"] == legacy["payload"]
    assert normalize_event(legacy)["node_id"] == "x-1"
    assert normalize_event(legacy)["pr_number"] == 42
    claim_event = {
        "type": "claim_acquired",
        "data": {
            "key": "node:x-1",
            "holder": "target-session:session-a",
        },
    }
    assert normalize_event(claim_event)["node_id"] == "x-1"
    assert normalize_event(claim_event)["session_id"] is None
    assert normalize_event(claim_event)["holder_session_id"] == "session-a"

    path = tmp_path / "events.jsonl"
    path.write_text(
        __import__("json").dumps(canonical)
        + "\n"
        + __import__("json").dumps({**canonical, "data": {"session_id": "other"}})
        + "\n",
        encoding="utf-8",
    )
    assert read_events(path, session_id="session-a") == [canonical]


def test_trace_joins_context_to_objective_delivery_and_outcomes() -> None:
    delivery = {
        "session_id": "build-sess",
        "sessions": ["harness-session", "build-sess"],
        "graph_node_id": "x-1",
        "plan_path": "/plans/context.md",
        "commit_sha": "abc123",
        "evidence_receipt": {"scope": "full", "sha": "abc123"},
        "pr_number": 42,
        "pr_url": "https://github.com/acme/fno/pull/42",
        "completed": "2026-07-03T11:00:00",
        "duration_minutes": 12.5,
        "cost_usd": 3.25,
    }
    node = {"id": "x-1", "title": "Observe context", "reverted_at": None}
    events = [
        {
            "type": "claim_acquired",
            "ts": "2026-07-03T09:59:00Z",
            "source": "target",
            "data": {
                "key": "node:x-1",
                "holder": "target-session:build-sess",
                "pid": 123,
                "host": "worker",
                "acquired_at": 1783072740000,
            },
        },
        {
            "type": "context_snapshot",
            "ts": "2026-07-03T10:00:00Z",
            "source": "hook",
            "data": {
                "session_id": "harness-session",
                "harness": "codex",
                "entry_state": "startup",
                "context_bytes": 32000,
                "estimated_tokens": 8000,
                "context_hash": "ctx-hash",
                "source_hashes": ["one", "two"],
                "source_manifest": [],
                "measurement_complete": True,
            },
        },
        {
            "type": "loop_check",
            "ts": "2026-07-03T10:45:00Z",
            "source": "hook",
            "data": {
                "session_id": "build-sess",
                "fingerprint": "abc123 | OPEN | SUCCESS | 2026-07-03",
                "ci": "SUCCESS",
                "pr_state": "OPEN",
                "reviewed": True,
            },
        },
        {
            "type": "review_attestation",
            "ts": "2026-07-03T10:46:00Z",
            "source": "target",
            "data": {
                "reviewer": "sigma",
                "head_sha": "abc123",
                "verdict": "pass",
            },
        },
        {
            "type": "session_satisfied",
            "ts": "2026-07-03T10:50:00Z",
            "source": "target",
            "data": {
                "session_id": "build-sess",
                "source": "pr_merge",
                "reason": "merged",
                "gate_state_hash": "g",
            },
        },
        {
            "type": "recovery_nudge",
            "ts": "2026-07-03T10:20:00Z",
            "source": "hook",
            "data": {"session_id": "build-sess", "reason": "postcompact"},
        },
    ]

    trace = build_context_outcome_trace(delivery, node, events)

    assert trace["objective"] == "Observe context"
    assert trace["plan_node"] == "x-1"
    assert trace["worker_session"] == "build-sess"
    assert trace["claim"] == {
        "observed": True,
        "events": [
            {
                "type": "claim_acquired",
                "at": "2026-07-03T09:59:00Z",
                "key": "node:x-1",
                "holder": "target-session:build-sess",
            }
        ],
    }
    assert trace["commit"] == "abc123"
    assert trace["evidence_receipt"] == {"scope": "full", "sha": "abc123"}
    assert trace["pr"] == {
        "number": 42,
        "url": "https://github.com/acme/fno/pull/42",
    }
    assert trace["context"] == {
        "harness": "codex",
        "entry_state": "startup",
        "bytes": 32000,
        "estimated_tokens": 8000,
        "context_hash": "ctx-hash",
        "source_hashes": ["one", "two"],
        "measurement_complete": True,
        "measurement_errors": [],
    }
    assert trace["outcomes"]["ci"]["state"] == "SUCCESS"
    assert trace["outcomes"]["review"]["reviewed"] is True
    assert trace["outcomes"]["review"]["attestations"] == 1
    assert trace["outcomes"]["merge"] == {
        "observed": True,
        "state": "merged",
        "merged": True,
        "at": "2026-07-03T10:50:00Z",
    }
    assert trace["outcomes"]["revert"] == {
        "observed": False,
        "reverted": None,
        "at": None,
    }
    assert trace["outcomes"]["recovery"][0]["type"] == "recovery_nudge"
    assert trace["outcomes"]["latency_minutes"] == 12.5
    assert trace["outcomes"]["spend_usd"] == 3.25
    assert trace["falsifiable"] is True
    assert trace["provenance"] == ["ledger", "graph", "events"]


def test_missing_context_and_downstream_observations_never_fabricate_green() -> None:
    trace = build_context_outcome_trace(
        {
            "session_id": "s",
            "graph_node_id": "x-1",
            "termination_reason": "DonePRGreen",
        },
        {"id": "x-1", "title": "Unobserved"},
        [],
    )

    assert trace["context"] is None
    assert trace["falsifiable"] is False
    assert "context_snapshot" in trace["missing"]
    assert "downstream_outcome" in trace["missing"]


def test_review_attestation_is_head_pinned_without_fixture_only_session_id() -> None:
    delivery = {
        "session_id": "run",
        "graph_node_id": "x-1",
        "commit_sha": "new-head",
    }
    events = [
        {
            "type": "review_attestation",
            "source": "target",
            "data": {
                "reviewer": "sigma",
                "head_sha": "old-head",
                "verdict": "pass",
            },
        },
        {
            "type": "review_attestation",
            "source": "target",
            "data": {
                "reviewer": "sigma",
                "head_sha": "new-head",
                "verdict": "pass",
            },
        },
    ]

    trace = build_context_outcome_trace(delivery, {"id": "x-1"}, events)

    assert trace["outcomes"]["review"]["attestations"] == 1
    assert trace["outcomes"]["review"]["reviewed"] is True


def test_comparison_requires_a_predeclared_complete_contract() -> None:
    trace = {
        "worker_session": "candidate",
        "completed_at": "2026-07-03T11:00:00",
        "context": {"bytes": 50, "context_hash": "candidate-hash"},
        "outcomes": {"latency_minutes": 8, "spend_usd": 2},
        "falsifiable": True,
    }

    assert evaluate_context_comparison([trace], None) == {
        "label": "rejected",
        "reason": "comparison_contract_missing",
    }
    rejected = evaluate_context_comparison(
        [trace],
        {
            "declared_at": "2026-07-02T00:00:00Z",
            "recorded_at": "2026-07-02T00:00:00Z",
            "cohorts": {"baseline": [], "candidate": ["candidate"]},
            "window": {
                "start": "2026-07-01T00:00:00Z",
                "end": "2026-07-05T00:00:00Z",
            },
            "exclusions": [],
            "budgets": {"max_candidate_spend_usd": 5},
        },
    )
    assert rejected["label"] == "rejected"
    assert rejected["reason"] == "cohort_members_missing"


def test_predeclared_falsifiable_comparison_can_label_improvement() -> None:
    traces = [
        {
            "worker_session": "baseline",
            "completed_at": "2026-07-02T11:00:00",
            "context": {"bytes": 100, "context_hash": "baseline-hash"},
            "outcomes": {"latency_minutes": 10, "spend_usd": 2},
            "falsifiable": True,
        },
        {
            "worker_session": "candidate",
            "completed_at": "2026-07-03T11:00:00",
            "context": {"bytes": 60, "context_hash": "candidate-hash"},
            "outcomes": {"latency_minutes": 8, "spend_usd": 2.5},
            "falsifiable": True,
        },
    ]
    contract = {
        "declared_at": "2026-06-30T00:00:00Z",
        "recorded_at": "2026-06-30T00:00:01Z",
        "claim": "context_pruning",
        "cohorts": {
            "baseline": ["baseline"],
            "candidate": ["candidate"],
        },
        "window": {
            "start": "2026-07-01T00:00:00Z",
            "end": "2026-07-05T00:00:00Z",
        },
        "exclusions": [],
        "budgets": {"max_candidate_spend_usd": 3},
    }

    result = evaluate_context_comparison(traces, contract)

    assert result["label"] == "improvement"
    assert result["claim"] == "context_pruning"
    assert result["context_bytes"]["baseline_median"] == 100
    assert result["context_bytes"]["candidate_median"] == 60
    assert result["context_bytes"]["reduction_pct"] == 40


def test_comparison_rejects_invalid_spend_observation() -> None:
    invalid_spend = float("inf")
    traces = [
        {
            "worker_session": "baseline",
            "completed_at": "2026-07-02T11:00:00",
            "context": {"bytes": 100},
            "outcomes": {"spend_usd": 1},
            "falsifiable": True,
        },
        {
            "worker_session": "candidate",
            "completed_at": "2026-07-03T11:00:00",
            "context": {"bytes": 50},
            "outcomes": {"spend_usd": invalid_spend},
            "falsifiable": True,
        },
    ]
    contract = {
        "declared_at": "2026-06-30T00:00:00Z",
        "recorded_at": "2026-06-30T00:00:01Z",
        "cohorts": {"baseline": ["baseline"], "candidate": ["candidate"]},
        "window": {
            "start": "2026-07-01T00:00:00Z",
            "end": "2026-07-05T00:00:00Z",
        },
        "exclusions": [],
        "budgets": {"max_candidate_spend_usd": 2},
    }

    assert evaluate_context_comparison(traces, contract)["reason"] == (
        "comparison_budget_measurement_missing"
    )


def test_comparison_rejects_missing_declared_member_instead_of_using_subset() -> None:
    traces = [
        {
            "worker_session": "baseline-present",
            "completed_at": "2026-07-02T11:00:00",
            "context": {"bytes": 100, "measurement_complete": True},
            "outcomes": {"spend_usd": 1},
            "falsifiable": True,
        },
        {
            "worker_session": "candidate",
            "completed_at": "2026-07-03T11:00:00",
            "context": {"bytes": 50, "measurement_complete": True},
            "outcomes": {"spend_usd": 1},
            "falsifiable": True,
        },
    ]
    contract = {
        "declared_at": "2026-06-30T00:00:00Z",
        "recorded_at": "2026-06-30T00:00:01Z",
        "claim": "context_pruning",
        "cohorts": {
            "baseline": ["baseline-present", "baseline-missing"],
            "candidate": ["candidate"],
        },
        "window": {
            "start": "2026-07-01T00:00:00Z",
            "end": "2026-07-05T00:00:00Z",
        },
        "exclusions": [],
        "budgets": {"max_candidate_spend_usd": 2},
    }

    assert evaluate_context_comparison(traces, contract)["reason"] == "cohort_members_missing"


def test_comparison_rejects_incomplete_context_measurement() -> None:
    traces = [
        {
            "worker_session": "baseline",
            "completed_at": "2026-07-02T11:00:00",
            "context": {"bytes": 100, "measurement_complete": True},
            "outcomes": {"spend_usd": 1},
            "falsifiable": True,
        },
        {
            "worker_session": "candidate",
            "completed_at": "2026-07-03T11:00:00",
            "context": {
                "bytes": 0,
                "measurement_complete": False,
                "measurement_errors": ["missing-source"],
            },
            "outcomes": {"spend_usd": 1},
            "falsifiable": True,
        },
    ]
    contract = {
        "declared_at": "2026-06-30T00:00:00Z",
        "recorded_at": "2026-06-30T00:00:01Z",
        "claim": "context_pruning",
        "cohorts": {"baseline": ["baseline"], "candidate": ["candidate"]},
        "window": {
            "start": "2026-07-01T00:00:00Z",
            "end": "2026-07-05T00:00:00Z",
        },
        "exclusions": [],
        "budgets": {"max_candidate_spend_usd": 2},
    }

    assert (
        evaluate_context_comparison(traces, contract)["reason"]
        == "context_measurement_incomplete"
    )


def test_comparison_rejects_incomplete_event_journal_coverage() -> None:
    contract = {
        "declared_at": "2026-06-30T00:00:00Z",
        "recorded_at": "2026-06-30T00:00:01Z",
        "cohorts": {"baseline": ["baseline"], "candidate": ["candidate"]},
        "window": {
            "start": "2026-07-01T00:00:00Z",
            "end": "2026-07-05T00:00:00Z",
        },
        "exclusions": [],
        "budgets": {"max_candidate_spend_usd": 2},
    }

    assert evaluate_context_comparison(
        [], contract, observation_complete=False
    ) == {
        "label": "rejected",
        "reason": "event_observation_incomplete",
    }


def test_risk_budget_can_prevent_an_improvement_label() -> None:
    traces = [
        {
            "worker_session": "baseline",
            "completed_at": "2026-07-02T11:00:00",
            "context": {"bytes": 100},
            "outcomes": {
                "latency_minutes": 10,
                "spend_usd": 2,
                "review": {"observed": True, "findings": 0},
                "revert": {"observed": True, "reverted": False},
            },
            "falsifiable": True,
        },
        {
            "worker_session": "candidate",
            "completed_at": "2026-07-03T11:00:00",
            "context": {"bytes": 60},
            "outcomes": {
                "latency_minutes": 8,
                "spend_usd": 2,
                "review": {"observed": True, "findings": 2},
                "revert": {"observed": True, "reverted": False},
            },
            "falsifiable": True,
        },
    ]
    result = evaluate_context_comparison(
        traces,
        {
            "declared_at": "2026-06-30T00:00:00Z",
            "recorded_at": "2026-06-30T00:00:01Z",
            "claim": "context_pruning",
            "cohorts": {"baseline": ["baseline"], "candidate": ["candidate"]},
            "window": {
                "start": "2026-07-01T00:00:00Z",
                "end": "2026-07-05T00:00:00Z",
            },
            "exclusions": [],
            "budgets": {"max_candidate_review_findings": 0},
        },
    )

    assert result["label"] == "no_improvement"
    assert result["budget_satisfied"] is False
    assert result["budget_observations"] == {
        "max_candidate_review_findings": 2.0
    }


def test_event_reader_deduplicates_journals_and_reports_corruption(tmp_path: Path) -> None:
    event = _snapshot_event("s", 1, "2026-07-03T10:00:00Z")
    first = tmp_path / "project.jsonl"
    second = tmp_path / "global.jsonl"
    encoded = __import__("json").dumps(event)
    first.write_text(encoded + "\n", encoding="utf-8")
    second.write_text(encoded + "\n{partial\n", encoding="utf-8")

    result = read_jsonl_events_with_coverage(
        [first, second], {"context_snapshot"}
    )

    assert result["events"] == [event]
    assert result["coverage"]["complete"] is False
    assert result["coverage"]["malformed_lines"] == 1


def test_reused_harness_session_cannot_leak_future_context_backward() -> None:
    events = [
        _snapshot_event("shared-thread", 10, "2026-07-03T10:00:00-07:00"),
        _snapshot_event("shared-thread", 20, "2026-07-03T11:00:00-07:00"),
    ]
    first = build_context_outcome_trace(
        {
            "session_id": "delivery-one",
            "sessions": ["delivery-one", "shared-thread"],
            "completed": "2026-07-03T10:30:00-07:00",
        },
        None,
        events,
    )
    second = build_context_outcome_trace(
        {
            "session_id": "delivery-two",
            "sessions": ["delivery-two", "shared-thread"],
            "completed": "2026-07-03T11:30:00-07:00",
        },
        None,
        events,
    )

    assert first["context"]["bytes"] == 10
    assert second["context"]["bytes"] == 20


def test_dead_merge_status_is_displayed_but_never_observed() -> None:
    dead_status = "queued"
    # No writer produces these values (the only writer stamps "merged" or
    # None), so a row carrying one is residue, not evidence. The raw string
    # still shows as the display state, but it observes nothing.
    trace = build_context_outcome_trace(
        {"session_id": "s", "commit_sha": "head"},
        {"id": "x-1", "merge_status": dead_status},
        [],
    )

    assert trace["outcomes"]["merge"] == {
        "observed": False,
        "state": dead_status,
        "merged": None,
        "at": None,
    }
    assert trace["falsifiable"] is False


def _wait_subprocess_keeper_exit(root: Path, timeout: float = 12.0) -> None:
    """A CLI subprocess spawns a detached keeper that self-exits after its
    idle window; the session reaper asserts nothing rooted under this tmp
    tree survives teardown, so the test that owns the keeper waits it out."""
    import time

    import psutil

    deadline = time.monotonic() + timeout
    while True:
        live = [
            proc
            for proc in psutil.process_iter(["cwd", "cmdline"], ad_value=None)
            if str(proc.info["cwd"] or "").startswith(str(root))
            and any("store-keeper" in part for part in (proc.info["cmdline"] or []))
        ]
        if not live:
            return
        if time.monotonic() >= deadline:
            raise AssertionError(
                f"keeper still rooted under {root} after {timeout}s: "
                f"{[p.pid for p in live]}"
            )
        time.sleep(0.2)


def test_scoreboard_cli_inventories_live_and_archived_delivery_roots(
    tmp_path: Path,
) -> None:
    archive_second = True
    state_dir = tmp_path / "state"
    state_dir.mkdir()
    roots = [tmp_path / "repo-a", tmp_path / "repo-b"]
    repo_b_canonical = tmp_path / "repo-b-canonical"
    for root in roots[:1] if archive_second else roots:
        (root / ".fno").mkdir(parents=True)
    plans = [tmp_path / "plans" / "a.md", tmp_path / "plans" / "b.md"]
    plans[0].parent.mkdir()
    for plan in plans:
        plan.write_text(
            "## Acceptance Criteria\n#### AC1-HP: observe context\n",
            encoding="utf-8",
        )
    completed = datetime.now().replace(microsecond=0).isoformat()
    rows = []
    for index, (root, plan) in enumerate(zip(roots, plans), start=1):
        project = f"fixture-{index}"
        delivery = {
            "completed": completed,
            "termination_reason": "DonePRGreen",
            "phases_completed": ["do", "ship"],
            "plan_path": str(plan),
            "project": project,
            "graph_node_id": f"x-{index}",
            "session_id": f"run-{index}",
            "root_path": str(root),
            "commit_sha": f"head-{index}",
            "duration_minutes": 5,
            "cost_usd": 1,
        }
        if archive_second and index == 2:
            delivery["graph_node_id"] = None
            delivery["canonical_root_path"] = str(repo_b_canonical)
        rows.extend(
            [
                {
                    "completed": completed,
                    "phases_completed": ["think", "plan"],
                    "plan_path": str(plan),
                    "project": project,
                    "session_id": f"plan-{index}",
                },
                delivery,
            ]
        )
    (state_dir / "ledger.json").write_text(json.dumps({"entries": rows}), encoding="utf-8")
    seed_graph(
        state_dir / "graph.json",
        json.dumps(
            {
                "entries": [
                    {
                        "id": "x-1",
                        "title": "First root",
                        "merge_status": "merged",
                        "completed_at": completed,
                    },
                    {
                        "id": "x-2",
                        "title": "Second root",
                        "merge_status": "merged",
                        "completed_at": completed,
                    },
                ]
            }
        ),
    )

    duplicate_snapshot = _snapshot_event("run-1", 110, "2026-07-03T11:00:00Z")
    duplicate_review = {
        "ts": "2026-07-03T11:01:00Z",
        "type": "review_attestation",
        "source": "target",
        "data": {
            "reviewer": "sigma",
            "head_sha": "head-1",
            "verdict": "pass",
        },
    }
    (state_dir / "events.jsonl").write_text(
        json.dumps(duplicate_snapshot) + "\n" + json.dumps(duplicate_review) + "\n",
        encoding="utf-8",
    )
    from fno.paths import project_log

    spaces_root = tmp_path / "spaces"  # pinned into the CLI subprocess below
    root_a_journal = project_log("events.jsonl", project_root=roots[0])
    root_a_journal.parent.mkdir(parents=True, exist_ok=True)
    root_a_journal.write_text(
        json.dumps(duplicate_snapshot)
        + "\n"
        + json.dumps(_snapshot_event("run-1", 90, "2026-07-03T10:00:00Z"))
        + "\n"
        + json.dumps(duplicate_review)
        + "\n",
        encoding="utf-8",
    )
    second_event_path = roots[1] / ".fno" / "events.jsonl"
    archived_first_path = None
    if archive_second:
        second_event_path = (
            repo_b_canonical / ".fno" / "salvage" / "20260725-feature-plan-only" / "events.jsonl"
        )
        second_event_path.parent.mkdir(parents=True)
        archived_first_path = roots[0] / ".fno" / "salvage" / "20260724-x-1" / "events.jsonl"
        archived_first_path.parent.mkdir(parents=True)
        archived_first_path.write_text(
            json.dumps(_snapshot_event("run-1", 80, "2026-07-03T09:00:00Z")) + "\n",
            encoding="utf-8",
        )
    second_event_path.write_text(
        json.dumps(_snapshot_event("run-2", 220, "2026-07-03T12:00:00Z")) + "\n",
        encoding="utf-8",
    )
    settings = tmp_path / "settings.yaml"
    settings.write_text(
        f"schema_version: 1\nconfig:\n  state_dir: {state_dir}/\n",
        encoding="utf-8",
    )
    executable = Path(sys.executable).parent / "fno-py"

    result = subprocess.run(
        [str(executable), "scoreboard", "--plan-fidelity", "--json", "--since", "2"],
        cwd=roots[0],
        env={
            **os.environ,
            "FNO_CONFIG": str(settings),
            "FNO_REPO_ROOT": str(roots[0]),
            "FNO_SPACES_DIR": str(spaces_root),
            "FNO_TEST_MODE": "1",
        },
        text=True,
        capture_output=True,
        check=False,
        timeout=30,
    )

    assert result.returncode == 0, result.stderr
    payload = json.loads(result.stdout)
    joined = {
        item["context_outcome_trace"]["worker_session"]: item["context_outcome_trace"]
        for item in payload["results"]
        if item["status"] == "joined"
    }
    assert joined["run-1"]["context"]["bytes"] == 110
    assert joined["run-1"]["outcomes"]["review"]["attestations"] == 1
    assert joined["run-2"]["context"]["bytes"] == 220
    observed_paths = {
        item["path"] for item in payload["event_coverage"]["paths"] if item["status"] == "ok"
    }
    expected_paths = {
        str((state_dir / "events.jsonl").resolve()),
        str(root_a_journal.resolve()),
        str(second_event_path.resolve()),
    }
    if archived_first_path is not None:
        expected_paths.add(str(archived_first_path.resolve()))
    assert observed_paths == expected_paths
    _wait_subprocess_keeper_exit(tmp_path)
