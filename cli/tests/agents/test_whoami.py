"""Tests for the held-node reader `fno.agents.whoami` keeps for the carveout
stamp. The `fno agents whoami` command is native; its contracts are the goldens
under crates/fno-agents/tests/golden/registry_commands.
"""
from __future__ import annotations

from fno.agents import whoami as whoami_mod


SID = "3410f056-d832-480c-9b55-09d1842a39b1"


def _write_manifest(tmp_path, body: str, *, transcript: str = SID):
    fno = tmp_path / ".fno"
    fno.mkdir()
    (fno / "target-state.md").write_text(
        f"---\nclaude_transcript_id: {transcript}\n---\n{body}\n", encoding="utf-8"
    )


class TestFindHeldNode:
    def test_reads_node_when_transcript_id_matches_session(self, tmp_path):
        _write_manifest(tmp_path, "graph_node_id: x-301a")
        assert whoami_mod.find_held_node(str(tmp_path), session_uuid=SID) == "node:x-301a"

    def test_transcript_mismatch_returns_none(self, tmp_path):
        # codex finding 1: the manifest belongs to a DIFFERENT session — never
        # attribute its node to this worker.
        _write_manifest(tmp_path, "graph_node_id: x-999a", transcript="other-session-uuid")
        assert whoami_mod.find_held_node(str(tmp_path), session_uuid=SID) is None


