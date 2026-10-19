"""Event-processing core coverage (idempotency, tolerant read, no-op, local-maxima)."""
from __future__ import annotations

from fno.skill_diff import engine


def _rc(run_id, skill_id="fno:blueprint", top="structural_validity", fails=2):
    return {
        "type": "skill_eval_run_complete",
        "data": {"run_id": run_id, "skill_id": skill_id,
                 "failure_ranking": [{"dimension": top, "fail_count": fails}]},
    }


def _finding(run_id, verdict="fail", dim="structural_validity", tool_fault=False, skill_id="fno:blueprint"):
    d = {"run_id": run_id, "skill_id": skill_id, "dimension": dim, "verdict": verdict}
    if tool_fault:
        d["tool_fault"] = True
    return {"type": "skill_eval_finding", "data": d}


def test_read_events_tolerant_skips_corrupt(tmp_path):  # AC3-ERR
    p = tmp_path / "events.jsonl"
    p.write_text(
        '{"ts":"2026-09-19T10:00:00Z","source":"test","type":"a","data":{}}\n'
        "NOT JSON\n\n"
        '{"ts":"2026-09-19T10:00:01Z","source":"test","type":"b","data":{}}\n'
    )
    out = engine.read_events_tolerant(p)
    assert [e["type"] for e in out] == ["a", "b"]


def test_unprocessed_and_idempotency():  # AC8-FR
    evs = [_rc("r1"), _rc("r2")]
    assert engine.unprocessed_runs(evs, "fno:blueprint") == ["r1", "r2"]
    evs.append({"type": "skill_diff_proposed", "data": {"run_id": "r1", "skill_id": "fno:blueprint"}})
    assert engine.unprocessed_runs(evs, "fno:blueprint") == ["r2"]
    evs.append({"type": "skill_diff_noop", "data": {"run_id": "r2", "skill_id": "fno:blueprint"}})
    assert engine.unprocessed_runs(evs, "fno:blueprint") == []


def test_unprocessed_excludes_skill_ref_runs():  # AC6-EDGE (x-ed13 Locked Decision 5)
    # A skill_ref-tagged run_complete is a replay/candidate eval (an
    # eval-after-merge after-run, or a manual `fno doctor observer replay`), never a
    # bare sweep - it must NOT be a proposer trigger, else the loop re-opens.
    evs = [
        _rc("sweep1"),
        {"type": "skill_eval_run_complete",
         "data": {"run_id": "after1", "skill_id": "fno:blueprint", "skill_ref": "deadbeef"}},
    ]
    assert engine.unprocessed_runs(evs, "fno:blueprint") == ["sweep1"]  # after1 excluded


def test_idempotency_key_is_run_id_and_skill():
    # A shared run_id across two skills: a terminal event for blueprint must not
    # mark the review run handled.
    evs = [
        _rc("shared", skill_id="fno:blueprint"),
        _rc("shared", skill_id="fno:review"),
        {"type": "skill_diff_proposed", "data": {"run_id": "shared", "skill_id": "fno:blueprint"}},
    ]
    assert engine.unprocessed_runs(evs, "fno:blueprint") == []
    assert engine.unprocessed_runs(evs, "fno:review") == ["shared"]


def test_findings_exclude_tool_fault():  # tool_fault rule
    evs = [_finding("r1"), _finding("r1", tool_fault=True), _finding("r1", verdict="pass")]
    got = engine.findings_for_run(evs, "r1")
    assert len(got) == 2 and all(f.get("tool_fault") is not True for f in got)


def test_local_maxima_does_not_trip_without_proposal():
    evs = [_rc("r1"), _rc("r2"), _rc("r3")]
    assert not engine.local_maxima_tripped(evs, "fno:blueprint", "r3")


def test_build_pr_body_records_both_hashes_when_drifted():  # AC9-FR
    body = engine.build_pr_body(
        run_id="r1", skill_id="fno:blueprint",
        hunks=[{"file": "skills/blueprint/SKILL.md", "cited_finding_ids": ["s1"], "rationale": "why"}],
        justification=None, bloat=None,
        version_observed="aaa", version_against="bbb", is_review_skill=False,
    )
    assert "`aaa`" in body and "`bbb`" in body
    assert "moved between eval and proposal" in body
    assert "s1" in body
