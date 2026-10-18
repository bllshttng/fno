"""x-3a3f: the merge gate's missing-row recompute.

A session with no target manifest never runs the stop hook, so no
review_coverage row can exist - the gate was unsatisfiable for that shape.
The merge now fires the standalone producer ONCE and re-reads. These pin the
non-weakening edges: unreviewed still refuses (with or without a working
recompute), a reviewed PR passes only after exactly one recompute, and a
stale head still refuses after one. The hermetic default stub in
cli/tests/conftest.py keeps the verb seam inert; each test here re-pins it.
"""

import json

from fno.pr import _merge, _reviews
from fno.pr._proc import Result

from .test_pr_merge import FakeRun, _last_json, enabled  # noqa: F401


def _stub_recompute(
    monkeypatch,
    tmp_path,
    *,
    coverage,
    count,
    head,
    calls,
    why="",
    reviewed_sha=None,
    ancestor=True,
):
    """Replace the verb seam with one that appends a coverage event to the
    project log - the observable effect of the real binary's append. ``why``
    models the verb's exit-4 degraded-run reason (x-b56a)."""

    def fake(pr_number, cwd, head_arg):
        calls.append((pr_number, head_arg))
        events = tmp_path / ".fno" / "events.jsonl"
        events.parent.mkdir(exist_ok=True)
        data = {"pr": pr_number, "coverage": coverage, "head_sha": head}
        if coverage in ("covered", "uncovered"):
            data["reviewed_count"] = count
        if coverage == "covered":
            # The modern producer resolves a rung on every covered row: a row
            # without the verdict reads as written by a pre-posture binary,
            # and the merge gate's upgrade refusal would shadow the behavior
            # these tests pin (the recompute loop, not the posture).
            data["review_posture"] = {
                "posture": "self_review",
                "rank": 3,
                "source": "legacy",
                "posture_satisfied": True,
                "posture_gaps": [],
            }
            data["verdicts"] = [
                {
                    "name": "code-review",
                    "producer": "local_attestation",
                    "verdict": "reviewed",
                    "reviewed_sha": reviewed_sha or head,
                    # The label the real producer writes: `stale` when the verdict
                    # describes a sha the rebase rewrote out (the Rust predicate's
                    # answer, read from the stored row ever since the Python
                    # mirror was deleted).
                    "freshness": "fresh" if ancestor else "stale",
                }
            ]
        with open(events, "a", encoding="utf-8") as fh:
            fh.write(
                json.dumps({"ts": "2026-08-14T03:00:00Z", "type": "review_coverage", "data": data})
                + "\n"
            )
        return True, why

    monkeypatch.setattr(_reviews, "_fire_review_coverage_verb", fake)
    # Route the gate through the REAL read (the `enabled` fixture's covered
    # stub would bypass the recompute entirely): the only seam is the verb.
    monkeypatch.setattr(
        _merge,
        "_review_coverage_for_pr",
        lambda pr, repo, head=None: _reviews.review_coverage_for_gate(pr, repo, head),
    )


def _stub_owner_from_row(monkeypatch, tmp_path):
    """The refusal is decide's now: ask the real gate for its line against the
    seeded row and hand it back as the held receipt the verb renders. The
    conftest's own autouse stub owns the covered default."""
    from fno.pr import _coverage_gate

    state, refusal, _covered, note = _coverage_gate.coverage_verdict(
        42, str(tmp_path), recompute=True
    )
    if state == _coverage_gate.COVERED:
        return None
    line = _coverage_gate.refusal_line(refusal, note)
    detail = f"unreviewed merge refused: {line}" if line else ("unreviewed merge refused")
    monkeypatch.setattr(
        _merge,
        "_authorized_merge",
        lambda pr, repo, **kw: {"outcome": "held", "detail": detail},
    )
    return detail


def test_recompute_unreviewed_still_refuses(enabled, monkeypatch, capsys, tmp_path):  # noqa: F811
    """Plan test 5: an unreviewed PR with a WORKING recompute recomputes to
    uncovered and the merge still exits 2, with the receipt naming the
    recompute."""
    calls: list = []
    _stub_recompute(monkeypatch, tmp_path, coverage="uncovered", count=0, head="abc", calls=calls)
    # The gate now refuses a head it could not fetch, so pin the head the
    # stubbed rows describe (no real gh call from a hermetic test).
    monkeypatch.setattr(_merge, "_pr_head_oid", lambda pr, repo: "abc")
    _stub_owner_from_row(monkeypatch, tmp_path)
    assert _merge.run_merge(["42"], cwd=str(tmp_path)) == 2
    obj = _last_json(capsys)
    assert obj["outcome"] == "held"
    assert "uncovered" in obj["reason"], obj["reason"]
    assert "recomputed" in obj["reason"], obj["reason"]
    assert len(calls) == 1, "exactly one recompute"


