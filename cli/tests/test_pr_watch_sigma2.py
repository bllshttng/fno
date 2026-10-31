"""Sigma-review round 2 fixes: tests written RED-first before implementation.

Covers:
  Fix #1 - _emit_event anchors to state_dir()/events.jsonl (not cwd-relative)
  Fix #1b - plist WorkingDirectory key present
  Fix #3 - PrWatchBlock numeric bounds (interval_seconds=0 raises)
  Fix #4 - PrObservation.merged field removed
  Fix #6 - closed-PR park at tick() orchestrator level
"""
from __future__ import annotations

import os
from pathlib import Path
from typing import Optional

import pytest


# ---------------------------------------------------------------------------
# Helpers shared across tests
# ---------------------------------------------------------------------------


def _make_obs(
    pr_number: int = 1,
    state: str = "OPEN",
    latest_review_ts: Optional[str] = None,
    opened_at: str = "2026-06-01T00:00:00Z",
):
    """Build a PrObservation WITHOUT the removed 'merged' field."""
    from fno.pr_watch._discover import PrObservation

    return PrObservation(
        pr_number=pr_number,
        state=state,
        latest_review_ts=latest_review_ts,
        opened_at=opened_at,
    )


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


def _make_tick_deps(
    tmp_path: Path,
    candidates=None,
    obs_map: Optional[dict] = None,
    fire_ok: bool = True,
    merge_ready: bool = False,
):
    """Minimal injectable stubs for tick()."""
    from fno.pr_watch._dispatch import DispatchResult

    events_emitted: list[dict] = []
    fired: list[dict] = []

    if candidates is None:
        candidates = []

    def fake_discover(entries):
        return candidates

    def fake_read_pr_state(candidate, *, reviewers, runner=None, timeout_s=30.0):
        if obs_map and candidate.pr_number in obs_map:
            return obs_map[candidate.pr_number]
        return _make_obs(pr_number=candidate.pr_number)

    def fake_fire_skill(command, pr_number, repo_dir, *, runner=None, model=None, env_seam=None):
        fired.append({"verb": command, "pr": pr_number})
        if fire_ok:
            return DispatchResult(ok=True, rc=0, is_error=False, raw='{"is_error":false}')
        return DispatchResult(ok=False, rc=1, is_error=True, raw='{"is_error":true}')

    def fake_emit(event_type: str, data: dict):
        events_emitted.append({"type": event_type, "data": data})

    def fake_post_merge_readiness(repo_root):
        class V:
            is_ready = merge_ready
        return V()

    class FakeClaim:
        def acquire_tick_lock(self, key, holder): pass
        def release_tick_lock(self, key, holder): pass
        def acquire_pr_lock(self, key, holder): pass
        def release_pr_lock(self, key, holder): pass
        def is_node_live(self, node_id): return False

    return {
        "events": events_emitted,
        "fired": fired,
        "discover": fake_discover,
        "read_pr_state": fake_read_pr_state,
        "fire_skill": fake_fire_skill,
        "emit": fake_emit,
        "reviewers_for": lambda _: [],
        "claim": FakeClaim(),
        "notify": lambda *a, **kw: None,
        "post_merge_readiness": fake_post_merge_readiness,
    }


# ---------------------------------------------------------------------------
# Fix #1a — _emit_event default path is state_dir()/events.jsonl, NOT cwd-relative
# ---------------------------------------------------------------------------


class TestEmitEventAnchoredPath:
    """AC1-VERIFY: _emit_event writes to state_dir()/events.jsonl when no explicit path given."""

    def test_default_events_path_is_under_state_dir_not_cwd(self, tmp_path, monkeypatch):
        """The daemon's events path must NOT be cwd-relative.

        Redirects HOME to tmp_path so state_dir() returns tmp_path/.fno.
        Changes cwd to a DIFFERENT tmp directory.
        Asserts the event lands in state_dir()/events.jsonl, not in the cwd.
        """
        # Set up a fake HOME with .fno so state_dir() resolves there
        fake_home = tmp_path / "home"
        fake_home.mkdir()
        (fake_home / ".fno").mkdir()
        monkeypatch.setenv("HOME", str(fake_home))

        # Change cwd to a completely different directory
        other_dir = tmp_path / "other_cwd"
        other_dir.mkdir()
        original_cwd = os.getcwd()
        os.chdir(other_dir)
        try:
            from fno.pr_watch.cli import _emit_event
            # Call WITHOUT explicit events_path — uses the default resolver
            _emit_event(
                "pr_watch_tick",
                {"open_prs": 0, "acted": 0, "swept_count": 0, "swept": {},
                 "dropped_count": 0, "dropped": {}},
            )
        finally:
            os.chdir(original_cwd)

        # Event must NOT land in the cwd
        cwd_events = other_dir / ".fno" / "events.jsonl"
        assert not cwd_events.exists(), (
            f"event landed in cwd-relative path {cwd_events} — bug #1 not fixed"
        )

        # Event MUST land under state_dir (HOME/.fno)
        monkeypatch.setenv("HOME", str(fake_home))

        from tests._event_rows import event_rows

        anchored_events = fake_home / ".fno" / "events.jsonl"
        lines = event_rows(anchored_events)
        assert len(lines) == 1
        ev = lines[0]
        assert ev["type"] == "pr_watch_tick"

