"""Tests for the mux server axis rename (x-f209).

Covers:
  AC11-HP  resolve_mux_session: the FNO_SESSION note prints when FNO_SESSION decided
  AC12-HP  pane-identity accepts --server, --session-id and --session; two
           spellings exit 2
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


# ---------------------------------------------------------------------------
# AC12-HP: pane-identity flag aliases through the real Typer runner
# ---------------------------------------------------------------------------


def _invoke_pane_identity(monkeypatch, argv: list[str]):
    from typer.testing import CliRunner

    from fno.agents.cli import agents_app

    monkeypatch.setenv("FNO_MUX_SUBPROCESS_TIMEOUT_S", "5")
    runner = CliRunner()
    return runner.invoke(agents_app, ["pane-identity", *argv])


def test_ac12_pane_identity_server_and_aliases_reach_the_resolver(
    monkeypatch,
) -> None:
    seen: list = []

    def fake_resolve(explicit=None):
        seen.append(explicit)
        return "irrelevant"

    def fake_run_mux(args, runner):
        class R:
            returncode = 1
            stdout = ""
            stderr = "no mux in unit test"

        return R()

    monkeypatch.setattr("fno.agents.mux_spawn.resolve_mux_session", fake_resolve)
    monkeypatch.setattr("fno.agents.mux_spawn._run_mux", fake_run_mux)

    for argv in (["--server", "s"], ["--session-id", "s"], ["--session", "s"]):
        seen.clear()
        _invoke_pane_identity(monkeypatch, argv)
        assert seen == ["s"], f"{argv} resolved to {seen}"

    for argv in (["--server", "a", "--session-id", "b"], ["--server", "a", "--session", "b"]):
        result = _invoke_pane_identity(monkeypatch, argv)
        assert result.exit_code == 2, f"{argv} must refuse both: {result.output}"
