"""Pure-fold + isolation coverage for the observer harness (x-57a5).

The fold's own ``__main__`` self-check covers the load-bearing invariants; these
add pytest coverage the plan's Task 7 asks for: attribution/outcome join, the
<10 insufficient guard, coverage_pct honesty, structural checks, review
precision (pass/degraded/fail), the replay None-dims path (A1), postmortem
corrupt-line tolerance, and the isolation-violation detective scan.
"""

from __future__ import annotations

from datetime import datetime

import pytest

from fno.observer import fold, isolation

NOW = datetime(2026, 7, 4, 12, 0, 0)


def _blueprint_row(node_id, session_id, completed="2026-07-01T10:00:00", reason="DonePRGreen"):
    return {
        "completed": completed,
        "termination_reason": reason,
        "graph_node_id": node_id,
        "sessions": [session_id],
        "phases_completed": ["plan"],  # -> attributed to fno:blueprint
    }


def test_build_corpus_attribution_and_outcome_join():
    rows = [
        _blueprint_row("x-1", "s1"),
        {**_blueprint_row("x-2", "s2"), "phases_completed": ["do"]},  # not blueprint
    ]
    nodes = [{"id": "x-1", "reverted": False}, {"id": "x-2", "reverted": False}]
    corpus = fold.build_corpus(
        rows, nodes, [], skill="blueprint", since_days=28, now=NOW,
        deliveries={"x-1": {"delivered": True}, "x-2": {"delivered": False}},
    )
    assert corpus["total_rows"] == 2
    assert corpus["attributed"] == 1
    assert corpus["items"][0]["outcome"] == "merged_clean"
    assert corpus["items"][0]["session_id"] == "s1"


def test_replay_batch_skips_min_gate_and_reports_true_size():
    # require_min=False (a replay is a targeted before/after, not a >=10 trend):
    # a single-item batch reports corpus_size=1, never a padded 10.
    summary = fold.build_run_summary(
        run_id="obs-x", skill_id="fno:blueprint", skill_version="unknown",
        findings=[("surface_fit", "pass")], corpus_size=1, scored_count=1,
        skill_ref=None, require_min=False,
    )
    assert summary["state"] == "ok"
    assert summary["corpus_size"] == 1
    assert summary["coverage_pct"] == 100
    # the default (sweep) still gates
    gated = fold.build_run_summary(
        run_id="obs-x", skill_id="fno:blueprint", skill_version="unknown",
        findings=[("surface_fit", "pass")], corpus_size=1, scored_count=1,
    )
    assert gated["state"] == "insufficient"


def _strategy_plan(wave_mode: str, shared: bool) -> str:
    second_surface = "['b.py', 'c.py']" if shared else "['c.py']"
    return (
        "---\ntitle: t\n---\n\n## Execution Strategy\n\n```yaml\n"
        "execution_mode: parallel\n"
        f"waves:\n- wave: 1\n  mode: {wave_mode}\n  tasks: ['1', '2']\n"
        "tasks:\n- id: '1'\n  title: t\n  surface: ['a.py', 'b.py']\n"
        "  verify: uv run pytest -q\n  acceptance: ['AC1']\n"
        f"- id: '2'\n  title: t\n  surface: {second_surface}\n"
        "  verify: uv run pytest -q\n  acceptance: ['AC1']\n"
        "```\n"
    )


@pytest.mark.dev_build
def test_collision_free_rides_the_validator():
    # AC2-HP: shared surface inside one parallel wave fails; the same pair
    # split across sequential waves passes, matching validate_execution.
    assert fold.score_blueprint_item({}, plan_text=_strategy_plan("parallel", shared=True))["collision_free"] == "fail"
    assert fold.score_blueprint_item({}, plan_text=_strategy_plan("sequential", shared=True))["collision_free"] == "pass"
    # AC1-HP: a plan without the (retired) Failure Modes heading scores clean
    # on every structural dimension that still exists.
    scored = fold.score_blueprint_item({}, plan_text=_strategy_plan("sequential", shared=False))
    assert scored["collision_free"] == "pass"
    assert "structural_validity" not in scored
    # AC1-ERR: text that is not a plan (no frontmatter, like a PR diff) is a
    # coverage gap, never a fail.
    assert fold.score_blueprint_item({}, plan_text="# diff\nplain text\n")["collision_free"] is None


def test_collision_free_is_a_gap_not_a_fabricated_pass_without_waves():
    # AC1-ERR (regression): a plan whose Execution Strategy declares no
    # `waves:` block never reaches the validator's wave-scoped surface check,
    # even when two tasks claim the same file - the old flat-task scan this
    # replaced would have failed it, so reporting "pass" here would be a
    # fabricated verdict, not a measured one.
    plan_text = (
        "---\ntitle: t\n---\n\n## Execution Strategy\n\n```yaml\n"
        "execution_mode: parallel\n"
        "tasks:\n- id: '1'\n  title: t\n  surface: ['a.py', 'b.py']\n"
        "  verify: uv run pytest -q\n  acceptance: ['AC1']\n"
        "- id: '2'\n  title: t\n  surface: ['b.py', 'c.py']\n"
        "  verify: uv run pytest -q\n  acceptance: ['AC1']\n"
        "```\n"
    )
    assert fold.score_blueprint_item({}, plan_text=plan_text)["collision_free"] is None


# --------------------------------------------------------------------------- #
# target: PR-anchored corpus (x-6ff0)
# --------------------------------------------------------------------------- #

T_NOW = datetime(2026, 7, 18, 12, 0, 0)


