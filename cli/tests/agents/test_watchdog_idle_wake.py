"""The synthetic idle fleet: idle glm/opus rows the wake lane must list, and
missing-identity rows the outage collector must attribute by model.

Registry shape mirrors the 2026-10-27 measurement: 38 of 45 rows carry no
route_provider_id/account_record_id, while 32 of 37 transcripts name the model
in the tail. The fallback keys both identity axes on that model - the cap
actor's own convention (provider_cap trap 2).
"""
from __future__ import annotations

from datetime import datetime, timezone
from pathlib import Path
from types import SimpleNamespace

from fno.agents.watchdog import (
    LEAVE,
    STALE,
    WAKE,
    Row,
    TailFacts,
    _route_fallback_identity,
    measure_provider_outages,
    verdicts,
)

NOW = datetime(2026, 10, 28, 7, 0, tzinfo=timezone.utc).timestamp()
PROMISE_TAIL = "<promise>MISSION COMPLETE: the PR is green and reviewed</promise>"
QUESTION_TAIL = "Should I also port the pane fallback while I am here?"

# The claude Fair-Use-Policy 429 body, offset-bearing reset so the stamp
# resolves without a timezone lookup (model-keyed accounts resolve none).
FUP = (
    "API Error: Request rejected (429) · [1313][Your account's current usage pattern "
    "does not comply with the Fair Usage Policy, and your request frequency has been "
    "limited. To restore access, please submit a request.]"
) + " Window resets at 2026-10-28T09:00:00+00:00."


def _facts(text: str, age_min: float = 5, role: str = "assistant") -> TailFacts:
    epoch = NOW - age_min * 60
    return TailFacts([(epoch, text)], epoch, text, role, text)


def _run(rows, transcripts):
    return verdicts(
        rows,
        transcript_for=lambda sid: transcripts.get(sid),
        claim_for=lambda node: {},
        node_state_for=lambda node: None,
        now_s=NOW,
    )


# ---------------------------------------------------------------------------
# Idle rows join the wake lane
# ---------------------------------------------------------------------------

def test_idle_row_with_stalled_tail_is_wakeable():
    rows = [Row("idle0001-0000", "glm-1", "idle", None, "/tmp/g1")]
    [v] = _run(rows, {"idle0001-0000": _facts("still working the tests", age_min=130)})
    assert v.verdict == WAKE
    assert v.basis.endswith("no 429 in tail")


def test_idle_row_with_promise_tail_is_not_woken():
    # A promise is a turn OUTCOME and never goes stale, so the row is done
    # whatever its age.
    rows = [Row("idle0002-0000", "opus-1", "idle", None, "/tmp/o1")]
    [v] = _run(rows, {"idle0002-0000": _facts(PROMISE_TAIL, age_min=130)})
    assert v.verdict == LEAVE
    assert "tail reads done" in v.basis


def test_idle_row_with_fresh_question_tail_is_not_woken():
    # A question still waiting on its human reads your-move while fresh, and
    # your-move is engaged, never woken. (Past the stalled threshold the same
    # tail reads stalled like any other lane member - the classifier's own
    # expiry rule, unchanged here.)
    rows = [Row("idle0003-0000", "glm-2", "idle", None, "/tmp/g2")]
    [v] = _run(rows, {"idle0003-0000": _facts(QUESTION_TAIL, age_min=5)})
    assert v.verdict == LEAVE
    assert "your-move" in v.basis


def test_idle_row_past_the_wake_ceiling_is_stale():
    # A week-idle row is abandoned work: the ceiling buckets it as needs-human,
    # never a blind wake.
    rows = [Row("idle0004-0000", "opus-2", "idle", None, "/tmp/o2")]
    [v] = _run(rows, {"idle0004-0000": _facts("parked mid turn", age_min=61 * 1440)})
    assert v.verdict == STALE
    assert "wake ceiling" in v.basis


