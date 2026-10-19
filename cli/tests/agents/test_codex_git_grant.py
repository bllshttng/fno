"""Positive tests for the Codex git-common-dir grant and registry receipt."""
from __future__ import annotations

import subprocess
from pathlib import Path

from fno.agents.harnesses import codex


def _git(cwd: Path, *args: str) -> str:
    result = subprocess.run(
        ["git", "-C", str(cwd), *args],
        check=True,
        capture_output=True,
        text=True,
    )
    return result.stdout.strip()


def _linked_worktree(tmp_path: Path) -> tuple[Path, str]:
    repo = tmp_path / "repo"
    repo.mkdir()
    _git(repo, "init", "--quiet")
    _git(repo, "config", "user.email", "test@example.com")
    _git(repo, "config", "user.name", "Test")
    (repo / "README").write_text("base\n", encoding="utf-8")
    _git(repo, "add", "README")
    _git(repo, "commit", "--quiet", "-m", "base")
    linked = tmp_path / "linked"
    _git(repo, "worktree", "add", "--quiet", "-b", "linked", str(linked), "HEAD")
    return linked, _git(linked, "rev-parse", "--path-format=absolute", "--git-common-dir")


def test_python_grant_is_the_common_dir_for_a_linked_worktree(tmp_path: Path) -> None:
    linked, common = _linked_worktree(tmp_path)
    grant = codex.git_writable_args(linked)

    assert grant == ["--add-dir", common]
    assert linked.joinpath(".git").is_file()
    assert str(linked / ".git") not in grant

