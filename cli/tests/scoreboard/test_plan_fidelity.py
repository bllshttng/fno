"""AC coverage for `fno scoreboard --plan-fidelity` (x-ed6b3294 / x-68d3).

Grades PLANNING quality by joining a planning thread's plan doc to its
delivery (PR diff + SUMMARY.md). Attributed to the planning session_id.

AC1  a planned row joined to a shipped node emits AC-coverage + scope-drift
     + data-model-surprise, attributed to the planning session_id.
AC2  a delivery touching schema/migration files absent from the ownership map
     scores non-zero data-model-surprise.
AC3  a planned row with no joinable delivery is `unjoined`, never scored 0%.
"""

from __future__ import annotations

import json
from datetime import datetime
from pathlib import Path

from fno.scoreboard.fold import build_plan_fidelity

NOW = datetime(2026, 7, 3, 20, 0, 0)

PLAN_DOC = """
## Acceptance Criteria
#### AC1-HP: ...
#### AC2-ERR: ...

## File Ownership Map
| File | Action | Owner |
|---|---|---|
| `cli/src/fno/scoreboard/fold.py` | modify | /blueprint |
"""


def _fidelity(rows, graph=None, *, plan_doc=PLAN_DOC, summary="", diff=None, deliveries=None):
    return build_plan_fidelity(
        rows,
        graph or [],
        since_days=28,
        now=NOW,
        read_plan_doc=lambda p: plan_doc,
        read_summary=lambda row: summary,
        read_diff=lambda pr: diff,
        deliveries=deliveries,
    )


# --- AC1 ---------------------------------------------------------------------
def test_joined_plan_emits_scores_attributed_to_planning_session():
    # Same plan dir, different WORKTREE prefixes: the join is prefix-independent
    # (keys on parent-dir + file), so a plan-thread and a build-thread in
    # separate worktrees still join.
    rows = [
        {"completed": "2026-07-03T10:00:00", "termination_reason": "NoWork",
         "phases_completed": ["think", "plan"], "plan_path": "/wt-a/feat-a/00-INDEX.md",
         "project": "fno", "session_id": "plan-sess", "cost_usd": 2.0},
        {"completed": "2026-07-03T11:00:00", "termination_reason": "DonePRGreen",
         "phases_completed": ["do", "ship"], "plan_path": "/wt-b/feat-a/00-INDEX.md",
         "project": "fno", "graph_node_id": "x-1", "pr_number": 42, "session_id": "build-sess", "cost_usd": 6.0},
    ]
    pf = _fidelity(rows, summary="AC1-HP verified. AC2-ERR verified.",
                   diff=["cli/src/fno/scoreboard/fold.py"])
    joined = [r for r in pf["results"] if r["status"] == "joined"]
    assert len(joined) == 1
    r = joined[0]
    assert r["session_id"] == "plan-sess"  # attributed to the PLANNING session
    assert r["pr_number"] == 42
    assert r["ac_coverage"] == {"verified": 2, "total": 2, "pct": 100}
    assert r["scope_drift"] == {"unplanned": [], "untouched": []}
    assert r["data_model_surprise"] == 0


# --- AC2 ---------------------------------------------------------------------
def test_schema_file_absent_from_map_scores_data_model_surprise():
    rows = [
        {"completed": "2026-07-03T10:00:00", "termination_reason": "NoWork",
         "phases_completed": ["think", "plan"], "plan_path": "/x/plan-a.md",
         "session_id": "plan-sess", "cost_usd": 2.0},
        {"completed": "2026-07-03T11:00:00", "termination_reason": "DonePRGreen",
         "phases_completed": ["do", "ship"], "plan_path": "/x/plan-a.md",
         "graph_node_id": "x-1", "pr_number": 42, "session_id": "build-sess", "cost_usd": 6.0},
    ]
    pf = _fidelity(rows, summary="",
                   diff=["cli/src/fno/scoreboard/fold.py", "db/migrations/0002_add_col.sql"])
    r = [x for x in pf["results"] if x["status"] == "joined"][0]
    assert r["data_model_surprise"] >= 1  # the unplanned .sql migration
    assert "db/migrations/0002_add_col.sql" in r["scope_drift"]["unplanned"]


