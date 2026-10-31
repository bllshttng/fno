"""Tests for pr_watch._state (watermark store) and pr_watch._dispatch (tick orchestrator).

TDD: tests written BEFORE implementation.  Every test targets a named
acceptance criterion from the task 1.2 spec.

Dependency injection is used throughout: no real claude, gh, launchd, or
filesystem-global writes.  Temporary directories replace ~/.fno.
"""
from __future__ import annotations

import json
import os
import subprocess
import time
from pathlib import Path
from typing import Optional

import pytest


def _quiet_stranded_door(rows):
    """Stand in for the fno-agents ``worktree-stranded`` door.

    The sweep runs in the binary since the worktree-family port; these tests
    exercise the Python leg around it, so the door answers the staged rows
    and refuses every other command (which reads as an empty leg, the same
    posture the deleted modules' stubs produced).
    """

    def _door(command, args, timeout=None):
        if command != "worktree-stranded":
            return ("stubbed door", None)
        return None, {"rows": list(rows), "outcomes": []}

    return _door


@pytest.fixture(autouse=True)
def _sandbox_bounce_receipts(tmp_path, monkeypatch):
    """Bounce receipts and pr_watch_bounce reads land in a tmp state dir,
    never the developer's ~/.fno."""
    monkeypatch.setattr("fno.paths.state_dir", lambda: tmp_path / "state")


@pytest.fixture(autouse=True)
def _hermetic_post_merge(monkeypatch):
    """Keep tick tests hermetic after the native cutover.

    The default ``_default_dispatch_ritual`` now speaks to the native
    ``pr-watch post-merge-dispatch`` arm through command_call. The stub mirrors
    the arm's own fail-closed auto_run read so the arming tests stay
    meaningful; tests injecting ``dispatch_ritual_fn`` never reach it.
    """
    import tomllib

    import fno.rust_binary as rb

    def _stub(command, payload, **_kw):
        pr = payload.get("pr_number")
        cwd = payload.get("node_cwd")
        armed = False
        if cwd:
            cfg = Path(cwd) / ".fno" / "config.toml"
            if cfg.is_file():
                with open(cfg, "rb") as fh:
                    armed = bool(
                        tomllib.load(fh).get("post_merge", {}).get("auto_run", False)
                    )
        if not armed:
            return {
                "outcome": "disabled",
                "pr_number": pr,
                "short_id": "",
                "detail": "auto-run-disabled",
            }
        return {
            "outcome": "dispatched",
            "pr_number": pr,
            "short_id": "verb",
            "detail": "cold: no-live-source-session",
        }

    monkeypatch.setattr(rb, "command_call", _stub)


@pytest.fixture(autouse=True)
def _free_gh_budget(monkeypatch):
    """The drain's fleet-budget read must never answer from the real ledger
    inside a test; the budget tests in test_pr_watch_merge_drain.py stub
    their own values."""
    import fno.pr_watch._dispatch as _dispatch_mod

    monkeypatch.setattr(_dispatch_mod, "_gh_budget_backoff_left", lambda: 0.0)


# ---------------------------------------------------------------------------
# Helpers / stubs
# ---------------------------------------------------------------------------


def _make_obs(
    pr_number: int = 1,
    state: str = "OPEN",
    merged: bool = False,  # kept for call-site compat; ignored (field removed)
    latest_review_ts: Optional[str] = None,
    opened_at: str = "2026-06-01T00:00:00Z",
):
    """Build a PrObservation without importing (tests run before impl exists)."""
    from fno.pr_watch._discover import PrObservation

    return PrObservation(
        pr_number=pr_number,
        state=state,
        latest_review_ts=latest_review_ts,
        opened_at=opened_at,
    )


def _arm_auto_run(repo_dir: Path) -> None:
    """Arm post_merge.auto_run for a repo dir. _default_dispatch_ritual honors it
    (x-7930): a bare tmp_path defaults auto_run off -> dispatch is 'disabled'."""
    cfg = repo_dir / ".fno" / "config.toml"
    cfg.parent.mkdir(parents=True, exist_ok=True)
    cfg.write_text("[post_merge]\nauto_run = true\n")


def _make_candidate(
    node_id: str = "x-abc12345",
    pr_number: int = 1,
    repo_dir: Optional[Path] = None,
    repo_slug: str = "owner/repo",
):
    from fno.pr_watch._discover import PrCandidate

    return PrCandidate(
        node_id=node_id,
        pr_number=pr_number,
        pr_url=f"https://github.com/{repo_slug}/pull/{pr_number}",
        repo_dir=repo_dir,
        repo_slug=repo_slug,
    )


def _listing_rows(rows: list[dict]):
    """The status-pr-list op answer shape for a successful sweep read."""
    return {"rows": rows, "error": None}


# ---------------------------------------------------------------------------
# State module tests
# ---------------------------------------------------------------------------


class TestTrackedStateBatch:
    """The production sweep reads each repository once and fails closed.

    x-c12c: the sweep runs one REST listing per repository through the
    ``status-pr-list`` op on the authorized-merge door (pr port 7 retired the
    subprocess runner transport); untracked-but-open PRs still surface, and
    a repo failure counts into the sweep_failures return."""

    def test_reads_all_requested_states_one_call_per_repo(self, monkeypatch):
        from fno.pr_watch._discover import read_tracked_pr_states

        slugs: list[str] = []

        def _door(command, payload, timeout=None):
            assert command == "authorized-merge" and payload.get("op") == "status-pr-list"
            # owner/one: tracked #1 open plus untracked open #99; tracked #2
            # is absent from the open listing (NOT_OPEN). owner/two: tracked
            # #3 absent too.
            rows_by_repo = {
                "owner/one": [{"number": 1, "state": "OPEN"},
                              {"number": 99, "state": "OPEN"}],
                "owner/two": [],
            }
            slug = payload["slug"]
            slugs.append(slug)
            return _listing_rows(rows_by_repo[slug])

        monkeypatch.setattr("fno.rust_binary.command_call", _door)
        states, sweep_failures = read_tracked_pr_states(
            {"owner/one#1", "owner/one#2", "owner/two#3"}
        )

        assert states == {
            "owner/one#1": "OPEN",
            "owner/one#2": "NOT_OPEN",
            "owner/one#99": "OPEN",
            "owner/two#3": "NOT_OPEN",
        }
        assert sweep_failures == 0
        # One listing op per repository: no closed listing, no exact reads.
        assert sorted(slugs) == ["owner/one", "owner/two"]

    def test_repo_read_failure_returns_unknown_for_each_requested_key(
        self, monkeypatch
    ):
        from fno.rust_binary import CommandUnavailable

        from fno.pr_watch._discover import read_tracked_pr_states

        def _door(command, payload, timeout=None):
            raise CommandUnavailable("network down")

        monkeypatch.setattr("fno.rust_binary.command_call", _door)
        states, sweep_failures = read_tracked_pr_states(
            {"owner/repo#1", "owner/repo#2"}
        )

        assert states == {"owner/repo#1": "UNKNOWN", "owner/repo#2": "UNKNOWN"}
        assert sweep_failures == 1


# ---------------------------------------------------------------------------
# State module tests
# ---------------------------------------------------------------------------


    def test_not_open_batch_keys_drop_without_a_closed_read(self, tmp_path):
        """x-c79d: absence from a successful open listing is the terminal
        answer. Three orphaned keys drop as not_open in one tick."""
        from fno.pr_watch._dispatch import tick
        from fno.pr_watch._state import WatermarkStore

        store_path = tmp_path / "state.json"
        store = WatermarkStore(path=store_path)
        for number in (1, 2, 3):
            store.set(f"owner/repo#{number}", {
                "last_review_ts": None,
                "last_seen_state": "OPEN",
                "merge_dispatched": False,
                "retries": 0,
                "parked": None,
            })
        deps = _make_tick_deps(tmp_path, candidates=[])

        tick(
            graph_path=tmp_path / "graph.json",
            store_path=store_path,
            discover_fn=deps["discover"],
            read_pr_state_fn=deps["read_pr_state"],
            read_tracked_states_fn=lambda keys: ({key: "NOT_OPEN" for key in keys}, 0),
            fire_skill_fn=deps["fire_skill"],
            emit=deps["emit"],
            reviewers_for=deps["reviewers_for"],
            claim=deps["claim"],
            notify=deps["notify"],
            post_merge_readiness_fn=deps["post_merge_readiness"],
            now_iso="2026-06-14T12:00:00Z",
        )

        assert WatermarkStore(path=store_path).load() == {}
        receipt = next(e["data"] for e in deps["events"] if e["type"] == "pr_watch_tick")
        assert receipt["swept_count"] == 3
        assert receipt["dropped_count"] == 3
        assert receipt["dropped"] == {"not_open": {"owner/repo": [1, 2, 3]}}
        assert receipt["failed_count"] == 0


# ---------------------------------------------------------------------------
# tick() orchestrator tests
# ---------------------------------------------------------------------------


def _make_tick_deps(
    tmp_path: Path,
    candidates=None,
    obs_map: Optional[dict] = None,
    fire_ok: bool = True,
    fire_rc: int = 0,
    claim_held: bool = False,
    node_claimed: bool = False,
    merge_ready: bool = True,
):
    """Build the full set of injectable stubs for tick()."""
    from fno.pr_watch._discover import PrObservation

    events_emitted: list[dict] = []
    notifications: list[dict] = []
    fired: list[dict] = []

    if candidates is None:
        candidates = []

    # graph returns flat list of node dicts that discover_open_prs consumes
    # We pass candidates directly via a pre-built list to avoid graph reading
    cands_list = candidates

    def fake_read_graph(path):
        # Returns the raw entries; discover_open_prs is the consumer
        return []  # tick will use pre_candidates injection

    def fake_discover(entries):
        return cands_list

    # obs_map: pr_number -> PrObservation
    def fake_read_pr_state(candidate, *, reviewers, runner=None, timeout_s=30.0):
        if obs_map and candidate.pr_number in obs_map:
            return obs_map[candidate.pr_number]
        return PrObservation(
            pr_number=candidate.pr_number,
            state="OPEN",
            latest_review_ts=None,
            opened_at="2026-06-01T00:00:00Z",
        )

    def fake_fire_skill(command, pr_number, repo_dir, *, node_id=None, runner=None, model=None, env_seam=None):
        from fno.pr_watch._dispatch import DispatchResult

        fired.append({"verb": command, "pr": pr_number, "model": model})
        if fire_rc:
            return DispatchResult(ok=False, rc=fire_rc, is_error=False, raw="")
        elif fire_ok:
            return DispatchResult(ok=True, rc=0, is_error=False, raw='{"is_error":false}')
        else:
            return DispatchResult(ok=False, rc=0, is_error=True, raw='{"is_error":true}')

    def fake_emit(event_type: str, data: dict):
        events_emitted.append({"type": event_type, "data": data})

    def fake_reviewers_for(repo_dir):
        return ["gemini-code-assist"]

    def fake_post_merge_readiness(repo_root):
        class V:
            is_ready = merge_ready

        return V()

    class FakeClaim:
        """Stub for the claim helper passed to tick()."""

        def __init__(self, held=False, node_live=False):
            self._held = held
            self._node_live = node_live
            self._held_keys: set = set()

        def acquire_tick_lock(self, key, holder):
            if self._held:
                raise _ClaimHeldByOtherStub()
            self._held_keys.add(key)

        def release_tick_lock(self, key, holder):
            self._held_keys.discard(key)

        def acquire_pr_lock(self, key, holder):
            self._held_keys.add(key)

        def release_pr_lock(self, key, holder):
            self._held_keys.discard(key)

        def is_node_live(self, node_id: str) -> bool:
            return self._node_live

    def fake_notify(msg: str, **kw):
        notifications.append({"msg": msg, **kw})

    fake_claim = FakeClaim(held=claim_held, node_live=node_claimed)

    return {
        "events": events_emitted,
        "fired": fired,
        "notifications": notifications,
        "read_graph": fake_read_graph,
        "discover": fake_discover,
        "read_pr_state": fake_read_pr_state,
        "fire_skill": fake_fire_skill,
        "emit": fake_emit,
        "reviewers_for": fake_reviewers_for,
        "claim": fake_claim,
        "notify": fake_notify,
        "post_merge_readiness": fake_post_merge_readiness,
    }


