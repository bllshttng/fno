"""Golden receipts: reconcile (post-merge closure).

GitHub is stubbed at PATH with an empty result set, so the run is hermetic
and the receipts pin the no-merged-PR path.
"""
from __future__ import annotations

from tests.goldens._door import door, make_sandbox, seed_node, warm, write_gh_stub


def test_reconcile_refuses_to_query_a_pr_without_repo_context(tmp_path):
    # A node carrying only a bare pr_number is never matched against an
    # ambient repo: the wrong-repo guard fails closed and names the fact.
    root = make_sandbox(tmp_path, [seed_node("x-bbb88000", "in_progress", pr_number=2703)])
    warm(root, "x-bbb88000")
    stub = write_gh_stub(tmp_path)
    code, out, err = door(root, ["reconcile"], path_prepend=stub)
    assert code == 4, err
    assert "ledger harvest: filled 0, marked 0\n" in out
    assert "status drift x-bbb88000: stored in_progress, derived in_review (not written)\n" in out
    assert "1 node(s) could not be resolved:\n" in err
    assert "PR #2703: no repo context (pr_url unparseable and cwd unset);" in err
    assert "refusing to query to avoid a wrong-repo match" in err
