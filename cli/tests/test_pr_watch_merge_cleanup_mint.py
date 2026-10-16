"""The PR-watch mints a merged PR's cleanup request when none stands.

``fno do pr merge`` mints the reap order at the merge; a merge through
``gh pr merge`` or the web button mints nothing, so its worktree waited on
the 6h sweep instead of the 30-minute grace. The tick is the detector that
sees the merged arm, so it mints through the merge verb's own mint, gated
once per merge by a journal check. Three tests, one per contract: the
journal gate, the default mint's gates, and the tick wiring. Hermetic by
construction: no test creates a .git entry, which is exactly the guard
that keeps the default mint off subprocesses.
"""
from __future__ import annotations

import json
from pathlib import Path

import pytest

from fno.pr_watch._discover import PrCandidate, PrObservation


@pytest.fixture(autouse=True)
def _free_gh_budget(monkeypatch):
    import fno.pr_watch._dispatch as _dispatch_mod

    monkeypatch.setattr(_dispatch_mod, "_gh_budget_backoff_left", lambda: 0.0)


def _make_obs(pr_number: int = 1, state: str = "OPEN") -> PrObservation:
    return PrObservation(
        pr_number=pr_number,
        state=state,
        latest_review_ts=None,
        opened_at="2026-06-01T00:00:00Z",
    )


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
    """The default mint: the merge verb's mint, gated once per merge."""

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

    def test_mints_once_through_the_merge_verb_mint(self, tmp_path, monkeypatch):
        from fno.pr_watch._dispatch import _default_ensure_merge_cleanup

        minted: list = []
        self._patch_parts(monkeypatch, minted, journal_rows=[], tmp_path=tmp_path)
        repo_dir = tmp_path / "wt"
        repo_dir.mkdir()
        (repo_dir / ".git").mkdir()

        _default_ensure_merge_cleanup(_make_candidate(repo_dir=repo_dir), "MERGED")

        want = (1, str(repo_dir), str(repo_dir) + "/.fno/target-state.md", ["x-abc12345"])
        assert minted == [want]

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


class TestTickMintsOnMergedArm:
    """The tick calls the mint on the merged arm, before the parked check."""

    def _tick(self, tmp_path, monkeypatch, candidates, obs_map, *, seed=(), listing_absent=(),
              node_live=False, delivery=None):
        import fno.pr_watch._dispatch as d
        from fno.pr_watch._dispatch import tick
        from fno.pr_watch._state import WatermarkStore

        store_path = tmp_path / "state.json"
        if seed:
            cache = WatermarkStore(path=store_path)
            for pr_number in seed:
                cache.set("owner/repo#" + str(pr_number), {
                    "last_review_ts": None,
                    "last_seen_state": "OPEN",
                    "merge_dispatched": False,
                    "retries": 0,
                    "parked": None,
                })
        if delivery:
            delivery_path = store_path.with_name(store_path.stem + "-delivery" + store_path.suffix)
            store = WatermarkStore(path=delivery_path)
            for key, entry in delivery.items():
                store.set(key, entry)

        def discover(_entries):
            return candidates

        def read_state(cand, *, reviewers):
            return obs_map.get(cand.pr_number, _make_obs(cand.pr_number))

        class Claim:
            def acquire_tick_lock(self, key, holder):
                pass

            def release_tick_lock(self, key, holder):
                pass

            def acquire_pr_lock(self, key, holder):
                pass

            def release_pr_lock(self, key, holder):
                pass

            def is_node_live(self, node_id):
                return node_live

        seen: list = []

        def recording_mint(cand, obs_state):
            seen.append((cand.pr_number, obs_state))

        monkeypatch.setattr(d, "_default_ensure_merge_cleanup", recording_mint)

        tick(
            graph_path=tmp_path / "graph.json",
            store_path=store_path,
            discover_fn=discover,
            read_pr_state_fn=read_state,
            read_tracked_states_fn=lambda keys: (
                {k: "OPEN" for k in keys if k not in listing_absent}, 0
            ),
            fire_skill_fn=lambda *a, **k: None,
            dispatch_ritual_fn=lambda cand, obs, fire: None,
            emit=lambda t, dd: None,
            reviewers_for=lambda _dd: [],
            claim=Claim(),
            notify=lambda *a, **k: None,
            post_merge_readiness_fn=lambda _dd: None,
            now_iso="2026-06-14T12:00:00Z",
        )
        return seen

    def test_mint_rides_the_merged_arm_before_the_parked_check(self, tmp_path, monkeypatch):
        cands = [
            _make_candidate(pr_number=1, repo_dir=tmp_path),
            _make_candidate(pr_number=2, repo_dir=tmp_path),
            _make_candidate(pr_number=3, repo_dir=tmp_path),
            _make_candidate(pr_number=4, repo_dir=None),
            _make_candidate(pr_number=5, repo_dir=tmp_path),
        ]
        obs_map = {
            1: _make_obs(1, "MERGED"),
            2: _make_obs(2, "OPEN"),
            3: _make_obs(3, "MERGED"),
            4: _make_obs(4, "MERGED"),
            5: _make_obs(5, "MERGED"),
        }

        seen = self._tick(
            tmp_path,
            monkeypatch,
            candidates=cands,
            obs_map=obs_map,
            seed=(1, 2, 3, 4),
            listing_absent=("owner/repo#5",),
            delivery={"owner/repo#5": {"parked": "auto-run-disabled"}},
        )

        # 2 reaches the call open (the default's own gate declines it);
        # 4 dies at the no-checkout wall; 5 rides the parked-relic synth.
        assert seen == [(1, "MERGED"), (2, "OPEN"), (3, "MERGED"), (5, "MERGED")]