class _ClaimHeldByOtherStub(Exception):
    def __init__(self, holder: str = "pr-watch:4242", pid: int = 4242):
        super().__init__(f"held by {holder}")
        self.holder = holder
        self.pid = pid


class TestTickOrchestrator:
    """Tests for the impure tick() function."""

    def test_oversized_tick_receipt_emits_bounded_exact_chunks(self):
        """The positive receipt survives the 64KB event validation ceiling."""
        from fno.events.store_client import validate
        from fno.pr_watch._dispatch import _emit_tick_receipt

        numbers = list(range(1, 10_001))
        receipt = {
            "open_prs": 0,
            "acted": 0,
            "swept_count": len(numbers),
            "swept": {"owner/repo": numbers},
            "dropped_count": len(numbers),
            "dropped": {"closed": {"owner/repo": numbers}},
            "normalized_count": 0,
            "normalized": [],
            "failed_count": 0,
            "failed": [],
        }
        events = []

        _emit_tick_receipt(lambda kind, data: events.append((kind, data)), receipt)

        chunks = [data for kind, data in events if kind == "pr_watch_sweep_chunk"]
        summary = next(data for kind, data in events if kind == "pr_watch_tick")
        assert len(chunks) > 1
        assert summary["receipt_chunks"] == len(chunks)
        assert sum(chunk["item_count"] for chunk in chunks) == 20_000
        for kind, data in events:
            assert len(json.dumps(data).encode()) < 65_536
            validate({
                "ts": "2026-06-14T12:00:00Z",
                "type": kind,
                "source": "daemon",
                "data": data,
            })

    def test_oversized_receipt_never_summarizes_after_a_failed_chunk(self):
        """A missing exact-detail chunk makes the whole tick fail closed."""
        from fno.pr_watch._dispatch import _emit_tick_receipt

        numbers = list(range(1, 10_001))
        receipt = {
            "open_prs": 0,
            "acted": 0,
            "swept_count": len(numbers),
            "swept": {"owner/repo": numbers},
            "dropped_count": len(numbers),
            "dropped": {"closed": {"owner/repo": numbers}},
            "normalized_count": 0,
            "normalized": [],
            "failed_count": 0,
            "failed": [],
        }
        events = []

        def emit(kind, data):
            events.append((kind, data))
            return not (kind == "pr_watch_sweep_chunk" and len(events) == 2)

        with pytest.raises(RuntimeError, match="chunk 2/"):
            _emit_tick_receipt(emit, receipt)

        assert all(kind != "pr_watch_tick" for kind, _data in events)

    def test_batch_baseline_does_not_fire_on_preexisting_reviews(self, tmp_path):
        """A first-seen batch baseline must not arm decide() to fire on review
        activity that predates tracking (the old rich baseline captured
        obs.latest_review_ts for exactly this reason)."""
        from fno.pr_watch._dispatch import tick

        store_path = tmp_path / "state.json"
        candidate = _make_candidate(pr_number=1, repo_dir=tmp_path)
        obs_map = {1: _make_obs(1, "OPEN", latest_review_ts="2026-06-13T00:00:00Z")}
        deps = _make_tick_deps(tmp_path, candidates=[candidate], obs_map=obs_map)

        for _ in range(2):
            tick(
                graph_path=tmp_path / "graph.json",
                store_path=store_path,
                discover_fn=deps["discover"],
                read_pr_state_fn=deps["read_pr_state"],
                fire_skill_fn=deps["fire_skill"],
                emit=deps["emit"],
                reviewers_for=deps["reviewers_for"],
                claim=deps["claim"],
                notify=deps["notify"],
                post_merge_readiness_fn=deps["post_merge_readiness"],
                now_iso="2026-06-14T12:00:00Z",
            )

        assert deps["fired"] == [], (
            "a review that predates the baseline must not fire a check on the next tick; "
            f"fired: {deps['fired']}"
        )

    def test_zero_candidate_tick_prunes_nothing(self, tmp_path):
        """A zero-candidate tick is the graph-hiccup signature, not a license to
        wipe every terminal PR's delivery facts."""
        from fno.pr_watch._dispatch import _delivery_state_path, tick

        store_path = tmp_path / "state.json"
        seeded = {"owner/gone#9": {"retries": 3, "parked": "retries-exhausted"}}
        _delivery_state_path(store_path).write_text(json.dumps(seeded))
        deps = _make_tick_deps(tmp_path, candidates=[])

        tick(
            graph_path=tmp_path / "graph.json",
            store_path=store_path,
            discover_fn=deps["discover"],
            read_pr_state_fn=deps["read_pr_state"],
            fire_skill_fn=deps["fire_skill"],
            emit=deps["emit"],
            reviewers_for=deps["reviewers_for"],
            claim=deps["claim"],
            notify=deps["notify"],
            post_merge_readiness_fn=deps["post_merge_readiness"],
            now_iso="2026-06-14T12:00:00Z",
        )

        receipt = next(e["data"] for e in deps["events"] if e["type"] == "pr_watch_tick")
        assert receipt["dropped"].get("delivery-pruned") is None
        assert json.loads(_delivery_state_path(store_path).read_text()) == seeded

    def test_tick_lock_held_returns_immediately(self, tmp_path):
        """AC-concurrency: if tick lock held, return without discovering/firing."""
        from fno.pr_watch._dispatch import tick

        candidate = _make_candidate(pr_number=1, repo_dir=tmp_path)
        deps = _make_tick_deps(tmp_path, candidates=[candidate], claim_held=True)

        tick(
            graph_path=tmp_path / "graph.json",
            store_path=tmp_path / "state.json",
            discover_fn=deps["discover"],
            read_pr_state_fn=deps["read_pr_state"],
            fire_skill_fn=deps["fire_skill"],
            emit=deps["emit"],
            reviewers_for=deps["reviewers_for"],
            claim=deps["claim"],
            notify=deps["notify"],
            post_merge_readiness_fn=deps["post_merge_readiness"],
            now_iso="2026-06-14T12:00:00Z",
        )
        # No events at all when lock held (tick exits immediately)
        assert deps["events"] == []
        assert deps["fired"] == []

    def test_config_disabled_skips_before_taking_the_lock(self, tmp_path):
        """x-aaaf wave 2 (AC4-HP): enabled=False no-ops before the lock is
        even attempted - no events, no discovery, no fire."""
        from fno.pr_watch._dispatch import tick

        candidate = _make_candidate(pr_number=1, repo_dir=tmp_path)
        deps = _make_tick_deps(tmp_path, candidates=[candidate])

        result = tick(
            graph_path=tmp_path / "graph.json",
            store_path=tmp_path / "state.json",
            discover_fn=deps["discover"],
            read_pr_state_fn=deps["read_pr_state"],
            fire_skill_fn=deps["fire_skill"],
            emit=deps["emit"],
            reviewers_for=deps["reviewers_for"],
            claim=deps["claim"],
            notify=deps["notify"],
            post_merge_readiness_fn=deps["post_merge_readiness"],
            now_iso="2026-06-14T12:00:00Z",
            enabled=False,
        )

        assert result.disabled is True
        assert result.open_prs == 0
        assert result.acted == 0
        assert deps["events"] == []
        assert deps["fired"] == []

    def test_no_checkout_emits_skipped(self, tmp_path):
        """AC-no-checkout: candidate with repo_dir=None -> pr_watch_skipped{reason:no-checkout}."""
        from fno.pr_watch._dispatch import tick

        candidate = _make_candidate(pr_number=1, repo_dir=None)
        deps = _make_tick_deps(tmp_path, candidates=[candidate])

        tick(
            graph_path=tmp_path / "graph.json",
            store_path=tmp_path / "state.json",
            discover_fn=deps["discover"],
            read_pr_state_fn=deps["read_pr_state"],
            fire_skill_fn=deps["fire_skill"],
            emit=deps["emit"],
            reviewers_for=deps["reviewers_for"],
            claim=deps["claim"],
            notify=deps["notify"],
            post_merge_readiness_fn=deps["post_merge_readiness"],
            now_iso="2026-06-14T12:00:00Z",
        )
        skipped = [e for e in deps["events"] if e["type"] == "pr_watch_skipped"]
        assert any(e["data"]["reason"] == "no-checkout" for e in skipped)
        assert deps["fired"] == []

    def test_live_node_claim_emits_skipped(self, tmp_path):
        """AC-concurrency: node with live node: claim -> pr_watch_skipped{reason:claimed}."""
        from fno.pr_watch._dispatch import tick

        candidate = _make_candidate(pr_number=1, repo_dir=tmp_path)
        deps = _make_tick_deps(tmp_path, candidates=[candidate], node_claimed=True)

        tick(
            graph_path=tmp_path / "graph.json",
            store_path=tmp_path / "state.json",
            discover_fn=deps["discover"],
            read_pr_state_fn=deps["read_pr_state"],
            fire_skill_fn=deps["fire_skill"],
            emit=deps["emit"],
            reviewers_for=deps["reviewers_for"],
            claim=deps["claim"],
            notify=deps["notify"],
            post_merge_readiness_fn=deps["post_merge_readiness"],
            now_iso="2026-06-14T12:00:00Z",
        )
        skipped = [e for e in deps["events"] if e["type"] == "pr_watch_skipped"]
        assert any(e["data"]["reason"] == "claimed" for e in skipped)
        assert deps["fired"] == []

    def test_review_transition_fires_check(self, tmp_path):
        """AC1-UI: new reviewer activity past watermark -> pr_watch_dispatched{kind:review}."""
        from fno.pr_watch._dispatch import tick
        from fno.pr_watch._state import WatermarkStore

        # Pre-seed the watermark so the PR is NOT first-seen
        store_path = tmp_path / "state.json"
        store = WatermarkStore(path=store_path)
        store.set("owner/repo#1", {
            "last_review_ts": "2026-06-10T00:00:00Z",
            "last_seen_state": "OPEN",
            "merge_dispatched": False,
            "retries": 0,
            "parked": None,
        })

        candidate = _make_candidate(pr_number=1, repo_dir=tmp_path)
        obs_map = {
            1: _make_obs(pr_number=1, state="OPEN", latest_review_ts="2026-06-12T00:00:00Z")
        }
        deps = _make_tick_deps(tmp_path, candidates=[candidate], obs_map=obs_map, fire_ok=True)

        tick(
            graph_path=tmp_path / "graph.json",
            store_path=store_path,
            discover_fn=deps["discover"],
            read_pr_state_fn=deps["read_pr_state"],
            fire_skill_fn=deps["fire_skill"],
            emit=deps["emit"],
            reviewers_for=deps["reviewers_for"],
            claim=deps["claim"],
            notify=deps["notify"],
            post_merge_readiness_fn=deps["post_merge_readiness"],
            now_iso="2026-06-14T12:00:00Z",
        )
        dispatched = [e for e in deps["events"] if e["type"] == "pr_watch_dispatched"]
        assert len(dispatched) == 1
        assert dispatched[0]["data"]["kind"] == "review"
        assert dispatched[0]["data"]["pr"] == 1

    def test_merge_observed_fires_merged(self, tmp_path):
        """AC2-HP: OPEN watermark + MERGED observation + merge_ready -> fire merged once."""
        from fno.pr_watch._dispatch import tick
        from fno.pr_watch._state import WatermarkStore

        store_path = tmp_path / "state.json"
        store = WatermarkStore(path=store_path)
        store.set("owner/repo#1", {
            "last_review_ts": None,
            "last_seen_state": "OPEN",
            "merge_dispatched": False,
            "retries": 0,
            "parked": None,
        })

        _arm_auto_run(tmp_path)
        candidate = _make_candidate(pr_number=1, repo_dir=tmp_path)
        obs_map = {1: _make_obs(1, "MERGED", merged=True)}
        deps = _make_tick_deps(tmp_path, candidates=[candidate], obs_map=obs_map, merge_ready=True)

        tick(
            graph_path=tmp_path / "graph.json",
            store_path=store_path,
            discover_fn=deps["discover"],
            read_pr_state_fn=deps["read_pr_state"],
            fire_skill_fn=deps["fire_skill"],
            emit=deps["emit"],
            reviewers_for=deps["reviewers_for"],
            claim=deps["claim"],
            notify=deps["notify"],
            post_merge_readiness_fn=deps["post_merge_readiness"],
            now_iso="2026-06-14T12:00:00Z",
        )
        dispatched = [e for e in deps["events"] if e["type"] == "pr_watch_dispatched"]
        assert len(dispatched) == 1
        assert dispatched[0]["data"]["kind"] == "merge"

        # The measured terminal state is acted on and then evicted.
        from fno.pr_watch._state import WatermarkStore as _WS
        fresh_store = _WS(path=store_path)
        assert fresh_store.get("owner/repo#1") is None

    def test_merge_already_dispatched_does_not_refire(self, tmp_path):
        """AC2-UI: merge_dispatched=True in watermark -> no re-fire on second tick."""
        from fno.pr_watch._dispatch import tick
        from fno.pr_watch._state import WatermarkStore

        store_path = tmp_path / "state.json"
        store = WatermarkStore(path=store_path)
        store.set("owner/repo#1", {
            "last_review_ts": None,
            "last_seen_state": "MERGED",
            "merge_dispatched": True,  # already dispatched
            "retries": 0,
            "parked": None,
        })

        candidate = _make_candidate(pr_number=1, repo_dir=tmp_path)
        obs_map = {1: _make_obs(1, "MERGED", merged=True)}
        deps = _make_tick_deps(tmp_path, candidates=[candidate], obs_map=obs_map, merge_ready=True)

        tick(
            graph_path=tmp_path / "graph.json",
            store_path=store_path,
            discover_fn=deps["discover"],
            read_pr_state_fn=deps["read_pr_state"],
            fire_skill_fn=deps["fire_skill"],
            emit=deps["emit"],
            reviewers_for=deps["reviewers_for"],
            claim=deps["claim"],
            notify=deps["notify"],
            post_merge_readiness_fn=deps["post_merge_readiness"],
            now_iso="2026-06-14T12:00:00Z",
        )
        dispatched = [e for e in deps["events"] if e["type"] == "pr_watch_dispatched"]
        assert dispatched == []
        assert deps["fired"] == []

    def test_merge_auto_run_off_parks_not_retries(self, tmp_path):
        """x-7930 / codex P1: a 'disabled' dispatch (auto_run opt-in off) is a
        deliberate no-op, NOT a failure: park 'auto-run-disabled', no retry, no
        failure event, no notify."""
        from fno.pr_watch._dispatch import tick
        from fno.pr_watch._state import WatermarkStore

        store_path = tmp_path / "state.json"
        WatermarkStore(path=store_path).set("owner/repo#1", {
            "last_review_ts": None,
            "last_seen_state": "OPEN",
            "merge_dispatched": False,
            "retries": 0,
            "parked": None,
        })

        # No _arm_auto_run: tmp_path has no config -> auto_run defaults off.
        candidate = _make_candidate(pr_number=1, repo_dir=tmp_path)
        obs_map = {1: _make_obs(1, "MERGED", merged=True)}
        deps = _make_tick_deps(tmp_path, candidates=[candidate], obs_map=obs_map, merge_ready=True)

        tick(
            graph_path=tmp_path / "graph.json",
            store_path=store_path,
            discover_fn=deps["discover"],
            read_pr_state_fn=deps["read_pr_state"],
            fire_skill_fn=deps["fire_skill"],
            emit=deps["emit"],
            reviewers_for=deps["reviewers_for"],
            claim=deps["claim"],
            notify=deps["notify"],
            post_merge_readiness_fn=deps["post_merge_readiness"],
            now_iso="2026-06-14T12:00:00Z",
        )
        assert WatermarkStore(path=store_path).get("owner/repo#1") is None
        assert [e for e in deps["events"] if e["type"] == "pr_watch_dispatch_failed"] == []
        skipped = [e for e in deps["events"] if e["type"] == "pr_watch_skipped"]
        assert any(e["data"].get("reason") == "auto-run-disabled" for e in skipped)
        assert deps["notifications"] == []

    def test_retry_exhaustion_parks(self, tmp_path):
        """AC1-FR: 3 consecutive failures -> parked{reason:retries-exhausted} + notify."""
        from fno.pr_watch._dispatch import tick
        from fno.pr_watch._state import WatermarkStore

        store_path = tmp_path / "state.json"
        store = WatermarkStore(path=store_path)
        store.set("owner/repo#1", {
            "last_review_ts": "2026-06-10T00:00:00Z",
            "last_seen_state": "OPEN",
            "merge_dispatched": False,
            "retries": 2,  # already failed twice
            "parked": None,
        })

        candidate = _make_candidate(pr_number=1, repo_dir=tmp_path)
        # New review activity to trigger a review dispatch
        obs_map = {1: _make_obs(1, "OPEN", latest_review_ts="2026-06-12T00:00:00Z")}
        deps = _make_tick_deps(tmp_path, candidates=[candidate], obs_map=obs_map, fire_ok=False)

        tick(
            graph_path=tmp_path / "graph.json",
            store_path=store_path,
            discover_fn=deps["discover"],
            read_pr_state_fn=deps["read_pr_state"],
            fire_skill_fn=deps["fire_skill"],
            emit=deps["emit"],
            reviewers_for=deps["reviewers_for"],
            claim=deps["claim"],
            notify=deps["notify"],
            post_merge_readiness_fn=deps["post_merge_readiness"],
            now_iso="2026-06-14T12:00:00Z",
        )
        parked = [e for e in deps["events"] if e["type"] == "pr_watch_parked"]
        assert len(parked) >= 1
        assert any(e["data"]["reason"] == "retries-exhausted" for e in parked)
        # notify called
        assert len(deps["notifications"]) >= 1
        # store entry reflects parked (re-read from disk to see tick's writes)
        from fno.pr_watch._state import WatermarkStore as _WS
        fresh_store = _WS(path=store_path)
        entry = fresh_store.get("owner/repo#1")
        assert entry["parked"] == "retries-exhausted"

    def test_admission_refusal_does_not_burn_a_retry(self, tmp_path):
        """An exit-82 refusal is not an attempt: the skip names
        admission-refused, retries stay put, and nothing parks - so an armed
        breaker held across three ticks cannot park every open PR."""
        from fno.pr_watch._dispatch import tick
        from fno.pr_watch._state import WatermarkStore

        store_path = tmp_path / "state.json"
        store = WatermarkStore(path=store_path)
        store.set("owner/repo#1", {
            "last_review_ts": "2026-06-10T00:00:00Z",
            "last_seen_state": "OPEN",
            "merge_dispatched": False,
            "retries": 1,
            "parked": None,
        })

        candidate = _make_candidate(pr_number=1, repo_dir=tmp_path)
        obs_map = {1: _make_obs(1, "OPEN", latest_review_ts="2026-06-12T00:00:00Z")}
        deps = _make_tick_deps(tmp_path, candidates=[candidate], obs_map=obs_map,
                               fire_rc=82)

        tick(
            graph_path=tmp_path / "graph.json",
            store_path=store_path,
            discover_fn=deps["discover"],
            read_pr_state_fn=deps["read_pr_state"],
            fire_skill_fn=deps["fire_skill"],
            emit=deps["emit"],
            reviewers_for=deps["reviewers_for"],
            claim=deps["claim"],
            notify=deps["notify"],
            post_merge_readiness_fn=deps["post_merge_readiness"],
            now_iso="2026-06-14T12:00:00Z",
        )

        skipped = [e for e in deps["events"] if e["type"] == "pr_watch_skipped"]
        assert any(e["data"].get("reason") == "admission-refused" for e in skipped)
        assert [e for e in deps["events"] if e["type"] == "pr_watch_dispatch_failed"] == []
        assert deps["notifications"] == []
        entry = WatermarkStore(path=store_path).get("owner/repo#1")
        assert entry["retries"] == 1
        assert not entry.get("parked")

    def test_corrupt_store_baseline_no_mass_fire(self, tmp_path):
        """AC-ERR: corrupt store -> first tick baselines (no fire) not mass-fires."""
        from fno.pr_watch._dispatch import tick

        store_path = tmp_path / "state.json"
        store_path.write_text("NOT VALID JSON {{{")  # corrupt

        candidates = [
            _make_candidate(pr_number=1, repo_dir=tmp_path, node_id="x-001"),
            _make_candidate(pr_number=2, repo_dir=tmp_path, node_id="x-002"),
        ]
        obs_map = {
            1: _make_obs(1, "OPEN", latest_review_ts="2026-06-12T00:00:00Z"),
            2: _make_obs(2, "MERGED", merged=True),
        }
        deps = _make_tick_deps(tmp_path, candidates=candidates, obs_map=obs_map,
                               fire_ok=True, merge_ready=True)

        tick(
            graph_path=tmp_path / "graph.json",
            store_path=store_path,
            discover_fn=deps["discover"],
            read_pr_state_fn=deps["read_pr_state"],
            fire_skill_fn=deps["fire_skill"],
            emit=deps["emit"],
            reviewers_for=deps["reviewers_for"],
            claim=deps["claim"],
            notify=deps["notify"],
            post_merge_readiness_fn=deps["post_merge_readiness"],
            now_iso="2026-06-14T12:00:00Z",
        )
        # Nothing fired: all PRs are first-seen (store was corrupt -> empty)
        assert deps["fired"] == []
        dispatched = [e for e in deps["events"] if e["type"] == "pr_watch_dispatched"]
        assert dispatched == []

