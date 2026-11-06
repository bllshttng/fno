"""Tests for fno.paths.inbox_path (Wave 1.1).

Covers AC5-HP (resolves from settings, template vars expanded) and
AC5-EDGE (symlinked vault path resolves to the canonical target so
sibling worktrees share one file).

inbox_path() is the backlog *capture-tier* file resolver.
"""
from __future__ import annotations

from pathlib import Path
from typing import Generator

import pytest


@pytest.fixture(autouse=True)
def _isolate(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> Generator[None, None, None]:
    monkeypatch.setenv("FNO_REPO_ROOT", str(tmp_path))
    monkeypatch.delenv("FNO_CONFIG", raising=False)
    yield


def test_inbox_path_default_anchors_to_repo_root(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """Obsidian on, vault link present, no override -> canonical parking-lot.md."""
    from fno.paths_testing import use_tmpdir
    settings = use_tmpdir(monkeypatch, tmp_path)
    (tmp_path / "internal").mkdir()
    settings.write_text(
        "schema_version: 1\n"
        "config:\n"
        f"  state_dir: {tmp_path}/.fno/\n"
        "  obsidian:\n"
        "    enabled: true\n"
        f"    vault: {tmp_path}/vault\n",
        encoding="utf-8",
    )
    import fno.paths as paths_mod

    result = paths_mod.inbox_path(project_root=tmp_path)
    assert result == (tmp_path / "internal/fno/backlog/parking-lot.md").resolve()


def test_inbox_path_obsidian_on_without_internal_stays_in_fno(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """Obsidian enabled globally, but this repo has no internal/ vault link.

    The global flag is machine-level consent, not per-project: a foreign repo
    touched by fno falls to .fno/backlog/parking-lot.md and never grows an
    internal/ directory of its own.
    """
    from fno.paths_testing import use_tmpdir
    settings = use_tmpdir(monkeypatch, tmp_path)
    settings.write_text(
        "schema_version: 1\n"
        "config:\n"
        f"  state_dir: {tmp_path}/.fno/\n"
        "  obsidian:\n"
        "    enabled: true\n"
        f"    vault: {tmp_path}/vault\n",
        encoding="utf-8",
    )
    import fno.paths as paths_mod

    result = paths_mod.inbox_path(project_root=tmp_path)
    assert result == (tmp_path / ".fno/backlog/parking-lot.md").resolve()
    assert not (tmp_path / "internal").exists()