# ---------------------------------------------------------------------------
# Fix #1b — plist WorkingDirectory = $HOME
# ---------------------------------------------------------------------------


class TestPlistWorkingDirectory:
    """AC1b-VERIFY: rendered plist contains a WorkingDirectory key set to $HOME."""

    def test_plist_has_working_directory_key(self, tmp_path, monkeypatch):
        """render_plist() includes <key>WorkingDirectory</key> pointing at HOME."""
        fake_home = tmp_path / "home"
        fake_home.mkdir()
        monkeypatch.setenv("HOME", str(fake_home))

        from fno.pr_watch._install import render_plist

        la_dir = tmp_path / "LaunchAgents"
        la_dir.mkdir()
        rendered = render_plist(
            launch_agents_dir=la_dir,
            fno_binary="/usr/local/bin/fno",
            install_path="/usr/bin:/bin",
        )

        assert "<key>WorkingDirectory</key>" in rendered, (
            "plist missing WorkingDirectory key — launchd will start in '/' "
            "causing cwd-relative paths to write to /.fno/"
        )
        # The value must contain the home directory path
        assert str(fake_home) in rendered or _xml_escape_simple(str(fake_home)) in rendered, (
            f"plist WorkingDirectory does not contain HOME ({fake_home})"
        )


def _xml_escape_simple(s: str) -> str:
    return s.replace("&", "&amp;").replace("<", "&lt;").replace(">", "&gt;")


# ---------------------------------------------------------------------------
# Fix #3 — PrWatchBlock numeric field bounds
# ---------------------------------------------------------------------------


class TestPrWatchBlockBounds:
    """AC3: PrWatchBlock rejects zero/invalid numeric values at config load."""

    def test_interval_seconds_zero_raises(self):
        """interval_seconds=0 must raise ValidationError (would set StartInterval:0)."""
        from pydantic import ValidationError
        from fno.config import PrWatchBlock

        with pytest.raises(ValidationError):
            PrWatchBlock(interval_seconds=0)

    def test_bad_value_within_mapping_now_raises(self):
        """A valid dict with an out-of-bounds value must now raise (intended tightening)."""
        from pydantic import ValidationError
        from fno.config import ConfigBlock

        with pytest.raises(ValidationError):
            ConfigBlock.model_validate({"pr_watch": {"interval_seconds": 0}})


# ---------------------------------------------------------------------------
# Fix #4 — PrObservation.merged field removed
# ---------------------------------------------------------------------------


class TestPrObservationMergedFieldRemoved:
    """AC4: PrObservation no longer has a 'merged' field."""

    def test_read_pr_state_does_not_set_merged(self):
        """read_pr_state result has no merged attribute (construction site updated)."""
        from fno.pr_watch._discover import PrObservation
        import dataclasses

        # Verify the construction in _discover.py doesn't still pass merged=
        # by checking the field set is as expected (is_draft rides the same
        # gh pr view call; the flip leg reads OPEN + is_draft).
        field_names = {f.name for f in dataclasses.fields(PrObservation)}
        expected = {"pr_number", "state", "latest_review_ts", "opened_at", "merge_sha", "is_draft"}
        assert "merged" not in field_names, "merged bool must stay removed"
        assert field_names == expected, (
            f"PrObservation fields mismatch. Expected {expected}, got {field_names}"
        )


# ---------------------------------------------------------------------------
# Fix #6 — closed-PR park test at tick() orchestrator level
# ---------------------------------------------------------------------------


class TestClosedPrEvictionAtOrchestrator:
    """AC6-FR: CLOSED PR is receipted and evicted at the tick() level.

    The decide()-level coverage exists; this verifies the full orchestrator
    path: watermark seeded, CLOSED observation fed through tick(), assert
    terminal cache eviction + pr_watch_parked event emitted + nothing fired.
    """

    def test_closed_pr_evicted_at_tick_level(self, tmp_path):
        """AC6-FR: tick() receipts CLOSED, evicts it, and fires nothing."""
        from fno.pr_watch._dispatch import tick
        from fno.pr_watch._state import WatermarkStore

        store_path = tmp_path / "state.json"
        store = WatermarkStore(path=store_path)
        # Pre-seed with a known open baseline so it's not first-seen
        store.set("owner/repo#5", {
            "last_review_ts": None,
            "last_seen_state": "OPEN",
            "merge_dispatched": False,
            "retries": 0,
            "parked": None,
        })

        closed_obs = _make_obs(pr_number=5, state="CLOSED")
        obs_map = {5: closed_obs}
        candidate = _make_candidate(pr_number=5, repo_dir=tmp_path)
        deps = _make_tick_deps(tmp_path, candidates=[candidate], obs_map=obs_map)

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

        # Nothing was fired
        assert deps["fired"] == [], f"expected no fires, got {deps['fired']}"

        # pr_watch_parked event emitted with reason "closed"
        parked_events = [e for e in deps["events"] if e["type"] == "pr_watch_parked"]
        assert len(parked_events) == 1, f"expected 1 parked event, got {parked_events}"
        assert parked_events[0]["data"]["reason"] == "closed"
        assert parked_events[0]["data"]["pr"] == 5

        assert WatermarkStore(path=store_path).get("owner/repo#5") is None
        receipt = next(e["data"] for e in deps["events"] if e["type"] == "pr_watch_tick")
        assert receipt["dropped"] == {"closed": {"owner/repo": [5]}}


