from __future__ import annotations

import subprocess
from pathlib import Path

import pytest


def _git(cwd: Path, *args: str) -> None:
    subprocess.run(["git", *args], cwd=cwd, check=True, capture_output=True, text=True)


@pytest.mark.dev_build
def test_lead_state_root_from_linked_worktree_is_canonical(tmp_path: Path) -> None:
    main = tmp_path / "main"
    main.mkdir()
    _git(main, "init", "-q")
    _git(main, "config", "user.email", "test@example.com")
    _git(main, "config", "user.name", "Test")
    _git(main, "commit", "--allow-empty", "-qm", "init")
    linked = tmp_path / "linked"
    _git(main, "worktree", "add", "-q", str(linked), "-b", "feature")

    from fno.lead.state import lead_state_root
    from fno.paths import space_dir

    # The coordination root is the repo's SPACE: canonical-keyed, so
    # both worktree spellings answer one path, and no checkout holds a role.
    expected = space_dir(main)
    assert lead_state_root(linked) == expected
    assert lead_state_root(main) == expected
