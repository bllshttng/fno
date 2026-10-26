"""x-e106: `fno config set pr_watch.enabled` couples to the launchd agent.

Setting enabled=true installs+loads the watcher; enabled=false unloads it.
Activation failure is loud and never reverts config (doctor is the guard).
The launchctl side is stubbed - these tests assert the coupling fires, not
that launchd works.
"""
from __future__ import annotations

import pytest
from typer.testing import CliRunner


@pytest.fixture()
def tmp_home(tmp_path, monkeypatch):
    monkeypatch.setenv("HOME", str(tmp_path))
    (tmp_path / ".fno").mkdir()
    try:
        pass
    except Exception:
        pass
    return tmp_path


def _stub_coupling(monkeypatch, activate_outcome="activated"):
    import fno.pr_watch.cli as pwcli

    calls: dict[str, object] = {}

    def _act() -> str:
        calls["activate"] = True
        return activate_outcome

    def _deact() -> str:
        calls["deactivate"] = True
        return "unloaded"

    monkeypatch.setattr(pwcli, "ensure_watcher_activated", _act)
    monkeypatch.setattr(pwcli, "deactivate_watcher", _deact)
    return calls


def test_enable_triggers_activation(tmp_home, monkeypatch):
    calls = _stub_coupling(monkeypatch)
    from fno.config.write_cli import app

    r = CliRunner().invoke(app, ["set", "config.pr_watch.enabled", "true"])
    assert r.exit_code == 0, r.output
    assert calls.get("activate") is True
    assert "installed and loaded" in r.output


def test_disable_triggers_unload(tmp_home, monkeypatch):
    calls = _stub_coupling(monkeypatch)
    from fno.config.write_cli import app

    r = CliRunner().invoke(app, ["set", "config.pr_watch.enabled", "false"])
    assert r.exit_code == 0, r.output
    assert calls.get("deactivate") is True


# ---------------------------------------------------------------------------
# The durable-grant coupling: arming the dispatch grant implies a live watcher
# ---------------------------------------------------------------------------


def _stub_grant_coupling(monkeypatch):
    """Stub the watcher-activation seam the grant coupling calls (its own
    import path, distinct from the enabled-coupling stubs above)."""
    import fno.pr_watch.cli as pwcli

    calls = {"activate": 0}

    def _act() -> str:
        calls["activate"] += 1
        return "activated"

    monkeypatch.setattr(pwcli, "ensure_watcher_activated", _act)
    return calls


def test_armed_dispatch_grant_activates_the_watcher(tmp_home, monkeypatch):
    calls = _stub_grant_coupling(monkeypatch)
    from fno.config.write_cli import app

    runner = CliRunner()
    r = runner.invoke(app, ["set", "config.auto_merge.enabled", "true"])
    assert r.exit_code == 0, r.output
    # enabled alone (grant defaults to none) arms nothing: a standing grant is
    # enabled AND grant=dispatch.
    assert calls["activate"] == 0
    r = runner.invoke(app, ["set", "config.auto_merge.grant", "dispatch"])
    assert r.exit_code == 0, r.output
    assert calls["activate"] == 1
    assert "activated to serve the standing dispatch grant" in r.output


def test_panic_switch_blocks_grant_activation(tmp_home, monkeypatch):
    """Subordinate to autonomy.enabled: with the panic switch off, arming the
    grant must not bring an autonomous merger to life behind its back."""
    calls = _stub_grant_coupling(monkeypatch)
    from fno.config.write_cli import app

    runner = CliRunner()
    runner.invoke(app, ["set", "config.autonomy.enabled", "false"])
    runner.invoke(app, ["set", "config.auto_merge.enabled", "true"])
    r = runner.invoke(app, ["set", "config.auto_merge.grant", "dispatch"])
    assert r.exit_code == 0, r.output
    assert calls["activate"] == 0


def test_activation_failure_is_loud_and_keeps_config(tmp_home, monkeypatch):
    """AC1-ERR: a launchctl failure warns loudly; the enable still stuck."""
    _stub_coupling(monkeypatch, activate_outcome="load-failed")
    from fno.config.write_cli import app

    r = CliRunner().invoke(app, ["set", "config.pr_watch.enabled", "true"])
    assert r.exit_code == 0, r.output
    assert "WARNING" in r.output
    assert "activation failed" in r.output

    # Config value stuck despite the activation failure.
    from fno.config import load_settings
    assert load_settings().pr_watch.enabled is True


def test_is_node_live_sees_global_claim(tmp_path, monkeypatch):
    """Regression (x-74aa): the node read routes by key to the global root, so
    a claim held there reads live from any cwd instead of free off the empty
    repo space."""
    from fno.claims import acquire_claim
    from fno.pr_watch.cli import ClaimAdapter

    monkeypatch.setenv("FNO_CLAIMS_ROOT", str(tmp_path / "global"))
    monkeypatch.setattr("fno.paths.space_dir", lambda: tmp_path / "space")
    acquire_claim("node:ab-1234", holder="target-session:sid-x")
    assert ClaimAdapter().is_node_live("ab-1234") is True
