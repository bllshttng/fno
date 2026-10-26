"""The Python spawn seam: an unpinned claude resume asks the Rust resume-pin
owner, pins the argv or refuses by name (x-9db4).

The transport stays hermetic through conftest's ``_hermetic_resume_pin``; the
tests here re-stub it only where the subject is the transport itself.
"""
from __future__ import annotations

from pathlib import Path
from types import SimpleNamespace
from typing import Any

import pytest

from fno.paths_testing import use_tmpdir

LINEAGE_UUID = "11111111-aaaa-bbbb-cccc-000000000001"
ROUTE_UUID = "11111111-aaaa-bbbb-cccc-000000000002"

ROUTE_ENV = {
    "ANTHROPIC_BASE_URL": "https://api.z.ai/api/anthropic",
    "ANTHROPIC_AUTH_TOKEN": "zai-secret-token",
    "ANTHROPIC_MODEL": "glm-5.2",
    "ANTHROPIC_DEFAULT_OPUS_MODEL": "glm-5.2",
    "ANTHROPIC_DEFAULT_SONNET_MODEL": "glm-5.2",
    "ANTHROPIC_DEFAULT_HAIKU_MODEL": "glm-5.2",
    "ANTHROPIC_DEFAULT_FABLE_MODEL": "glm-5.2",
}


def _home(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    use_tmpdir(monkeypatch, tmp_path)
    home = tmp_path / "home"
    home.mkdir(exist_ok=True)
    monkeypatch.setenv("HOME", str(home))
    for key in ("FNO_AGENT_SELF", "FNO_AGENT_HARNESS", "FNO_AGENT_SESSION"):
        monkeypatch.delenv(key, raising=False)


def _seed_row(
    name: str,
    uuid: str,
    *,
    requested_model: str | None = None,
    provider: str | None = None,
    route_settings_path: str | None = None,
):
    from fno.agents.registry import AgentEntry, update_registry

    row = AgentEntry(
        name=name,
        harness="claude",
        cwd="/tmp",
        log_path="/tmp/lineage.log",
        short_id="aaa11111",
        harness_session_id=uuid,
        requested_model=requested_model,
        provider=provider,
        route_settings_path=route_settings_path,
    )
    update_registry(lambda entries: entries + [row])


def _spawn_resume(tmp_path, monkeypatch, **kwargs):
    """Run dispatch_spawn with the claude bg_create stubbed; return
    (result, captured bg_create kwargs)."""
    from fno.agents import dispatch as dispatch_mod
    from fno.agents.dispatch import dispatch_spawn

    captured: dict[str, Any] = {}
    monkeypatch.setattr(dispatch_mod.time, "sleep", lambda s: None)

    def fake_bg_create(**kw):
        captured.update(kw)
        return SimpleNamespace(session_id_out="bbbb2222", duration_ms=1)

    monkeypatch.setattr("fno.agents.harnesses.claude.bg_create", fake_bg_create)
    result = dispatch_spawn(
        name="wk-wakefork",
        message="continue",
        harness="claude",
        cwd=tmp_path,
        **kwargs,
    )
    return result, captured


def test_ac4_hp_unpinned_fork_pins_the_row_request(tmp_path, monkeypatch):
    _home(tmp_path, monkeypatch)
    _seed_row("wk-lineage", LINEAGE_UUID, requested_model="claude-opus-5")
    result, captured = _spawn_resume(
        tmp_path, monkeypatch, resume_session_id=LINEAGE_UUID
    )
    assert captured["model"] == "claude-opus-5"
    assert captured["resume_session_id"] == LINEAGE_UUID

    from fno.agents.registry import load_registry

    minted = [e for e in load_registry() if e.name == "wk-wakefork"]
    assert minted and minted[0].requested_model == "claude-opus-5"


def test_ac4_err_rowless_request_refuses_and_launches_nothing(tmp_path, monkeypatch):
    _home(tmp_path, monkeypatch)
    _seed_row("wk-lineage", LINEAGE_UUID)  # no model axis on the row
    from fno.agents.fork_lineage import ResumeUnpinned

    with pytest.raises(ResumeUnpinned) as excinfo:
        _spawn_resume(tmp_path, monkeypatch, resume_session_id=LINEAGE_UUID)
    assert excinfo.value.exit_code == 2

    from fno.agents.registry import load_registry

    assert [e for e in load_registry() if e.name == "wk-wakefork"] == []


def test_ac6_edge_unavailable_owner_refuses_never_defaults(tmp_path, monkeypatch):
    _home(tmp_path, monkeypatch)
    _seed_row("wk-lineage", LINEAGE_UUID, requested_model="claude-opus-5")
    from fno.agents import fork_lineage
    from fno.agents.spawn_axes_client import SpawnAxesUnavailable

    def _dead(payload):
        raise SpawnAxesUnavailable("fno-agents binary missing")

    monkeypatch.setattr(fork_lineage, "spawn_axes_call", _dead)
    from fno.agents.fork_lineage import ResumeUnpinned

    with pytest.raises(ResumeUnpinned) as excinfo:
        _spawn_resume(tmp_path, monkeypatch, resume_session_id=LINEAGE_UUID)
    assert "unavailable" in str(excinfo.value)


def test_stale_owner_answer_shape_refuses_not_degrades(tmp_path, monkeypatch):
    """An older binary routes the resume_pin payload to the axes plan, whose
    answer carries no decision keys; the seam must refuse, never launch
    unpinned (the review's mixed-version case)."""
    _home(tmp_path, monkeypatch)
    from fno.agents import fork_lineage
    from fno.agents.fork_lineage import ResumeUnpinned

    def _axes_plan(payload):
        return {"inject": [], "applied": [], "suppressed": [], "messages": []}

    monkeypatch.setattr(fork_lineage, "spawn_axes_call", _axes_plan)
    with pytest.raises(ResumeUnpinned) as excinfo:
        fork_lineage.resume_axes(None, LINEAGE_UUID, None, None, routed=False)
    assert "stale resume-pin owner" in str(excinfo.value)


def _stamp_account(name: str, account: str) -> None:
    """Record `account` on the named row's launch_account axis."""
    from fno.agents.registry import update_registry

    def _stamp(entries):
        for entry in entries:
            if entry.name == name:
                entry.launch_account = account
        return entries

    update_registry(_stamp)


def test_ac3_err_unresolvable_recorded_account_refuses_launching_nothing(
    tmp_path, monkeypatch
):
    # AC3-ERR: a recorded account that no longer resolves raises ResumeUnpinned
    # (exit 2) before bg_create, and nothing is minted.
    _home(tmp_path, monkeypatch)
    _seed_row("wk-lineage", LINEAGE_UUID, requested_model="claude-opus-5")
    _stamp_account("wk-lineage", "makers")
    from fno.agents.fork_lineage import ResumeUnpinned
    from fno.agents.spawn_overlay_client import AccountRefused

    def _dead(account_id, **kwargs):
        raise AccountRefused("no login for makers")

    monkeypatch.setattr("fno.agents.spawn_overlay_client.account_overlay", _dead)
    monkeypatch.setattr(
        "fno.agents.harnesses.claude.bg_create",
        lambda **kwargs: (_ for _ in ()).throw(AssertionError("spawned past refusal")),
    )
    with pytest.raises(ResumeUnpinned) as excinfo:
        _spawn_resume(tmp_path, monkeypatch, resume_session_id=LINEAGE_UUID)
    assert excinfo.value.exit_code == 2
    assert "account 'makers' does not resolve" in str(excinfo.value)

    from fno.agents.registry import load_registry

    assert [e for e in load_registry() if e.name == "wk-wakefork"] == []

