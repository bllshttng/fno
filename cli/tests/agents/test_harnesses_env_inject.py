"""Tests for FNO_AGENT_* env injection on provider subprocess spawn.

Task 2.2 from 2026-05-22-fno-agents-observability.md.

Each provider's create() path must inject:
- FNO_AGENT_SELF=<agent name>
- FNO_AGENT_HARNESS=<provider name>

into the spawned subprocess's environment so nested `fno agents ask`
calls from inside that agent attribute back to this parent via the
caller_kind=nested_agent branch in context.build_context().

FNO_AGENT_SESSION is intentionally omitted on create — the session
id is not known until the subprocess returns. The nested-attribution
path handles its absence as caller_kind=nested_agent + from_session_id=None.
"""
from __future__ import annotations

from pathlib import Path
from typing import Any, Dict

import pytest


# ---------------------------------------------------------------------------
# claude.bg_create
# ---------------------------------------------------------------------------


# ---------------------------------------------------------------------------
# codex.create -> _run_codex
# ---------------------------------------------------------------------------


def test_codex_create_threads_agent_self_to_run_codex(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """codex.create forwards agent_self into _run_codex's spawn env."""
    from fno.agents.harnesses import codex as codex_mod

    captured: Dict[str, Any] = {}

    def fake_popen(argv: list[str], **kw: Any) -> Any:
        captured["argv"] = argv
        captured["env"] = kw.get("env")
        raise codex_mod.CodexInvocationError(1)

    monkeypatch.setattr(codex_mod, "_subprocess_popen", fake_popen)

    output_path = tmp_path / "out.jsonl"
    output_path.touch()

    with pytest.raises(codex_mod.CodexInvocationError):
        codex_mod.create(
            cwd=tmp_path,
            prompt="hello",
            from_name="orchestrator",
            yolo=False,
            output_path=output_path,
            agent_self="beta",
        )

    env = captured["env"]
    assert env is not None
    assert env["FNO_AGENT_SELF"] == "beta"
    assert env["FNO_AGENT_HARNESS"] == "codex"
    assert env["FNO_HARNESS_NAME"] == "codex"
    assert "FNO_HARNESS_SESSION_ID" not in env
    assert "PATH" in env


# ---------------------------------------------------------------------------
# Failure-recovery / degradation contract
# ---------------------------------------------------------------------------


def test_harness_from_env_legacy_window_warns_once(monkeypatch, capsys):
    """AC8: a worker carrying only the pre-cutover FNO_AGENT_PROVIDER resolves,
    uses the value, and emits exactly one stderr line naming FNO_AGENT_HARNESS."""
    import fno.harness_identity as hi

    monkeypatch.setattr(hi, "_HARNESS_ENV_WARNED", False)
    env = {"FNO_AGENT_PROVIDER": "codex"}
    assert hi.harness_from_env(env) == "codex"
    err1 = capsys.readouterr().err
    assert "FNO_AGENT_PROVIDER is the pre-cutover name for FNO_AGENT_HARNESS" in err1
    # A second resolution in the same process stays quiet (warn fires once).
    assert hi.harness_from_env(env) == "codex"
    err2 = capsys.readouterr().err
    assert err2 == ""
