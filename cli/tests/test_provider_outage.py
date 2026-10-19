from __future__ import annotations

import json
from datetime import datetime, timezone
from pathlib import Path

import pytest

from fno.agents.provider_outage import (
    CanaryProof,
    EvidenceIdentity,
    OutageEvidence,
    RouteCandidate,
    collect_transcript_evidence,
    fold_provider_outages,
    measure_and_persist,
    run_health_canary,
    select_healthy_destination,
)


NOW = datetime(2026, 8, 18, 18, 0, tzinfo=timezone.utc).timestamp()
FUP = (
    "API Error: Request rejected (429) · [1313][Your account's current usage pattern "
    "does not comply with the Fair Usage Policy, and your request frequency has been "
    "limited. To restore access, please submit a request.]"
)
# NOW + 2h, offset-bearing so the stamp resolves without a timezone.
FUP_LIVE_RESET = FUP + " Window resets at 2026-08-18T20:00:00+00:00."


def _record(row_id, at, content, *, status, kind="api_error", role="assistant",
            provider="zai", account="acct-a", source="transcript", pane_id=None,
            persisted=True, snapshot_at=None, reset_at=None):
    return OutageEvidence(
        source=source, observed_at=at, row_id=row_id, harness="claude",
        provider=provider, account=account, role=role, raw_status=status,
        raw_kind=kind, content=content, pane_id=pane_id, persisted=persisted,
        snapshot_at=snapshot_at, reset_at=reset_at,
    )


def _fold(records, prior=None):
    return fold_provider_outages(records, prior_state=prior, now_s=NOW)


def test_ac2_429_two_distinct_rows_open_one_manual_restore_breaker():
    report, _ = _fold([
        _record("row-1", NOW - 60, FUP, status=429),
        _record("row-2", NOW - 10, FUP, status=429),
    ])
    assert len(report["breakers"]) == 1
    breaker = report["breakers"][0]
    assert (breaker["provider"], breaker["account"]) == ("zai", "acct-a")
    assert breaker["row_ids"] == ["row-1", "row-2"]
    assert breaker["reset_at"] is None and breaker["manual_restoration"] is True
    assert "positive quorum=2" in breaker["basis"]


def test_ac1_det_groups_only_the_same_explicit_provider_and_account():
    report, _ = _fold([
        _record("row-1", NOW - 30, FUP, status=429),
        _record("row-2", NOW - 20, FUP, status=429, account="acct-b"),
        _record("row-3", NOW - 10, FUP, status=429, provider="vendor-c"),
    ])
    assert report["breakers"] == []
    assert report["counts"]["terminal"] == 3


def test_ac1_det_real_claude_fup_record_is_collected_from_raw_transcript(tmp_path):
    transcript = tmp_path / "session.jsonl"
    transcript.write_text(json.dumps({
        "type": "assistant",
        "timestamp": "2026-08-18T17:59:30Z",
        "isApiErrorMessage": True,
        "apiErrorStatus": 429,
        "message": {"role": "assistant", "content": [{"type": "text", "text": FUP}]},
    }) + "\n", encoding="utf-8")
    identity = EvidenceIdentity(
        row_id="row-1", harness="claude", provider="anthropic",
        account="acct-a", session_id="session-1", cwd=str(tmp_path),
    )

    records, refusals = collect_transcript_evidence(
        [identity], now_s=NOW,
        transcript_path_for=lambda _identity: transcript,
    )

    assert refusals == []
    assert len(records) == 1
    assert records[0].raw_status == 429
    assert records[0].raw_kind == "api_error"
    assert records[0].role == "assistant"
    assert "Fair Usage Policy" in records[0].content


