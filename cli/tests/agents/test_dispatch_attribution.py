"""Dispatch attribution: env-safe agent names and the create-event envelope.

agent_ask_started + agent_ask_done share one request_id and carry from_name.
We monkeypatch ``events.emit`` to capture records in-memory. Since
``emit_with_context`` delegates to ``emit``, the patched function sees
records from both legacy and migrated call sites.
"""
from __future__ import annotations

import re
from pathlib import Path
from typing import Any

import pytest

from fno.paths_testing import use_tmpdir
from tests.agents._fake_claude import configure_fake, install_fake_claude


REQUEST_ID_RE = re.compile(r"^[a-f0-9]{32}$")


def _install_fake_path(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    """Put a fake claude on PATH so dispatch_ask's create path succeeds."""
    bin_dir = tmp_path / "bin"
    install_fake_claude(bin_dir)
    monkeypatch.setenv("PATH", str(bin_dir))
    configure_fake(monkeypatch)


def _patch_emit_capture(monkeypatch: pytest.MonkeyPatch) -> list[tuple[str, dict[str, Any]]]:
    """Replace events.emit with an in-memory list. Returns the list."""
    captured: list[tuple[str, dict[str, Any]]] = []
    from fno.agents import events as events_mod

    def fake_emit(kind: str, **kw: Any) -> None:
        captured.append((kind, dict(kw)))

    monkeypatch.setattr(events_mod, "emit", fake_emit)
    return captured


def _clear_agent_env(monkeypatch: pytest.MonkeyPatch) -> None:
    for k in (
        "FNO_AGENT_SELF",
        "FNO_AGENT_HARNESS",
        "FNO_AGENT_SESSION",
        "MCP_CHANNEL_INBOUND_POKE",
        "CRON_JOB",
        "INVOCATION_ID",
    ):
        monkeypatch.delenv(k, raising=False)


def test_dispatch_rejects_name_with_env_corrupting_char(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """Names containing NUL / \\n / \\r / = are rejected at validate time with
    exit_code=2 (one tuple scan guards all four), before such a name lands in
    FNO_AGENT_SELF and crashes subprocess.run or splits the env."""
    use_tmpdir(monkeypatch, tmp_path)
    _clear_agent_env(monkeypatch)
    from fno.agents.dispatch import DispatchAskError, _validate_inputs

    name = "agent\x00name"
    with pytest.raises(DispatchAskError) as excinfo:
        _validate_inputs(name=name, message="hi", from_name="fno")
    assert excinfo.value.exit_code == 2
    assert "forbidden character" in str(excinfo.value)


# ---------------------------------------------------------------------------
# Spawn-path context (codex P2, PR #457): dispatch_spawn wraps the create
# helpers in the same _DISPATCH_CTX envelope the old ask dispatcher used, so
# the helpers' agent_ask_started/agent_ask_done emits keep request_id /
# from_name / caller attribution after the create moved off ask.
# ---------------------------------------------------------------------------


def test_spawn_create_events_carry_context(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """dispatch_spawn (claude plain) emits create events with the full envelope."""
    use_tmpdir(monkeypatch, tmp_path)
    _install_fake_path(tmp_path, monkeypatch)
    _clear_agent_env(monkeypatch)
    monkeypatch.setenv("HOME", str(tmp_path / "home"))
    (tmp_path / "home").mkdir()
    captured = _patch_emit_capture(monkeypatch)

    from fno.agents.dispatch import dispatch_spawn

    result = dispatch_spawn(
        name="spawned-alpha",
        message="hello",
        harness="claude",
        cwd=tmp_path,
        from_name="orchestrator",
    )
    assert result.kind == "created"

    started = [e for e in captured if e[0] == "agent_ask_started"]
    done = [e for e in captured if e[0] == "agent_ask_done"]
    assert started, f"missing agent_ask_started; got: {[e[0] for e in captured]}"
    assert done, f"missing agent_ask_done; got: {[e[0] for e in captured]}"
    for label, ev in (("started", started[0][1]), ("done", done[0][1])):
        assert "request_id" in ev, f"{label} event lacks request_id"
        assert REQUEST_ID_RE.match(ev["request_id"]), (
            f"{label} request_id format invariant violated: {ev['request_id']!r}"
        )
        assert ev.get("from_name") == "orchestrator", (
            f"{label} event lacks from_name attribution: {ev}"
        )
    assert started[0][1]["request_id"] == done[0][1]["request_id"], (
        "started/done must share one request_id"
    )
