"""The fno-py to Rust-front hand-off refuses to recurse.

A PATH `fno` that forwards to fno-py re-enters `_run_rust_front`; the
FNO_PY_HANDOFF marker turns the second entry into a named refusal instead
of the 2026-10-02 stack (~9,000 processes, 89 GB resident).
"""

import os
import shutil
import subprocess
import sys
from pathlib import Path

import pytest
import typer
from typer.testing import CliRunner

import fno
from fno.cli import _run_rust_front, app


def test_recursive_handoff_refuses_with_marker_set(monkeypatch, capsys):
    monkeypatch.setenv("FNO_PY_HANDOFF", "1")
    with pytest.raises(typer.Exit) as exc:
        _run_rust_front(["mux"])
    assert exc.value.exit_code == 1
    captured = capsys.readouterr()
    assert "FNO_PY_HANDOFF" in captured.err
    assert "recursive" in captured.err


def test_handoff_sets_marker_for_the_child(monkeypatch, tmp_path):
    dump = tmp_path / "dump.txt"
    script = tmp_path / "fake-fno"
    script.write_text(f"#!/bin/sh\nprintenv FNO_PY_HANDOFF > {dump}\n")
    script.chmod(0o755)
    monkeypatch.setattr(shutil, "which", lambda name: str(script))
    with pytest.raises(typer.Exit) as exc:
        _run_rust_front(["version"])
    assert exc.value.exit_code == 0
    assert dump.read_text().strip() == "1"


def test_old_binary_cannot_recurse_through_the_backlog_forward(tmp_path):
    """A stub plays a binary that execs every backlog command back into fno-py."""
    hops = tmp_path / "hops"
    driver = tmp_path / "driver.py"
    stub = tmp_path / "fno-agents"
    driver.write_text(
        "from pathlib import Path\n"
        "import fno.rust_binary as r\n"
        f"r.find_dev_binary = lambda: Path({str(stub)!r})\n"
        "from fno.cli import main\n"
        "main()\n"
    )
    # The cap stops a regression from flooding the machine.
    stub.write_text(
        "#!/bin/sh\n"
        f"echo hop >> {hops}\n"
        f'[ "$(wc -l < {hops})" -ge 5 ] && exit 99\n'
        f'exec {sys.executable} {driver} "$@"\n'
    )
    stub.chmod(0o755)
    env = {k: v for k, v in os.environ.items() if k != "FNO_PY_BACKLOG_FORWARD"}
    env["PYTHONPATH"] = str(Path(fno.__file__).parents[1])
    done = subprocess.run(
        [sys.executable, str(driver), "backlog", "script-door", "plan-rung", "p.md"],
        env=env, capture_output=True, text=True, timeout=120,
    )
    assert done.returncode == 1, done.stderr
    assert "recursive backlog forward refused" in done.stderr
    assert len(hops.read_text().splitlines()) == 1


def test_inherited_marker_for_another_command_still_forwards(tmp_path, monkeypatch):
    """A worker spawned under one backlog command can run a different one."""
    dump = tmp_path / "args.txt"
    stub = tmp_path / "fno-agents"
    stub.write_text(f'#!/bin/sh\necho "$@" > {dump}\n')
    stub.chmod(0o755)
    monkeypatch.setattr("fno.rust_binary.find_dev_binary", lambda: stub)
    monkeypatch.setenv("FNO_PY_BACKLOG_FORWARD", '["advance"]')
    result = CliRunner().invoke(app, ["backlog", "get", "x-1"])
    assert result.exit_code == 0, result.output
    assert dump.read_text().strip() == "backlog get x-1"