# --- AC3 ---------------------------------------------------------------------
def test_unjoined_plan_never_scored_zero():
    rows = [
        {"completed": "2026-07-03T10:00:00", "termination_reason": "NoWork",
         "phases_completed": ["think", "plan"], "plan_path": "/x/orphan-plan.md",
         "session_id": "plan-sess", "cost_usd": 2.0},
    ]
    pf = _fidelity(rows)
    assert len(pf["results"]) == 1
    r = pf["results"][0]
    assert r["status"] == "unjoined"
    assert "ac_coverage" not in r  # no fabricated 0%
    assert "data_model_surprise" not in r


# --- review fixes (gemini PR#317) --------------------------------------------
def test_cross_project_same_basename_does_not_collide():
    """Two `00-INDEX.md` plans in different projects must NOT join to each other."""
    rows = [
        {"completed": "2026-07-03T10:00:00", "termination_reason": "NoWork",
         "phases_completed": ["think", "plan"], "plan_path": "/a/feat/00-INDEX.md",
         "project": "proj-a", "session_id": "plan-a", "cost_usd": 2.0},
        # A shipped row in a DIFFERENT project with the same filename+parent.
        {"completed": "2026-07-03T11:00:00", "termination_reason": "DonePRGreen",
         "phases_completed": ["do", "ship"], "plan_path": "/b/feat/00-INDEX.md",
         "project": "proj-b", "graph_node_id": "x-9", "pr_number": 99, "session_id": "build-b", "cost_usd": 6.0},
    ]
    pf = _fidelity(rows, summary="", diff=["x.py"])
    # The proj-a plan has no delivery IN proj-a -> unjoined, never mis-joined to proj-b.
    plan_a = [r for r in pf["results"] if r["session_id"] == "plan-a"][0]
    assert plan_a["status"] == "unjoined"


# --- AC3-HP: done_probes join declaration to evidence (x-e54c) ----------------
_PROBE_PLAN = (
    "---\ntitle: p\ndone_probes:\n"
    '  - "fno agents mail list --since 24h | grep -q groom"\n'
    '  - "test -n \\"$(fno backlog groom --status)\\""\n'
    "---\n\n# plan\n\n## Acceptance Criteria\n"
)

_PROBE_ROWS = [
    {"completed": "2026-07-03T10:00:00", "termination_reason": "NoWork",
     "phases_completed": ["think", "plan"], "plan_path": "/wt-a/feat/00-INDEX.md",
     "project": "fno", "session_id": "plan-sess", "cost_usd": 2.0},
    {"completed": "2026-07-03T12:00:00", "termination_reason": "DonePRGreen",
     "phases_completed": ["do", "ship"], "plan_path": "/wt-b/feat/00-INDEX.md",
     "project": "fno", "graph_node_id": "x-1", "pr_number": 7,
     "session_id": "build-sess", "cost_usd": 6.0},
]


def _probe_event(session_id, probes):
    return {"type": "loop_check", "data": {"session_id": session_id, "done_probes": probes}}


def test_probes_count_only_passes_not_declarations():
    events = [_probe_event("build-sess", {
        "fno agents mail list --since 24h | grep -q groom": "fail:1",
        'test -n "$(fno backlog groom --status)"': "pass",
    })]
    pf = build_plan_fidelity(
        _PROBE_ROWS, [], since_days=28, now=NOW,
        read_plan_doc=lambda p: _PROBE_PLAN,
        read_summary=lambda row: "",
        read_diff=lambda pr: [],
        loop_check_events=events,
    )
    joined = [r for r in pf["results"] if r["status"] == "joined"][0]
    assert joined["probes"] == {"declared": 2, "passed": 1}