class TestReadPrStateBudget:
    """One read spends one budget. Measured 2026-09-03 over eight real open
    PRs: the merge-state leg means 1.07s and the reviews leg 1.42s, so neither
    leg is the cost. The cost was that ``timeout_s`` bounded EACH leg, so one
    candidate could spend 60s against a tick deadline checked only between
    candidates."""

    def _cand(self, tmp_path):
        from fno.pr_watch._discover import PrCandidate

        return PrCandidate(
            node_id="x-abc",
            pr_number=7,
            pr_url="https://github.com/owner/repo/pull/7",
            repo_dir=tmp_path,
            repo_slug="owner/repo",
        )

    @staticmethod
    def _info_stdout():
        return json.dumps(
            {
                "state": "open",
                "number": 7,
                "html_url": "https://github.com/owner/repo/pull/7",
                "merged": False,
                "merged_at": None,
                "head": {"sha": "head", "ref": "feature/test"},
                "base": {"ref": "main"},
            }
        )

    def test_a_sliver_of_budget_refuses_the_second_leg_too(self, tmp_path, monkeypatch):
        """A floor, not a zero check. A remaining budget under what the leg
        costs would start a read certain to time out, spending the deadline
        to learn nothing."""
        from fno.graph._reconcile import ReconcileError
        from fno.pr_watch import _discover
        from fno.pr_watch._discover import REVIEWS_LEG_FLOOR_S, read_pr_state

        clock = [100.0]
        monkeypatch.setattr(_discover.time, "monotonic", lambda: clock[0])
        launched = []

        def runner(cmd, **kw):
            # Leaves a positive sliver, strictly under the floor.
            clock[0] += 10.0 - (REVIEWS_LEG_FLOOR_S / 2)
            return subprocess.CompletedProcess(
                args=cmd, returncode=0, stdout=self._info_stdout(), stderr=""
            )

        def fake_command_call(command, payload, unavailable=None, timeout=30):
            launched.append(command)

        monkeypatch.setattr(_discover, "command_call", fake_command_call)
        with pytest.raises(ReconcileError):
            read_pr_state(self._cand(tmp_path), reviewers=[], runner=runner, timeout_s=10.0)

        assert launched == [], "the op ran on a budget it cannot finish in"


