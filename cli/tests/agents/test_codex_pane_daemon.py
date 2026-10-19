"""Tests for the codex pane lane's daemon-owned threads (x-a095).

The create form asserts the shared app-server daemon (`--remote unix://`,
`pre_exec` daemon start), the spawn refuses when that start fails, the mesh
identity reaches daemon-run tools as config-set leaves, and the trust screen
is named, never answered.
"""
from __future__ import annotations

import os
import signal
import subprocess
from pathlib import Path

import pytest

from fno.agents import codex_pane
from tests.agents.test_spawn_pane import CODEX_HARNESS, FakeRunner, _spawn


def test_ensure_codex_daemon_does_not_wait_for_a_daemon_child_with_open_pipes(
    tmp_path: Path, monkeypatch
) -> None:
    """A detached daemon child may inherit the startup command's output fds."""
    from fno.agents.dispatch import DispatchAskError

    bin_dir = tmp_path / "bin"
    bin_dir.mkdir()
    child_pid_file = tmp_path / "child.pid"
    codex = bin_dir / "codex"
    codex.write_text(
        "#!/bin/sh\n"
        "(sleep 3) &\n"
        "printf '%s\\n' \"$!\" > \"$CODEX_CHILD_PID_FILE\"\n"
        "exit 0\n",
        encoding="utf-8",
    )
    codex.chmod(0o755)
    env = {
        **os.environ,
        "PATH": f"{bin_dir}{os.pathsep}{os.environ.get('PATH', '')}",
        "CODEX_CHILD_PID_FILE": str(child_pid_file),
    }
    monkeypatch.setenv("PATH", env["PATH"])
    monkeypatch.setattr(codex_pane, "_CODEX_DAEMON_START_TIMEOUT_S", 1.0)

    failure = None
    try:
        codex_pane.ensure_codex_daemon(subprocess.run, env=env)
    except DispatchAskError as exc:
        failure = str(exc)
    finally:
        if child_pid_file.exists():
            try:
                os.kill(int(child_pid_file.read_text(encoding="utf-8")), signal.SIGTERM)
            except (ProcessLookupError, ValueError):
                pass

    assert failure is None, failure


def test_codex_shell_env_args_refuses_a_key_outside_the_leaf_shape() -> None:
    """A dotted or quoted key would write a different config path."""
    from fno.agents import codex_pane
    from fno.agents.dispatch import DispatchAskError

    with pytest.raises(DispatchAskError, match="A.B=1"):
        codex_pane.codex_shell_env_args(["A.B=1"])
    with pytest.raises(DispatchAskError, match="JUST_A_KEY"):
        codex_pane.codex_shell_env_args(["JUST_A_KEY"])


def test_codex_pane_spawn_refused_when_daemon_start_fails(
    tmp_path: Path, monkeypatch
) -> None:
    """A dead daemon start refuses the spawn before any pane exists: the
    create form asserts the daemon, so launching without one would mint the
    private thread this lane exists to prevent."""
    from fno.agents.dispatch import DispatchAskError

    class DaemonStartFails(FakeRunner):
        def __call__(self, argv, **kwargs):
            if list(argv)[:4] == ["codex", "app-server", "daemon", "start"]:
                self.calls.append(list(argv))
                return subprocess.CompletedProcess(argv, 1, "", "daemon start exploded")
            return super().__call__(argv, **kwargs)

    runner = DaemonStartFails()
    with pytest.raises(DispatchAskError, match="daemon start exploded"):
        _spawn(
            monkeypatch, tmp_path, provider=CODEX_HARNESS, runner=runner,
            codex_binding=False,
        )
    assert not any(call[1:4] == ["mux", "pane", "run"] for call in runner.calls)


def test_codex_trust_screen_refusal_fires_despite_hook_trust_bypass() -> None:
    """`--remote` ignores a config trust override, so an untrusted cwd parks
    on the project trust screen even under the bypass posture; the readiness
    probe names it and never answers the security decision."""
    from fno.agents.mux_spawn import _codex_trust_refusal

    refusal = _codex_trust_refusal(
        "? Do you trust the contents of this directory?",
        cwd=Path("/w/proj"),
        hook_trust_bypassed=True,
    )
    assert refusal is not None
    assert refusal.startswith("Codex project trust required for /w/proj")


def _codex_pane_run_tail(runner: FakeRunner) -> list[str]:
    run_call = next(c for c in runner.calls if c[1:4] == ["mux", "pane", "run"])
    return run_call[run_call.index("--") + 1 :]


def test_codex_pane_mesh_identity_rides_config_set_args(
    tmp_path: Path, monkeypatch
) -> None:
    """A daemon-run tool sees none of the TUI's env, so the mesh pairs ride
    into the tool shell as `-c shell_environment_policy.set` leaves; the
    env(1) wrapper still carries them for the TUI process itself."""
    from fno.agents import codex_pane

    monkeypatch.setattr(codex_pane, "ensure_codex_daemon", lambda *_a, **_k: None)

    runner = FakeRunner()
    _spawn(
        monkeypatch, tmp_path, provider=CODEX_HARNESS, name="w1", runner=runner
    )

    tail = _codex_pane_run_tail(runner)
    assert tail[0] == "env"
    assert "FNO_AGENT_SELF=w1" in tail
    codex_at = tail.index("codex")
    assert tail[codex_at + 1 : codex_at + 3] == [
        "-c",
        'shell_environment_policy.set.FNO_AGENT_SELF="w1"',
    ]

    claude_runner = FakeRunner()
    _spawn(monkeypatch, tmp_path, name="w2", runner=claude_runner)
    claude_tail = _codex_pane_run_tail(claude_runner)
    assert not any("shell_environment_policy" in tok for tok in claude_tail)


def test_codex_daemon_ambiguity_still_reaps_rather_than_guessing(
    tmp_path: Path, monkeypatch
) -> None:
    """Two new session ids in this cwd is a race between sibling panes; the
    daemon oracle refuses to guess, so the spawn fails exactly like the
    binding-window-expired case rather than misbinding."""
    from fno.agents.dispatch import DispatchAskError
    from fno.agents.registry import load_registry

    monkeypatch.setattr(
        codex_pane, "_make_codex_bind_probe", lambda **_kwargs: (lambda: None)
    )

    runner = FakeRunner()
    with pytest.raises(DispatchAskError, match="session binding.*reaped"):
        _spawn(
            monkeypatch, tmp_path, provider=CODEX_HARNESS, runner=runner,
            codex_binding=False,
        )
    assert load_registry() == []
    assert runner.kill_calls