def test_probe_evidence_reads_the_real_events_file_shape(tmp_path):
    """The production wiring: build_plan_fidelity is fed by read_jsonl_events.

    Every other probe test injects events directly, so a shape mismatch here
    (unwrapped `data`, wrong kind key) would leave them all green while the
    scoreboard silently reported 0 passed for every plan - indistinguishable
    from the real signal this feature exists to produce.
    """
    from fno.scoreboard.fold import read_jsonl_events

    events_file = tmp_path / "events.jsonl"
    events_file.write_text(
        json.dumps({
            "type": "loop_check",
            "data": {
                "session_id": "build-sess",
                "done_probes": {
                    "fno agents mail list --since 24h | grep -q groom": "pass",
                    'test -n "$(fno backlog groom --status)"': "pass",
                },
            },
        })
        + "\n",
        encoding="utf-8",
    )

    pf = build_plan_fidelity(
        _PROBE_ROWS, [], since_days=28, now=NOW,
        read_plan_doc=lambda p: _PROBE_PLAN,
        read_summary=lambda row: "",
        read_diff=lambda pr: [],
        loop_check_events=read_jsonl_events([events_file], {"loop_check"}),
    )
    joined = [r for r in pf["results"] if r["status"] == "joined"][0]
    assert joined["probes"] == {"declared": 2, "passed": 2}


# --- AC4-CORE: one join, two dispositions (x-cbab) ---------------------------
#
# The unmeasurable knob is the inversion, never a second join. 'unjoined' (the
# default) is byte-identical to today's telemetry; 'refuse' adds a gate key that
# marks unjoined planned rows as a would-be refusal. The carveout waiver that can
# overturn would_refuse is tested via fno.plan.fidelity.fidelity_refusal.

_ORPHAN_ROWS = [
    {"completed": "2026-07-03T10:00:00", "termination_reason": "NoWork",
     "phases_completed": ["think", "plan"], "plan_path": "/x/orphan.md",
     "session_id": "plan-sess", "cost_usd": 2.0},
]


def test_refuse_disposition_marks_unjoined_as_a_would_be_refusal():
    """The same join, but an unjoined planned row is a refusal rather than n/a."""
    pf = build_plan_fidelity(
        _ORPHAN_ROWS, [], since_days=28, now=NOW,
        read_plan_doc=lambda p: PLAN_DOC, read_summary=lambda r: "",
        read_diff=lambda pr: None, unmeasurable="refuse",
    )
    gate = pf["gate"]
    assert gate["would_refuse"] is True
    assert gate["unjoined_count"] == 1
    assert gate["planned"] == 1 and gate["delivered"] == 0
    # the join itself is unchanged: the unjoined row is still reported.
    assert pf["results"][0]["status"] == "unjoined"


def test_unknown_unmeasurable_raises_rather_than_silently_defaulting():
    import pytest

    with pytest.raises(ValueError, match="unmeasurable"):
        build_plan_fidelity(
            _ORPHAN_ROWS, [], since_days=28, now=NOW,
            read_plan_doc=lambda p: PLAN_DOC, read_summary=lambda r: "",
            read_diff=lambda pr: None, unmeasurable="bogus",
        )


def test_fidelity_refusal_refuses_an_uncovered_shortfall():
    """AC5 heart: a planned-minus-delivered row with no covering carveout refuses."""
    from fno.plan.fidelity import fidelity_refusal

    unjoined = [{"session_id": "s1", "plan_path": "/x/p.md"}]
    decision = fidelity_refusal(unjoined_rows=unjoined, carveouts=[])
    assert decision["refused"] is True
    assert decision["shortfall"] == 1
    assert decision["covered"] is False
    assert "not a carveout" in decision["reason"]


def test_fidelity_refusal_refuses_any_carveout_when_forbidden():
    """A carveout no longer covers anything when the plan said none may exist."""
    from fno.plan.fidelity import fidelity_refusal

    decision = fidelity_refusal(unjoined_rows=[], carveouts=[{"id": "c1"}], forbidden=True)
    assert decision["refused"] is True
    assert "plan forbids carve-outs" in decision["reason"]