# ---------------------------------------------------------------------------
# Warm-route merge dispatch (shared marker with reconcile)
# ---------------------------------------------------------------------------


class TestWarmMergeRouting:
    """The tick's merge branch routes through the shared post-merge dispatcher."""

    def _run_merge_tick(self, tmp_path, ritual_outcome, ritual_detail=None, *, ticks=1):
        from fno.pr_watch._dispatch import tick
        from fno.pr_watch._state import WatermarkStore

        store_path = tmp_path / "state.json"
        store = WatermarkStore(path=store_path)
        store.set("owner/repo#1", {
            "last_review_ts": None,
            "last_seen_state": "OPEN",
            "merge_dispatched": False,
            "retries": 0,
            "parked": None,
        })

        candidate = _make_candidate(pr_number=1, repo_dir=tmp_path)
        obs_map = {1: _make_obs(1, "MERGED", merged=True)}
        deps = _make_tick_deps(tmp_path, candidates=[candidate], obs_map=obs_map, merge_ready=True)

        ritual_calls: list[tuple] = []

        def fake_ritual(cand, obs, fire):
            from types import SimpleNamespace

            ritual_calls.append((cand.pr_number, getattr(obs, "merge_sha", None)))
            return SimpleNamespace(
                outcome=ritual_outcome,
                pr_number=cand.pr_number,
                short_id="abcd1234",
                detail=ritual_detail,
            )

        for _ in range(ticks):
            tick(
                graph_path=tmp_path / "graph.json",
                store_path=store_path,
                discover_fn=deps["discover"],
                read_pr_state_fn=deps["read_pr_state"],
                fire_skill_fn=deps["fire_skill"],
                dispatch_ritual_fn=fake_ritual,
                emit=deps["emit"],
                reviewers_for=deps["reviewers_for"],
                claim=deps["claim"],
                notify=deps["notify"],
                post_merge_readiness_fn=deps["post_merge_readiness"],
                now_iso="2026-06-14T12:00:00Z",
            )
        return deps, store_path, ritual_calls

    def test_routed_warm_counts_as_dispatched(self, tmp_path):
        """A warm inject is a completed hand-off and terminal state is evicted."""
        from fno.pr_watch._state import WatermarkStore

        deps, store_path, ritual_calls = self._run_merge_tick(tmp_path, "routed-warm")
        assert ritual_calls == [(1, None)]
        assert deps["fired"] == []  # the ritual seam owns any cold fire
        dispatched = [e for e in deps["events"] if e["type"] == "pr_watch_dispatched"]
        assert len(dispatched) == 1
        assert dispatched[0]["data"]["kind"] == "merge"
        assert dispatched[0]["data"]["route"] == "warm"
        assert WatermarkStore(path=store_path).get("owner/repo#1") is None

    def test_already_dispatched_marker_exists_advances_watermark(self, tmp_path):
        """US3: reconcile got there first and WROTE the marker (completed dedup)
        -> the daemon marks its watermark and fires nothing."""
        from fno.pr_watch._state import WatermarkStore

        deps, store_path, _calls = self._run_merge_tick(
            tmp_path, "already-dispatched", ritual_detail="marker-exists"
        )
        assert deps["fired"] == []
        assert [e for e in deps["events"] if e["type"] == "pr_watch_dispatched"] == []
        skips = [e for e in deps["events"] if e["type"] == "pr_watch_skipped"]
        assert skips and skips[0]["data"]["reason"] == "already-dispatched"
        assert WatermarkStore(path=store_path).get("owner/repo#1") is None

    def test_lock_contention_is_receipted_before_terminal_eviction(self, tmp_path):
        """A concurrent hand-off remains explicit without retaining merged state."""
        from fno.pr_watch._state import WatermarkStore

        deps, store_path, calls = self._run_merge_tick(
            tmp_path, "already-dispatched", ritual_detail="lock-contention", ticks=2
        )
        assert len(calls) == 2
        assert deps["fired"] == []
        assert [e for e in deps["events"] if e["type"] == "pr_watch_dispatched"] == []
        skips = [e for e in deps["events"] if e["type"] == "pr_watch_skipped"]
        assert skips and skips[0]["data"]["reason"] == "dispatch-in-flight"
        assert WatermarkStore(path=store_path).get("owner/repo#1") is None

    def test_spawn_failed_is_receipted_then_the_candidate_leaves(self, tmp_path):
        """One failed hand-off is receipted, then the merged candidate leaves
        the watch set - no retry ladder, no retries-exhausted park for a PR
        that already merged."""
        from fno.pr_watch._dispatch import _delivery_state_path
        from fno.pr_watch._state import WatermarkStore

        deps, store_path, calls = self._run_merge_tick(tmp_path, "spawn-failed", ticks=4)
        assert len(calls) == 1
        assert WatermarkStore(path=store_path).get("owner/repo#1") is None
        failed = [e for e in deps["events"] if e["type"] == "pr_watch_dispatch_failed"]
        assert [event["data"]["retries"] for event in failed] == [1]
        assert [e for e in deps["events"] if e["type"] == "pr_watch_parked"] == []
        receipts = [e["data"] for e in deps["events"] if e["type"] == "pr_watch_tick"]
        assert [receipt["dropped_count"] for receipt in receipts] == [1, 0, 0, 0]
        rec = json.loads(_delivery_state_path(store_path).read_text())["owner/repo#1"]
        assert rec["handled"] == "MERGED"


# ---------------------------------------------------------------------------
# Tick entry/exit records + wall-clock deadline (x-c12c wave 1)
# ---------------------------------------------------------------------------


