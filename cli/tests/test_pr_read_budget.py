"""The PR-candidate loop spends a bounded budget per read.

Split out of ``test_agents_watchdog.py``, which is over the 5,000-line file
budget and may only shrink. The question this module answers is narrow: how
much of the tick deadline may one PR read spend, and what does a failure
under a clamped budget mean?

Measured 2026-09-02: with the watchdog armed, six consecutive pr-watch ticks
died at their 480s deadline and emitted zero watchdog events. The PR
dimension projected 201.3s against a phase handed about 175s, and its budget
checkpoint sat at the TOP of the candidate loop, so a read starting with a
sliver of budget still ran to completion.
"""
from __future__ import annotations

import time
from datetime import datetime, timezone
from types import SimpleNamespace


from fno.agents import unfinished_work as uw

NOW_1840 = datetime(2026, 8, 16, 18, 40, 0, tzinfo=timezone.utc).timestamp()


def _pr_cand(number=7, node_id="x-pr"):
    return SimpleNamespace(
        pr_number=number,
        node_id=node_id,
        pr_url=f"https://github.com/owner/repo/pull/{number}",
        repo_dir=None,
        repo_slug="owner/repo",
    )


def _collect_prs(candidates, reader, deadline_monotonic=None):
    return uw.collect_observations(
        [],
        now_s=NOW_1840,
        graph_entries=[],
        registry_rows=({}, True),
        claim_status_fn=lambda node: {"state": "free"},
        truth_resolver=lambda handle: None,
        pr_candidates=candidates,
        pr_state_reader=reader,
        deadline_monotonic=deadline_monotonic,
    )


def test_a_budget_under_the_floor_stops_the_scan_without_reading():
    """A read that cannot finish is an unscanned candidate, not a failure."""
    read = []

    def reader(cand, *, reviewers):
        read.append(cand.pr_number)
        return SimpleNamespace(state="OPEN", opened_at=None)

    obs = _collect_prs(
        [_pr_cand()],
        reader,
        deadline_monotonic=time.monotonic() + (uw.PR_READ_FLOOR_S / 2),
    )

    assert read == [], "a read was started with less budget than one costs"
    assert obs.prs_unscanned is True
    assert obs.github_ok is True, "running short of time is not a GitHub outage"


def test_a_clamped_read_that_fails_reads_unscanned_not_broken(monkeypatch):
    """Through the DEFAULT reader, which is the only one the budget reaches.
    An injected reader keeps its own signature and never sees the budget, so
    a failure there must not be attributed to one."""

    def fake_read_pr_state(cand, *, reviewers, timeout_s=30.0):
        raise RuntimeError(f"gh pr view timed out after {timeout_s}s")

    import fno.pr_watch._discover as discover_mod

    monkeypatch.setattr(discover_mod, "read_pr_state", fake_read_pr_state)

    obs = _collect_prs(
        [_pr_cand()],
        None,
        # Above the floor, below the full budget: the read is worth starting
        # and its timeout is the deadline, not read_pr_state's own default.
        deadline_monotonic=time.monotonic() + (uw.PR_READ_BUDGET_S / 2),
    )

    assert obs.prs_unscanned is True
    assert obs.github_ok is True
    assert any("tick budget" in w for w in obs.warnings)


def test_a_failure_on_the_full_budget_still_reads_as_a_github_outage():
    """The mirror. A real outage must not hide behind the new branch."""

    def reader(cand, *, reviewers):
        raise RuntimeError("gh: could not resolve host")

    obs = _collect_prs([_pr_cand()], reader, deadline_monotonic=None)

    assert obs.github_ok is False
    assert obs.prs_unscanned is False
    assert any("pr read failed" in w for w in obs.warnings)
