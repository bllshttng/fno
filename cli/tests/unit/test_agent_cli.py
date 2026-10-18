"""Tests for the relocated self-introspection commands: fno whoami / fno status.

Covers the operating-stack summary (whoami) and gate/events view (status),
plus the read-only invariant (paired-state hash diff). These were formerly
`fno agent whoami` / `fno agent status`; the `fno agent` (singular) namespace
was retired in ab-12dd2a5d (suggest/capabilities trimmed). Commands are invoked
through the real top-level `fno` app so the lazy registration is exercised too.
"""
from __future__ import annotations

import hashlib
import json
from pathlib import Path
from typing import List

import pytest
from typer.testing import CliRunner

from fno.cli import app
from fno.paths_testing import use_tmpdir

FIXTURES = Path(__file__).parent / "fixtures" / "agent"


def _file_hash(path: Path) -> str:
    return hashlib.md5(path.read_bytes()).hexdigest() if path.exists() else "(absent)"


def _clear_markers(monkeypatch) -> None:
    from fno.harness_identity import HARNESS_SESSION_MARKERS

    for marker, _ in HARNESS_SESSION_MARKERS:
        monkeypatch.delenv(marker, raising=False)
    monkeypatch.delenv("FNO_AGENT_SELF", raising=False)


def _only_marker(monkeypatch, marker: str, value: str) -> None:
    """Resolve ambient identity to exactly `marker`, clearing higher-precedence
    markers (and FNO_AGENT_SELF) so the test is deterministic on any host."""
    _clear_markers(monkeypatch)
    monkeypatch.setenv(marker, value)


def _make_workspace(tmp_path: Path, *, target: bool = False, walker: bool = False,
                    fleet: bool = False, malformed: bool = False) -> Path:
    project = tmp_path / "project"
    fno = project / ".fno"
    fno.mkdir(parents=True)
    if target and not malformed:
        (fno / "target-state.md").write_text(
            (FIXTURES / "target-state.md").read_text()
        )
    if malformed:
        (fno / "target-state.md").write_text(
            (FIXTURES / "malformed-state.md").read_text()
        )
    if walker:
        (fno / "megawalk-state.md").write_text(
            (FIXTURES / "megawalk-state.md").read_text()
        )
    if fleet:
        fleet_root = tmp_path / "fake_home" / ".fno" / "fleet" / "fleet-fixture-001"
        fleet_root.mkdir(parents=True)
        body = (FIXTURES / "fleet-mission.md").read_text().replace(
            "__PROJECT_ROOT__", str(project.resolve())
        )
        (fleet_root / "00-INDEX.md").write_text(body)
    return project


@pytest.fixture
def runner() -> CliRunner:
    return CliRunner()


def _state_files(project: Path) -> List[Path]:
    return [
        project / ".fno" / "target-state.md",
        project / ".fno" / "megawalk-state.md",
        project / ".fno" / "session-state.md",
    ]


def _state_hashes(project: Path) -> dict:
    return {p.name: _file_hash(p) for p in _state_files(project)}


def _invoke(runner: CliRunner, project: Path, monkeypatch, *args, env_home: Path = None):
    """Invoke the real top-level `fno` app. `args` is the verb followed by any
    command-level flags (flags come AFTER the verb now that the options live on
    the command, not on a group callback)."""
    monkeypatch.chdir(project)
    if env_home is not None:
        monkeypatch.setenv("HOME", str(env_home))
    return runner.invoke(app, list(args))


# --- whoami --------------------------------------------------------------