class TestTickRecordsAndDeadline:
    """AC7-HP/AC8-HP/AC9-EDGE: the CLI command records entry and exit, and a tick
    that outlives its deadline writes a timeout record and exits 75 instead of
    suppressing its launchd successors."""

    def _invoke_tick(self, monkeypatch, dispatch_tick, grant_queue=None,
                     pr_watch_enabled=True, command_error=None,
                     fleet_bucket: "int | None" = 0):
        import typer
        from typer.testing import CliRunner
        from unittest.mock import MagicMock

        from fno.pr_watch import cli as prcli

        monkeypatch.setattr("fno.pr_watch._dispatch.tick", dispatch_tick, raising=True)
        # The merge phase asks Rust for its queue, never the sweep's result.
        command_calls: list = []
        self._command_calls = command_calls

        def _command(command_name, payload, **kw):
            command_calls.append(payload)
            if command_error is not None:
                from fno.rust_binary import CommandUnavailable

                raise CommandUnavailable(command_error)
            return grant_queue or {"candidates": 0, "verdicts": {}, "queue": []}

        monkeypatch.setattr("fno.rust_binary.command_call", _command, raising=True)

        # A MagicMock interval int()s to 1, so the derived deadline is ONE
        # SECOND and the real catch-up leg races it. Give the harness a
        # generous bound unless the test pinned its own (the timeout test does,
        # before it gets here).
        if not os.environ.get("FNO_PR_WATCH_TICK_TIMEOUT"):
            monkeypatch.setenv("FNO_PR_WATCH_TICK_TIMEOUT", "60")

        settings = MagicMock()
        settings.pr_watch.max_age_days = 30
        settings.pr_watch.retries = 3
        settings.pr_watch.enabled = pr_watch_enabled
        # A real interval: the fleet-tail cadence buckets on it, and a
        # MagicMock interval would make the bucket wall-clock noise.
        settings.pr_watch.interval_seconds = 600
        settings.recovery.enabled = False
        # Fleet-tail cadence: stranded/recovery/watchdog run on the
        # interval bucket's slot (0/1/2). Pin the bucket so the phase the
        # test exercises actually runs; None leaves the clock to the test.
        if fleet_bucket is not None:
            monkeypatch.setattr("time.time",
                                lambda b=fleet_bucket: b * 600 + 1.0, raising=True)
        monkeypatch.setattr(prcli, "load_settings", lambda: settings, raising=True)

        events: list[tuple[str, dict]] = []

        def _capture(event_type, data, *, events_path=None):
            events.append((event_type, dict(data)))
            return True

        monkeypatch.setattr(prcli, "_emit_event", _capture, raising=True)

        app = typer.Typer()
        app.command()(prcli.tick)
        res = CliRunner().invoke(app, [])
        return res, events

    def test_attempt_record_precedes_settings_load(self, monkeypatch):
        """AC8-HP: a hang or crash during config load still leaves an attempt."""
        import typer
        from typer.testing import CliRunner

        from fno.pr_watch import cli as prcli

        def _boom():
            raise RuntimeError("settings exploded during import")

        monkeypatch.setattr(prcli, "load_settings", _boom, raising=True)
        events: list[tuple[str, dict]] = []
        monkeypatch.setattr(
            prcli, "_emit_event",
            lambda t, d, **_kw: events.append((t, dict(d))) or True,
            raising=True,
        )

        app = typer.Typer()
        app.command()(prcli.tick)
        res = CliRunner().invoke(app, [])

        assert events[0][0] == "pr_watch_tick_attempt"
        assert events[0][1]["pid"] > 0
        # The end record still fires with outcome error and the settings phase.
        ends = [d for t, d in events if t == "pr_watch_tick_end"]
        assert len(ends) == 1
        assert ends[0]["outcome"] == "error"
        assert ends[0]["phase"] == "settings"
        assert res.exit_code != 0

    def test_sigterm_during_a_tick_writes_its_death_record(self, monkeypatch):
        """A bootout's SIGTERM cannot unwind the tick, so the handler writes
        the end record itself: why=killed (an external kill), and the process
        still dies BY the signal."""
        import signal as signal_mod

        from fno.pr_watch import cli as prcli

        installed: dict[int, object] = {}

        def _record_signal(sig, handler):
            installed[sig] = handler
            return None

        monkeypatch.setattr(prcli.signal, "signal", _record_signal)
        killed: list[int] = []
        monkeypatch.setattr(prcli.os, "kill", lambda pid, sig: killed.append(sig))

        res, events = self._invoke_tick(
            monkeypatch, lambda **_kw: None
        )
        handler = installed.get(signal_mod.SIGTERM)
        assert handler is not None, "tick installed no SIGTERM handler"

        handler(signal_mod.SIGTERM, None)
        ends = [d for t, d in events if t == "pr_watch_tick_end"]
        death = ends[-1]
        assert death["outcome"] == "error"
        assert death["why"] == "killed"
        assert death["phase"]
        assert death["sender"] == "unrecorded"
        rows = [d for t, d in events if t == "control_plane_tick"
                and d.get("arm") == "pr_watch_merge"]
        assert rows and "killed by a signal mid-tick" in rows[-1]["detail"]
        assert "started and did not complete" in rows[-1]["detail"]
        assert rows[-1]["detail"].count("phase=") == 1
        assert rows[-1]["detail"].endswith("sender=unrecorded")
        assert killed == [signal_mod.SIGTERM]
        assert installed[signal_mod.SIGTERM] == signal_mod.SIG_DFL

    def _cut_sweep_world(self, monkeypatch, tmp_path):
        """The shared cheapness stubs behind a deliberately cut sweep: the
        phases after it must be cheap or the tick reads as a different cut."""
        from fno.pr_watch import cli as prcli

        monkeypatch.setenv("FNO_PR_WATCH_TICK_TIMEOUT", "30")
        monkeypatch.setitem(prcli._PHASE_CAP_S, "sweep", 1)
        monkeypatch.setattr(prcli, "_run_notify_watch_phase",
                            lambda _roots=None, timeout_s=None, **_kw: None,
                            raising=True)
        monkeypatch.setattr(prcli, "_catchup_roots", lambda: [tmp_path], raising=True)
        monkeypatch.setattr(prcli, "_STRANDED_FLOOR_S", 10_000.0, raising=True)

    def test_merge_phase_drains_its_queue_without_waiting_for_the_sweep(
        self, monkeypatch, tmp_path
    ):
        """AC6-HP (x-e69d): merge runs before the sweep, so the Rust grant
        queue drains on the fresh wall whatever the sweep does after; a cut
        sweep still leaves the queue intact and named."""
        import time as _time

        def _stall(**_kw):
            _time.sleep(2)
            raise AssertionError("deadline did not interrupt the stalled sweep")

        self._cut_sweep_world(monkeypatch, tmp_path)
        drained: list = []

        def _record_queue(queue, **_kw):
            drained.append(list(queue))
            return {"executed": 1, "held": 0, "failed": 0, "skipped": 0, "budget": 0}

        monkeypatch.setattr("fno.pr_watch._dispatch.run_execute_queue", _record_queue,
                            raising=True)
        grant_queue = {
            "candidates": 2,
            "verdicts": {"granted": 1, "held": 1},
            "elapsed_ms": 812,
            "queue": [{
                "node_id": "x-abc12345", "pr": 1, "repo_slug": "owner/repo",
                "cwd": str(tmp_path),
                "grant": {"source": "config", "recorded_by": "spawner-session",
                          "recorded_at": "2026-08-24T10:00:00Z"},
            }],
        }
        res, events = self._invoke_tick(monkeypatch, _stall, grant_queue=grant_queue)

        assert res.exit_code == 0, res.output
        assert len(drained) == 1 and len(drained[0]) == 1
        cand, key, grant = drained[0][0]
        assert cand.pr_number == 1
        assert key == "owner/repo#1"
        assert grant["recorded_by"] == "spawner-session"
        rotate_payloads = [p for p in self._command_calls if p.get("op") == "grant-queue"]
        assert rotate_payloads, self._command_calls
        assert isinstance(rotate_payloads[0].get("rotate"), int)
        rows = [d for t, d in events if t == "control_plane_tick"]
        merge_rows = [d for d in rows if d.get("arm") == "pr_watch_merge"]
        assert merge_rows and merge_rows[-1].get("acted") == 1
        assert merge_rows[-1].get("skip_reason") is None
        assert merge_rows[-1]["detail"] == (
            "merge ok candidates=2 granted=1 executed=1 held=0 failed=0 skipped=0 budget=0 read_ms=812"
        )
        ends = [d for t, d in events if t == "pr_watch_tick_end"]
        assert ends and ends[-1].get("cut") == ["sweep"]

    def test_stranded_leg_report_mode_still_freshens_the_cache(
        self, monkeypatch, tmp_path
    ):
        """P1 review finding: the cache is display state, not recovery state.
        With the recovery lane unarmed (the documented default) the sweep must
        still classify and persist - report posture, never acting."""
        import typer
        from typer.testing import CliRunner

        from unittest.mock import MagicMock

        from fno.branch_provenance_cache import read_cache
        from fno.pr_watch import cli as prcli
        from fno.pr_watch._dispatch import TickResult

        repo = tmp_path / "repo"
        repo.mkdir()
        settings = MagicMock()
        settings.pr_watch.enabled = True
        settings.pr_watch.max_age_days = 30
        settings.pr_watch.retries = 3
        settings.pr_watch.graphql_min_remaining = 200
        settings.pr_watch.interval_seconds = 600
        settings.recovery.enabled = False
        monkeypatch.setattr(prcli, "load_settings", lambda: settings)
        monkeypatch.setattr(
            "fno.pr_watch._dispatch.tick", lambda **kw: TickResult(open_prs=0, acted=0)
        )
        monkeypatch.setattr(prcli, "_emit_event", lambda *a, **k: True)
        # Slot 0 of the fleet-tail cadence is the stranded leg.
        monkeypatch.setattr("time.time", lambda: 1.0, raising=True)
        monkeypatch.setenv("FNO_PR_WATCH_TICK_TIMEOUT", "600")
        monkeypatch.setattr(prcli, "_catchup_roots", lambda: [repo])
        # The sweep runs in the binary; the leg's own contract is the door
        # call. Report posture travels as --no-wake in the argv, and the
        # returned rows still reach the provenance cache.
        stranded_row = {
            "class": "STRANDED",
            "node": "x-beef",
            "unpushed": 4,
            "age": "2 hours ago",
            "facts": {"path": "/wt/d", "branch": "feature/x-beef",
                      "has_remote": False, "pr_number": None, "live": False},
        }
        door = _quiet_stranded_door([stranded_row])
        seen = []

        def _recording_door(command, args, timeout=None):
            if command == "census":
                # The lane's arming read answers unarmed, the documented default.
                return None, {"armed": False}
            seen.append((command, list(args)))
            return door(command, args, timeout=timeout)

        monkeypatch.setattr("fno.rust_binary.call_binary_json", _recording_door)
        monkeypatch.setattr(prcli, "_replay_views", lambda: None)

        app = typer.Typer()
        app.command()(prcli.tick)
        result = CliRunner().invoke(app, [])

        assert result.exit_code == 0, result.output
        assert seen and seen[0][0] == "worktree-stranded"
        assert "--no-wake" in seen[0][1], "report posture must never act"
        cached = read_cache(repo)
        assert [r["node"] for r in cached] == ["x-beef"], f"cache said: {cached}"
        assert "report" in result.output

    def test_sweep_failure_end_record_reads_degraded(self, monkeypatch):
        """AC4-EDGE at the CLI boundary: the end record distinguishes degraded."""
        from fno.pr_watch._dispatch import TickResult

        res, events = self._invoke_tick(
            monkeypatch,
            lambda **_kw: TickResult(open_prs=0, acted=0, sweep_failures=2),
        )
        assert res.exit_code == 0, res.output
        ends = [d for t, d in events if t == "pr_watch_tick_end"]
        assert ends[0]["outcome"] == "degraded"
        assert ends[0]["sweep_failures"] == 2


# ---------------------------------------------------------------------------
# Fleet leg ordering + heartbeat
# ---------------------------------------------------------------------------


class TestFleetLegRunsAfterACutPRLeg:
    """x-c79d: per-phase slices moved the fleet leg behind the PR legs.

    The old ordering test proved recovery ran BEFORE a stalling PR leg; its
    successor proves recovery still runs, on its own slice, AFTER the sweep
    is cut mid-stall - and still writes its heartbeat.
    """

    def _invoke(self, monkeypatch, tmp_path, dispatch_tick, recovery_answer):
        import typer
        from typer.testing import CliRunner
        from unittest.mock import MagicMock

        from fno.pr_watch import cli as prcli

        door_calls: list[list[str]] = []

        monkeypatch.setattr("fno.pr_watch._dispatch.tick", dispatch_tick, raising=True)

        def _fake_command_call(command_name, payload, **kw):
            door_calls.append(list(command_name))
            return {"candidates": 0, "verdicts": {}, "queue": []}

        monkeypatch.setattr("fno.rust_binary.command_call", _fake_command_call)

        # The silence backstop reads the real registry and resolves every
        # worker's transcript; that is seconds of work these tests do not want.
        def _census(_command, args=(), **_kw):
            return recovery_answer if list(args) == ["recovery-sweep"] else (None, {})

        monkeypatch.setattr("fno.rust_binary.call_binary_json", _census, raising=True)
        # Determinism, not contract: the arms between the cut sweep and the
        # recovery phase must be cheap, or a loaded runner cuts recovery too.
        monkeypatch.setattr(prcli, "_run_notify_watch_phase",
                            lambda _roots=None, timeout_s=None, **_kw: None, raising=True)
        monkeypatch.setattr(prcli, "_catchup_roots", lambda: [tmp_path], raising=True)
        monkeypatch.setattr(prcli, "_STRANDED_FLOOR_S", 10_000.0, raising=True)

        if not os.environ.get("FNO_PR_WATCH_TICK_TIMEOUT"):
            monkeypatch.setenv("FNO_PR_WATCH_TICK_TIMEOUT", "60")

        settings = MagicMock()
        settings.pr_watch.max_age_days = 30
        settings.pr_watch.retries = 3
        # This class's subject is the RECOVERY leg: pin the interval bucket to
        # recovery's fleet-tail slot (1) so it runs every invocation.
        settings.pr_watch.interval_seconds = 600
        monkeypatch.setattr("time.time", lambda: 1 * 600 + 1.0, raising=True)
        settings.recovery.enabled = True
        settings.autonomy.enabled = True
        monkeypatch.setattr(prcli, "load_settings", lambda: settings, raising=True)

        events: list[tuple[str, dict]] = []
        monkeypatch.setattr(
            prcli, "_emit_event",
            lambda t, d, **_kw: events.append((t, dict(d))) or True,
            raising=True,
        )

        app = typer.Typer()
        app.command()(prcli.tick)
        res = CliRunner().invoke(app, [])
        return res, events, door_calls

    def test_ac7_edge_a_raising_fleet_leg_still_lets_the_pr_legs_run(
        self, monkeypatch, tmp_path
    ):
        from fno.pr_watch._dispatch import TickResult

        pr_ran: list[str] = []

        def _pr(**_kw):
            pr_ran.append("pr")
            return TickResult(open_prs=0, acted=0)

        res, events, door_calls = self._invoke(
            monkeypatch, tmp_path, _pr, ("registry unreadable", None)
        )

        assert pr_ran == ["pr"]
        assert res.exit_code == 0, res.output
        ends = [d for t, d in events if t == "pr_watch_tick_end"]
        assert ends[0]["outcome"] == "ok"
        # The leg raised BEFORE its watermark, so no heartbeat door call fires:
        # the watermark honestly reads stale rather than claiming a sweep that
        # never finished.
        assert not any("fleet-state" in call for call in door_calls)


