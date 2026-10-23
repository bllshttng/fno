"""CLI wiring for `fno agents spawn --account <id>` (x-d012, US2).

The four-lane overlay resolution lives in `accounts::env` and
`accounts::launch` (their goldens pin it); this pins the CLI wiring: a resolved
overlay reaches dispatch_spawn_pane as `account_env` and the receipt names the
account (AC1-HP), and a resolver refusal fails closed before any spawn
(AC1-ERR / AC2-ERR).
"""
from __future__ import annotations

import json

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


def test_proven_credential_carrier_overrides_record_derivation(monkeypatch, runner):
    """AC4-HP: the outage handoff's canary proved a SPECIFIC env for this
    account and shipped it through FNO_DISPATCH_ACCOUNT_ENV; the spawn stages
    that env VERBATIM instead of re-deriving from the record, and the receipt
    names the provenance and the staged keys (never their values)."""
    received = _stub_pane_path(monkeypatch)
    from fno.agents import spawn_overlay_client

    carrier = json.dumps({"CLAUDE_CONFIG_DIR": "/proved/account", "AUTH_PROVEN": "1"})
    asked: list = []

    def answer(payload, **_kw):
        asked.append(payload)
        return {
            "env": json.loads(payload["carrier"]),
            "credential_source": "canary-proven carrier",
            "credential_env_keys": ["AUTH_PROVEN", "CLAUDE_CONFIG_DIR"],
        }

    monkeypatch.setattr(spawn_overlay_client, "spawn_overlay_call", answer)
    monkeypatch.setenv("FNO_DISPATCH_ACCOUNT_ENV", carrier)

    from fno.agents.cli import agents_app

    result = runner.invoke(
        agents_app,
        [
            "spawn", "--name", "w1", "hi", "--harness", "claude",
            "--dispatch-account", "makers", "--here", "--as", "pane", "--split", "left",
        ],
    )
    assert result.exit_code == 0, result.output
    assert received["account_env"] == {
        "CLAUDE_CONFIG_DIR": "/proved/account", "AUTH_PROVEN": "1",
    }
    receipt = json.loads(
        next(ln for ln in result.output.splitlines() if ln.startswith("{"))
    )
    assert receipt["credential_source"] == "canary-proven carrier"
    assert receipt["credential_env_keys"] == ["AUTH_PROVEN", "CLAUDE_CONFIG_DIR"]
    # The carrier travels to the Rust owner verbatim, against the spawn's harness.
    assert asked[0]["kind"] == "dispatch-account"
    assert (asked[0]["account"], asked[0]["harness"], asked[0]["carrier"]) == (
        "makers", "claude", carrier,
    )


def test_account_refusal_fails_closed(monkeypatch, runner):
    """AC1-ERR: a resolver refusal exits 2 before any spawn (pane stub untouched)."""
    received = _stub_pane_path(monkeypatch)
    from fno.agents import spawn_overlay_client

    def boom(*a, **k):
        raise spawn_overlay_client.AccountRefused("account 'nope' is not registered.")

    monkeypatch.setattr(spawn_overlay_client, "account_overlay", boom)

    from fno.agents.cli import agents_app

    result = runner.invoke(agents_app, ["spawn", "--name", "w1", "hi", "--account", "nope"])
    assert result.exit_code == 2
    assert "is not registered" in result.output
    assert received == {}  # never reached the pane dispatch


def test_composed_receipt_names_live_credential_and_payer(monkeypatch, runner):
    """AC2-HP (x-8552): a composed spawn's receipt carries auth naming the
    route's vendor and bills naming that vendor, beside account naming the
    profile - all derived from the composed env, never the flags. The cure for
    `--account makers -P zai` reading as "billing makers"."""
    from fno.agents import dispatch, spawn_gate, spawn_overlay_client

    monkeypatch.setenv("ZAI_API_KEY", "zk-live")
    monkeypatch.setattr(spawn_gate, "run_gate", lambda *a, **k: type("G", (), {"release": lambda self: None})())
    monkeypatch.setattr(
        spawn_overlay_client,
        "account_overlay",
        lambda *a, **k: {"account_id": "readyrule", "env": {"CLAUDE_CONFIG_DIR": "/x/.claude"},
                         "lane": "config-dir"},
    )

    def fake_dispatch(**kwargs):
        return dispatch.SpawnResult(
            kind="created",
            name=kwargs["name"],
            provider=kwargs["harness"],
            short_id="abcd1234",
        )

    monkeypatch.setattr("fno.agents.dispatch.dispatch_spawn", fake_dispatch)
    from fno.agents.cli import agents_app

    result = runner.invoke(
        agents_app,
        ["spawn", "--name", "w1", "hi", "--harness", "claude",
         "--account", "readyrule", "--route", "zai,glm-5.2"],
    )
    assert result.exit_code == 0, result.output
    receipt = json.loads(
        next(ln for ln in result.output.splitlines() if '"short_id"' in ln)
    )
    assert receipt["account"] == "readyrule"
    assert receipt["auth"] == "route:zai"
    assert receipt["bills"] == "zai"
    # The same content reaches the interactive surface, picker-shaped.
    assert "profile only" in result.output
    assert "bills zai" in result.output


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


def test_account_non_claude_provider_refused(monkeypatch, runner):
    """--account with a non-claude provider is a user error caught before spawn."""
    received = _stub_pane_path(monkeypatch)
    from fno.agents.cli import agents_app

    result = runner.invoke(
        agents_app, ["spawn", "--name", "w1", "hi", "--harness", "codex", "--account", "x"]
    )
    assert result.exit_code == 2
    assert "claude-only" in result.output
    assert received == {}
