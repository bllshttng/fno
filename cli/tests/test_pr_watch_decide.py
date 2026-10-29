"""Tests for the PR-state watcher core: decide() matrix and discovery + read error path.

Task 1.1: pure core + read layer. No I/O inside decide(); runner injectable for
read_pr_state tests.

Run: uv run --project cli pytest cli/tests/test_pr_watch_decide.py -q
"""
from __future__ import annotations

import json
import subprocess
from typing import Optional

import pytest

# These imports will fail until we implement the module -- that is the RED phase.
from fno.pr_watch import decide
from fno.pr_watch._discover import (
    PrCandidate,
    PrObservation,
    discover_open_prs,
    read_pr_state,
)
from fno.graph._reconcile import ReconcileError


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------

def _obs(
    *,
    pr_number: int = 42,
    state: str = "OPEN",
    merged: bool = False,  # kept for call-site compatibility; ignored (field removed)
    latest_review_ts: Optional[str] = None,
    opened_at: Optional[str] = "2026-06-01T00:00:00Z",
) -> PrObservation:
    return PrObservation(
        pr_number=pr_number,
        state=state,  # type: ignore[arg-type]
        latest_review_ts=latest_review_ts,
        opened_at=opened_at,
    )


def _watermark(**kwargs) -> dict:
    return dict(kwargs)


NOW = "2026-06-14T12:00:00Z"


# ---------------------------------------------------------------------------
# AC1-HP: new reviewer activity fires "review"
# ---------------------------------------------------------------------------

def test_ac1_hp_new_review_activity_fires_review():
    """AC1-HP: reviewer activity timestamp T+1 > watermark T, reviewers non-empty
    -> Decision.kind == 'review'."""
    obs = _obs(latest_review_ts="2026-06-14T10:00:00Z")
    wm = _watermark(last_review_ts="2026-06-13T10:00:00Z")
    d = decide(obs, watermark=wm, reviewers=["codex"], merge_ready=False, now_iso=NOW)
    assert d.kind == "review"
    assert d.pr_number == 42


# ---------------------------------------------------------------------------
# AC1-EDGE: latest activity == watermark -> noop
# ---------------------------------------------------------------------------

@pytest.mark.parametrize(
    "latest_ts",
    [
        "2026-06-13T10:00:00Z",  # equal to the watermark: not strictly newer
    ],
)
def test_ac1_edge_not_newer_timestamp_is_noop(latest_ts):
    """AC1-EDGE: activity equal to or older than the watermark does not fire."""
    obs = _obs(latest_review_ts=latest_ts)
    wm = _watermark(last_review_ts="2026-06-13T10:00:00Z")
    d = decide(obs, watermark=wm, reviewers=["codex"], merge_ready=False, now_iso=NOW)
    assert d.kind == "noop"


# ---------------------------------------------------------------------------
# AC2-HP: MERGED + merge_ready -> "merge"
# ---------------------------------------------------------------------------

def test_ac2_hp_merged_and_ready_fires_merge():
    """AC2-HP: state MERGED, merge_ready True, not yet dispatched -> 'merge'."""
    obs = _obs(state="MERGED", merged=True)
    d = decide(obs, watermark={}, reviewers=[], merge_ready=True, now_iso=NOW)
    assert d.kind == "merge"
    assert d.pr_number == 42


# ---------------------------------------------------------------------------
# Boundary: discovery of open and done nodes
# ---------------------------------------------------------------------------

@pytest.mark.parametrize(
    "completed_at,expect",
    [
        ("2026-06-13T18:02:00Z", ["x-0010"]),  # inside the 14-day grace window
        ("2026-05-01T00:00:00Z", []),  # outside the window: dropped
    ],
)
def test_done_node_grace_window(tmp_path, completed_at, expect):
    """AC1-EDGE / Wave 2: a done-at-PR-green node stays watched inside the
    max_age_days grace window and drops outside it."""
    repo_dir = tmp_path / "repo"
    repo_dir.mkdir()
    (repo_dir / ".git").mkdir()
    node = {
        "id": "x-0010",
        "pr_number": 11,
        "pr_url": "https://github.com/owner/repo/pull/11",
        "completed_at": completed_at,
        "cwd": str(repo_dir),
    }
    result = discover_open_prs([node], now_iso=NOW, max_age_days=14)
    assert [c.node_id for c in result] == expect