# ---------------------------------------------------------------------------
# GraphQL budget preflight (x-c12c wave 2)
# ---------------------------------------------------------------------------


class TestQuotaPreflight:
    """AC5-HP/AC6-EDGE: read the shared GraphQL budget before spending it."""

    def _tick_with_quota(self, tmp_path, quota, candidates=None):
        from fno.pr_watch._dispatch import tick

        deps = _make_tick_deps(
            tmp_path,
            candidates=candidates
            or [_make_candidate(pr_number=1, repo_dir=tmp_path)],
        )
        result = tick(
            graph_path=tmp_path / "graph.json",
            store_path=tmp_path / "state.json",
            discover_fn=deps["discover"],
            read_pr_state_fn=deps["read_pr_state"],
            fire_skill_fn=deps["fire_skill"],
            emit=deps["emit"],
            reviewers_for=deps["reviewers_for"],
            claim=deps["claim"],
            notify=deps["notify"],
            post_merge_readiness_fn=deps["post_merge_readiness"],
            now_iso="2026-06-14T12:00:00Z",
            graphql_remaining_fn=lambda: quota,
            graphql_min_remaining=200,
        )
        return result, deps

    def test_low_budget_skips_dispatch_and_mints_no_tick(self, tmp_path):
        """AC5-HP/AC9-EDGE: below the floor, no per-PR read fires, no
        pr_watch_tick is emitted, and the skip carries count + reset."""
        result, deps = self._tick_with_quota(tmp_path, (42, "2026-08-17T07:00:00Z"))

        assert result.quota_skip is True
        assert result.quota_remaining == 42
        assert result.quota_reset == "2026-08-17T07:00:00Z"
        assert deps["fired"] == [], "no headless fire may run on a skipped pass"
        ticks = [e for e in deps["events"] if e["type"] == "pr_watch_tick"]
        assert ticks == [], "a quota skip must not mint liveness"

    def test_unreadable_budget_proceeds(self, tmp_path):
        """AC6-EDGE: (None, None) is unknown, not zero - the tick runs."""
        result, deps = self._tick_with_quota(tmp_path, (None, None))

        assert result.quota_skip is False
        assert result.quota_unknown is True
        assert result.open_prs == 1
        assert any(e["type"] == "pr_watch_tick" for e in deps["events"])

    def test_deadline_raised_inside_the_sweep_aborts_the_tick(self, tmp_path):
        """AC7 at the swallow site. TickDeadlineExceeded raised inside the
        guarded sweep must abort the whole tick. If a broad except seam can
        swallow it as a 'sweep failed' warning, the tick continues past the
        one-shot alarm with nothing bounding the rest - exactly the stall
        class the deadline exists to stop."""
        from fno.pr_watch._dispatch import tick
        from fno.pr_watch._state import WatermarkStore
        from fno.pr_watch.cli import TickDeadlineExceeded

        store_path = tmp_path / "state.json"
        WatermarkStore(path=store_path).set("owner/repo#7", {
            "last_review_ts": None,
            "last_seen_state": "OPEN",
            "merge_dispatched": False,
            "retries": 0,
            "parked": None,
        })
        deps = _make_tick_deps(tmp_path, candidates=[])

        def _stall(keys):
            raise TickDeadlineExceeded()

        with pytest.raises(TickDeadlineExceeded):
            tick(
                graph_path=tmp_path / "graph.json",
                store_path=store_path,
                discover_fn=deps["discover"],
                read_pr_state_fn=deps["read_pr_state"],
                read_tracked_states_fn=_stall,
                fire_skill_fn=deps["fire_skill"],
                emit=deps["emit"],
                reviewers_for=deps["reviewers_for"],
                claim=deps["claim"],
                notify=deps["notify"],
                post_merge_readiness_fn=deps["post_merge_readiness"],
                now_iso="2026-06-14T12:00:00Z",
                graphql_remaining_fn=lambda: (4800, "2026-08-17T07:00:00Z"),
            )

    def test_dispatch_budget_skips_new_action_and_mints_receipt(self, tmp_path):
        """A spent cadence budget leaves work for the next tick, not timeout."""
        from fno.pr_watch._dispatch import tick

        candidate = _make_candidate(pr_number=77, repo_dir=tmp_path)
        deps = _make_tick_deps(
            tmp_path,
            candidates=[candidate],
            obs_map={77: _make_obs(77, "MERGED")},
        )
        result = tick(
            graph_path=tmp_path / "graph.json",
            store_path=tmp_path / "state.json",
            discover_fn=deps["discover"],
            read_pr_state_fn=deps["read_pr_state"],
            read_tracked_states_fn=lambda keys: ({key: "MERGED" for key in keys}, 0),
            fire_skill_fn=deps["fire_skill"],
            emit=deps["emit"],
            reviewers_for=deps["reviewers_for"],
            claim=deps["claim"],
            notify=deps["notify"],
            post_merge_readiness_fn=deps["post_merge_readiness"],
            now_iso="2026-06-14T12:00:00Z",
            dispatch_deadline=time.monotonic() - 1,
        )

        assert result.acted == 0
        assert deps["fired"] == []
        assert {event["data"].get("reason") for event in deps["events"]} >= {"tick-budget"}
        receipt = next(event["data"] for event in deps["events"] if event["type"] == "pr_watch_tick")
        assert receipt["swept_count"] == 1
        # AC3-EDGE: the break fired before any rich read, so scanned stays 0
        # while completed stays true - "reached nothing", not "found nothing".
        assert receipt["merge_scan"]["completed"] is True
        assert receipt["merge_scan"]["scanned"] == 0


# ---------------------------------------------------------------------------
# Durable-grant execution: the watcher merges an OPEN PR whose worker parked
# ---------------------------------------------------------------------------


