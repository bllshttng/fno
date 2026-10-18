"""Plugin-script resolution + the persisted pointer (#2).

The bug: `fno do target init` / `fno gate set` could not find their plugin scripts
when run from a foreign project with no env hint. `fno` is a uv-tool install
whose wheel does not carry hooks/, and CLAUDE_PLUGIN_ROOT is not propagated to
arbitrary `fno` subprocesses, so env + package-relative both miss and the agent
had to export FNO_REPO_ROOT by hand. resolve_plugin_script falls back to the
persisted ~/.fno/install/plugin-root pointer, written only by the session-start hook
from an installed or canonical root, as the env-less source.

The resolver reads os.environ fresh on every call (no lru_cache), so tests just
monkeypatch FNO_HOME / the plugin-root env vars - nothing to clear.
"""

from __future__ import annotations

import subprocess
from pathlib import Path

import pytest

from fno import paths


def _make_plugin(root: Path) -> Path:
    (root / ".claude-plugin").mkdir(parents=True)
    (root / ".claude-plugin" / "plugin.json").write_text("{}")
    (root / "hooks" / "helpers").mkdir(parents=True)
    (root / "hooks" / "helpers" / "init-target-state.sh").write_text("#!/bin/bash\n")
    (root / "scripts" / "lib").mkdir(parents=True)
    (root / "scripts" / "lib" / "set-gate.sh").write_text("#!/bin/bash\n")
    return root


@pytest.fixture
def isolated_home(tmp_path, monkeypatch):
    """FNO_HOME -> tmp dir; all plugin-root env hints cleared."""
    home = tmp_path / "fno-home"
    (home / "install").mkdir(parents=True, exist_ok=True)
    monkeypatch.setenv("FNO_HOME", str(home))
    monkeypatch.delenv("CLAUDE_PLUGIN_ROOT", raising=False)
    monkeypatch.delenv("CODEX_PLUGIN_ROOT", raising=False)
    monkeypatch.delenv("FNO_REPO_ROOT", raising=False)
    monkeypatch.delenv("CONDUCTOR_ROOT_PATH", raising=False)
    return home


def _pointer_path(home: Path) -> Path:
    path = home / "install" / "plugin-root"
    path.parent.mkdir(parents=True, exist_ok=True)
    return path


def test_env_hint_resolves_without_writing_pointer(tmp_path, monkeypatch, isolated_home):
    """The hook is the sole pointer writer, so an env resolve writes none
    (an env-hint root is just as often a worker's linked worktree)."""
    plugin = _make_plugin(tmp_path / "plugin")
    monkeypatch.setenv("CLAUDE_PLUGIN_ROOT", str(plugin))
    got = paths.resolve_plugin_script("scripts/lib/set-gate.sh")
    assert got == plugin / "scripts" / "lib" / "set-gate.sh"
    assert not _pointer_path(isolated_home).exists()


def test_resolve_falls_to_persisted_when_env_and_pkg_miss(tmp_path, monkeypatch, isolated_home):
    """The env-less foreign-project case: only the persisted pointer remains."""
    plugin = _make_plugin(tmp_path / "plugin")
    isolated_home.mkdir(parents=True, exist_ok=True)
    _pointer_path(isolated_home).write_text(str(plugin) + "\n")
    # Force package-relative (the in-tree repo root) to NOT count as a plugin,
    # so resolution must reach the persisted pointer.
    monkeypatch.setattr(paths, "_is_plugin_root", lambda r: Path(r) == plugin)
    got = paths.resolve_plugin_script("scripts/lib/set-gate.sh")
    assert got == plugin / "scripts" / "lib" / "set-gate.sh"


def _git(cwd: Path, *args: str) -> None:
    subprocess.run(["git", "-C", str(cwd), *args], check=True, capture_output=True, text=True)


def _make_worktree_plugin(tmp_path: Path) -> tuple[Path, Path]:
    """A canonical git checkout carrying the plugin + one linked worktree that
    also carries it (mirrors how every footnote worktree shares tracked files).
    Returns (canonical_root, worktree_root)."""
    canon = _make_plugin(tmp_path / "canon")
    _git(canon, "init", "-q")
    _git(canon, "-c", "user.email=t@t", "-c", "user.name=t", "add", "-A")
    _git(canon, "-c", "user.email=t@t", "-c", "user.name=t", "commit", "-q", "-m", "init")
    wt = tmp_path / "wt"
    _git(canon, "worktree", "add", "-q", str(wt))
    return canon, wt


def test_persisted_worktree_pointer_self_heals_to_canonical(tmp_path, isolated_home):
    """The session-start hook writes the CURRENT worktree to the pointer; the
    reader canonicalizes it so resolution runs the canonical init script and a
    cold-start receipt stays clean. This is the sole canonicalization point."""
    canon, wt = _make_worktree_plugin(tmp_path)
    isolated_home.mkdir(parents=True, exist_ok=True)
    _pointer_path(isolated_home).write_text(str(wt) + "\n")
    got = paths._read_persisted_plugin_root()
    assert got is not None and got.resolve() == canon.resolve()
