"""Tests for the mux server axis rename (x-f209).

Covers:
  AC11-HP  resolve_mux_session: the FNO_SESSION note prints when FNO_SESSION decided
  AC17-HP  default_dedup_key with only FNO_SERVER equals the FNO_SESSION key
"""
from __future__ import annotations

from fno.events.gate_escape import default_dedup_key


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


# ---------------------------------------------------------------------------
# AC17-HP: the gate-escape dedup key reads both variables identically
# ---------------------------------------------------------------------------


def test_ac17_dedup_key_treats_both_vars_as_one_axis() -> None:
    via_server = default_dedup_key("bypass", {"FNO_SERVER": "s"})
    via_session = default_dedup_key("bypass", {"FNO_SESSION": "s"})
    assert via_server == via_session
    assert ":s:" in via_server
    # The pid fallback still works when neither variable is set.
    via_pid = default_dedup_key("bypass", {"FNO_SESSION_PID": "4242"})
    assert ":4242:" in via_pid
