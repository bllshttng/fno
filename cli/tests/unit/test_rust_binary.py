"""Binary-locator guards for ``fno.rust_binary``.

Two things this module can get wrong silently, both covered here:

* The ``__file__``-relative depths. The locator moved up one package level out
  of ``fno/agents/``, so ``_bundled_binary``'s ``parent`` and
  ``_cargo_dev_binary``'s ``parents[3]`` both shifted. A wrong depth returns
  ``None`` on every install instead of raising, so nothing else would notice.
* ``$FNO_AGENTS_BIN``. Both "binary not found" messages tell the operator to
  set it, and for a long time the Python resolver did not read it.
"""

from __future__ import annotations

import stat
from pathlib import Path

from fno import rust_binary


def _make_exe(path: Path) -> Path:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text("#!/bin/sh\nexit 0\n")
    path.chmod(path.stat().st_mode | stat.S_IXUSR)
    return path


def test_env_override_outranks_the_search(tmp_path, monkeypatch):
    override = _make_exe(tmp_path / "override" / rust_binary.BINARY_NAME)
    other = _make_exe(tmp_path / "onpath" / rust_binary.BINARY_NAME)
    monkeypatch.setenv("PATH", str(other.parent))
    monkeypatch.setenv(rust_binary.BINARY_ENV, str(override))
    assert rust_binary.resolve_binary() == override


def test_unusable_env_override_falls_through(tmp_path, monkeypatch):
    """A stale export must not make an installed binary unreachable."""
    on_path = _make_exe(tmp_path / "onpath" / rust_binary.BINARY_NAME)
    monkeypatch.setenv("PATH", str(on_path.parent))
    monkeypatch.setattr(rust_binary, "_bundled_binary", lambda: None)
    monkeypatch.setattr(rust_binary, "_sibling_binary", lambda: None)

    for bad in ("", "   ", str(tmp_path / "does-not-exist")):
        monkeypatch.setenv(rust_binary.BINARY_ENV, bad)
        assert rust_binary.resolve_binary() == on_path, f"failed to fall through for {bad!r}"

    # Present but not executable: also a fall-through, not a hard stop.
    not_exec = tmp_path / "plain" / rust_binary.BINARY_NAME
    not_exec.parent.mkdir(parents=True, exist_ok=True)
    not_exec.write_text("not runnable")
    not_exec.chmod(not_exec.stat().st_mode & ~stat.S_IXUSR & ~stat.S_IXGRP & ~stat.S_IXOTH)
    monkeypatch.setenv(rust_binary.BINARY_ENV, str(not_exec))
    assert rust_binary.resolve_binary() == on_path


def test_command_call_attaches_returncode(monkeypatch) -> None:
    """A clean non-zero exit carries its code as a structured field.

    The gh budget's admit gate refuses on a signalled reader (negative code or
    137) and admits on a clean failure, so the code must be readable as data,
    not only as prose in the message.
    """
    import subprocess

    import pytest

    from fno.rust_binary import CommandUnavailable

    proc = subprocess.CompletedProcess(
        ["fno-agents", "fleet-incident"], returncode=1, stdout="", stderr="no ledger"
    )
    monkeypatch.setattr(rust_binary, "find_dev_binary", lambda: "/dev/null/fno-agents")
    monkeypatch.setattr(subprocess, "run", lambda *a, **k: proc)
    with pytest.raises(CommandUnavailable) as excinfo:
        rust_binary.command_call("fleet-incident", {})
    assert excinfo.value.returncode == 1
    assert "exited 1" in str(excinfo.value)
