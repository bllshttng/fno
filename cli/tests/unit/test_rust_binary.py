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


# ── The native-family signposts hand the call to the front door ──
#
# A gate-main run resolved fno to this wheel and died on the doctor signpost
# three times writing no grade while the real front door sat one PATH entry
# later. The refusal was only true when no door exists, so the signposts exec
# one first; these tests pin the handoff and the preserved refusal.

def _stub_door(tmp_path, code=0):
    """A door stand-in that records its argv and exits `code`."""
    log = tmp_path / "door-argv.txt"
    stub = tmp_path / "fno"
    stub.write_text("#!/bin/sh\n" f'printf "%s\\n" "$@" > {log}\n' f"exit {code}\n")
    stub.chmod(0o755)
    return stub


def test_front_door_handoff_runs_the_door_with_family_and_tail(tmp_path, monkeypatch):
    stub = _stub_door(tmp_path, code=7)
    monkeypatch.setattr(rust_binary, "resolve_front_binary", lambda: stub)
    code = rust_binary.front_door_handoff(["evals", "run", "--tag", "x"], "doctor")
    assert code == 7
    assert (tmp_path / "door-argv.txt").read_text().splitlines() == [
        "doctor",
        "evals",
        "run",
        "--tag",
        "x",
    ]


def test_front_door_handoff_reads_a_self_launcher_as_no_door(tmp_path, monkeypatch):
    # PATH's `fno` being an fno-py shim must read as no door: exec-ing it
    # re-enters the signpost.
    self_shim = tmp_path / "fno-py-shim"
    self_shim.write_text("#!/bin/sh\nexit 9\n")
    self_shim.chmod(0o755)
    monkeypatch.setattr(rust_binary, "resolve_front_binary", lambda: self_shim)
    monkeypatch.setattr(rust_binary.sys, "argv", [str(self_shim), "doctor"])
    assert rust_binary.front_door_handoff(["evals"], "doctor") is None


def test_front_door_handoff_answers_none_when_no_door_exists(monkeypatch):
    monkeypatch.setattr(rust_binary, "resolve_front_binary", lambda: None)
    assert rust_binary.front_door_handoff(["evals"], "doctor") is None


def test_doctor_signpost_hands_off_instead_of_refusing(tmp_path, monkeypatch):
    from typer.testing import CliRunner

    from fno.cli import app

    stub = _stub_door(tmp_path)
    monkeypatch.setattr(rust_binary, "resolve_front_binary", lambda: stub)
    done = CliRunner().invoke(app, ["doctor", "evals", "run", "--tag", "x"])
    assert done.exit_code == 0, done.output
    assert (tmp_path / "door-argv.txt").read_text().splitlines() == [
        "doctor",
        "evals",
        "run",
        "--tag",
        "x",
    ]


def test_front_door_handoff_falls_back_when_the_door_cannot_exec(tmp_path, monkeypatch):
    # The other resolve_front_binary callers treat an exec failure as "no
    # door"; the handoff must too, or a door deleted between the is_file
    # check and the run tracebacks instead of refusing.
    stub = tmp_path / "fno"
    stub.write_text("#!/bin/sh\nexit 0\n")
    stub.chmod(0o644)
    monkeypatch.setattr(rust_binary, "resolve_front_binary", lambda: stub)
    assert rust_binary.front_door_handoff(["evals"], "doctor") is None


def test_signpost_refusal_stays_when_no_door_exists(monkeypatch):
    from typer.testing import CliRunner

    from fno.cli import app

    monkeypatch.setattr(rust_binary, "resolve_front_binary", lambda: None)
    done = CliRunner().invoke(app, ["doctor", "evals", "run"])
    assert done.exit_code == 2
    assert "native to the Rust front door" in done.output
