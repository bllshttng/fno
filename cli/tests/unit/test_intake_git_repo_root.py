"""Tests for graph._intake._git_repo_root.

`_git_repo_root` now delegates the worktree-list parsing to the shared
`paths.resolve_canonical_worktree` (covered by test_resolve_canonical_worktree.py);
here we test only the delegation + the `--show-toplevel` fallback when the
helper finds no usable working tree (bare-only / separate-git-dir).
(ab-91a004af worktree-resolution; originally ab-d6cf58be.)
"""
from __future__ import annotations

import os

import pytest


def test_falls_back_to_show_toplevel_when_helper_none(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """Helper returns None (bare-only / separate-git-dir with no linked
    checkout) -> fall back to `git rev-parse --show-toplevel`."""
    from fno.graph import _intake

    monkeypatch.setattr(_intake._paths, "resolve_canonical_worktree", lambda *a, **k: None)
    monkeypatch.setattr(
        _intake.subprocess, "check_output", lambda *a, **k: "/some/checkout\n"
    )
    assert _intake._git_repo_root() == os.path.normpath("/some/checkout")
