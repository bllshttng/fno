"""Tests for fno.agents.truth_status - the fno-truth worker status resolver.

Covers the state table (working/waiting/suspect/stalled/unknown), the node-id
parse, the loop_check tail scan, render strings, missing-file degradation, and
one real-claim-file integration path.
"""

from __future__ import annotations

import json

import pytest

from fno.agents import truth_status as ts

SID = "20260709T001358Z-cl21834-267287"
HOLDER = f"target-session:{SID}"


# --------------------------------------------------------------------------
# state table (claim_status monkeypatched -> tests the combine logic only)
# --------------------------------------------------------------------------
def _patch_claim(monkeypatch, status):
    monkeypatch.setattr(ts, "claim_status", lambda key, root=None: status)


def test_live_recent_fire_is_working(monkeypatch):  # AC1-HP
    _patch_claim(monkeypatch, {"state": "live", "holder": HOLDER})
    r = ts.resolve_truth_status("x-4a48", loop_check_ages={SID: 120})
    assert r["state"] == "working"
    assert r["last_loop_check_age_s"] == 120
    assert r["session_id"] == SID


def test_manifest_claim_holder_mismatch_is_not_trusted(tmp_path, monkeypatch):
    thread_id = "019f48e1-e641-7170-9ea9-921f07021967"
    holder = f"target-session:{thread_id}"
    _patch_claim(monkeypatch, {"state": "live", "holder": holder})
    state_dir = tmp_path / ".fno"
    state_dir.mkdir()
    (state_dir / "target-state.md").write_text(
        "---\n"
        f"session_id: {SID}\n"
        "---\n"
        'target_claim_key: "node:x-4a48"\n'
        'target_claim_holder: "target-session:another-thread"\n'
    )

    r = ts.resolve_truth_status(
        "x-4a48", manifest_cwd=str(tmp_path), loop_check_ages={SID: 120}
    )

    assert r["state"] == "waiting"
    assert r["session_id"] == thread_id


def test_live_stale_fire_is_waiting(monkeypatch):  # AC3-EDGE
    _patch_claim(monkeypatch, {"state": "live", "holder": HOLDER})
    r = ts.resolve_truth_status(
        "x-4a48", loop_check_ages={SID: 9999}, recency_window_s=1800
    )
    assert r["state"] == "waiting"


def test_suspect_claim(monkeypatch):  # AC3b-EDGE
    _patch_claim(monkeypatch, {"state": "suspect", "holder": HOLDER})
    r = ts.resolve_truth_status("x-4a48", loop_check_ages={SID: 10})
    assert r["state"] == "suspect"  # never working/waiting even with a fire


def test_stale_claim(monkeypatch):  # AC4-HP
    _patch_claim(monkeypatch, {"state": "stale", "holder": HOLDER})
    r = ts.resolve_truth_status("x-4a48", loop_check_ages={SID: 10})
    assert r["state"] == "stalled"


# --------------------------------------------------------------------------
# loop_check tail scan
# --------------------------------------------------------------------------
def _write_events(path, records):
    path.write_text("".join(json.dumps(r) + "\n" for r in records))


def test_build_index_newest_fire_wins(tmp_path):
    ev = tmp_path / "events.jsonl"
    _write_events(
        ev,
        [
            {"ts": "2026-07-09T01:00:00Z", "type": "loop_check", "data": {"session_id": SID}},
            {"ts": "2026-07-09T01:05:00Z", "type": "loop_check", "data": {"session_id": SID}},
            {"ts": "2026-07-09T01:02:00Z", "type": "other", "data": {"session_id": SID}},
        ],
    )
    now = _epoch("2026-07-09T01:07:00Z")
    idx = ts.build_loop_check_index(events_path=ev, now_s=now)
    assert idx[SID] == pytest.approx(120.0)  # newest fire, 01:05 -> 2m ago


def test_build_index_skips_malformed_line(tmp_path):  # Concurrency: partial append
    ev = tmp_path / "events.jsonl"
    ev.write_text(
        json.dumps({"ts": "2026-07-09T01:00:00Z", "type": "loop_check", "data": {"session_id": SID}})
        + "\n"
        + '{"ts":"2026-07-09T01:05:00Z","type":"loop_check","data":{"sess'  # truncated
    )
    now = _epoch("2026-07-09T01:01:00Z")
    idx = ts.build_loop_check_index(events_path=ev, now_s=now)
    assert idx[SID] == pytest.approx(60.0)  # only the whole line counted


def _epoch(iso):
    from datetime import datetime

    return datetime.fromisoformat(iso.replace("Z", "+00:00")).timestamp()


# --------------------------------------------------------------------------
# render strings
# --------------------------------------------------------------------------
@pytest.mark.parametrize(
    "state,age,expected",
    [
        ("working", 120, "Working (loop 2m ago)"),
        ("waiting", None, "Waiting (claim live)"),
        ("unknown", None, None),
    ],
)
def test_render_truth_status(state, age, expected):
    r = {"state": state, "last_loop_check_age_s": age}
    assert ts.render_truth_status(r) == expected


# --------------------------------------------------------------------------
# integration: a real claim file + real events tail -> working (AC1-HP e2e)
# --------------------------------------------------------------------------
def test_real_live_claim_end_to_end(tmp_path):
    from fno.claims.core import acquire_claim

    # Live claim: default pid = this test process (alive), future TTL.
    acquire_claim(
        "node:x-4a48",
        HOLDER,
        root=tmp_path,
        ttl_ms=600_000,
        reason="test",
    )
    ev = tmp_path / "events.jsonl"
    _write_events(
        ev, [{"ts": "2026-07-09T01:05:00Z", "type": "loop_check", "data": {"session_id": SID}}]
    )
    now = _epoch("2026-07-09T01:06:00Z")
    r = ts.resolve_truth_status(
        "x-4a48", claims_root=tmp_path, events_path=ev, now_s=now
    )
    assert r["state"] == "working"
    assert r["claim_state"] == "live"
    assert ts.render_truth_status(r) == "Working (loop 1m ago)"


# ---------------------------------------------------------------------------
# x-84b2: the canonical dispatch-name family resolves; legacy rows keep reading
# ---------------------------------------------------------------------------


def test_parse_worker_mission_maps_verb_codes_to_kinds():
    # t/f are target-class; bp/r/th are the plan-artifact branch.
    assert ts.parse_worker_mission("t-x-1-build") == ("x-1", "target")
    assert ts.parse_worker_mission("f-x-1-mend") == ("x-1", "target")
    assert ts.parse_worker_mission("bp-x-1-design") == ("x-1", "think")
    assert ts.parse_worker_mission("th-th-x-1-slug") == ("x-1", "think")
    assert ts.parse_worker_mission("ac-bp-x-1-design") == ("x-1", "think")
    assert ts.parse_worker_mission("rd-t-x-1-destub") == ("x-1", "target")