def test_ac1_det_quoted_user_record_and_missing_route_identity_do_not_vote(tmp_path):
    transcript = tmp_path / "session.jsonl"
    transcript.write_text("\n".join([
        json.dumps({
            "type": "user", "timestamp": "2026-08-18T17:59:20Z",
            "isApiErrorMessage": True, "apiErrorStatus": 429,
            "message": {"role": "user", "content": FUP},
        }),
        json.dumps({
            "type": "assistant", "timestamp": "2026-08-18T17:59:30Z",
            "isApiErrorMessage": True, "apiErrorStatus": 429,
            "message": {"role": "assistant", "content": FUP},
        }),
    ]) + "\n", encoding="utf-8")
    identities = [
        EvidenceIdentity("quoted", "claude", "anthropic", "acct-a", "s1", str(tmp_path)),
        EvidenceIdentity("unknown", "claude", None, "acct-b", "s2", str(tmp_path)),
    ]

    records, refusals = collect_transcript_evidence(
        identities, now_s=NOW,
        transcript_path_for=lambda _identity: transcript,
    )

    assert [record.row_id for record in records] == ["quoted"]
    assert all(record.role == "assistant" for record in records)
    assert refusals == [{
        "row_id": "unknown", "reason": "unknown_route_identity",
        "harness": "claude", "count": 1,
    }]


def test_ac3_err_unparsed_shape_refuses_by_name_carrying_harness_and_path(tmp_path):
    """A harness whose transcript shape has no parser is a NAMED refusal with
    the harness and the resolved path - never an empty result that reads as a
    quiet, healthy fleet. codex, gemini, and opencode all land here."""
    transcript = tmp_path / "rollout.jsonl"
    transcript.write_text('{"not":"a claude record"}\n', encoding="utf-8")
    identities = [
        EvidenceIdentity("codex-row", "codex", "openai", "acct-a", "s1", str(tmp_path)),
        EvidenceIdentity("gemini-row", "gemini", "google", "acct-b", "s2", str(tmp_path)),
        EvidenceIdentity("opencode-row", "opencode", "opencode", "acct-c", "s3", str(tmp_path)),
    ]

    records, refusals = collect_transcript_evidence(
        identities, now_s=NOW,
        transcript_path_for=lambda _identity: transcript,
    )

    assert records == []
    by_row = {item["row_id"]: item for item in refusals}
    assert set(by_row) == {"codex-row", "gemini-row", "opencode-row"}
    for row_id, harness in (
        ("codex-row", "codex"),
        ("gemini-row", "gemini"),
        ("opencode-row", "opencode"),
    ):
        item = by_row[row_id]
        assert item["reason"] == "transcript_shape_unsupported"
        assert item["harness"] == harness
        assert item["path"] == str(transcript)
        assert item["count"] == 1


def test_ac1_det_stale_or_unpersisted_pane_evidence_is_refused():
    report, _ = _fold([
        _record("row-1", NOW - 10, FUP, status=429, source="pane",
                pane_id="pane-1", persisted=False, snapshot_at=NOW - 10),
        _record("row-2", NOW - 300, FUP, status=429, source="pane",
                pane_id="pane-2", persisted=True, snapshot_at=NOW - 300),
    ])
    assert report["breakers"] == [] and report["counts"]["refused"] == 2
    assert {item["reason"] for item in report["refusals"]} == {
        "pane_not_persisted", "pane_snapshot_stale"
    }


def test_ac4_err_unreadable_journal_is_unknown_and_does_not_act(tmp_path):
    journal = tmp_path / "recovery" / "provider-outages.json"
    journal.parent.mkdir()
    journal.write_text("{not-json", encoding="utf-8")
    report = measure_and_persist([
        _record("row-1", NOW - 20, FUP, status=429),
        _record("row-2", NOW - 10, FUP, status=429),
    ], now_s=NOW, path=journal)
    assert report["instrument"] == "unknown" and report["breakers"] == []
    assert report["counts"] == {"journal_unreadable": 1}
    assert report["refusals"][0]["reason"] == "journal_unreadable"
    assert journal.read_text(encoding="utf-8") == "{not-json"


def test_ac3_529_retries_until_three_consecutive_records_span_two_minutes():
    records = [
        _record("row-1", NOW - 100, "API Error: 529 Overloaded", status=529),
        _record("row-1", NOW - 50, "API Error: 529 Overloaded", status=529),
        _record("row-1", NOW - 1, "API Error: 529 Overloaded", status=529),
    ]
    report, _ = _fold(records)
    assert report["sessions"]["row-1"]["state"] == "retrying"
    records[0] = _record("row-1", NOW - 121, "API Error: 529 Overloaded", status=529)
    report, _ = _fold(records)
    assert report["sessions"]["row-1"]["state"] == "session_persistent"
    assert report["breakers"] == []


