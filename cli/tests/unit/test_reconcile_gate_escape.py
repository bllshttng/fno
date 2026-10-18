"""Tier-1 gate_escape auto-emit on reconcile oob-merge close (x-f894).

Exercises the #222 boundary (the load-bearing correctness surface) and the
fail-open + emit-failure-visible contract, driving the pure helper with an
injected reviews_fetcher so no gh/config is needed.
"""

from __future__ import annotations

import json
from pathlib import Path

from fno.graph._reconcile import MergeDriftRecord, emit_gate_escape_for_record


def _epath(tmp_path: Path) -> Path:
    """Explicit events log so tests never touch the real canonical root."""
    return tmp_path / ".fno" / "events.jsonl"


def _record(cwd: Path, *, pr: int = 218, node: str = "x-cccc") -> MergeDriftRecord:
    return MergeDriftRecord(
        node_id=node,
        plan_path=None,
        pr_number=pr,
        pr_url="https://github.com/owner/repo/pull/%d" % pr,
        pr_state="MERGED",
        merged_at="2026-07-07T00:00:00Z",
        error=None,
        session_id="sess-1",
        cwd=str(cwd),
    )


def _events(cwd: Path) -> list[dict]:
    from tests._event_rows import event_rows

    return event_rows(cwd / ".fno" / "events.jsonl")


def _gate_escapes(cwd: Path) -> list[dict]:
    return [e for e in _events(cwd) if e.get("type") == "gate_escape"]


def test_ac1_hp_emits_on_required_bot_never_reviewed(tmp_path):
    """AC1-HP: required bot never reviewed an oob-merged PR -> one dead-bot."""
    rec = _record(tmp_path)
    emit_gate_escape_for_record(
        rec,
        required_bots=["codex"],
        reviews_fetcher=lambda *a, **k: set(),
        events_path=_epath(tmp_path),
    )
    escapes = _gate_escapes(tmp_path)
    assert len(escapes) == 1
    data = escapes[0]["data"]
    assert data["reason"] == "dead-bot"
    assert data["pr"] == 218
    assert data["graph_node_id"] == "x-cccc"
    assert "codex" in data["detail"]


def test_short_name_bot_matches_gh_bot_suffix_login(tmp_path):
    """A configured short name (`gemini`) must count a review posted under gh's
    `[bot]`-suffixed login (`gemini-code-assist[bot]`), same as the ship gate.
    Exact equality would falsely flag a bot that DID review as dead-bot."""
    rec = _record(tmp_path)
    emit_gate_escape_for_record(
        rec,
        required_bots=["gemini"],
        reviews_fetcher=lambda *a, **k: {"gemini-code-assist[bot]"},
        events_path=_epath(tmp_path),
    )
    assert _gate_escapes(tmp_path) == []


def test_ac4_inv_no_double_count_same_pr_reason(tmp_path):
    """AC4-INV: two closes racing the same events.jsonl count the escape once."""
    rec = _record(tmp_path)
    for _ in range(2):
        emit_gate_escape_for_record(
            rec,
            required_bots=["codex"],
            reviews_fetcher=lambda *a, **k: set(),
            events_path=_epath(tmp_path),
        )
    assert len(_gate_escapes(tmp_path)) == 1


def test_ac5_err_fail_open_and_ac7_failure_logged(tmp_path, monkeypatch):
    """AC5-ERR: a failed emit never raises. AC7-FR: it is logged durably."""
    import fno.events.store_client as store_client_mod

    def _boom(*a, **k):
        raise OSError("disk full")

    monkeypatch.setattr(store_client_mod, "emit_envelope", _boom)
    rec = _record(tmp_path)
    # Must NOT raise (fail open).
    out = emit_gate_escape_for_record(
        rec,
        required_bots=["codex"],
        reviews_fetcher=lambda *a, **k: set(),
        events_path=_epath(tmp_path),
    )
    assert out is None
    assert _gate_escapes(tmp_path) == []  # nothing landed
    # ...but the failure is visible in the durable counter (AC7).
    fail_log = tmp_path / ".fno" / "gate_escape_emit_failures.jsonl"
    assert fail_log.exists()
    lines = [json.loads(x) for x in fail_log.read_text().splitlines() if x.strip()]
    assert len(lines) == 1
    assert lines[0]["reason"] == "dead-bot"


# ---- zero-coverage escape (x-0eaf) ----
#
# These tests pass events_path EXPLICITLY (the same path loop-check writes to).
#
# The x-0eaf finding-3 caveat that used to sit here - loop-check writes
# review_coverage to the SESSION worktree's .fno/events.jsonl while reconcile
# reads the CANONICAL one, so the escape never fires in the default worktree
# workflow - was real, and it was the same divergence that made `fno do pr merge`
# refuse a PR that had been reviewed. x-f43c closed it: loop-check emits
# review_coverage to BOTH logs (repo-scoped), and every reader consults the
# global one alongside its project log. `test_coverage_read_spans_the_global_log`
# below covers the default worktree workflow the caveat said was uncovered.


def _write_event(cwd: Path, ev: dict) -> None:
    p = cwd / ".fno" / "events.jsonl"
    p.parent.mkdir(parents=True, exist_ok=True)
    with p.open("a", encoding="utf-8") as fh:
        fh.write(json.dumps(ev) + "\n")


def test_zero_coverage_unknown_escapes(tmp_path):
    """Unknown coverage (read failed at gate time) escapes - fail-closed for audit."""
    rec = _record(tmp_path)
    _write_event(
        tmp_path,
        {"type": "review_coverage", "data": {"pr": 218, "coverage": "unknown", "head_sha": "abc"}},
    )
    emit_gate_escape_for_record(
        rec,
        required_bots=[],
        reviews_fetcher=lambda *a, **k: set(),
        events_path=_epath(tmp_path),
    )
    escapes = _gate_escapes(tmp_path)
    assert len(escapes) == 1
    assert escapes[0]["data"]["reason"] == "zero-coverage"
