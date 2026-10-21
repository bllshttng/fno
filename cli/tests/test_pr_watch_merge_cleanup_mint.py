"""The PR-watch mints a merged PR's cleanup request when none stands.

``fno do pr merge`` mints the reap order at the merge; a merge through
``gh pr merge`` or the web button mints nothing, so its worktree waited on
the 6h sweep instead of the 30-minute grace. The tick is the detector that
sees the merged arm, so it mints through the merge command's own mint, gated
once per merge by a journal check. The test pins the default mint's gates:
a standing request, a checkout with no .git, no checkout, and an open PR
each hold the mint.
"""
from __future__ import annotations

import json
from pathlib import Path

from fno.pr_watch._discover import PrCandidate


def _make_candidate(pr_number: int = 1, repo_dir=None, repo_slug="owner/repo", node_id="x-abc12345"):
    return PrCandidate(
        node_id=node_id,
        pr_number=pr_number,
        pr_url=f"https://github.com/{repo_slug}/pull/{pr_number}",
        repo_dir=repo_dir,
        repo_slug=repo_slug,
    )


def _write_journal(path: Path, rows: list) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    rows_text = [r if isinstance(r, str) else json.dumps(r) for r in rows]
    path.write_text("\n".join(rows_text) + "\n", encoding="utf-8")


def _request(repo: str, pr: int) -> dict:
    return {"type": "merge_cleanup_requested", "data": {"repo": repo, "pr": pr}}


class TestDefaultEnsureMergeCleanup:
    """The default mint: the merge command's mint, gated once per merge."""

    def _patch_parts(self, monkeypatch, minted: list, journal_rows: list, tmp_path: Path) -> None:
        monkeypatch.setattr(
            "fno.pr._merge._merge_request_repo_and_project", lambda cwd: ("/repo", "proj")
        )
        monkeypatch.setattr(
            "fno.pr._merge._manifest_file", lambda cwd: str(cwd) + "/.fno/target-state.md"
        )
        journal = tmp_path / "events.jsonl"
        _write_journal(journal, journal_rows)
        monkeypatch.setattr("fno.agents.events.daemon_lifecycle_log", lambda: journal)

        def fake_mint(pr_number, cwd, state_file, bound_node_ids):
            minted.append((pr_number, cwd, state_file, list(bound_node_ids)))

        monkeypatch.setattr("fno.pr._merge._emit_merge_cleanup_request", fake_mint)

    def test_every_closed_gate_holds_the_mint(self, tmp_path, monkeypatch):
        from fno.pr_watch._dispatch import _default_ensure_merge_cleanup

        minted: list = []
        self._patch_parts(
            monkeypatch, minted, journal_rows=[_request("/repo", 1)], tmp_path=tmp_path
        )
        repo_dir = tmp_path / "wt"
        repo_dir.mkdir()
        (repo_dir / ".git").mkdir()
        plain = tmp_path / "plain"
        plain.mkdir()

        standing = _make_candidate(repo_dir=repo_dir)
        nogit = _make_candidate(repo_dir=plain)
        nocheckout = _make_candidate(repo_dir=None)
        open_obs = _make_candidate(repo_dir=repo_dir)

        _default_ensure_merge_cleanup(standing, "MERGED")
        _default_ensure_merge_cleanup(nogit, "MERGED")
        _default_ensure_merge_cleanup(nocheckout, "MERGED")
        _default_ensure_merge_cleanup(open_obs, "OPEN")

        assert minted == []
