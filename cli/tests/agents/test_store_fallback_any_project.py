"""Harness-store healing resolves from any project.

The project confinement retired 2026-10-25 (ruling d-460ea8c7) refused a hit
whose cwd sat outside the caller's project and taught callers to retry with
the retired --cross-project flag. The heal is machine-wide now: a full session
id, a short id, or a name resolves from any project, and the one guard left is
uniqueness (more than one store hit refuses with the candidate list).

This test pins the machine-wide behavior on the Python healer; the native
healer's adoption runs in tests/test-agents-heal-token.sh.
"""
from __future__ import annotations

import json
import subprocess
from pathlib import Path

import pytest

from fno.agents.store_fallback import StoreHit, heal_from_harness_store


def _git_repo(path: Path) -> Path:
    path.mkdir(parents=True, exist_ok=True)
    subprocess.run(["git", "init", "-q"], cwd=path, check=True)
    subprocess.run(["git", "config", "user.email", "t@t.t"], cwd=path, check=True)
    subprocess.run(["git", "config", "user.name", "t"], cwd=path, check=True)
    (path / "x").write_text("x")
    subprocess.run(["git", "add", "-A"], cwd=path, check=True)
    subprocess.run(["git", "commit", "-q", "-m", "i"], cwd=path, check=True)
    return path


def _git_worktree(main: Path, dest: Path) -> Path:
    subprocess.run(
        ["git", "-C", str(main), "worktree", "add", "-q", str(dest)],
        check=True,
    )
    return dest


@pytest.fixture
def two_repos(tmp_path):
    """A main repo, a worktree of it, and an unrelated foreign repo."""
    main = _git_repo(tmp_path / "footnote")
    worktree = _git_worktree(main, tmp_path / "wt")
    foreign = _git_repo(tmp_path / "regready")
    return main, worktree, foreign


# ---------------------------------------------------------------------------
# heal_from_harness_store: a foreign-repo hit heals with no flag
# ---------------------------------------------------------------------------


def _stub_hits(monkeypatch, hits):
    monkeypatch.setattr(
        "fno.agents.store_fallback.complete_store_hits",
        lambda token, **kw: list(hits),
    )


def test_heal_adopts_a_foreign_hit_without_a_flag(two_repos, monkeypatch, tmp_path):
    """The retired confinement refused exactly this shape behind --cross-project."""
    main, _, foreign = two_repos
    sid = "ab12cdef-0000-0000-0000-000000000001"
    _stub_hits(monkeypatch, [StoreHit("claude", sid, str(foreign))])
    entry = heal_from_harness_store(
        "ab12cdef",
        registry_path=tmp_path / "registry.json",
    )
    assert entry is not None
    assert entry.harness_session_id == sid


# ---------------------------------------------------------------------------
# Binary: the exec'd `fno agents heal-token` resolves a foreign session
# ---------------------------------------------------------------------------


def _seed_claude_transcript(projects_dir: Path, session_id: str, cwd: Path) -> None:
    """Drop a claude-shaped transcript whose recorded cwd is ``cwd``."""
    slot = projects_dir / "footnote-test-project"
    slot.mkdir(parents=True, exist_ok=True)
    (slot / f"{session_id}.jsonl").write_text(
        json.dumps({"cwd": str(cwd)}) + "\n"
    )
