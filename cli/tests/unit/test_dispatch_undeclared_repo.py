"""The dispatcher's pin seam for a spawn into an undeclared foreign repo.

The pin decision itself lives in the fno-agents binary
(crates/fno-agents/src/worktree/policy.rs) and is characterized by the
dispatch_pin goldens. What stays here is the Python seam's own contract: it
computes the pin for an explicit foreign --cwd, returns it without touching
the ambient env, and prints the receipt.
"""
from __future__ import annotations

import subprocess
from pathlib import Path

import pytest

from fno.rust_binary import find_dev_binary

requires_rust = pytest.mark.skipif(
    find_dev_binary() is None,
    reason="compiled fno-agents binary not present (build with `cargo build -p fno-agents`)",
)


def _make_repo(path: Path, config_text: str = "") -> Path:
    subprocess.run(["git", "init", "-b", "main", str(path)], check=True, capture_output=True)
    subprocess.run(["git", "config", "user.email", "t@t"], cwd=path, check=True, capture_output=True)
    subprocess.run(["git", "config", "user.name", "T"], cwd=path, check=True, capture_output=True)
    (path / "README.md").write_text("x\n")
    subprocess.run(["git", "add", "."], cwd=path, check=True, capture_output=True)
    subprocess.run(["git", "commit", "-m", "init"], cwd=path, check=True, capture_output=True)
    if config_text:
        (path / ".fno").mkdir()
        (path / ".fno" / "config.toml").write_text(config_text, encoding="utf-8")
    return path


@pytest.fixture(autouse=True)
def _sole_config(tmp_path_factory, monkeypatch):
    """No global config leak; tests that need repo config rely on the repo."""
    monkeypatch.setenv(
        "FNO_GLOBAL_SETTINGS_PATH", str(tmp_path_factory.mktemp("iso") / "none.yaml")
    )
    monkeypatch.delenv("FNO_CONFIG", raising=False)
    monkeypatch.delenv("FNO_WORKTREE_POLICY", raising=False)
    yield


@requires_rust
def test_rust_seam_returns_pin_for_foreign_cwd(tmp_path, monkeypatch, capsys):
    """The Rust route execs before cmd_spawn, so the seam computes the pin for
    an explicit foreign --cwd and route_to_rust merges it into the exec env."""
    from fno.agents.rust_runtime import _worktree_policy_pin_at_seam

    caller = _make_repo(tmp_path / "caller")
    target = _make_repo(tmp_path / "target")
    monkeypatch.chdir(caller)
    monkeypatch.delenv("FNO_WORKTREE_POLICY", raising=False)
    pin = _worktree_policy_pin_at_seam(
        ["spawn", "--cwd", str(target), "do the thing"],
    )
    import os

    assert pin == {"FNO_WORKTREE_POLICY": "never"}
    assert "FNO_WORKTREE_POLICY" not in os.environ  # compute only; exec exports
    assert "undeclared repo" in capsys.readouterr().err