# --- carveouts: forbidden (x-ee91) -------------------------------------------

_REPO = Path(__file__).resolve().parents[3]
_SPECIMEN_BODY = (
    "## What\nretire rows\n\n## Explicitly not in this PR\n"
    "- a - x-7649\n- b - x-63be\n- c - x-ea5b\n- d - x-65b5\n- e - x-d2ba\n"
)


def _forbidden_setup(monkeypatch, tmp_path, *, carveouts_line, body_reader):
    import fno.paths as paths
    import fno.plan.fidelity as fid
    import fno.scoreboard.fold as fold

    plan = tmp_path / "plan.md"
    plan.write_text(
        f"---\nnode: x-1\nstatus: ready\ncreated: 2026-09-12\n{carveouts_line}---\n\n# plan\n"
    )
    monkeypatch.setattr(fold, "load_ledger_rows", lambda *a, **k: [])
    monkeypatch.setattr(paths, "_slug_from_git_remote", lambda root=None: "footnote")
    monkeypatch.setattr(paths, "resolve_plugin_script", lambda rel: _REPO / rel)
    monkeypatch.setattr(
        fid, "_load_graph_nodes",
        # A relative path, as ledger rows carry it: the lookup keys on the node id.
        lambda: [{"id": "x-1", "plan_path": "internal/fno/plans/plan.md", "pr_number": 1599}],
    )
    monkeypatch.setattr(fid, "_read_pr_body", body_reader)
    return fid, plan


def test_compute_plan_fidelity_refuses_declared_exclusions_when_plan_forbids(monkeypatch, tmp_path):
    """AC4-HP: the real bash gate runs (the positive control that the parser
    fired) and its first stderr line is the reason, with no ledger rows."""
    fid, plan = _forbidden_setup(
        monkeypatch, tmp_path, carveouts_line="carveouts: forbidden\n",
        body_reader=lambda pr, root: _SPECIMEN_BODY,
    )
    decision = fid.compute_plan_fidelity(plan_path=str(plan))
    assert decision["refused"] is True
    assert "declares 5 exclusion item(s)" in decision["reason"], decision["reason"]
    assert decision["carveouts_policy"] == "forbidden"
    assert decision["carveouts_pr"] == 1599


def test_compute_plan_fidelity_refuses_when_the_body_parser_is_absent(monkeypatch, tmp_path):
    """A bare install ships no check-oos-tracked.sh: the gate refuses, never passes."""
    import fno.paths as paths

    fid, plan = _forbidden_setup(
        monkeypatch, tmp_path, carveouts_line="carveouts: forbidden\n",
        body_reader=lambda pr, root: _SPECIMEN_BODY,
    )
    monkeypatch.setattr(paths, "resolve_plugin_script", lambda rel: tmp_path / "absent" / rel)
    decision = fid.compute_plan_fidelity(plan_path=str(plan))
    assert decision["refused"] is True
    assert "carve-out check unreadable" in decision["reason"], decision["reason"]


def test_compute_plan_fidelity_treats_a_typoed_carveouts_value_as_forbidden(monkeypatch, tmp_path):
    fid, plan = _forbidden_setup(
        monkeypatch, tmp_path, carveouts_line="carveouts: forbiden\n",
        body_reader=lambda pr, root: _SPECIMEN_BODY,
    )
    assert fid.compute_plan_fidelity(plan_path=str(plan))["refused"] is True


def test_compute_plan_fidelity_refuses_on_unreadable_body_when_forbidden(monkeypatch, tmp_path):
    fid, plan = _forbidden_setup(
        monkeypatch, tmp_path, carveouts_line="carveouts: forbidden\n",
        body_reader=lambda pr, root: None,
    )
    decision = fid.compute_plan_fidelity(plan_path=str(plan))
    assert decision["refused"] is True
    assert "PR body unreadable" in decision["reason"]


