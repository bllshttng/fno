"""The fleet-sweep watermark: liveness proof plus the fallback-chain memory.

Run: cd cli && uv run pytest tests/unit/test_fleet_state.py -q
"""
from __future__ import annotations

import time
from pathlib import Path

from fno import fleet_state


def _p(tmp_path: Path) -> Path:
    return tmp_path / "fleet-sweep-state.json"


class TestHeartbeat:


    def test_a_heartbeat_preserves_the_chain_memory(self, tmp_path: Path) -> None:
        p = _p(tmp_path)
        fleet_state.record_link("node-1", "codex/gpt-5.6-sol", path=p)
        fleet_state.write_heartbeat(candidates=1, path=p)
        assert fleet_state.links_tried("node-1", path=p) == ["codex/gpt-5.6-sol"]

    def test_a_corrupt_file_reads_as_no_memory(self, tmp_path: Path) -> None:
        # Fail-open: a corrupt watermark costs one repeated chain link. A raise
        # costs the failover trigger not running at all.
        p = _p(tmp_path)
        p.write_text("{not json", encoding="utf-8")
        assert fleet_state.read_fleet_state(p) == {}
        assert fleet_state.links_tried("node-1", path=p) == []


class TestChainMemory:
    def test_links_accumulate_and_do_not_duplicate(self, tmp_path: Path) -> None:
        p = _p(tmp_path)
        fleet_state.record_link("n1", "codex/sol", path=p)
        fleet_state.record_link("n1", "claude/sonnet", path=p)
        fleet_state.record_link("n1", "codex/sol", path=p)
        assert fleet_state.links_tried("n1", path=p) == ["codex/sol", "claude/sonnet"]

    def test_clear_forgets_one_node_only(self, tmp_path: Path) -> None:
        p = _p(tmp_path)
        fleet_state.record_link("n1", "codex/sol", path=p)
        fleet_state.record_link("n2", "claude/sonnet", path=p)
        fleet_state.clear_node("n1", path=p)
        assert fleet_state.links_tried("n1", path=p) == []
        assert fleet_state.links_tried("n2", path=p) == ["claude/sonnet"]

    def test_the_stored_walk_is_capped(self, tmp_path: Path) -> None:
        p = _p(tmp_path)
        for i in range(40):
            fleet_state.record_link("n1", f"h{i}/m", path=p)
        assert len(fleet_state.links_tried("n1", path=p)) == 16


class TestWalkAgesOut:
    def test_a_walk_older_than_the_ttl_is_forgotten(self, tmp_path: Path) -> None:
        # Without this, one systematic spawn failure burns the chain link by
        # link and the node emits failover_exhausted forever, including long
        # after every provider has recovered.
        p = _p(tmp_path)
        old = time.time() - 25 * 3600
        fleet_state.record_link("n1", "codex/sol", path=p, now=old)
        assert fleet_state.links_tried("n1", path=p) == []

    def test_each_attempt_restamps_the_walk(self, tmp_path: Path) -> None:
        # The TTL measures time since the LAST attempt, so a node still being
        # walked never forgets mid-walk.
        p = _p(tmp_path)
        old = time.time() - 23 * 3600
        fleet_state.record_link("n1", "codex/sol", path=p, now=old)
        fleet_state.record_link("n1", "claude/sonnet", path=p)
        assert fleet_state.links_tried("n1", path=p) == ["codex/sol", "claude/sonnet"]


class TestSilentMemo:

    def test_a_handle_that_recovers_re_arms(self, tmp_path: Path) -> None:
        p = _p(tmp_path)
        fleet_state.set_silent_seen(["w1", "w2"], path=p)
        fleet_state.set_silent_seen(["w2"], path=p)
        assert fleet_state.silent_seen(p) == {"w2"}
