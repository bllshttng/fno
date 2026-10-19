"""Tests for `fno agents whoami` — a mesh worker learns its own registered name.

Backlog node x-301a. Covers the pure-logic resolver (`fno.agents.whoami`),
the CLI wiring (`cmd_whoami` via the agents app), and the read-only invariant
(paired-state md5, mirroring `fno whoami` / `fno status`). The `cli/tests/agents`
conftest auto-forces `FNO_AGENTS_RUNTIME=python` so `runner.invoke` stays on the
Python dispatch instead of exec-ing an installed binary.
"""
from __future__ import annotations

import json

import pytest
from typer.testing import CliRunner

from fno.agents.cli import agents_app
from fno.agents.registry import AgentEntry, write_registry
from fno.agents import whoami as whoami_mod
from fno.paths_testing import use_tmpdir


def _claude(**kw) -> AgentEntry:
    base = dict(
        name="spawn-x-301a-whoami",
        harness="claude",
        cwd="/Users/foo/code/proj",
        log_path="/Users/foo/.fno/agents/spawn-x-301a-whoami/output.jsonl",
        short_id="4a1f9c2b",
        created_at="2026-06-16T17:00:00Z",
        status="live",
    )
    base.update(kw)
    return AgentEntry(**base)


@pytest.fixture
def runner() -> CliRunner:
    return CliRunner()


# --- pure-logic resolver -------------------------------------------------


class TestResolveSelf:


    def test_env_session_miss_falls_through_to_session_uuid(self):
        # A stale FNO_AGENT_SESSION never blocks the CLAUDE uuid tier.
        reg = [_claude(harness_session_id="11111111-2222-3333-4444-555555555555")]
        result = whoami_mod.resolve_self(
            env={"FNO_AGENT_SESSION": "s-stale"},
            registry=reg,
            session_uuid="11111111-2222-3333-4444-555555555555",
        )
        assert result.registered is True
        assert result.resolved_via == "session-fallback"


    def test_session_fallback_matches_short_id_prefix_of_uuid(self):
        # codex P2: an older claude row may carry ONLY the 8-hex short id (a
        # 32-bit prefix of the full session UUID). The full CLAUDE_CODE_SESSION_ID
        # must still resolve it on the fallback path.
        reg = [_claude(short_id="3410f056", harness_session_id=None)]
        result = whoami_mod.resolve_self(
            env={},
            registry=reg,
            session_uuid="3410f056-d832-480c-9b55-09d1842a39b1",
        )
        assert result.registered is True
        assert result.name == "spawn-x-301a-whoami"
        assert result.resolved_via == "session-fallback"

    def test_session_fallback_prefers_exact_full_id_over_prefix(self):
        # The exact full-id pass runs across every row first, so a shared 8-hex
        # prefix never steals a real full-UUID match.
        prefix_row = _claude(name="prefix-collision", short_id="3410f056",
                             harness_session_id=None)
        exact_row = _claude(name="exact-match", short_id="ffffffff",
                            harness_session_id="3410f056-d832-480c-9b55-09d1842a39b1")
        result = whoami_mod.resolve_self(
            env={},
            registry=[prefix_row, exact_row],
            session_uuid="3410f056-d832-480c-9b55-09d1842a39b1",
        )
        assert result.name == "exact-match"


# --- CLI wiring ----------------------------------------------------------