class TestWhoami:
    def test_ac1_hp_full_stack_visible(self, tmp_path, runner, monkeypatch):
        project = _make_workspace(tmp_path, target=True, walker=True, fleet=True)
        result = _invoke(
            runner, project, monkeypatch, "whoami",
            env_home=tmp_path / "fake_home",
        )
        assert result.exit_code == 0, result.stdout + result.stderr
        out = result.stdout
        assert "project:" in out
        assert "fleet-fixture-001" in out
        assert "walker:" in out
        assert "20260512T010101Z-99999-fixaaa" in out  # target session id
        assert "harness:" in out
        assert "provider:" not in out

    def test_ac2_err_malformed_target_state_fails_fast(self, tmp_path, runner, monkeypatch):
        project = _make_workspace(tmp_path, malformed=True)
        result = _invoke(runner, project, monkeypatch, "whoami")
        assert result.exit_code == 2
        assert "target-state.md" in result.stderr
        assert "malformed" in result.stderr.lower()


    def test_positive_manifest_session_mismatch_reports_visitor(
        self, tmp_path, runner, monkeypatch
    ):
        project = _make_workspace(tmp_path, target=True)
        manifest = project / ".fno" / "target-state.md"
        manifest.write_text(
            "---\n"
            "session_id: resident-run\n"
            "harness_session_id: resident-session\n"
            "input: resident-work\n"
            "plan_path: /tmp/resident-plan.md\n"
            "---\n",
            encoding="utf-8",
        )
        _only_marker(monkeypatch, "CLAUDE_CODE_SESSION_ID", "visitor-session")

        result = _invoke(runner, project, monkeypatch, "whoami")

        assert result.exit_code == 0, result.stdout + result.stderr
        assert "visitor: manifest names session resident-session, not this one" in result.stdout
        assert "run:" not in result.stdout
        payload = json.loads(
            _invoke(runner, project, monkeypatch, "whoami", "--json").stdout
        )
        assert payload["session"] is None
        assert payload["visitor"] == "manifest names session resident-session, not this one"

    # --- x-5ee2 US1: mail handle + run: relabel -------------------------------

    def test_ac1_hp_mail_handle_surfaced(self, tmp_path, runner, monkeypatch):
        """With an ambient harness session id, whoami prints the canonical reply
        handle (human `mail:` line + additive JSON keys), and it equals the value
        `stamp_from(None)` self-stamps on a name-lane send."""
        from fno.agents.self_stamp import stamp_from

        _only_marker(monkeypatch, "CLAUDE_CODE_SESSION_ID", "879d8d26-2505-4977-9b87-000000000000")
        project = _make_workspace(tmp_path, target=True)
        result = _invoke(runner, project, monkeypatch, "whoami")
        assert result.exit_code == 0, result.stdout + result.stderr
        # AC1-UI: the bare short-id only - no harness-prefixed form anywhere on
        # the line, including a parenthetical "legacy" annotation.
        assert "mail:     879d8d26  (reply handle" in result.stdout
        assert "claude-879d8d26" not in result.stdout
        # the run: relabel replaced the session-misnomer human line
        assert "run:" in result.stdout and "session:" not in result.stdout
        payload = json.loads(
            _invoke(runner, project, monkeypatch, "whoami", "--json").stdout
        )
        assert payload["mail_handle"] == "879d8d26" == stamp_from(None)
        assert payload["harness_session_id"] == "879d8d26-2505-4977-9b87-000000000000"

    # --- x-730d: dead-letterbox visibility (unread count) --------------------

    def _isolate_bus(self, tmp_path, monkeypatch):
        """Point the bus + md render at tmp so seeded mail is the only mail."""
        monkeypatch.delenv("FNO_BUS_DIR", raising=False)
        monkeypatch.setenv("FNO_INBOX_ROOT", str(tmp_path))
        use_tmpdir(monkeypatch, tmp_path)

    def test_mail_unread_count_surfaced(self, tmp_path, runner, monkeypatch):
        """Unread bus mail addressed to this session's handle shows as a
        `mail: N unread` line and a `mail_unread` JSON key."""
        from fno.inbox.store import write_new_thread

        _only_marker(monkeypatch, "CLAUDE_CODE_SESSION_ID", "879d8d26-2505-4977-9b87-000000000000")
        self._isolate_bus(tmp_path, monkeypatch)
        for _ in range(3):
            write_new_thread(
                recipient="879d8d26", sender="etl", kind="send",
                body="ping", to_kind="name",
            )
        project = _make_workspace(tmp_path, target=True)
        result = _invoke(runner, project, monkeypatch, "whoami")
        assert result.exit_code == 0, result.stdout + result.stderr
        # A distinct label, not a second `mail:` line (the handle line stays
        # unambiguous for --from-name copying).
        assert "mail_unread: 3" in result.stdout
        assert result.stdout.count("mail:") == 1
        payload = json.loads(
            _invoke(runner, project, monkeypatch, "whoami", "--json").stdout
        )
        assert payload["mail_unread"] == 3


# --- status --------------------------------------------------------------