class TestDurableGrantExecution:
    """The merge phase's drain: the Rust grant queue feeds it, and each row
    runs reserved -> canonical merge -> executed/held/failed."""

    GRANT = {"source": "config", "recorded_by": "spawner-session",
             "recorded_at": "2026-08-24T10:00:00Z"}

    def _fake_merge(self, monkeypatch, rc, reason=None):
        merge_calls: list[dict] = []
        self._merge_calls = merge_calls

        def _merge(command, payload, timeout=None):
            assert command == "authorized-merge"
            assert payload.get("authority") == "durable_grant"
            merge_calls.append({"pr": int(payload["argv"][0]), "timeout": timeout})
            if isinstance(rc, BaseException):
                raise rc
            receipt = {"pr": int(payload["argv"][0]), "exit": rc}
            if reason is not None:
                receipt.update(reason=reason, observed_head="headsha")
            return receipt

        monkeypatch.setattr("fno.rust_binary.command_call", _merge)
        return merge_calls

    def _queue(self, tmp_path):
        return [(
            _make_candidate(repo_dir=tmp_path),
            "owner/repo#1",
            dict(self.GRANT),
        )]

    def _drain(self, queue, deps, monkeypatch, tmp_path, *, budget_left=None,
               max_retries=2):
        from fno.pr_watch._dispatch import run_execute_queue
        import fno.pr_watch._dispatch as d

        if budget_left is not None:
            monkeypatch.setattr(d, "_phase_deadline", budget_left)
        return run_execute_queue(
            queue,
            store_path=tmp_path / "state.json",
            emit=deps["emit"],
            notify=deps["notify"],
            max_retries=max_retries,
            claim=deps["claim"],
        )

    def _grant_events(self, deps, phase):
        return [
            e for e in deps["events"]
            if e["type"] == "merge_grant_execution" and e["data"].get("phase") == phase
        ]

    def test_executed_merges_and_marks_dispatched(self, tmp_path, monkeypatch):
        deps = _make_tick_deps(tmp_path, candidates=[])
        self._seed_entries(tmp_path, [1])
        self._fake_merge(monkeypatch, 0)
        counts = self._drain(self._queue(tmp_path), deps, monkeypatch, tmp_path)

        assert counts == {"executed": 1, "held": 0, "failed": 0, "skipped": 0, "budget": 0}
        reserved = self._grant_events(deps, "reserved")
        executed_events = self._grant_events(deps, "executed")
        assert len(reserved) == 1 and len(executed_events) == 1
        assert reserved[0]["data"]["actor"] == "pr-watch"
        assert reserved[0]["data"]["pr"] == 1
        assert reserved[0]["data"]["node_id"] == "x-abc12345"
        assert reserved[0]["data"]["recorded_by"] == "spawner-session"
        from fno.pr_watch._state import WatermarkStore

        entry = WatermarkStore(path=tmp_path / "state.json").get("owner/repo#1")
        assert entry["merge_dispatched"] is True
        assert entry["retries"] == 0

    def test_held_consumes_no_failure_budget(self, tmp_path, monkeypatch):
        deps = _make_tick_deps(tmp_path, candidates=[])
        self._seed_entries(tmp_path, [1])
        self._fake_merge(
            monkeypatch, 2,
            reason="held: checks are pending; require_checks_pass forbids merging without green",
        )
        self._drain(self._queue(tmp_path), deps, monkeypatch, tmp_path)

        assert len(self._grant_events(deps, "held")) == 1
        assert not self._grant_events(deps, "failed")
        from fno.pr_watch._state import WatermarkStore

        entry = WatermarkStore(path=tmp_path / "state.json").get("owner/repo#1")
        assert entry["retries"] == 0
        assert not entry.get("parked")

    @pytest.mark.parametrize(("reason", "park", "missing_head"), [
        ("held: checks are red; the healer or the worker owns the next push", "checks-red", ""),
        ("held: worktree_dirty: /w carries uncommitted changes", "worktree_dirty", True),
    ])
    def test_head_bound_hold_parks_until_the_pr_head_moves(
        self, tmp_path, monkeypatch, reason, park, missing_head
    ):
        """Head-bound holds park once with the REST head and await a push."""
        deps = _make_tick_deps(tmp_path, candidates=[])
        self._seed_entries(tmp_path, [1])
        merge_calls = self._fake_merge(monkeypatch, 2, reason=reason)
        if missing_head:
            import fno.rust_binary

            original = fno.rust_binary.command_call

            def no_head(command, payload, timeout=None):
                receipt = original(command, payload, timeout=timeout)
                receipt.pop("observed_head", None)
                return receipt

            monkeypatch.setattr(fno.rust_binary, "command_call", no_head)
        counts = self._drain(self._queue(tmp_path), deps, monkeypatch, tmp_path)

        assert counts == {"executed": 0, "held": 1, "failed": 0, "skipped": 0, "budget": 0}
        parked = [e for e in deps["events"] if e["type"] == "pr_watch_parked"]
        assert [e["data"]["reason"] for e in parked] == [park]
        assert len(deps["notifications"]) == 1
        from fno.pr_watch._state import WatermarkStore

        entry = WatermarkStore(path=tmp_path / "state.json").get("owner/repo#1")
        assert entry["parked"] == park
        assert entry["parked_head"] == (None if missing_head else "headsha")
        assert entry["retries"] == 0

        next_deps = _make_tick_deps(tmp_path, candidates=[])
        counts = self._drain(self._queue(tmp_path), next_deps, monkeypatch, tmp_path)
        assert counts["skipped"] == 1
        assert merge_calls == [{"pr": 1, "timeout": 900}]
        assert any(
            event["type"] == "pr_watch_skipped" and event["data"]["reason"] == "parked"
            for event in next_deps["events"]
        )

    def test_already_terminal_on_outcome_prefixed_reason(self, tmp_path, monkeypatch):
        """Same prefix defeats the ALREADY_TERMINAL exemption: 'held: PR
        already merged; ...' must mark the row NOT_OPEN, never retry."""
        deps = _make_tick_deps(tmp_path, candidates=[])
        self._seed_entries(tmp_path, [1])
        self._fake_merge(
            monkeypatch, 2,
            reason="held: PR already merged; nothing to merge ",
        )
        counts = self._drain(self._queue(tmp_path), deps, monkeypatch, tmp_path)

        assert counts == {"executed": 0, "held": 1, "failed": 0, "skipped": 0, "budget": 0}
        from fno.pr_watch._state import WatermarkStore

        entry = WatermarkStore(path=tmp_path / "state.json").get("owner/repo#1")
        assert entry["last_seen_state"] == "NOT_OPEN"
        assert not entry.get("parked")

    @staticmethod
    def _seed_entries(tmp_path, prs):
        from fno.pr_watch._state import WatermarkStore

        store_path = tmp_path / "state.json"
        store = WatermarkStore(path=store_path)
        for pr in prs:
            store.set(f"owner/repo#{pr}", {
                "last_review_ts": None,
                "last_seen_state": "OPEN",
                "merge_dispatched": False,
                "retries": 0,
                "parked": None,
            })
        return store_path

    def test_drain_skips_entry_already_merged_by_overlapping_tick(
        self, tmp_path, monkeypatch
    ):
        """The tick lock is gone by merge-phase time, so an overlapping tick
        may have merged the queued PR already; the fresh load under the
        per-PR lock sees merge_dispatched and attempts nothing."""
        deps = _make_tick_deps(tmp_path, candidates=[])
        self._seed_entries(tmp_path, [1])
        from fno.pr_watch._state import WatermarkStore

        store = WatermarkStore(path=tmp_path / "state.json")
        entry = store.get("owner/repo#1")
        entry["merge_dispatched"] = True
        store.set("owner/repo#1", entry)
        self._fake_merge(monkeypatch, 0)
        counts = self._drain(self._queue(tmp_path), deps, monkeypatch, tmp_path)

        assert counts["skipped"] == 1 and counts["executed"] == 0
        merged = [
            e for e in deps["events"]
            if e["type"] == "pr_watch_skipped" and e["data"].get("reason") == "merged"
        ]
        assert [e["data"]["pr"] for e in merged] == [1]
        assert self._merge_calls == []
        assert not self._grant_events(deps, "reserved")

    def test_merge_cut_mid_call_spends_one_retry(self, tmp_path, monkeypatch):
        """AC1-HP (the outage shape): the merge call is cut by the
        phase alarm mid-call. The retry spent BEFORE the call survives, so
        the cut counts as one failed attempt."""
        from fno.pr_watch.cli import TickDeadlineExceeded

        deps = _make_tick_deps(tmp_path, candidates=[])
        self._seed_entries(tmp_path, [1])
        self._fake_merge(monkeypatch, TickDeadlineExceeded())

        import pytest

        with pytest.raises(TickDeadlineExceeded):
            self._drain(self._queue(tmp_path), deps, monkeypatch, tmp_path)
        from fno.pr_watch._state import WatermarkStore

        entry = WatermarkStore(path=tmp_path / "state.json").get("owner/repo#1")
        assert entry["retries"] == 1
        assert not entry.get("parked")

    def test_three_failed_drains_park_and_fourth_reserves_nothing(
        self, tmp_path, monkeypatch
    ):
        """AC2-EDGE: a PR whose merge fails three drains in a row parks at
        retries-exhausted, and a fourth drain reserves nothing for it."""
        deps = _make_tick_deps(tmp_path, candidates=[])
        self._seed_entries(tmp_path, [1])
        self._fake_merge(monkeypatch, 1)
        queue = self._queue(tmp_path)
        for _ in range(3):
            self._drain(queue, deps, monkeypatch, tmp_path, max_retries=3)

        assert len(self._grant_events(deps, "failed")) == 3
        parked = [e for e in deps["events"] if e["type"] == "pr_watch_parked"]
        assert parked and parked[0]["data"]["reason"] == "retries-exhausted"
        self._drain(queue, deps, monkeypatch, tmp_path, max_retries=3)
        assert len(self._grant_events(deps, "reserved")) == 3
        assert len(self._merge_calls) == 3

class TestScanResumesLeastRecentlyPolled:
    """x-b083 absorbed into x-d211: the rich scan resumes where the last
    budget break left off instead of restarting at graph order, and cheap
    disposals (parked, terminal, baselined, unresolvable) cost no rich read."""

    @staticmethod
    def _seed(tmp_path: Path, prs, *, parked_prs=()):
        from fno.pr_watch._state import WatermarkStore

        store_path = tmp_path / "state.json"
        store = WatermarkStore(path=store_path)
        for pr in prs:
            store.set(f"owner/repo#{pr}", {
                "last_review_ts": None,
                "last_seen_state": "OPEN",
                "merge_dispatched": False,
                "retries": 0,
                "parked": "retries-exhausted" if pr in parked_prs else None,
            })
        return store_path

    @staticmethod
    def _counting_reads(deps, clock):
        base_read = deps["read_pr_state"]
        reads: list[int] = []

        def counting_read(candidate, **kw):
            reads.append(candidate.pr_number)
            clock["t"] += 1.0
            return base_read(candidate, **kw)

        deps["read_pr_state"] = counting_read
        return reads

    def _tick(self, tmp_path, deps, monkeypatch, store_path, *, deadline=None):
        from fno.pr_watch._dispatch import tick

        return tick(
            graph_path=tmp_path / "graph.json",
            store_path=store_path,
            discover_fn=deps["discover"],
            read_pr_state_fn=deps["read_pr_state"],
            read_tracked_states_fn=lambda keys: ({k: "OPEN" for k in keys}, 0),
            fire_skill_fn=deps["fire_skill"],
            emit=deps["emit"],
            reviewers_for=deps["reviewers_for"],
            claim=deps["claim"],
            notify=deps["notify"],
            post_merge_readiness_fn=deps["post_merge_readiness"],
            now_iso="2026-06-14T12:00:00Z",
            max_retries=2,
            graphql_remaining_fn=lambda: (4800, None),
            dispatch_deadline=deadline,
        )

    def test_five_ticks_reach_pr_45_and_stamp_each_window(self, tmp_path, monkeypatch):
        """AC3-HP: 50 graph-ordered candidates, 10 rich reads per tick, one
        store. LRU order carries every window forward; the granted PR at
        position 45 executes by tick five."""
        from types import SimpleNamespace

        import fno.pr_watch._dispatch as d

        prs = list(range(1, 51))
        candidates = [
            _make_candidate(pr_number=n, node_id=f"x-{n:08d}", repo_dir=tmp_path)
            for n in prs
        ]
        deps = _make_tick_deps(tmp_path, candidates=candidates)
        store_path = self._seed(tmp_path, prs)

        clock = {"t": 1000.0}
        monkeypatch.setattr(d, "time", SimpleNamespace(monotonic=lambda: clock["t"]))
        reads = self._counting_reads(deps, clock)

        results = []
        for _ in range(5):
            # 10 reads per tick: the read floor is 15s, so the window is
            # deadline - 15; each read costs 1s of the faked clock.
            deadline = clock["t"] + 24.0
            res = self._tick(tmp_path, deps, monkeypatch, store_path, deadline=deadline)
            results.append(res)
            assert not res.quota_skip

        # Every PR observed exactly once, in ten-PR windows, no repeats.
        assert reads == prs
        # The merge phase drains a granted row for PR 45 straight from the
        # Rust queue (the sweep no longer queues).
        monkeypatch.setattr(
            "fno.rust_binary.command_call", lambda command, payload, timeout=None: {"exit": 0}
        )
        from fno.pr_watch._dispatch import run_execute_queue

        run_execute_queue(
            [(_make_candidate(pr_number=45, node_id="x-00000045", repo_dir=tmp_path),
              "owner/repo#45", TestDurableGrantExecution.GRANT)],
            store_path=store_path, emit=deps["emit"],
            notify=deps["notify"], max_retries=2, claim=deps["claim"],
        )
        executed = [
            e for e in deps["events"]
            if e["type"] == "merge_grant_execution" and e["data"].get("phase") == "executed"
        ]
        assert [e["data"]["pr"] for e in executed] == [45]

        from fno.pr_watch._state import WatermarkStore

        state = WatermarkStore(path=store_path).load()
        assert state["owner/repo#45"].get("merge_dispatched") is True
        stamped = [k for k, v in state.items() if isinstance(v, dict) and v.get("last_polled_at")]
        assert len(stamped) == 50, "every rich read stamped its cursor"

    def test_fire_budget_skips_the_spawn_not_the_scan(self, tmp_path, monkeypatch):
        """AC2-EDGE: a merge/review decision on a tick with less than
        _FIRE_FLOOR_S of phase left emits fire-budget, fires nothing, and the
        iteration still lands last_seen_state and the scan still counts the
        candidate as scanned."""
        from types import SimpleNamespace

        import fno.pr_watch._dispatch as d
        from fno.pr_watch._state import WatermarkStore

        candidates = [_make_candidate(pr_number=7, repo_dir=tmp_path)]
        deps = _make_tick_deps(
            tmp_path, candidates=candidates,
            obs_map={7: _make_obs(pr_number=7, state="OPEN",
                                  latest_review_ts="2026-06-14T11:00:00Z")},
        )
        store_path = self._seed(tmp_path, [7])

        clock = {"t": 1000.0}
        monkeypatch.setattr(d, "time", SimpleNamespace(monotonic=lambda: clock["t"]))
        # 35s of phase left -> _ritual_timeout() = 25 < _FIRE_FLOOR_S.
        monkeypatch.setattr(d, "_phase_deadline", clock["t"] + 35.0)
        reads = self._counting_reads(deps, clock)

        res = self._tick(tmp_path, deps, monkeypatch, store_path,
                         deadline=clock["t"] + 600.0)

        assert reads == [7], "the scan still reaches the candidate"
        assert res.acted == 0
        assert deps["fired"] == [], "no spawn may fire without fire budget"
        skipped = [
            e["data"].get("reason") for e in deps["events"]
            if e["type"] == "pr_watch_skipped"
        ]
        assert skipped == ["fire-budget"]
        state = WatermarkStore(path=store_path).load()
        entry = state["owner/repo#7"]
        assert entry.get("last_seen_state") == "OPEN"
        assert entry.get("last_polled_at"), "the cursor stamps even on a skipped fire"
        tick_receipt = next(e["data"] for e in deps["events"] if e["type"] == "pr_watch_tick")
        assert tick_receipt["merge_scan"] == {
            "completed": True, "scanned": 1,
        }

