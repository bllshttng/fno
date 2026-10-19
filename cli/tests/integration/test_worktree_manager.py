"""Integration tests for scripts/lib/worktree-manager.sh.

Cover the AC4-EDGE seeds called out in the worktree-management-overhaul plan:

- create honors worktree_base from config and is idempotent on re-call
- setup copies declared env files and skips install when the lockfile hash matches
- cleanup and migrate --auto never remove a worktree with status: IN_PROGRESS,
  and migrate treats an unreadable state file as live

Tests use a temp git repo + temp $HOME so we never touch real worktrees.
"""
from __future__ import annotations

import json
import os
import subprocess
import textwrap
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


def write_settings(home: Path, settings_yaml: str) -> Path:
    """Write a global flat config.toml under $HOME/.fno/ (parses the YAML fixture
    text, lifts any legacy config: wrapper, strips None, emits TOML)."""
    import tomli_w
    import yaml

    fno = home / ".fno"
    fno.mkdir(parents=True, exist_ok=True)
    data = yaml.safe_load(settings_yaml) or {}
    cfg = data.get("config")
    if isinstance(cfg, dict):
        data = {**{k: v for k, v in data.items() if k != "config"}, **cfg}

    def _sn(x):
        if isinstance(x, dict):
            return {k: _sn(v) for k, v in x.items() if v is not None}
        if isinstance(x, list):
            return [_sn(v) for v in x]
        return x

    path = fno / "config.toml"
    path.write_text(tomli_w.dumps(_sn(data)))
    return path




# ---------------------------------------------------------------
# create verb
# ---------------------------------------------------------------


def test_create_uses_worktree_base_from_settings_and_is_idempotent(tmp_repo, isolated_env):
    """create against a project with custom worktree_base produces path under
    it; re-calling create with the same slug returns existing=true, no error."""
    base = Path(isolated_env["HOME"]) / "conductor" / "workspaces" / "myproj"
    write_settings(Path(isolated_env["HOME"]), textwrap.dedent(f"""
        work:
          workspaces:
            ws1:
              projects:
                - name: myproj
                  worktree_base: {base}
    """))
    result = run_wtm("create", "myproj", "feature-x", cwd=tmp_repo, env=isolated_env)
    assert result.returncode == 0, f"stderr: {result.stderr}\nstdout: {result.stdout}"
    payload = parse_json(result.stdout)
    assert payload["status"] == "ok"
    assert payload["existing"] is False
    assert payload["path"] == f"{base}/feature-x"
    assert payload["branch"] == "feature/feature-x"
    assert Path(payload["path"]).is_dir()

    again = run_wtm("create", "myproj", "feature-x", cwd=tmp_repo, env=isolated_env)
    assert again.returncode == 0, again.stderr
    payload2 = parse_json(again.stdout)
    assert payload2["existing"] is True
    assert payload2["path"] == payload["path"]


# ---------------------------------------------------------------
# setup verb
# ---------------------------------------------------------------


def test_setup_copies_env_files_and_skips_install_on_lockfile_match(tmp_repo, isolated_env):
    """First setup copies declared env files and runs install; a second run
    with the same lockfile hash hits the cache."""
    write_settings(Path(isolated_env["HOME"]), textwrap.dedent("""
        config:
          worktree:
            env_files:
              - .env.test
            setup_command: "echo setup-ran"
    """))
    (tmp_repo / "package-lock.json").write_text('{"name": "test"}')
    (tmp_repo / ".env.test").write_text("FOO=bar\n")

    create = run_wtm("create", ".", "cache-test", cwd=tmp_repo, env=isolated_env)
    assert create.returncode == 0, create.stderr
    wt_path = Path(parse_json(create.stdout)["path"])
    # The created worktree shares the lockfile via git checkout
    (wt_path / "package-lock.json").write_text('{"name": "test"}')

    first = run_wtm("setup", str(wt_path), cwd=tmp_repo, env=isolated_env)
    assert first.returncode == 0, first.stderr
    p1 = parse_json(first.stdout)
    assert p1["cached"] is False
    assert p1["install"] == "ok"
    assert int(p1["env_files_copied"]) >= 1
    assert (wt_path / ".env.test").read_text() == "FOO=bar\n"

    second = run_wtm("setup", str(wt_path), cwd=tmp_repo, env=isolated_env)
    assert second.returncode == 0, second.stderr
    p2 = parse_json(second.stdout)
    assert p2["cached"] is True, f"expected cache hit, got: {p2}"


# ---------------------------------------------------------------
# cleanup verb (lighter coverage - delegates to existing lifecycle script)
# ---------------------------------------------------------------


def test_cleanup_skips_in_progress_target_session(tmp_repo, isolated_env):
    """A worktree with status: IN_PROGRESS is never removed."""
    create = run_wtm("create", ".", "active-test", cwd=tmp_repo, env=isolated_env)
    wt_path = Path(parse_json(create.stdout)["path"])

    fnodir = wt_path / ".fno"
    fnodir.mkdir(parents=True, exist_ok=True)
    (fnodir / "target-state.md").write_text(textwrap.dedent("""
        ---
        status: IN_PROGRESS
        ---
    """))

    # Force-old by lying about commit time would require git plumbing; instead
    # we run cleanup with --older-than=0 so the worktree is "old enough" to
    # qualify, and verify the IN_PROGRESS sentinel keeps it safe.
    result = run_wtm(
        "cleanup", "--mode=all", "--older-than=0d",
        cwd=tmp_repo, env=isolated_env,
    )
    assert result.returncode == 0, result.stderr
    assert wt_path.is_dir(), "active target session worktree must be preserved"


# ---------------------------------------------------------------
# migrate verb
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
