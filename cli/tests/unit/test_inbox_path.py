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


def test_inbox_path_no_vault_prefers_existing_internal(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """Back-compat: a pre-existing internal/ inbox is kept when no vault is set.

    A non-vault repo that already captured fu-* items to the old
    internal/fno/backlog/inbox.md keeps using it, so an upgrade never
    silently strands those items (codex review, PR #424).
    """
    from fno.paths_testing import use_tmpdir
    use_tmpdir(monkeypatch, tmp_path)
    legacy = tmp_path / "internal" / "fno" / "backlog" / "inbox.md"
    legacy.parent.mkdir(parents=True)
    legacy.write_text("# existing captures\n", encoding="utf-8")

    from fno.paths import inbox_path
    result = inbox_path(project_root=tmp_path)
    assert result == legacy.resolve()


def test_inbox_path_honors_post_merge_parking_lot_path(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """config.post_merge.parking_lot_path (the producer's per-project queue) drives
    the capture-tier resolver, so add/list/tidy and /fno:ship pr merged all point
    at ONE file in every repo - not the fno-area default, which would make
    producer-written items invisible to the read commands (codex review, PR #434).
    """
    from fno.paths_testing import use_tmpdir
    settings = use_tmpdir(monkeypatch, tmp_path)
    settings.write_text(
        "schema_version: 1\n"
        "config:\n"
        f"  state_dir: {tmp_path}/.fno/\n"
        "  obsidian:\n"
        "    enabled: true\n"
        f"    vault: {tmp_path}/vault\n"
        "  post_merge:\n"
        "    parking_lot_path: internal/etl/backlog/parking-lot.md\n",
        encoding="utf-8",
    )
    import fno.paths as paths_mod

    result = paths_mod.inbox_path(project_root=tmp_path)
    assert result == (tmp_path / "internal/etl/backlog/parking-lot.md").resolve()


def test_inbox_path_explicit_override_beats_post_merge(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """config.paths.inbox_path (explicit capture-tier override) wins over
    config.post_merge.parking_lot_path when both are set."""
    from fno.paths_testing import use_tmpdir
    settings = use_tmpdir(monkeypatch, tmp_path)
    custom = tmp_path / "custom" / "my-inbox.md"
    settings.write_text(
        "schema_version: 1\n"
        "config:\n"
        f"  state_dir: {tmp_path}/.fno/\n"
        "  paths:\n"
        f"    inbox_path: {custom}\n"
        "  post_merge:\n"
        "    parking_lot_path: internal/etl/backlog/parking-lot.md\n",
        encoding="utf-8",
    )
    import fno.paths as paths_mod

    result = paths_mod.inbox_path(project_root=tmp_path)
    assert result == custom.resolve()


def test_explicit_project_root_loads_that_projects_capture_config(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """Cross-project readers resolve each repository's own parking-lot path."""
    current = tmp_path / "current"
    sibling = tmp_path / "sibling"
    for repo, configured in (
        (current, "internal/current/parking-lot.md"),
        (sibling, "internal/sibling/parking-lot.md"),
    ):
        config = repo / ".fno" / "config.toml"
        config.parent.mkdir(parents=True)
        config.write_text(
            f'[post_merge]\nparking_lot_path = "{configured}"\n',
            encoding="utf-8",
        )

    monkeypatch.setenv("FNO_REPO_ROOT", str(current))
    import fno.paths as paths_mod


    result = paths_mod.inbox_path(project_root=sibling)

    assert result == (sibling / "internal/sibling/parking-lot.md").resolve()
