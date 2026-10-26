"""Tests for the pi and agy integration legs (`fno.setup.integration`)."""
from __future__ import annotations

import json

import pytest

from fno.setup import integration as I


@pytest.fixture
def pi_agent_env(tmp_path, monkeypatch):
    """A relocated agent dir via PI_CODING_AGENT_DIR, plus a scratch HOME.

    The install must honor the agent dir pi actually reads and write nothing
    under the ambient HOME's ~/.pi.
    """
    from fno.rust_binary import find_dev_binary

    binary = find_dev_binary()
    if binary is None:
        pytest.skip("no fno-agents dev build (cargo build -p fno-agents)")
    # call_binary_json resolves through $FNO_AGENTS_BIN first, so the tests
    # exercise the dev binary, which carries the pi arm.
    monkeypatch.setenv("FNO_AGENTS_BIN", str(binary))
    agent = tmp_path / "agent"
    monkeypatch.setenv("PI_CODING_AGENT_DIR", str(agent))
    monkeypatch.setenv("HOME", str(tmp_path / "home"))
    return agent


@pytest.mark.dev_build
def test_pi_install_copies_extension_and_is_installed(tmp_path, pi_agent_env):
    assert I._pi_is_installed() is False

    res = I._pi_install()
    assert res.ok and res.cli == "pi"

    dest = pi_agent_env / "extensions" / "footnote.ts"
    assert dest.exists()
    assert dest.read_text(encoding="utf-8") == I._pi_extension_src().read_text(
        encoding="utf-8"
    )
    # Nothing under the scratch HOME's default pi tree: the agent dir is
    # where pi itself reads, not $HOME/.pi.
    assert not (tmp_path / "home" / ".pi").exists()
    assert I._pi_is_installed() is True


# --- agy (native Stop-hook registration) -------------------------------------

@pytest.fixture
def agy_rust_door(monkeypatch):
    """Pin the agy hooks door to THIS checkout's fno-agents build.

    call_binary_json resolves through $FNO_AGENTS_BIN first, so the tests
    exercise the dev binary, and skip where this checkout has none (the same
    contract the native_backlog_door fixture implements)."""
    from fno.rust_binary import find_dev_binary

    binary = find_dev_binary()
    if binary is None:
        pytest.skip("no fno-agents dev build (cargo build -p fno-agents)")
    monkeypatch.setenv("FNO_AGENTS_BIN", str(binary))


def _fake_agy_adapter(tmp_path, monkeypatch):
    """Point _agy_adapter_path at a real tmp file so install is deterministic
    (independent of whether the test env can resolve the real plugin root).
    The role and guard adapters resolve to None for the same reason: no test
    may depend on what the machine's plugin stage happens to ship."""
    adapter = tmp_path / "plugin" / "hooks" / "footnote-agy-target-stop-hook.sh"
    adapter.parent.mkdir(parents=True, exist_ok=True)
    adapter.write_text("#!/usr/bin/env bash\n", encoding="utf-8")
    monkeypatch.setattr(I, "_agy_adapter_path", lambda: adapter)
    monkeypatch.setattr(I, "_agy_role_adapter_path", lambda: None)
    monkeypatch.setattr(I, "_agy_guard_adapter_path", lambda: None)
    return adapter


@pytest.mark.dev_build
def test_agy_install_registers_stop_hook_and_is_installed(tmp_path, monkeypatch, agy_rust_door):
    monkeypatch.setenv("HOME", str(tmp_path))
    monkeypatch.chdir(tmp_path)  # keep any workspace writes inside the tmp tree
    adapter = _fake_agy_adapter(tmp_path, monkeypatch)

    assert I._agy_is_installed() is False

    res = I._agy_install()
    assert res.ok and res.cli == "agy"

    hooks = I._agy_hooks_json()
    data = json.loads(hooks.read_text(encoding="utf-8"))
    stop = data["footnote"]["Stop"]
    assert stop[0]["command"] == str(adapter)
    assert stop[0]["type"] == "command"
    assert I._agy_is_installed() is True
    # No workspace breadcrumb: agy parses AGENTS.md natively, and `.agent/`
    # singular is not an agy path at all.
    assert not (tmp_path / ".agent").exists()


def test_agy_install_manual_when_adapter_absent(tmp_path, monkeypatch):
    monkeypatch.setenv("HOME", str(tmp_path))
    monkeypatch.setattr(I, "_agy_adapter_path", lambda: None)
    res = I._agy_install()
    # A CLI-only install can't wire a path that doesn't exist -> manual, never ok.
    assert res.status == "manual" and not res.ok