def test_recompute_unavailable_fails_closed(enabled, monkeypatch, capsys, tmp_path):  # noqa: F811
    """Plan test 6: fno-agents unresolvable -> the refusal keeps today's exit 2
    and names the recompute's absence, not a bare count."""
    monkeypatch.setattr(
        _reviews, "_fire_review_coverage_verb", lambda *a, **k: (False, "fno-agents not found")
    )
    monkeypatch.setattr(
        _merge,
        "_review_coverage_for_pr",
        lambda pr, repo, head=None: _reviews.review_coverage_for_gate(pr, repo, head),
    )
    monkeypatch.setattr(_merge, "_pr_head_oid", lambda pr, repo: "abc")
    _stub_owner_from_row(monkeypatch, tmp_path)
    assert _merge.run_merge(["42"], cwd=str(tmp_path)) == 2
    reason = _last_json(capsys)["reason"]
    # No row AND no instrument: the answer is unmeasurable, the remedy named,
    # never a Python-side freshness (the deleted mirror's old job).
    assert "unmeasurable" in reason, reason
    assert "fno doctor update" in reason, reason
    assert "recompute unavailable: fno-agents not found" in reason, reason


def test_recompute_reviewed_pr_passes_after_exactly_one(enabled, monkeypatch, capsys, tmp_path):  # noqa: F811
    """Plan test 7: a reviewed PR with no prior event clears the coverage guard
    after ONE recompute - never a loop."""
    (tmp_path / ".fno").mkdir()
    fake = FakeRun(gh_merge=Result(0, "Merged pull request", ""), toplevel=str(tmp_path))
    monkeypatch.setattr(_merge, "run", fake)
    calls: list = []
    _stub_recompute(monkeypatch, tmp_path, coverage="covered", count=1, head="abc", calls=calls)
    monkeypatch.setattr(_merge, "_pr_head_oid", lambda pr, repo: "abc")
    assert _merge.run_merge(["42"], cwd=str(tmp_path)) == 0
    assert _last_json(capsys)["outcome"] == "merged"
    assert len(calls) == 1, f"the recompute must fire exactly once, fired {len(calls)}"


# ---- x-b56a: a degraded recompute names its reason ----


def test_recompute_quota_exhaustion_names_the_reason(monkeypatch, tmp_path):
    """A recompute that ran but degraded to unknown because the GraphQL quota
    was exhausted must say so in the note. A bare "recomputed" beside an
    unknown row reads as "re-checked, still unreviewed" - the exact confusion
    that sent operators to re-review PRs whose only problem was the quota
    window. Distinct from the unavailable case: the verb RAN (exit 4)."""
    calls: list = []
    _stub_recompute(
        monkeypatch,
        tmp_path,
        coverage="unknown",
        count=0,
        head="abc",
        calls=calls,
        why=(
            "GraphQL quota exhausted (0 remaining, resets in ~14m). "
            "`gh pr view` / `gh pr checks` cannot succeed until the reset."
        ),
    )
    data, note = _reviews.review_coverage_for_gate(42, str(tmp_path), "abc")
    assert data is not None and data.get("coverage") == "unknown"
    assert "degraded to unknown" in note, note
    assert "quota exhausted" in note, note
    assert len(calls) == 1


def test_exit4_outage_falls_back_to_the_exit_code_note():
    """Exit 4 with no stated reason (an outage with a healthy quota, or a
    probe that could not answer) must still yield a non-empty `why`: exit 4
    itself is the degradation signal, and an empty `why` would stamp a bare
    "recomputed" beside an unknown row - reading as "genuinely unreviewed"
    when the read in fact failed."""
    outage = '{"coverage":"unknown","graphql_remaining":4890,"graphql_exhausted":false}'
    for stdout in (outage, "not json", "", None):
        why = _reviews._exit4_reason_or_unstated(stdout)
        assert why == "gh read failed (exit 4)", (stdout, why)
    stated = '{"graphql_exhausted":true,"reason":"GraphQL quota exhausted."}'
    assert _reviews._exit4_reason_or_unstated(stated) == "GraphQL quota exhausted."