class TestStatus:
    def test_ac1_hp_gate_satisfaction_reported(self, tmp_path, runner, monkeypatch):
        project = _make_workspace(tmp_path, target=True)
        result = _invoke(runner, project, monkeypatch, "status")
        assert result.exit_code == 0, result.stdout + result.stderr
        assert "gates:" in result.stdout
        assert "quality_check_passed" in result.stdout

    def test_ac3_ui_inconsistencies_flagged_inline(self, tmp_path, runner, monkeypatch):
        # target fixture has pr_number=1234 + external_review_passed missing/false.
        project = _make_workspace(tmp_path, target=True)
        # Force external_review_passed: false explicitly to trigger the rule.
        state_path = project / ".fno" / "target-state.md"
        state_path.write_text(
            "---\n"
            "status: IN_PROGRESS\n"
            "session_id: 20260512T010101Z-99999-fixaaa\n"
            "current_phase: external\n"
            "pr_number: 1234\n"
            "external_review_passed: false\n"
            "---\n# fixture\n"
        )
        result = _invoke(runner, project, monkeypatch, "status")
        assert result.exit_code == 0
        assert "inconsistencies:" in result.stdout
        assert "WARNING" in result.stdout
        assert "/fno:ship pr check" in result.stdout


    def test_ac5_fr_mid_append_event_skipped_silently(self, tmp_path, runner, monkeypatch):
        project = _make_workspace(tmp_path, target=True)
        events = project / ".fno" / "events.jsonl"
        events.write_text(
            json.dumps({"ts": "T1", "type": "phase_init"}) + "\n"
            + json.dumps({"ts": "T2", "type": "phase_transition"}) + "\n"
            + '{"ts":"T3","type":"phase_truncated_marker_xyz'  # partial line
        )
        result = _invoke(runner, project, monkeypatch, "status")
        assert result.exit_code == 0
        assert "phase_init" in result.stdout
        assert "phase_transition" in result.stdout
        # Critical: the partial line must NOT appear in any form. The
        # marker is distinctive enough that an accidental render would
        # surface it.
        assert "phase_truncated_marker_xyz" not in result.stdout


# --- read-only invariant -------------------------------------------------


class TestReadOnlyInvariant:
    """Tests that neither command mutates state files (the design's hard rule)."""

    @pytest.mark.parametrize("verb", ["whoami"])
    def test_paired_state_hash_unchanged(
        self, tmp_path, runner, monkeypatch, verb
    ):
        project = _make_workspace(tmp_path, target=True, walker=True)
        before = _state_hashes(project)
        for _ in range(3):  # repeated invocation should still not mutate
            result = _invoke(runner, project, monkeypatch, verb)
            assert result.exit_code in (0, 2), result.stdout + result.stderr
        after = _state_hashes(project)
        assert before == after, f"{verb} mutated state: {before} -> {after}"


# --- model resolution: "what model am I actually running?" -------------------
#
# The parsing itself belongs to fno.agents.self_stamp (harness-aware, and it
# already rejects sidechain / non-assistant rows). These cover the seam whoami
# adds on top: surface it when known, stay silent when not, on EVERY harness.


def _codex_rollout(home: Path, session_id: str, models: List[str]) -> None:
    home.mkdir(parents=True, exist_ok=True)
    home.joinpath(f"rollout-{session_id}.jsonl").write_text(
        "".join(
            json.dumps({"type": "turn_context", "payload": {"model": m}}) + "\n" for m in models
        ),
        encoding="utf-8",
    )


def _only_harness(monkeypatch, marker: str, session: str) -> None:
    for m in ("CLAUDE_CODE_SESSION_ID", "CODEX_THREAD_ID", "CODEX_SESSION_ID", "GEMINI_SESSION_ID"):
        monkeypatch.delenv(m, raising=False)
    monkeypatch.setenv(marker, session)


def test_session_model_resolves_for_a_codex_session(tmp_path: Path, monkeypatch) -> None:
    """Codex records the model in turn_context.payload, not message.model. A
    claude-shaped parser reported null here even though the model was present."""
    from fno.agent import cli as agent_cli

    _only_harness(monkeypatch, "CODEX_THREAD_ID", "sid-x")
    monkeypatch.setenv("FNO_CODEX_SESSIONS_DIR", str(tmp_path / "codex"))
    _codex_rollout(tmp_path / "codex", "sid-x", ["gpt-5.6-luna"])
    assert agent_cli._session_model() == "gpt-5.6-luna"