# ---------------------------------------------------------------------------
# Candidate read discipline: the candidate path stops re-reading merged PRs
# every tick, and the open count reads the listing's answer, not stale rows.
# ---------------------------------------------------------------------------


class TestCandidateReadDiscipline:
    """The listing's terminal answer lands on cached candidate rows, OPEN
    candidates read first, and a recorded terminal outcome skips the read."""

    @staticmethod
    def _listing(not_open: set, open_keys: set = frozenset()):
        def read(keys):
            states = {}
            for key in keys:
                if key in not_open:
                    states[key] = "NOT_OPEN"
                elif key in open_keys:
                    states[key] = "OPEN"
                else:
                    states[key] = "MERGED"
            return states, 0

        return read

    def _tick(
        self, tmp_path, deps, store_path, *, listing, deadline=None, ritual_fn=None,
    ):
        from fno.pr_watch._dispatch import tick

        return tick(
            graph_path=tmp_path / "graph.json",
            store_path=store_path,
            discover_fn=deps["discover"],
            read_pr_state_fn=deps["read_pr_state"],
            read_tracked_states_fn=listing,
            fire_skill_fn=deps["fire_skill"],
            emit=deps["emit"],
            reviewers_for=deps["reviewers_for"],
            claim=deps["claim"],
            notify=deps["notify"],
            post_merge_readiness_fn=deps["post_merge_readiness"],
            now_iso="2026-06-14T12:00:00Z",
            dispatch_deadline=deadline,
            dispatch_ritual_fn=ritual_fn,
        )

    @staticmethod
    def _counting_reads(deps, clock):
        base_read = deps["read_pr_state"]
        reads: list[int] = []

        def counting_read(candidate, **kw):
            reads.append(candidate.pr_number)
            clock["t"] += 1.0
            return base_read(candidate, **kw)

        deps["read_pr_state"] = counting_read
        return reads

    def test_failed_listing_preserves_cached_candidate_rows(self, tmp_path):
        """sweep_failures 1: UNKNOWN writes nothing; rows keep the old answer
        until a healthy sweep corrects them."""
        from fno.pr_watch._state import WatermarkStore

        store_path = tmp_path / "state.json"
        WatermarkStore(path=store_path).set("owner/repo#1", {
            "last_review_ts": None,
            "last_seen_state": "OPEN",
            "merge_dispatched": False,
            "retries": 0,
            "parked": None,
        })
        candidate = _make_candidate(pr_number=1, repo_dir=tmp_path)
        deps = _make_tick_deps(tmp_path, candidates=[candidate])

        result = self._tick(
            tmp_path, deps, store_path,
            listing=lambda keys: ({key: "UNKNOWN" for key in keys}, 1),
            deadline=time.monotonic() - 1,
        )

        row = WatermarkStore(path=store_path).get("owner/repo#1")
        assert row["last_seen_state"] == "OPEN"
        assert result.open_prs == 1
        receipt = next(e["data"] for e in deps["events"] if e["type"] == "pr_watch_tick")
        assert receipt["sweep_failures"] == 1

    def test_open_candidate_jumps_the_merged_queue_under_budget(self, tmp_path, monkeypatch):
        """20 merged candidates ahead of 1 OPEN in discovery order, budget one
        read: the OPEN candidate is the one that gets it."""
        from types import SimpleNamespace

        import fno.pr_watch._dispatch as d

        clock = {"t": 1000.0}
        monkeypatch.setattr(d, "time", SimpleNamespace(monotonic=lambda: clock["t"]))

        merged = [
            _make_candidate(pr_number=n, node_id=f"x-{n:08d}", repo_dir=tmp_path)
            for n in range(1, 21)
        ]
        open_cand = _make_candidate(pr_number=99, node_id="x-00000063", repo_dir=tmp_path)
        deps = _make_tick_deps(
            tmp_path, candidates=[*merged, open_cand],
            obs_map={
                **{n: _make_obs(n, "MERGED") for n in range(1, 21)},
                99: _make_obs(99, "OPEN"),
            },
            merge_ready=False,
        )
        reads = self._counting_reads(deps, clock)

        # The OPEN PR is already tracked; a first-seen OPEN row would be
        # baselined in the batch loop and never rich-read this tick.
        from fno.pr_watch._state import WatermarkStore

        store_path = tmp_path / "state.json"
        WatermarkStore(path=store_path).set("owner/repo#99", {
            "last_review_ts": None,
            "last_seen_state": "OPEN",
            "merge_dispatched": False,
            "retries": 0,
            "parked": None,
        })

        self._tick(
            tmp_path, deps, store_path,
            listing=self._listing({f"owner/repo#{n}" for n in range(1, 21)},
                                  open_keys={"owner/repo#99"}),
            # read floor is 15s, each read costs 1s: exactly one read fits.
            deadline=clock["t"] + 15.5,
        )

        assert reads == [99]

    def test_handled_candidate_skips_the_rich_read(self, tmp_path):
        """A merged candidate whose ritual already ran is read once; the memo
        then silences it while the listing keeps saying NOT_OPEN."""
        from fno.pr_watch._dispatch import _delivery_state_path
        from fno.pr_watch._state import WatermarkStore

        store_path = tmp_path / "state.json"
        WatermarkStore(path=store_path).set("owner/repo#1", {
            "last_review_ts": None,
            "last_seen_state": "OPEN",
            "merge_dispatched": False,
            "retries": 0,
            "parked": None,
        })
        candidate = _make_candidate(pr_number=1, repo_dir=tmp_path)
        deps = _make_tick_deps(
            tmp_path, candidates=[candidate], obs_map={1: _make_obs(1, "MERGED")},
        )
        reads = self._counting_reads(deps, {"t": 0.0})

        def fake_ritual(cand, obs, fire):
            from types import SimpleNamespace

            return SimpleNamespace(
                outcome="already-dispatched",
                pr_number=cand.pr_number,
                short_id="abcd1234",
                detail="marker-exists",
            )

        listing = self._listing({"owner/repo#1"})
        self._tick(tmp_path, deps, store_path, listing=listing, ritual_fn=fake_ritual)
        self._tick(tmp_path, deps, store_path, listing=listing, ritual_fn=fake_ritual)

        assert reads == [1]
        rec = json.loads(_delivery_state_path(store_path).read_text())["owner/repo#1"]
        assert rec["handled"] == "MERGED"
        second_tick_skips = [
            e["data"]["reason"] for e in deps["events"]
            if e["type"] == "pr_watch_skipped"
        ]
        assert second_tick_skips.count("already-dispatched") == 1, (
            "the memo skip must be silent; the one receipt came from tick one"
        )

    def test_reopened_candidate_gets_a_fresh_read(self, tmp_path):
        """A handled CLOSED candidate the listing now calls OPEN falls through
        the memo skip and gets the rich read; a handled MERGED one never
        does, because GitHub cannot reopen a merge."""
        from fno.pr_watch._dispatch import _delivery_state_path
        from fno.pr_watch._state import WatermarkStore

        store_path = tmp_path / "state.json"
        _delivery_state_path(store_path).write_text(json.dumps(
            {"owner/repo#1": {"handled": "CLOSED", "retries": 0, "parked": None},
             "owner/repo#2": {"handled": "MERGED", "retries": 0, "parked": None}}
        ))
        # Rows already track both PRs; first-seen OPEN rows would be
        # baselined in the batch loop and never rich-read this tick.
        for number in (1, 2):
            WatermarkStore(path=store_path).set(f"owner/repo#{number}", {
                "last_review_ts": None,
                "last_seen_state": "OPEN",
                "merge_dispatched": False,
                "retries": 0,
                "parked": None,
            })
        candidates = [
            _make_candidate(pr_number=1, repo_dir=tmp_path),
            _make_candidate(pr_number=2, repo_dir=tmp_path),
        ]
        deps = _make_tick_deps(
            tmp_path, candidates=candidates,
            obs_map={1: _make_obs(1, "OPEN"), 2: _make_obs(2, "OPEN")},
        )
        reads = self._counting_reads(deps, {"t": 0.0})

        self._tick(tmp_path, deps, store_path,
                   listing=self._listing(set(), open_keys={"owner/repo#1", "owner/repo#2"}))

        assert reads == [1]
        row = WatermarkStore(path=store_path).get("owner/repo#1")
        assert row["last_seen_state"] == "OPEN"


class TestCatchupRoots:
    """The sidecar scan that feeds the tick's roots (moved here from
    test_sync_catchup.py when the sync logic went native; the roots scan is
    still Python and feeds notify_watch/heal/stranded)."""

    def test_roots_come_from_the_graph_deduped(self, tmp_path, monkeypatch):
        """launchd starts the daemon in `/`, so there is no ambient project.

        A bare load_settings() there reads global config, where post_merge is
        unset - which silently disabled the old catch-up leg. Roots come from
        every sidecar's cwd via one ``load_all()`` scan, not a direct
        graph.json read, and not filtered to open tracker state.
        """
        import json

        import fno.rust_binary as rb
        from fno.pr_watch import cli as pw

        alpha = tmp_path / "alpha"
        alpha.mkdir()
        sidecar_dir = tmp_path / "sidecars"
        sidecar_dir.mkdir()
        rows = {
            "a": str(alpha),
            "b": str(alpha),  # duplicate project
            "c": str(tmp_path / "gone"),  # deleted checkout
            "d": None,  # node with no cwd
        }
        sidecars = {}
        for node_id, cwd in rows.items():
            payload = {"id": node_id}
            if cwd is not None:
                payload["cwd"] = cwd
            (sidecar_dir / f"{node_id}.json").write_text(json.dumps(payload))
            sidecars[node_id] = payload
        monkeypatch.setattr(rb, "sidecar_all", lambda: sidecars)
        monkeypatch.setenv("FNO_TRACKER_BACKEND", "github")
        assert pw._catchup_roots() == [alpha]
