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




# ---------------------------------------------------------------------------
# claude.bg_create
# ---------------------------------------------------------------------------


# ---------------------------------------------------------------------------
# codex.create -> _run_codex
# ---------------------------------------------------------------------------


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