def test_superseded_done_node_never_discovered(tmp_path):
    """AC1-EDGE / Wave 2: a superseded node is excluded even inside the window."""
    repo_dir = tmp_path / "repo"
    repo_dir.mkdir()
    (repo_dir / ".git").mkdir()
    node = {
        "id": "x-0012",
        "pr_number": 13,
        "pr_url": "https://github.com/owner/repo/pull/13",
        "completed_at": "2026-06-13T00:00:00Z",
        "superseded_by": "x-0099",
        "cwd": str(repo_dir),
    }
    assert discover_open_prs([node], now_iso=NOW, max_age_days=14) == []


# ---------------------------------------------------------------------------
# Boundary: zero reviewers
# ---------------------------------------------------------------------------

def test_boundary_zero_reviewers_gate_the_review_arm():
    """Boundary: new reviewer activity with nobody configured does not fire."""
    wm = _watermark(last_review_ts="2026-06-13T00:00:00Z")
    obs = _obs(latest_review_ts="2026-06-14T10:00:00Z")
    d = decide(obs, watermark=wm, reviewers=[], merge_ready=False, now_iso=NOW)
    assert d.kind == "noop"


# ---------------------------------------------------------------------------
# merge-not-ready: MERGED but merge_ready False
# ---------------------------------------------------------------------------

def test_merge_not_ready_is_noop():
    """merge-not-ready: state MERGED but merge_ready False -> 'noop', reason 'merge-not-ready'."""
    obs = _obs(state="MERGED", merged=True)
    d = decide(obs, watermark={}, reviewers=[], merge_ready=False, now_iso=NOW)
    assert d.kind == "noop"
    assert d.reason == "merge-not-ready"


# ---------------------------------------------------------------------------
# AC1-ERR: read_pr_state raises ReconcileError on gh failure
# ---------------------------------------------------------------------------

def _make_candidate(pr_number: int = 42, repo_slug: str = "owner/repo") -> PrCandidate:
    return PrCandidate(
        node_id="x-0001",
        pr_number=pr_number,
        pr_url=f"https://github.com/{repo_slug}/pull/{pr_number}",
        repo_dir=None,
        repo_slug=repo_slug,
    )


def _stub_runner_ok_merge_state(*args, **kwargs) -> subprocess.CompletedProcess:
    """Stub runner returning a valid OPEN pr state for the merge-state call."""
    payload = json.dumps({
        "number": 42,
        "state": "open",
        "merged": False,
        "html_url": "https://github.com/owner/repo/pull/42",
        "created_at": "2026-06-01T00:00:00Z",
        "merged_at": None,
        "merge_commit_sha": None,
        "head": {"sha": "head-sha", "ref": "feature/test"},
        "base": {"ref": "main"},
    })
    return subprocess.CompletedProcess(args=args[0], returncode=0, stdout=payload, stderr="")


def test_ac1_err_read_pr_state_raises_reconcile_error_on_gh_failure(monkeypatch):
    """AC1-ERR: an op error answer raises ReconcileError; a missing binary or
    a timeout raises the same exception through command_call."""
    cand = _make_candidate()

    def fake_command_call(command, payload, unavailable=None, timeout=30):
        return {"error": "repos/owner/repo/pulls/42/reviews failed: HTTP 404: Not Found"}

    monkeypatch.setattr("fno.pr_watch._discover.command_call", fake_command_call)
    with pytest.raises(ReconcileError):
        read_pr_state(cand, reviewers=["codex"], runner=_stub_runner_ok_merge_state)


# ---------------------------------------------------------------------------
# Boundary: max-age parks before any dispatch
# ---------------------------------------------------------------------------

def test_max_age_parks_before_review():
    """A stale PR parks even with fresh reviewer activity: the age gate is
    the watcher's own judgment and outranks a dispatch."""
    obs = _obs(state="OPEN", opened_at="2026-01-01T00:00:00Z",
               latest_review_ts="2026-06-14T11:00:00Z")
    d = decide(
        obs, watermark={"last_review_ts": None, "merge_dispatched": False},
        reviewers=["codex"], merge_ready=False, now_iso=NOW,
    )
    assert d.kind == "park"
    assert d.reason == "max-age"
