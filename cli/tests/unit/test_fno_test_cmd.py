"""Tests for `fno doctor test` (x-8b64 G).

The load-bearing behaviour: the verb returns pytest's *real* exit code (no pipe
masking) and pins PYTHONPATH to the worktree source. We run a tiny generated
test file through `_run` and assert the exit code is the genuine pytest result.
"""
from __future__ import annotations

import sys
from pathlib import Path

import pytest

from fno import test_cmd


def _write(p: Path, body: str) -> Path:
    p.write_text(body, encoding="utf-8")
    return p


def _run_captured_to(tmp_path, monkeypatch, body: str):
    """Run one captured pytest over `body`; return (rc, log text)."""
    f = _write(tmp_path / "test_case.py", body)
    monkeypatch.setattr(test_cmd, "_resolve_interpreter", lambda root: sys.executable)
    log = tmp_path / "custom.log"
    rc = test_cmd._run([str(f)], log_override=log)
    return rc, log.read_text(encoding="utf-8")


def test_captured_log_ends_with_real_exit_code(tmp_path, monkeypatch):
    rc, log = _run_captured_to(tmp_path, monkeypatch, "def test_bad():\n    assert False\n")
    assert rc == 1
    assert log.rstrip().endswith("EXIT=1")


def test_missing_interpreter_writes_exit_127(tmp_path, monkeypatch, no_native_owner):
    # The 127 OSError return must leave the marker too: a log that just ends
    # mid-run reads as "still going", not "the interpreter never started".
    monkeypatch.setattr(test_cmd, "_resolve_interpreter", lambda root: "/nonexistent/python")
    log = tmp_path / "custom.log"
    rc = test_cmd._run(["-q"], log_override=log)
    assert rc == 127
    assert log.read_text(encoding="utf-8").rstrip().endswith("EXIT=127")


def test_stream_with_log_refuses(tmp_path, capsys):
    # Direct callback call: the verb is a bare click Command, and the runner
    # surface differs across click versions in this environment.
    with pytest.raises(SystemExit) as exc:
        test_cmd.test_command.callback(
            stream=True, log_override=tmp_path / "x.log", runner_args=()
        )
    assert exc.value.code == 2
    assert "--log" in capsys.readouterr().err


def _popen_fake(recorder, returncode: int = 0):
    """A subprocess.Popen stand-in for the group-spawning runner.

    The suite runner now spawns via Popen in its own process group, so cmd
    capture must intercept Popen and answer wait(timeout) like a real exit.
    """
    calls = []

    class _FakePopen:
        def __init__(self, cmd, env=None, **kw):
            self.pid = 4242
            calls.append({"cmd": cmd, "env": env, "kw": kw})
            recorder(cmd, env=env, kw=kw)

        def wait(self, timeout=None):
            return returncode

        def kill(self):
            pass

    return _FakePopen


def _fake_run_capture(monkeypatch, tmp_path):
    """Set up a tmp checkout + a fake runner that captures the cmd."""
    captured = {}

    class _Proc:
        returncode = 0

    def fake_run(cmd, env=None, **kw):
        captured["cmd"] = cmd
        return _Proc()

    monkeypatch.setattr(test_cmd, "_resolve_interpreter", lambda root: sys.executable)
    monkeypatch.setattr(test_cmd.subprocess, "run", fake_run)
    monkeypatch.setattr(
        test_cmd.subprocess,
        "Popen",
        _popen_fake(lambda cmd, env=None, kw=None: captured.update(cmd=cmd)),
    )
    monkeypatch.chdir(tmp_path)
    (tmp_path / "cli" / "src" / "fno").mkdir(parents=True)
    (tmp_path / "cli" / "src" / "fno" / "__init__.py").write_text("", encoding="utf-8")
    return captured


def test_k_value_does_not_count_as_target(tmp_path, monkeypatch):
    """A `-k expr` value must not be mistaken for a collection path."""
    captured = _fake_run_capture(monkeypatch, tmp_path)
    assert test_cmd._run(["-k", "myexpr"]) == 0
    assert captured["cmd"][-1] == str((tmp_path / "cli" / "tests").resolve())


def test_explicit_target_not_overridden(tmp_path, monkeypatch):
    """An explicit path/nodeid is respected; cli/tests is not appended."""
    captured = _fake_run_capture(monkeypatch, tmp_path)
    assert test_cmd._run(["tests/foo.py::test_x"]) == 0
    assert captured["cmd"][-1] == "tests/foo.py::test_x"
    assert str((tmp_path / "cli" / "tests").resolve()) not in captured["cmd"]


def test_child_env_drops_outer_xdist_transport(monkeypatch):
    for name in (
        "PYTEST_XDIST_WORKER",
        "PYTEST_XDIST_WORKER_COUNT",
        "PYTEST_XDIST_TESTRUNUID",
    ):
        monkeypatch.setenv(name, "outer-worker")

    env = test_cmd._child_env(Path.cwd())

    assert not set(env).intersection(
        {
            "PYTEST_XDIST_WORKER",
            "PYTEST_XDIST_WORKER_COUNT",
            "PYTEST_XDIST_TESTRUNUID",
        }
    )


