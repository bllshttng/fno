"""Integration tests for scripts/lib/worktree-manager.sh.

migrate --auto removes a stale worktree, never one with status: IN_PROGRESS,
and treats an unreadable state file as live.

Tests use a temp git repo + temp $HOME so we never touch real worktrees.
"""
from __future__ import annotations

import json
import os
import subprocess
from pathlib import Path

import pytest


def _repo_root() -> Path:
    """Resolve the repo root via git, not via parents[N] - the latter
    silently breaks if the test file moves up or down a directory.
    """
    out = subprocess.check_output(
        ["git", "rev-parse", "--show-toplevel"],
        cwd=str(Path(__file__).parent),
        text=True,
    ).strip()
    return Path(out)


REPO_ROOT = _repo_root()
SCRIPT = REPO_ROOT / "scripts" / "lib" / "worktree-manager.sh"


# ---------------------------------------------------------------
# Fixtures
# ---------------------------------------------------------------


@pytest.fixture
def tmp_repo(tmp_path: Path) -> Path:
    """Initialize a minimal git repo with one commit on main."""
    subprocess.run(["git", "init", "-b", "main", str(tmp_path)],
                   check=True, capture_output=True)
    subprocess.run(["git", "config", "user.email", "test@test.com"],
                   cwd=tmp_path, check=True, capture_output=True)
    subprocess.run(["git", "config", "user.name", "T"],
                   cwd=tmp_path, check=True, capture_output=True)
    (tmp_path / "README.md").write_text("# test\n")
    subprocess.run(["git", "add", "."], cwd=tmp_path, check=True, capture_output=True)
    subprocess.run(["git", "commit", "-m", "init"],
                   cwd=tmp_path, check=True, capture_output=True)
    return tmp_path


@pytest.fixture
def isolated_env(tmp_path: Path, monkeypatch) -> dict[str, str]:
    """Point HOME at a clean temp dir so global ~/.fno/settings.yaml
    can be controlled per-test without touching the user's real settings.
    """
    fake_home = tmp_path / "home"
    fake_home.mkdir()
    monkeypatch.setenv("HOME", str(fake_home))
    return {"HOME": str(fake_home)}


def run_wtm(*args: str, cwd: Path | None = None, env: dict[str, str] | None = None,
            check: bool = False) -> subprocess.CompletedProcess:
    """Invoke worktree-manager.sh with the given arguments."""
    full_env = os.environ.copy()
    if env:
        full_env.update(env)
    return subprocess.run(
        ["bash", str(SCRIPT), *args],
        cwd=str(cwd) if cwd else None,
        env=full_env,
        capture_output=True,
        text=True,
        check=check,
    )


def parse_json(stdout: str) -> dict:
    # Take the LAST line that parses as JSON. The script may emit log lines
    # to stderr, but stdout should be the JSON payload alone.
    lines = [ln for ln in stdout.strip().splitlines() if ln.strip()]
    assert lines, "no stdout from worktree-manager (stdout was empty)"
    last = lines[-1]
    try:
        return json.loads(last)
    except json.JSONDecodeError as e:  # pragma: no cover - debug aid
        raise AssertionError(f"bad JSON: {last!r}\nfull stdout: {stdout!r}") from e


# ---------------------------------------------------------------
# migrate command
# ---------------------------------------------------------------


def test_migrate_auto_removes_stale_but_preserves_live(tmp_repo, isolated_env):
    """migrate --auto removes stale worktrees and preserves IN_PROGRESS ones.

    A worktree whose target-state.md is unreadable (chmod 000) is LIVE, not
    stale: an unreadable state file must never let migrate --auto delete an
    active session.
    """
    stale = run_wtm("create", ".", "stale-wt", cwd=tmp_repo, env=isolated_env)
    stale_path = Path(parse_json(stale.stdout)["path"])

    live = run_wtm("create", ".", "live-wt", cwd=tmp_repo, env=isolated_env)
    live_path = Path(parse_json(live.stdout)["path"])
    fnodir = live_path / ".fno"
    fnodir.mkdir(parents=True, exist_ok=True)
    (fnodir / "target-state.md").write_text("---\nstatus: IN_PROGRESS\n---\n")

    unreadable = run_wtm("create", ".", "unreadable-wt", cwd=tmp_repo, env=isolated_env)
    unreadable_path = Path(parse_json(unreadable.stdout)["path"])
    (unreadable_path / ".fno").mkdir(parents=True, exist_ok=True)
    state = unreadable_path / ".fno" / "target-state.md"
    state.write_text("---\nstatus: IN_PROGRESS\n---\n")

    try:
        state.chmod(0o000)
        result = run_wtm("migrate", "--auto", cwd=tmp_repo, env=isolated_env)
    finally:
        state.chmod(0o644)  # restore so pytest can clean up
    assert result.returncode == 0, result.stderr
    payload = parse_json(result.stdout)
    assert payload["status"] == "ok"
    assert not stale_path.is_dir(), "stale worktree should be removed by --auto"
    assert live_path.is_dir(), "IN_PROGRESS worktree must NEVER be removed"
    assert unreadable_path.is_dir(), "unreadable state must be treated as LIVE"
    assert int(payload["live"]) >= 2