def test_ac3_529_two_persistent_sessions_open_breaker():
    records = []
    for row_id, offset in (("row-1", 0), ("row-2", 20)):
        records += [
            _record(row_id, NOW - 180 + offset, "API Error: 529 Overloaded", status=529),
            _record(row_id, NOW - 120 + offset, "API Error: 529 Overloaded", status=529),
            _record(row_id, NOW - 60 + offset, "API Error: 529 Overloaded", status=529),
        ]
    report, _ = _fold(records)
    assert len(report["breakers"]) == 1
    assert report["breakers"][0]["kind"] == "overloaded_529"
    assert "positive quorum=2" in report["breakers"][0]["basis"]


def test_ac5_hlth_route_selection_excludes_every_candidate_on_broken_provider():
    proof = CanaryProof(
        source="transcript", content="FNO_PROVIDER_HEALTH_OK", observed_at=NOW - 1,
        persisted=True, assistant_role=True,
    )
    candidates = [
        RouteCandidate(
            record_id="other-account", harness="opencode", provider="zai",
            account="acct-b", account_env={}, canary=proof,
        ),
        RouteCandidate(
            record_id="openai-account", harness="codex", provider="openai",
            account="acct-c", account_env={"OPENAI_API_KEY": "test"}, canary=proof,
        ),
    ]

    selected = select_healthy_destination(
        candidates, broken_provider="zai", now_s=NOW,
    )

    assert selected == candidates[1]
    assert selected.harness == "codex"
    assert selected.provider == "openai"
    assert selected.account == "acct-c"
    assert selected.record_id == "openai-account"


@pytest.mark.parametrize(
    "proof",
    [
        CanaryProof(
            source="transcript", content="FNO_PROVIDER_HEALTH_OK",
            observed_at=NOW - 121, persisted=True, assistant_role=True,
        ),
        CanaryProof(
            source="pane", content="prompt: FNO_PROVIDER_HEALTH_OK",
            observed_at=NOW - 1, persisted=True, assistant_role=False,
            pane_id="pane-1",
        ),
    ],
)
def test_ac5_hlth_absent_stale_or_prompt_echo_marker_is_not_health(proof):
    candidate = RouteCandidate(
        record_id="a", harness="codex", provider="openai", account="acct",
        account_env={}, canary=proof,
    )
    assert select_healthy_destination([candidate], broken_provider="zai", now_s=NOW) is None


def test_ac5_hlth_canary_uses_canonical_spawn_outside_node_then_stops(tmp_path: Path):
    node_cwd = tmp_path / "node"
    canary_cwd = tmp_path / "neutral"
    node_cwd.mkdir()
    canary_cwd.mkdir()
    candidate = RouteCandidate(
        record_id="a", harness="agy", provider="google", account="acct",
        account_env={"GOOGLE_API_KEY": "test"}, canary=None,
    )
    calls = []
    stopped = []
    snapshots = iter([{"node:existing"}, {"node:existing"}])

    def spawn(**kwargs):
        calls.append(kwargs)
        return {"pane_id": 7}

    proof = run_health_canary(
        candidate,
        canary_cwd=canary_cwd,
        node_cwd=node_cwd,
        now_s=NOW,
        spawn=spawn,
        collect_proof=lambda _spawned: CanaryProof(
            source="pane", content="FNO_PROVIDER_HEALTH_OK", observed_at=NOW - 1,
            persisted=True, assistant_role=False, pane_id="7", stopped=False,
        ),
        stop=lambda spawned: stopped.append(spawned) is None,
        claim_snapshot=lambda: next(snapshots),
    )

    assert proof is not None and proof.stopped is True
    assert calls[0]["provider"] == "agy"
    assert calls[0]["cwd"] == canary_cwd
    assert "node" not in calls[0] and "provenance" not in calls[0]
    assert "FNO_PROVIDER_HEALTH_OK" not in calls[0]["message"]
    assert stopped == [{"pane_id": 7}]