def test_quiet_contract_failure_tails_log(tmp_path, monkeypatch, capsys):
    """Default mode captures output to .fno/last-test.log and prints only the
    TAIL on failure (errors live at the end; expand upward via the log)."""
    # A suite larger than the tail window, so the pytest header can only reach
    # the transcript by leaking - a one-test suite fits entirely in the tail.
    body = "\n".join(
        f"def test_bad{i}():\n    assert False, 'boom{i}'" for i in range(12)
    )
    f = _write(tmp_path / "test_fail.py", body + "\n")
    monkeypatch.setattr(test_cmd, "_resolve_interpreter", lambda root: sys.executable)
    monkeypatch.chdir(tmp_path)
    (tmp_path / "cli" / "src" / "fno").mkdir(parents=True)
    (tmp_path / "cli" / "src" / "fno" / "__init__.py").write_text("", encoding="utf-8")

    rc = test_cmd._run([str(f)])
    assert rc == 1
    log = tmp_path / ".fno" / "last-test.log"
    assert log.exists()
    full = log.read_text(encoding="utf-8")
    assert "test session starts" in full  # full output lives in the log...
    out = capsys.readouterr().out
    assert "test session starts" not in out  # ...not in the transcript
    assert "FAIL" in out
    assert str(log) in out
    assert "boom11" in out  # the tail carries the end of the failure output
    assert len(out.splitlines()) <= test_cmd._TAIL_LINES + 3


def _fake_checkout_with_crates(tmp_path, monkeypatch, n=2):
    monkeypatch.chdir(tmp_path)
    (tmp_path / "cli" / "src" / "fno").mkdir(parents=True)
    (tmp_path / "cli" / "src" / "fno" / "__init__.py").write_text("", encoding="utf-8")
    for name in ["alpha", "beta"][:n]:
        d = tmp_path / "crates" / name
        d.mkdir(parents=True)
        (d / "Cargo.toml").write_text("[package]\n", encoding="utf-8")


def test_rust_mode_runs_each_crate_quietly(tmp_path, monkeypatch, capsys):
    """`fno doctor test rust` runs cargo test -q per crates/*/Cargo.toml (no nextest)."""
    cmds = []

    def record(cmd, env=None, kw=None):
        cmds.append(cmd)
        if cmd[0] == "cargo":  # sensor calls (lanes reading) pass no env
            assert env["RTK_DISABLED"] == "1"

    _fake_checkout_with_crates(tmp_path, monkeypatch)
    monkeypatch.setattr(test_cmd.subprocess, "Popen", _popen_fake(record))
    monkeypatch.setattr(test_cmd.shutil, "which", lambda name: None)
    assert test_cmd._run_rust([]) == 0
    # The lanes reading is real here (unpatched), so its sensor subprocess
    # calls (macmon, memory_pressure) ride in the capture; the contract under
    # test is the per-crate sweep, so count the cargo runs.
    cargo_cmds = [cmd for cmd in cmds if cmd[0] == "cargo"]
    assert len(cargo_cmds) == 2
    for cmd in cargo_cmds:
        assert cmd[:3] == ["cargo", "test", "-q"]
        assert "--manifest-path" in cmd


def test_rust_mode_explicit_manifest_single_run(tmp_path, monkeypatch):
    cmds = []

    def record(cmd, env=None, kw=None):
        cmds.append(cmd)

    _fake_checkout_with_crates(tmp_path, monkeypatch)
    monkeypatch.setattr(test_cmd.subprocess, "Popen", _popen_fake(record, returncode=3))
    monkeypatch.setattr(test_cmd.shutil, "which", lambda name: None)
    rc = test_cmd._run_rust(["--manifest-path", "crates/alpha/Cargo.toml"])
    assert rc == 3  # real cargo exit code propagated
    # Cargo only: the real lanes reading may shell out (env=None) first.
    cargo_cmds = [cmd for cmd in cmds if cmd[0] == "cargo"]
    assert len(cargo_cmds) == 1  # user's manifest respected, no crate sweep


def test_pythonpath_pins_worktree_src(tmp_path, monkeypatch, no_native_owner):
    """_run must prepend <root>/cli/src to PYTHONPATH so a worktree tests its
    own source. We capture the child env through the Popen fake."""
    captured = {}

    def record(cmd, env=None, kw=None):
        captured["env"] = env
        captured["cmd"] = cmd

    monkeypatch.setattr(test_cmd, "_resolve_interpreter", lambda root: sys.executable)
    monkeypatch.setattr(test_cmd.subprocess, "Popen", _popen_fake(record))
    monkeypatch.chdir(tmp_path)
    (tmp_path / "cli" / "src" / "fno").mkdir(parents=True)
    (tmp_path / "cli" / "src" / "fno" / "__init__.py").write_text("", encoding="utf-8")

    assert test_cmd._run(["-q"]) == 0
    pp = captured["env"]["PYTHONPATH"]
    assert pp.split(":")[0] == str((tmp_path / "cli" / "src").resolve())
    assert captured["env"]["RTK_DISABLED"] == "1"
    assert captured["cmd"][1:3] == ["-m", "pytest"]