# ---------------------------------------------------------------------------
# Control-plane arm rows (x-1b88): the tick command writes one row per arm
# ---------------------------------------------------------------------------


class TestControlPlaneArmRows:
    """Drives the REAL cli.tick() command with every phase unarmed and asserts
    the three pr-watch-hosted arm rows land in the anchored journal, each
    saying why it did nothing."""

    def test_tick_writes_watchdog_and_merge_arm_rows(self, tmp_path, monkeypatch):
        from types import SimpleNamespace
        from unittest.mock import patch

        fake_home = tmp_path / "home"
        fno_dir = fake_home / ".fno"
        fno_dir.mkdir(parents=True)
        monkeypatch.setenv("HOME", str(fake_home))
        monkeypatch.setenv("PR_WATCH_FIRE_CMD", "true")
        # Fleet-tail cadence: the watchdog phase runs on the interval
        # bucket's slot 2. Pin the bucket or a single-tick invocation runs
        # the phase on only one tick in three and the arm row reads
        # off_cadence instead of the lane verdict this test asserts.
        monkeypatch.setattr("time.time", lambda: 1201.0)

        def ns(**kw):
            return SimpleNamespace(**kw)

        settings = ns(
            pr_watch=ns(
                enabled=False,
                interval_seconds=600,
                tick_timeout_seconds=None,
                max_age_days=14,
                retries=2,
                graphql_min_remaining=200,
                model="m",
            ),
            review=ns(github_apps=[], required_bots=[]),
            autonomy=ns(enabled=False),
            recovery=ns(enabled=False, watchdog=ns(enabled=False, mode="report")),
            lead=ns(wake_enabled=False, wake_debounce_seconds=900),
            auto_heal=ns(enabled=False),
        )

        with patch("fno.pr_watch.cli.load_settings", return_value=settings), \
             patch("fno.pr_watch.cli.claim_status", return_value={"state": "free"}), \
             patch("fno.claims.acquire_claim"), \
             patch("fno.claims.release_claim"):
            from fno.pr_watch.cli import tick
            tick()

        from tests._event_rows import event_rows as _event_rows
        rows = [
            r
            for r in _event_rows(fno_dir / "events.jsonl")
            if r.get("type") == "control_plane_tick"
        ]
        by_arm = {r["data"]["arm"]: r["data"] for r in rows}
        assert by_arm["watchdog"]["skip_reason"] == "watchdog_off"
        assert by_arm["watchdog"]["scheduler"] == "launchd:sh.fno.pr-watcher"
        assert by_arm["pr_watch_merge"]["skip_reason"] == "disabled"
        assert by_arm["pr_watch_merge"]["interval_s"] == 600
        detail = by_arm["pr_watch_merge"]["detail"]
        # The merge row opens with its verdict head, then the skip. A clean
        # skip still names no phase.
        assert detail.startswith("merge ok")
        assert detail.endswith("pr_watch disabled")
        assert "phase" not in detail, (
            f"a clean skip must not name a phase. Got: {detail}"
        )
        from fno.events.store_client import validate
        for data in by_arm.values():
            validate(
                {
                    "ts": "2026-09-05T00:00:00Z",
                    "type": "control_plane_tick",
                    "source": "daemon",
                    "data": data,
                }
            )


def test_tick_end_bits_names_the_phase_only_when_the_tick_broke():
    from fno.pr_watch._install import tick_end_bits

    timeout = tick_end_bits(
        {"outcome": "timeout", "phase": "merge", "duration_s": 480.0}
    )
    assert "phase: merge" in timeout, f"timeout bits: {timeout}"
    assert "480.0s" in timeout, f"timeout bits: {timeout}"

    degraded = tick_end_bits(
        {"outcome": "degraded", "phase": "catchup", "sweep_failures": 2,
         "duration_s": 31.2}
    )
    assert "2 sweep failures" in degraded, f"degraded bits: {degraded}"
    assert not any(b.startswith("phase:") for b in degraded), (
        f"a degraded tick must not name a phase. Got: {degraded}"
    )
