from __future__ import annotations

import pytest


def test_missing_agents_binary_refuses_without_a_python_fallback(monkeypatch):
    from fno.claims import verdict

    monkeypatch.setattr(verdict, "resolve_binary", lambda: None)

    with pytest.raises(verdict.ClaimVerdictUnavailable, match="FNO_AGENTS_BIN") as exc:
        verdict.claim_verdicts(["node:missing"])

    assert "reinstall" in str(exc.value).lower()