def _pr(number, node_id=None, merged="2026-07-10T10:00:00Z", closed=None, repo="o/r", head=None):
    url = f"https://github.com/{repo}/pull/{number}"
    if head is None:
        head = f"feature/{node_id}" if node_id else "hotfix/manual"
    return {"number": number, "headRefName": head, "mergedAt": merged, "closedAt": closed,
            "url": url, "state": "MERGED" if merged else "CLOSED"}


def test_target_partition_and_denominator():
    """AC1-HP: items + unattributed strictly partition the PR list; denominator
    is the PR count, not the ledger row count."""
    nodes = [{"id": "x-a", "reverted": False, "pr_number": 1, "pr_url": "https://github.com/o/r/pull/1"}]
    prs = [_pr(1, "x-a"), _pr(2, None)]  # one attributed, one unattributable
    corpus = fold.build_target_corpus(prs, nodes, [], {}, since_days=28, now=T_NOW)
    cov = corpus["coverage"]
    assert cov["prs_total"] == 2
    assert cov["attributed"] == 1 and cov["unattributed_pr"] == 1
    assert cov["attributed"] + cov["unattributed_pr"] == cov["prs_total"]
    # AC1-EDGE: the unattributable PR is counted with an all-None vector, not dropped
    assert corpus["unattributed"][0]["pr_number"] == 2
    assert corpus["unattributed"][0]["graph_node_id"] is None


def test_target_resolves_via_graph_pr_number_when_branch_absent():
    """Squash-merge drops the branch (headRefName empty): the reverse graph
    pr_number/pr_url lookup recovers the node - branch name alone re-creates the
    52% loss (Domain Pitfall)."""
    nodes = [{"id": "x-sq", "reverted": False, "pr_number": 7, "pr_url": "https://github.com/o/r/pull/7"}]
    prs = [_pr(7, head="")]  # branch gone
    corpus = fold.build_target_corpus(prs, nodes, [], {}, since_days=28, now=T_NOW)
    assert corpus["coverage"]["attributed"] == 1
    assert corpus["items"][0]["graph_node_id"] == "x-sq"


def test_target_first_try_green_and_converged_from_structured_signals():
    nodes = [{"id": "x-a", "reverted": False, "pr_number": 1, "pr_url": "https://github.com/o/r/pull/1",
              "sessions": [{"session_id": "sa", "phase": "ship"}]}]
    prs = [_pr(1, "x-a")]
    events = {"sa": [
        {"type": "loop_check", "ts": "2026-07-10T08:00:00Z", "data": {"session_id": "sa", "ci": "SUCCESS", "intent": "promise"}},
        {"type": "termination", "ts": "2026-07-10T09:00:00Z", "data": {"session_id": "sa", "reason": "DonePRGreen"}},
    ]}
    item = fold.build_target_corpus(prs, nodes, [], events, since_days=28, now=T_NOW)["items"][0]
    scores = fold.score_target_item(item)
    assert scores["first_try_green"] == "pass"  # 0 red episodes
    assert scores["converged"] == "pass"        # ship terminal reason
    assert item["signals"]["promises"] == 1 and item["signals"]["loop_fires"] == 1


def test_target_no_pr_class_corrects_survivorship():
    """AC3-HP: a churny build attempt with no PR becomes its own labeled class,
    bucketed by stop-cause; a plan-only thread is excluded."""
    nodes = [{"id": "x-a", "reverted": False, "pr_number": 1, "pr_url": "https://github.com/o/r/pull/1"}]
    prs = [_pr(1, "x-a")]
    rows = [
        {"completed": "2026-07-14T10:00:00", "termination_reason": "NoProgress", "phases_completed": ["do"], "sessions": ["snope"]},
        {"completed": "2026-07-14T11:00:00", "termination_reason": "Aborted", "phases_completed": ["review"], "sessions": ["sabort"]},
        {"completed": "2026-07-14T12:00:00", "termination_reason": "DoneAdvisory", "phases_completed": ["think"], "sessions": ["splan"]},  # plan-only, excluded
    ]
    corpus = fold.build_target_corpus(prs, nodes, rows, {}, since_days=28, now=T_NOW)
    cov = corpus["coverage"]
    assert cov["no_pr_attempts"] == 2
    assert cov["no_pr_stop_cause"] == {"NoProgress": 1, "Aborted": 1}


def test_isolation_violation_detected(tmp_path):
    leaked = "20260704T999999Z-leak"
    real_ledger = tmp_path / "ledger.json"
    real_ledger.write_text('{"entries":[{"session_id":"' + leaked + '","cost_usd":1}]}\n')
    result = isolation.check_isolation({leaked}, {"ledger_json": real_ledger})
    assert result.verdict == "violated"
    assert result.violations[0].session_id == leaked
    # a clean scan (id absent) -> clean
    assert isolation.check_isolation({"not-present"}, {"ledger_json": real_ledger}).verdict == "clean"


def test_unreadable_graph_store_fails_isolation_closed(monkeypatch, tmp_path):
    from fno.graph import store
    monkeypatch.setattr(store, "read_graph_strict", lambda _path: (_ for _ in ()).throw(RuntimeError()))
    result = isolation.check_isolation({"eval-leak"}, {"graph_json": tmp_path / "graph.json"})
    assert result.verdict == "violated"


@pytest.fixture(autouse=True)
def hermetic_vocabulary(monkeypatch):
    """The fold's shipped-reason check reads the delivered vocabulary through
    a fail-closed binary door; the smoke shard deletes the binary, so the
    seam pins to the stub (the true set) like the scoreboard fixtures do."""
    from tests._delivery_reference import delivered_stub

    monkeypatch.setattr("fno.rust_binary.delivered_terminals", delivered_stub)