def test_compute_plan_fidelity_keeps_backstop_delivery_in_scope(monkeypatch, tmp_path):
    import fno.paths as paths
    import fno.plan.fidelity as fid
    import fno.scoreboard.fold as fold

    plan = tmp_path / "plan.md"
    plan.write_text("# plan\n")
    rows = [
        {"completed": "2026-07-03T10:00:00", "termination_reason": "NoWork",
         "phases_completed": ["think", "plan"], "plan_path": str(plan),
         "project": "footnote", "session_id": "plan-sess"},
        {"completed": "2026-07-03T11:00:00", "termination_reason": "reconcile-backstop",
         "project": "fno", "graph_node_id": "x-1", "pr_number": 42,
         "session_id": "delivery-sess"},
    ]
    monkeypatch.setattr(paths, "_slug_from_git_remote", lambda root=None: "footnote")
    monkeypatch.setattr(fold, "load_ledger_rows", lambda *a, **k: rows)
    monkeypatch.setattr(
        fid,
        "_load_graph_nodes",
        lambda: [{"id": "x-1", "plan_path": str(plan), "project": "fno"}],
    )
    monkeypatch.setattr(fid, "_read_covering_carveouts", lambda *a, **k: [])
    monkeypatch.setattr(
        fold,
        "classify_deliveries",
        lambda *a, **k: {"by_node": {"x-1": {"delivered": True}}},
    )
    monkeypatch.setattr(fold, "_default_read_diff", lambda pr: [])
    monkeypatch.setattr(fold, "_default_read_summary", lambda row: "")
    monkeypatch.setattr(fold, "_default_read_plan_doc", lambda path: plan.read_text())

    decision = fid.compute_plan_fidelity(plan_path=str(plan))

    assert decision["refused"] is False
    assert decision["planned"] == 1
    assert decision["delivered"] == 1


def test_compute_plan_fidelity_scopes_ledger_rows_before_the_fold(monkeypatch, tmp_path):
    """x-8ad8: the fold shells a real `gh pr diff` per joined delivery row it is
    given, so folding the WHOLE global ledger for a single-plan query pays for a
    live network call per joined row across every plan ever shipped - the
    reproduced hang (exit 124, zero bytes, no matter the timeout). The fix scopes
    `rows` to this plan BEFORE calling build_plan_fidelity, not just its output
    after; assert the fold never sees an unrelated plan's row at all."""
    import fno.paths as paths
    import fno.plan.fidelity as fid
    import fno.scoreboard.fold as fold

    plan = tmp_path / "feat" / "00-INDEX.md"
    plan.parent.mkdir(parents=True)
    plan.write_text("# plan\n")

    seen_rows = []

    def spy_build_plan_fidelity(rows, graph_nodes, **kwargs):
        seen_rows.extend(rows)
        return {"state": "ok", "results": []}

    ledger_rows = [
        {"plan_path": str(plan), "project": "footnote", "session_id": "s-mine",
         "completed": "2026-08-01T00:00:00", "termination_reason": "none"},
    ] + [
        {"plan_path": f"/other/plan-{i}.md", "project": "footnote",
         "session_id": f"s-other-{i}", "completed": "2026-08-01T00:00:00",
         "termination_reason": "none"}
        for i in range(50)
    ]
    monkeypatch.setattr(fold, "build_plan_fidelity", spy_build_plan_fidelity)
    monkeypatch.setattr(fid, "_load_graph_nodes", lambda: [])
    monkeypatch.setattr(fid, "_read_covering_carveouts", lambda *a, **k: [])
    monkeypatch.setattr(paths, "_slug_from_git_remote", lambda root=None: "footnote")
    monkeypatch.setattr(fold, "load_ledger_rows", lambda *a, **k: ledger_rows)

    fid.compute_plan_fidelity(plan_path=str(plan))

    # Only this plan's own row reached the fold; the 50 unrelated rows never did.
    assert len(seen_rows) == 1
    assert seen_rows[0]["session_id"] == "s-mine"