class TestWhoamiCLI:
    def test_ac1_hp_registered_worker_learns_name(self, tmp_path, runner, monkeypatch):
        use_tmpdir(monkeypatch, tmp_path)
        write_registry([_claude(harness_session_id="s-cli")])
        monkeypatch.setenv("FNO_AGENT_SESSION", "s-cli")
        monkeypatch.setenv("FNO_AGENT_HARNESS", "claude")
        result = runner.invoke(agents_app, ["whoami"])
        assert result.exit_code == 0, result.stdout + result.stderr
        # JSON is emitted under CliRunner (non-TTY); assert on the parsed shape.
        payload = json.loads(result.stdout)
        assert payload["name"] == "spawn-x-301a-whoami"
        assert payload["provider"] == "claude"
        assert payload["short_id"] == "4a1f9c2b"

    def test_ac1_err_corrupt_registry_no_traceback(self, tmp_path, runner, monkeypatch):
        use_tmpdir(monkeypatch, tmp_path)
        from fno import paths

        reg_path = paths.agents_registry_path()
        reg_path.parent.mkdir(parents=True, exist_ok=True)
        reg_path.write_text("{ this is not valid json", encoding="utf-8")
        monkeypatch.setenv("FNO_AGENT_SESSION", "s-cli")
        result = runner.invoke(agents_app, ["whoami"])
        assert result.exit_code == 3, result.stdout + result.stderr
        assert "Traceback" not in (result.stdout + result.stderr)
        assert "WARN" in result.stderr


    def test_ambiguous_harness_markers_refuse_with_distinct_exit(self, tmp_path, runner, monkeypatch):
        use_tmpdir(monkeypatch, tmp_path)
        monkeypatch.delenv("FNO_AGENT_SESSION", raising=False)
        monkeypatch.setenv("CODEX_THREAD_ID", "01a02125-aaaa-bbbb-cccc-dddddddddddd")
        monkeypatch.setenv("CLAUDE_CODE_SESSION_ID", "2782a6e1-aaaa-bbbb-cccc-dddddddddddd")
        monkeypatch.setattr(
            "fno.claims.session_pid.resolve_session_harness",
            lambda from_pid=None: None,
        )
        result = runner.invoke(agents_app, ["whoami", "--json"])
        assert result.exit_code == 4
        assert "cannot decide which session is 'self'" in result.stderr
        assert "CODEX_THREAD_ID" in result.stderr
        assert "resolve with: find ~/.codex/sessions ~/.claude/projects" in result.stderr

    def test_proven_claude_identity_beats_inherited_codex_marker(self, tmp_path, runner, monkeypatch):
        use_tmpdir(monkeypatch, tmp_path)
        claude_sid = "2782a6e1-aaaa-bbbb-cccc-dddddddddddd"
        write_registry([_claude(harness_session_id=claude_sid)])
        monkeypatch.delenv("FNO_AGENT_SESSION", raising=False)
        monkeypatch.setenv("CODEX_THREAD_ID", "01a02125-aaaa-bbbb-cccc-dddddddddddd")
        monkeypatch.setenv("CLAUDE_CODE_SESSION_ID", claude_sid)
        monkeypatch.setattr(
            "fno.claims.session_pid.resolve_session_harness",
            lambda from_pid=None: "claude",
        )
        result = runner.invoke(agents_app, ["whoami", "--json"])
        assert result.exit_code == 0, result.stdout + result.stderr
        payload = json.loads(result.stdout)
        assert payload["name"] == "spawn-x-301a-whoami"
        assert payload["provider"] == "claude"
        assert payload["resolved_via"] == "session-fallback"
        assert "01a02125" not in result.stdout

    def test_live_status_shellout_warning_surfaced(self, tmp_path, runner, monkeypatch):
        # codex finding 2: claude_agents_json returns ({}, [warns]) WITHOUT
        # raising on a shellout failure, so the warning must be forwarded to
        # stderr (not silently dropped) even though live_status degrades to null.
        use_tmpdir(monkeypatch, tmp_path)
        write_registry([_claude(harness_session_id="s-cli")])
        monkeypatch.setenv("FNO_AGENT_SESSION", "s-cli")
        monkeypatch.setenv("FNO_AGENT_HARNESS", "claude")
        from fno.agents.harnesses import claude as claude_mod

        monkeypatch.setattr(
            claude_mod, "claude_agents_json",
            lambda *a, **k: ({}, ["claude agents --json timed out"]),
        )
        result = runner.invoke(agents_app, ["whoami", "--json"])
        assert result.exit_code == 0, result.stdout + result.stderr
        assert json.loads(result.stdout)["live_status"] is None
        assert "claude agents --json timed out" in result.stderr


# --- find_held_node ------------------------------------------------------


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