def test_ac5_hlth_canary_refuses_failed_stop_new_claim_or_node_worktree(tmp_path: Path):
    candidate = RouteCandidate(
        record_id="a", harness="codex", provider="openai", account="acct",
        account_env={}, canary=None,
    )
    fresh = CanaryProof(
        source="transcript", content="FNO_PROVIDER_HEALTH_OK", observed_at=NOW,
        persisted=True, assistant_role=True,
    )
    def spawn(**_kwargs):
        return object()
    assert run_health_canary(
        candidate, canary_cwd=tmp_path, node_cwd=tmp_path / "node", now_s=NOW,
        spawn=spawn, collect_proof=lambda _spawned: fresh, stop=lambda _spawned: False,
        claim_snapshot=lambda: set(),
    ) is None
    snapshots = iter([set(), {"node:new"}])
    assert run_health_canary(
        candidate, canary_cwd=tmp_path, node_cwd=tmp_path / "node", now_s=NOW,
        spawn=spawn, collect_proof=lambda _spawned: fresh, stop=lambda _spawned: True,
        claim_snapshot=lambda: next(snapshots),
    ) is None
    assert run_health_canary(
        candidate, canary_cwd=tmp_path / "node" / "inside", node_cwd=tmp_path / "node",
        now_s=NOW, spawn=spawn, collect_proof=lambda _spawned: fresh,
        stop=lambda _spawned: True, claim_snapshot=lambda: set(),
    ) is None


def _fup_transcript(tmp_path, name, content, *, age_s):
    stamp = datetime.fromtimestamp(NOW - age_s, tz=timezone.utc).strftime(
        "%Y-%m-%dT%H:%M:%SZ"
    )
    path = tmp_path / name
    path.write_text(json.dumps({
        "type": "assistant", "timestamp": stamp,
        "isApiErrorMessage": True, "apiErrorStatus": 429,
        "message": {"role": "assistant", "content": [{"type": "text", "text": content}]},
    }) + "\n", encoding="utf-8")
    return path


def test_stale_reset_stale_evidence_is_a_named_refusal_not_a_silent_zero(tmp_path):
    """A record past the freshness window must reach the caller as a
    counted ``evidence_stale`` refusal. The positive control proves the
    classifier live first: the same record inside the window is accepted."""
    fresh = _fup_transcript(tmp_path, "fresh.jsonl", FUP, age_s=60)
    stale = _fup_transcript(tmp_path, "stale.jsonl", FUP, age_s=1200)
    identity = EvidenceIdentity(
        row_id="row-1", harness="claude", provider="zai",
        account="acct-a", session_id="s1", cwd=str(tmp_path),
    )

    records, _ = collect_transcript_evidence(
        [identity], now_s=NOW, transcript_path_for=lambda _i: fresh,
    )
    assert len(records) == 1 and records[0].raw_status == 429

    records, refusals = collect_transcript_evidence(
        [identity], now_s=NOW, transcript_path_for=lambda _i: stale,
    )
    assert records == []
    assert len(refusals) == 1
    assert refusals[0]["reason"] == "evidence_stale"
    assert refusals[0]["row_id"] == "row-1"
    assert refusals[0]["age_s"] == 1200
    assert refusals[0]["count"] == 1


def test_stale_reset_breaker_opens_on_stale_live_evidence_past_the_cross_row_window():
    """The measured event, replayed: rows noticed the cap 466s apart, both
    lines older than the 600s freshness window, both bodies naming a reset
    still ~2h out. Quorum 2 must open the breaker even though the vote span
    exceeds fup_window_s - the shared live reset IS the event boundary."""
    records = [
        _record("row-1", NOW - 900, FUP_LIVE_RESET, status=429, reset_at=NOW + 2 * 3600),
        _record("row-2", NOW - 434, FUP_LIVE_RESET, status=429, reset_at=NOW + 2 * 3600),
    ]
    report, _ = _fold(records)
    assert len(report["breakers"]) == 1
    breaker = report["breakers"][0]
    assert breaker["row_ids"] == ["row-1", "row-2"]
    assert breaker["reset_at"] == NOW + 2 * 3600
    assert breaker["manual_restoration"] is False
    assert breaker["outage_epoch"] == NOW - 900

    # Without a resolvable reset the same stale lines refuse by name and
    # nothing opens: the epoch, not the age, is what admits them.
    bare = [
        _record("row-1", NOW - 900, FUP, status=429),
        _record("row-2", NOW - 434, FUP, status=429),
    ]
    report, _ = _fold(bare)
    assert report["breakers"] == []
    assert {item["reason"] for item in report["refusals"]} == {"evidence_stale"}
