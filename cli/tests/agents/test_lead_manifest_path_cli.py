"""`fno agents lead manifest-path` resolves a live role to its manifest.

The stop hook calls the deprecated `fno lead manifest-path` spelling, which
verb_moves forwards onto the agents app. The verb missed the agents fold, so
the resolver exited 2 and every stop on an active leads dir burned its
unavailable-retries before allowing exit. These tests pin the verb onto the
agents app and pin the deprecated spelling onto the same command.
"""
from __future__ import annotations

from pathlib import Path

import pytest
from typer.testing import CliRunner

from fno.agents.registry import AgentEntry, update_registry
from fno.lead.state import lead_manifest_path, write_manifest
from fno.paths_testing import use_tmpdir

CALLER_SESSION = "0c1f2f9a-2222-4000-8000-000000000002"
SCOPE = "epic-y"


@pytest.fixture(autouse=True)
def _clear_parent_markers(monkeypatch):
    for marker in (
        "FNO_SESSION",
        "CODEX_THREAD_ID",
        "CLAUDE_CODE_SESSION_ID",
        "CODEX_SESSION_ID",
        "GEMINI_SESSION_ID",
    ):
        monkeypatch.delenv(marker, raising=False)


@pytest.fixture
def team(tmp_path, monkeypatch):
    use_tmpdir(monkeypatch, tmp_path)
    monkeypatch.chdir(tmp_path)
    monkeypatch.setattr(
        "fno.rust_binary.call_binary_json", lambda *a, **k: (None, {"ready": True})
    )
    return tmp_path


def _seat_role():
    update_registry(
        lambda rows: rows
        + [
            AgentEntry(
                name="promoted-lead",
                cwd="/tmp",
                log_path="",
                harness="claude",
                harness_session_id=CALLER_SESSION,
                status="busy",
                role_level=2,
                role_scope=SCOPE,
                role_grantor="human",
            )
        ]
    )
    return lead_manifest_path(SCOPE, state_root=Path(".fno"))


def _manifest_path(*args: str):
    from fno.lead.cli import agents_lead_app

    return CliRunner().invoke(
        agents_lead_app, ["manifest-path", *args]
    )


def test_manifest_path_keys_on_the_role_row_cwd(team, monkeypatch) -> None:
    """The writer arms under the role row's cwd space; the reader must key
    the same row. A lead whose shell sits outside the repo still resolves its
    manifest with no --state-root (x-8387)."""
    from fno.lead.state import lead_state_root

    kingrepo = team / "kingrepo"
    kingrepo.mkdir()
    assert lead_state_root(kingrepo) != lead_state_root(), (
        "positive control: the row cwd and the shell cwd must key different spaces"
    )
    update_registry(
        lambda rows: rows
        + [
            AgentEntry(
                name="promoted-lead",
                cwd=str(kingrepo),
                log_path="",
                harness="claude",
                harness_session_id=CALLER_SESSION,
                status="busy",
                role_level=2,
                role_scope=SCOPE,
                role_grantor="human",
            )
        ]
    )
    manifest = lead_manifest_path(SCOPE, state_root=lead_state_root(kingrepo))
    write_manifest(manifest, scope=SCOPE, harness_session_id=CALLER_SESSION)

    elsewhere = team / "elsewhere"
    elsewhere.mkdir()
    monkeypatch.chdir(elsewhere)

    result = _manifest_path("--harness-session-id", CALLER_SESSION)

    assert result.exit_code == 0, result.output
    assert result.stdout.strip() == str(manifest)


def test_manifest_path_frees_a_stranger(team) -> None:
    """No registry row names this session: exit 1, the hook's "stranger goes
    free" contract, never the exit-2 parse failure the missing verb produced."""
    result = _manifest_path(
        "--harness-session-id", CALLER_SESSION,
        "--state-root", str(team / ".fno"),
    )

    assert result.exit_code == 1, result.output
    assert result.stdout.strip() == "", "no path prints on a miss"
    assert "lead manifest-path:" in (result.stderr or ""), (
        "the reason lands on stderr; the old bare silence named no cause"
    )


def test_deprecated_lead_spelling_forwards_onto_the_agents_app(team) -> None:
    """The stop hook's literal argv: `fno lead manifest-path ...`. The banner
    must stay on stderr so the hook's stdout capture reads a clean path."""
    from fno.cli import app

    manifest = _seat_role()
    write_manifest(manifest, scope=SCOPE, harness_session_id=CALLER_SESSION)

    result = CliRunner().invoke(
        app,
        [
            "lead", "manifest-path",
            "--harness-session-id", CALLER_SESSION,
            "--state-root", str(team / ".fno"),
        ],
    )

    assert result.exit_code == 0, result.output
    assert result.stdout.strip() == str(team / ".fno" / "leads" / f"{SCOPE}.md"), (
        "stdout carries exactly the path, one line"
    )
    assert str(manifest) in result.output
    assert "is now" in (result.stderr or ""), "the rename banner names the new spelling"


def test_lead_init_refuses_a_path_unsafe_scope_without_a_traceback(
    team, monkeypatch
) -> None:
    """The path-safety refusal in lead_manifest_path is an operator typo
    ('a/b'), so it must surface as a named refusal at exit 2, not as a
    ValueError traceback."""
    import fno.lead.state as lead_state
    from fno.cli import app

    monkeypatch.setattr(lead_state, "lead_loop_enabled", lambda: True)
    monkeypatch.setattr(lead_state, "lead_state_root", lambda: team / ".fno")

    result = CliRunner().invoke(
        app,
        ["lead", "init", "--scope", "a/b", "--harness-session-id", CALLER_SESSION],
    )

    assert result.exit_code == 2, result.output
    assert "unsafe lead scope" in result.output
    assert not isinstance(result.exception, ValueError)
