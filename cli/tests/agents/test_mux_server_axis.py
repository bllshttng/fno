"""Tests for the mux server axis rename (x-f209).

Covers:
  AC11-HP  resolve_mux_session: the FNO_SESSION note prints when FNO_SESSION decided
The gate-escape deduplication case now runs with the native event tests.
"""
from __future__ import annotations



# ---------------------------------------------------------------------------
# AC11-HP: resolve_mux_session precedence and the deprecation note
# ---------------------------------------------------------------------------


def test_ac11_fno_session_alone_warns_once_per_call(monkeypatch, capsys) -> None:
    from fno.agents.mux_spawn import resolve_mux_session

    monkeypatch.setenv("FNO_SESSION", "env-b")
    monkeypatch.delenv("FNO_SERVER", raising=False)
    assert resolve_mux_session(None) == "env-b"
    err = capsys.readouterr().err
    assert err.count("FNO_SESSION is deprecated") == 1
    assert "FNO_SERVER instead" in err
