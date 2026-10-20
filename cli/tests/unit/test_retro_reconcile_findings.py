"""Unit tests for the retro reconcile-findings scan (phantom-node sweep).

Drives scan_addressed_findings with injected fetchers so no gh/network is hit.
The `fno backlog reconcile-findings` door retired; the scan function is the
live surface the retro routine drives.
"""
from __future__ import annotations

from fno.retro.reconcile_findings import retro_targets, scan_addressed_findings


def retro_node(node_id, pr, comment_id, *, completed=False, superseded=False):
    """A backlog node shaped like one retro land writes: a Source permalink + trailer."""
    details = (
        f"Some finding body.\n\n"
        f"Source: PR #{pr}, reviewer `chatgpt-codex-connector[bot]`, "
        f"https://github.com/o/r/pull/{pr}#discussion_r{comment_id}\n\n"
        f"<!-- retro-triage source_pr={pr} finding_hash=deadbeef -->"
    )
    node = {"id": node_id, "details": details}
    if completed:
        node["completed_at"] = "2026-07-22T00:00:00Z"
    if superseded:
        node["superseded_by"] = "ab-other"
    return node


def _fetchers(*, threads_by_pr=None, comments_by_pr=None, commits_by_pr=None, unavailable_prs=()):
    threads_by_pr = threads_by_pr or {}
    comments_by_pr = comments_by_pr or {}
    commits_by_pr = commits_by_pr or {}

    def thread_state(pr, *, repo=None, warnings=None):
        if pr in unavailable_prs:
            return [], True
        return threads_by_pr.get(pr, []), False

    def comments(pr, *, repo=None, warnings=None):
        return comments_by_pr.get(pr, [])

    def commit_dates(pr, *, repo=None, warnings=None):
        return commits_by_pr.get(pr, []), False

    return dict(
        thread_state_fetcher=thread_state,
        comments_fetcher=comments,
        commit_dates_fetcher=commit_dates,
    )


def test_resolved_thread_finding_is_addressed():
    entries = [retro_node("ab-1", 555, "3629390387")]
    fetchers = _fetchers(
        threads_by_pr={555: [{"is_resolved": True, "comment_ids": ["3629390387"]}]}
    )
    found = scan_addressed_findings(entries, **fetchers)
    assert [f.node_id for f in found] == ["ab-1"]
    assert found[0].signal == "resolved/outdated thread"
    assert found[0].pr_number == 555 and found[0].repo == "o/r"


def test_unavailable_thread_state_is_skipped_with_warning():
    entries = [retro_node("ab-4", 555, "333")]
    fetchers = _fetchers(unavailable_prs={555})
    warnings: list = []
    found = scan_addressed_findings(entries, warnings=warnings, **fetchers)
    assert found == []  # never close on uncertainty
    assert any("unavailable" in w for w in warnings)


def test_designed_node_with_plan_path_is_never_swept():
    # x-3f39/x-cde1 guard: a node someone designed carries real residual work the
    # commit-after heuristic can't see. It must be skipped even when its comment
    # would otherwise read as addressed.
    node = retro_node("ab-designed", 555, "999")
    node["plan_path"] = "internal/fno/plans/2026-07-22-thing-ab-designed.md"
    fetchers = _fetchers(
        threads_by_pr={555: [{"is_resolved": True, "comment_ids": ["999"]}]}
    )
    assert scan_addressed_findings([node], **fetchers) == []
    assert retro_targets([node]) == []

