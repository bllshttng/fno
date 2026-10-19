"""Tests for inbox store.py - resolve_project() (Task 2.2)."""
from __future__ import annotations

from pathlib import Path

import pytest
import yaml


def _write_settings(path: Path, project: str) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(yaml.dump({"project": project}))


def test_ac1_hp_walk_up(tmp_path):
    """AC1-HP: walks up directory tree to find settings.yaml."""
    from fno.inbox.store import resolve_project

    # settings.yaml at top level; cwd at nested subdir
    settings = tmp_path / ".fno" / "settings.yaml"
    _write_settings(settings, "top-level-project")

    nested = tmp_path / "src" / "module"
    nested.mkdir(parents=True)

    result = resolve_project(cwd=nested)
    assert result == "top-level-project"


def test_ac2_err_exact_message(tmp_path):
    """AC2-ERR: error message is exactly the canonical string."""
    from fno.inbox.store import ProjectIdentificationError, resolve_project

    with pytest.raises(ProjectIdentificationError) as exc_info:
        resolve_project(cwd=tmp_path)

    assert str(exc_info.value) == "set 'project' in .fno/config.toml or pass --from"


def test_config_project_id_preferred_over_deprecated_top_level(tmp_path):
    """config.project.id is canonical and wins over deprecated top-level project.id."""
    from fno.inbox.store import resolve_project

    settings = tmp_path / ".fno" / "settings.yaml"
    settings.parent.mkdir(parents=True, exist_ok=True)
    settings.write_text(
        yaml.dump({
            "project": {"id": "old-name"},
            "config": {"project": {"id": "canonical-name"}},
        })
    )

    result = resolve_project(cwd=tmp_path)
    assert result == "canonical-name"
