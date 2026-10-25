"""The caller ladder `fno agents org chart term` still runs in Python.

`shape` answers natively now (`fno-agents lead-shape --user-verb`, pinned by
the lead_reads goldens). `_own_role_argv` stays for `term` until the lifecycle
verbs port.
"""
from __future__ import annotations

import types
from pathlib import Path

import pytest

from fno.paths_testing import use_tmpdir

CALLER_SESSION = "5d4c3b2a-1111-4000-8000-000000000001"
SCOPE = "epic-x"


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


def test_own_role_argv_keys_the_root_on_the_caller_row_cwd(tmp_path, monkeypatch) -> None:
    """--root must name the caller row's space, so a lead whose shell sits
    outside the repo still declares on its own manifest (x-8387)."""
    import fno.agents.role as role_mod
    from fno.lead.cli import _own_role_argv
    from fno.lead.state import lead_state_root as _ksr

    use_tmpdir(monkeypatch, tmp_path)
    kingrepo = tmp_path / "kingrepo"
    kingrepo.mkdir()
    row = types.SimpleNamespace(
        harness_session_id=CALLER_SESSION,
        cc_session_id=None,
        harness="claude",
        role_scope=SCOPE,
        cwd=str(kingrepo),
    )
    monkeypatch.setattr(role_mod, "calling_agent_row", lambda: row)
    monkeypatch.setattr("fno.rust_binary.resolve_binary", lambda: Path("/fake/fno-agents"))
    elsewhere = tmp_path / "elsewhere"
    elsewhere.mkdir()
    monkeypatch.chdir(elsewhere)

    argv, own = _own_role_argv("lead-shape", "")

    assert own == SCOPE
    i = argv.index("--root")
    assert argv[i + 1] == str(_ksr(kingrepo)), argv
