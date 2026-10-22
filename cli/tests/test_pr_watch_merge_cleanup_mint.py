"""The PR-watch mints a merged PR's cleanup request when none stands.

``fno do pr merge`` mints the reap order at the merge; a merge through
``gh pr merge`` or the web button mints nothing, so its worktree waited on
the 6h sweep instead of the 30-minute grace. The tick is the detector that
sees the merged arm, so it mints through the native ``pr-watch
merge-cleanup-mint`` verb, gated once per merge by a journal check. The test
pins the default mint's gates: a standing request, a checkout with no .git,
no checkout, and an open PR each hold the mint.
"""
from __future__ import annotations

import json
import subprocess
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
    """The default mint: the native pr-watch mint, gated once per merge."""

    def _patch_parts(
        self,
        monkeypatch,
        minted: list,
        journal_rows: list,
        tmp_path: Path,
        *,
        pr_json: str = '{"state": "MERGED", "headRefName": "feature/x"}',
    ) -> None:
        journal = tmp_path / "events.jsonl"
        _write_journal(journal, journal_rows)
        monkeypatch.setattr("fno.agents.events.daemon_lifecycle_log", lambda: journal)
        monkeypatch.setattr(
            "fno.pr_watch._dispatch._cleanup_request_repo_and_project",
            lambda cwd: ("/repo", "proj"),
        )

        def fake_emit(envelope, events_path, **_kwargs):
            # The store write path, simulated in place: the journal contract
            # (a request or a skip, never silence) stays observable.
            with open(events_path, "a", encoding="utf-8") as handle:
                handle.write(json.dumps(envelope, separators=(",", ":")) + "\n")
            return {"inserted": True}

        monkeypatch.setattr("fno.events.store_client.emit_envelope", fake_emit)

        def fake_run(cmd, **_kwargs):
            if cmd[:1] == ["gh"]:
                return subprocess.CompletedProcess(cmd, 0, pr_json, "")
            raise AssertionError(f"unexpected subprocess: {cmd}")

        monkeypatch.setattr(subprocess, "run", fake_run)

        def fake_mint(verb, payload, timeout=None):
            assert verb == ["pr-watch", "merge-cleanup-mint"], verb
            minted.append(payload)

        monkeypatch.setattr("fno.rust_binary.verb_call", fake_mint)
        return journal

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

    def test_a_merged_pr_with_no_standing_request_mints_once(
        self, tmp_path, monkeypatch
    ):
        from fno.pr_watch._dispatch import _default_ensure_merge_cleanup

        minted: list = []
        self._patch_parts(monkeypatch, minted, journal_rows=[], tmp_path=tmp_path)
        repo_dir = tmp_path / "wt"
        repo_dir.mkdir()
        (repo_dir / ".git").mkdir()

        _default_ensure_merge_cleanup(_make_candidate(repo_dir=repo_dir), "MERGED")

        assert len(minted) == 1
        payload = minted[0]
        assert payload["pr"] == 1
        assert payload["branch"] == "feature/x"
        assert payload["repo"] == "/repo"
        assert payload["node_ids"] == ["x-abc12345"]

    def test_an_unmerged_gh_row_skips_with_a_journal_row(self, tmp_path, monkeypatch):
        """A request or a skip, never silence: the journal contract requires
        the skip row so the next tick's gate reads a decided state."""
        from fno.agents.events import daemon_lifecycle_log

        from fno.pr_watch._dispatch import _default_ensure_merge_cleanup

        minted: list = []
        journal = self._patch_parts(
            monkeypatch, minted, journal_rows=[], tmp_path=tmp_path,
            pr_json='{"state": "OPEN", "headRefName": "feature/x"}',
        )
        repo_dir = tmp_path / "wt"
        repo_dir.mkdir()
        (repo_dir / ".git").mkdir()

        _default_ensure_merge_cleanup(_make_candidate(repo_dir=repo_dir), "MERGED")

        assert minted == []
        rows = journal.read_text().splitlines()
        skips = [json.loads(r) for r in rows if "merge_cleanup_skipped" in r]
        assert any(s["data"]["reason"] == "not-merged" for s in skips)
