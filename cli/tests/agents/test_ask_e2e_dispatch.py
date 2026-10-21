"""CLI-level `fno agents ask` follow-up through the compiled Rust client.

A registry row written by the retained Python create helper is read by the
Rust `ask` follow-up against a fake codex on PATH: both runtimes share one
registry. Skips (not fails) when the compiled ``fno-agents`` binary is
absent (sdist test env), mirroring ``test_rust_command_parity.py``.
"""
from __future__ import annotations

import os
import stat
import subprocess
from pathlib import Path

import pytest

from fno.paths_testing import use_tmpdir
from fno.rust_binary import find_dev_binary


# --------------------------------------------------------------------------- #
# Rust binary discovery (mirrors test_rust_command_parity).
# --------------------------------------------------------------------------- #

RUST_BIN = find_dev_binary()
requires_rust = pytest.mark.skipif(
    RUST_BIN is None,
    reason="compiled fno-agents binary not present (build with `cargo build -p fno-agents`)",
)


# --------------------------------------------------------------------------- #
# Shared fake codex installer. The script is the exact contract emitted by
# the fake codex used in crates/fno-agents/tests/codex_ask_dispatch.rs and
# codex_ask_parity.rs; keeping the wire format identical lets one fake serve
# the Python create and the Rust follow-up.
# --------------------------------------------------------------------------- #

_FAKE_CODEX_SCRIPT = r"""#!/bin/sh
set -e
if [ -n "$FAKE_CODEX_EXIT" ] && [ "$FAKE_CODEX_EXIT" != "0" ]; then
  exit "$FAKE_CODEX_EXIT"
fi
if [ -n "$FAKE_CODEX_SESSION_ID" ]; then
  printf '{"type":"thread.started","thread_id":"%s"}\n' "$FAKE_CODEX_SESSION_ID"
fi
if [ -n "$FAKE_CODEX_REPLY" ]; then
  printf '{"type":"item.completed","item":{"type":"agent_message","text":"%s"}}\n' "$FAKE_CODEX_REPLY"
fi
printf '{"type":"turn.completed"}\n'
exit 0
"""


def _install_fake_codex(bin_dir: Path) -> None:
    bin_dir.mkdir(parents=True, exist_ok=True)
    path = bin_dir / "codex"
    path.write_text(_FAKE_CODEX_SCRIPT, encoding="utf-8")
    mode = path.stat().st_mode
    path.chmod(mode | stat.S_IXUSR | stat.S_IXGRP | stat.S_IXOTH)


# Per-provider fixture: (installer, env vars to set FAKE_*_SESSION_ID / _REPLY).
# Keyed so the followup matrix below stays a single parametrized test.
_PROVIDER_FAKES = {
    "codex": (
        _install_fake_codex,
        lambda sid, reply: {"FAKE_CODEX_SESSION_ID": sid, "FAKE_CODEX_REPLY": reply},
    ),
}


# A deterministic session id the fake codex echoes back. The codex `create`
# path derives the agent's short-id from the session id (8 hex chars of the
# leading uuid block), so a fixed id keeps the registry row deterministic.
_SESSION_ID = "11111111-2222-3333-4444-555555555555"


@requires_rust
@pytest.mark.parametrize("provider", ["codex"])
def test_ask_followup_e2e_registry_seeded(provider, tmp_path: Path, monkeypatch) -> None:
    """A follow-up ask through the Rust client returns the provider's reply
    verbatim and exit 0, reading a registry row the Python create machinery
    wrote (exactly the production shape: both runtimes share one registry).
    Follow-ups pass no `--harness` so the provider resolves from the row.
    The Python dispatch half of the old parity test died with the ported
    ask legs; the retained `_codex_create_path` still writes the row."""

    install_fake, env_for = _PROVIDER_FAKES[provider]
    bin_dir = tmp_path / "bin"
    install_fake(bin_dir)
    fake_path = f"{bin_dir}{os.pathsep}{os.environ.get('PATH', '')}"
    create_reply = "create reply"
    followup_reply = "followup reply"

    cwd = tmp_path / "workdir"
    cwd.mkdir()

    # Seed via the retained create helper (writes the registry row).
    py_home = tmp_path / "py-home"
    py_home.mkdir()
    use_tmpdir(monkeypatch, py_home)
    monkeypatch.setenv("PATH", fake_path)

    from fno import paths
    from fno.agents.dispatch import _codex_create_path

    class _Lock:
        def detach(self) -> None:
            pass

    for k, v in env_for(_SESSION_ID, create_reply).items():
        monkeypatch.setenv(k, v)
    _codex_create_path(
        name="fu-agent",
        message="hi",
        cwd=cwd,
        from_name="fno",
        yolo=False,
        timeout_sec=10.0,
        lock_handle=_Lock(),
    )

    # Followup through the binary, from the same cwd so the registry-pinned
    # cwd is honored.
    rs_home = paths.agents_registry_path().parent
    env = {
        **os.environ,
        "FNO_AGENTS_HOME": str(rs_home),
        "PATH": fake_path,
        **env_for(_SESSION_ID, followup_reply),
    }

    rs = subprocess.run(
        [str(RUST_BIN), "ask", "fu-agent", "again"],
        env=env,
        cwd=cwd, capture_output=True, text=True, timeout=30, check=False,
    )

    assert rs.returncode == 0, f"rust {provider} followup failed: {rs.stderr}"
    assert rs.stdout == followup_reply, (
        f"{provider} followup stdout must be the reply verbatim:\n"
        f"  stdout: {rs.stdout!r}\n"
        f"  rust stderr: {rs.stderr}"
    )
