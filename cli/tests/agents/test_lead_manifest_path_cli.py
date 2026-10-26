"""`fno agents lead init` refuses a path-unsafe scope by name.

`manifest-path` answers natively now (`fno-agents lead-state --manifest-path`,
pinned by the lead_reads goldens). `init` still shares the scope-to-path
refusal in `lead_manifest_path`.
"""
from __future__ import annotations

import pytest
from typer.testing import CliRunner

from fno.paths_testing import use_tmpdir

CALLER_SESSION = "0c1f2f9a-2222-4000-8000-000000000002"


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
