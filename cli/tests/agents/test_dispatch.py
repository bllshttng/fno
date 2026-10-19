"""Tests for fno.agents.dispatch provider selection and reconcile.

dispatch.select_provider() honors the registry provider field and refuses a
mismatch on follow-up; reconcile skips a retired provider's row.
"""
from __future__ import annotations

from pathlib import Path

import pytest

from fno.paths_testing import use_tmpdir


# ---------------------------------------------------------------------------
# dispatch.select_provider — registry mismatch rejection
# ---------------------------------------------------------------------------


def test_select_provider_rejects_mismatch_on_follow_up(tmp_path: Path, monkeypatch) -> None:
    """When requested_provider disagrees with the recorded provider, raise ProviderMismatchError."""
    use_tmpdir(monkeypatch, tmp_path)
    from fno.agents.dispatch import ProviderMismatchError, select_provider
    from fno.agents.registry import AgentEntry, write_registry

    write_registry(
        [AgentEntry(name="locked", harness="claude", cwd="/tmp", log_path="/tmp/y.log")]
    )

    with pytest.raises(ProviderMismatchError) as exc_info:
        select_provider(name="locked", requested_provider="codex")
    msg = str(exc_info.value)
    assert "locked" in msg
    assert "claude" in msg  # recorded
    assert "codex" in msg  # requested


def test_select_provider_requires_provider_for_new_agent(tmp_path: Path, monkeypatch) -> None:
    """A first-time ask with no requested_provider has nothing to select — raise ValueError."""
    use_tmpdir(monkeypatch, tmp_path)
    from fno.agents.dispatch import select_provider

    with pytest.raises(ValueError, match="provider is required for new agent"):
        select_provider(name="brand-new", requested_provider=None)


def test_reconcile_warns_and_skips_retired_gemini_row(
    tmp_path: Path, monkeypatch, capsys
) -> None:
    """A legacy Gemini registry row remains readable but is not probed."""
    use_tmpdir(monkeypatch, tmp_path)
    from fno.agents.dispatch import reconcile_agents
    from fno.agents.registry import AgentEntry, write_registry

    write_registry(
        [
            AgentEntry(
                name="legacy-gemini",
                harness="gemini",
                cwd="/tmp",
                log_path="/tmp/gemini.log",
                harness_session_id="legacy-session",
                status="live",
            )
        ]
    )

    result = reconcile_agents()

    assert result.scanned == 1
    assert result.errors == [
            {
                "name": "legacy-gemini",
                "provider": None,
                "harness": "gemini",
                "id": "legacy-session",
            "reason": "retired-provider",
        }
    ]
    assert "retired provider 'gemini'" in capsys.readouterr().err


def test_select_provider_rejects_unknown_provider(tmp_path: Path, monkeypatch) -> None:
    """select_provider rejects an unsupported provider name."""
    use_tmpdir(monkeypatch, tmp_path)
    from fno.agents.dispatch import select_provider

    with pytest.raises(ValueError, match="unknown provider"):
        select_provider(name="x", requested_provider="not-real")
