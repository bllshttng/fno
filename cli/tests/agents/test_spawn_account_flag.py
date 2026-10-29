"""CLI wiring for `fno agents spawn --account <id>` (x-d012, US2).

The four-lane overlay resolution lives in `accounts::env` and
`accounts::launch` (their goldens pin it); this pins the CLI wiring: a resolved
overlay reaches dispatch_spawn_pane as `account_env` and the receipt names the
account (AC1-HP), and a resolver refusal fails closed before any spawn
(AC1-ERR / AC2-ERR).
"""
from __future__ import annotations


import pytest
from typer.testing import CliRunner


@pytest.fixture
def runner() -> CliRunner:
    return CliRunner()


@pytest.fixture(autouse=True)
def _no_harness_markers(monkeypatch):
    for m in ("CODEX_THREAD_ID", "CLAUDE_CODE_SESSION_ID", "CODEX_SESSION_ID"):
        monkeypatch.delenv(m, raising=False)


def _stub_pane_path(monkeypatch) -> dict:
    received: dict = {}
    from fno.agents import mux_spawn, spawn_gate

    class _Gate:
        def release(self) -> None:
            pass

    monkeypatch.setattr(spawn_gate, "run_gate", lambda *a, **k: _Gate())
    monkeypatch.setattr(mux_spawn, "resolve_provenance", lambda *a, **k: None)

    def fake_pane(**kwargs):
        received.update(kwargs)
        return mux_spawn.MuxSpawnResult(
            name=kwargs["name"], provider=kwargs["provider"], session="s",
            pane_id=1, child_pid=None, session_uuid=None,
        )

    monkeypatch.setattr(mux_spawn, "dispatch_spawn_bounded_pane", fake_pane)
    return received


def test_codex_one_shot_refuses_a_pinned_account(monkeypatch, tmp_path):
    """A lane that cannot carry the overlay refuses instead of launching.

    _codex_create_path takes no account_env, and cmd_spawn exports only
    PROVENANCE_KEYS to os.environ for one-shots, so a resolved overlay would
    reach nothing here. Launching anyway would bill the ambient account
    silently, which is the failure both account flags are fail-closed against.
    """
    from fno.agents import dispatch as dsp

    def boom(**kw):
        pytest.fail("must not create a codex agent on the ambient account")

    monkeypatch.setattr(dsp, "_codex_create_path", boom)

    with pytest.raises(dsp.DispatchAskError) as exc:
        dsp.dispatch_spawn(
            name="t-once",
            message="hi",
            harness="codex",
            cwd=tmp_path,
            once=True,
            account_env={"HOME": "/accounts/zai-1/home"},
        )
    assert "one-shot" in str(exc.value)
    assert "drop --once" in str(exc.value)