def test_idle_row_without_transcript_is_not_ghost():
    # idle is not in _GHOST_STATES: a missing transcript on an idle row is a
    # plain leave, not a ghost report.
    rows = [Row("idle0005-0000", "glm-3", "idle", None, "/tmp/g3")]
    [v] = _run(rows, {})
    assert v.verdict == LEAVE
    assert "no transcript, state idle" in v.basis


# ---------------------------------------------------------------------------
# Route identity falls back to the model
# ---------------------------------------------------------------------------

def _entry(**overrides):
    fields = dict(
        harness="claude", harness_session_id="sess-1", cwd="/tmp/g1",
        model_name=None, route_provider_id=None, account_record_id=None,
    )
    fields.update(overrides)
    return SimpleNamespace(**fields)


def test_route_fallback_prefers_the_registry_model_name():
    entry = _entry(model_name="claude-opus-5-5")
    entries = [{"message": {"model": "glm-5.3-flash"}}]
    assert _route_fallback_identity(entry, entries) == (
        "claude-opus-5-5", "claude-opus-5-5",
    )


def test_route_fallback_reads_the_transcript_model_line():
    entries = [
        {"type": "assistant", "timestamp": "2026-10-28T06:00:00Z",
         "message": {"role": "assistant", "model": "glm-5.3-flash",
                     "content": [{"type": "text", "text": "working"}]}},
    ]
    assert _route_fallback_identity(_entry(), entries) == (
        "glm-5.3-flash", "glm-5.3-flash",
    )


def test_route_fallback_returns_none_without_any_model():
    assert _route_fallback_identity(_entry(), [{"message": {"role": "assistant"}}]) == (None, None)
    assert _route_fallback_identity(None, None) == (None, None)


def test_route_fallback_keeps_declared_fields():
    entry = _entry(route_provider_id="zai", account_record_id="acct-a",
                   model_name="glm-5.3-flash")
    assert _route_fallback_identity(entry, []) == ("zai", "acct-a")


def _measure(rows_by_id, entries_by_row, tmp_path):
    """Run the outage measure over one row with injected seams."""
    rows = [Row(row_id, name, "idle", None, str(tmp_path))
            for row_id, name in rows_by_id.items()]
    entries_provider = lambda: [  # noqa: E731 - test seam, mirrors load_registry
        _entry(harness_session_id=row_id, cwd=str(tmp_path))
        for row_id in rows_by_id
    ]
    report = measure_provider_outages(
        rows, now_s=NOW,
        entries_provider=entries_provider,
        transcript_path_for=lambda _identity: tmp_path / "session.jsonl",
        entries_for=lambda row_id: entries_by_row.get(row_id),
        journal=tmp_path / "journal.json",
        pane_snapshot_dir=tmp_path / "panes",
    )
    return report


def test_measure_admits_model_keyed_evidence(tmp_path):
    # The measured fleet shape: no declared identity, the tail names the model,
    # the transcript carries a 429. The row must produce evidence attributed by
    # its model, not an unknown_route_identity refusal.
    tail = [
        {"type": "assistant", "timestamp": "2026-10-28T06:59:30Z",
         "isApiErrorMessage": True, "apiErrorStatus": 429,
         "message": {"role": "assistant", "model": "glm-5.3-flash",
                     "content": [{"type": "text", "text": FUP}]}},
    ]
    report = _measure({"idle0006-0000": "glm-4"}, {"idle0006-0000": tail}, tmp_path)
    reasons = [item.get("reason") for item in report.get("refusals", [])]
    assert "unknown_route_identity" not in reasons
    assert report["refusals"] == []


def test_measure_still_refuses_rows_without_any_model(tmp_path):
    report = _measure({"idle0007-0000": "opus-3"}, {"idle0007-0000": []}, tmp_path)
    reasons = [item.get("reason") for item in report.get("refusals", [])]
    assert reasons == ["unknown_route_identity"]
