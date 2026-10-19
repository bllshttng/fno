"""Settings are deep-merged across project-local and per-user global files.

Before this change the loader used the first candidate file that parsed and
ignored every lower-priority file, so once a repo had a .fno/settings.yaml
the per-user global was never consulted for Python-modeled keys. These tests
pin the resolver roots and drive the global candidate via
FNO_GLOBAL_SETTINGS_PATH to exercise the merge directly, without standing up
real git worktrees.
"""
from __future__ import annotations

from pathlib import Path

import pytest


@pytest.fixture(autouse=True)
def _isolate(monkeypatch: pytest.MonkeyPatch):
    monkeypatch.delenv("FNO_CONFIG", raising=False)
    monkeypatch.delenv("FNO_REPO_ROOT", raising=False)
    yield


def _write(path: Path, body: str) -> Path:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(body, encoding="utf-8")
    return path


def _pin(
    monkeypatch: pytest.MonkeyPatch, *, project: Path, global_file: Path
) -> None:
    """Pin worktree==canonical to `project` and the global candidate to `global_file`.

    With worktree == canonical the candidate walk dedupes to
    [project/.fno/settings.yaml, global_file].
    """
    import fno.paths as paths_mod

    monkeypatch.setattr(paths_mod, "resolve_repo_root", lambda: project)
    monkeypatch.setattr(paths_mod, "resolve_canonical_repo_root", lambda: project)
    monkeypatch.setenv("FNO_GLOBAL_SETTINGS_PATH", str(global_file))


# ---------------------------------------------------------------------------
# Deep merge through the full loader + model
# ---------------------------------------------------------------------------


def test_project_overrides_global_per_key_global_fills_rest(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """Headline: global holds shared defaults; project overrides only its deltas.

    Global sets obsidian.{enabled,vault} and post_merge.parking_lot_path; the
    project overrides post_merge.parking_lot_path. The merged result keeps
    obsidian from global and takes parking_lot_path from the project.
    """
    project_root = tmp_path / "repo"
    global_file = tmp_path / "global" / "settings.yaml"
    _write(
        global_file,
        "schema_version: 1\n"
        "config:\n"
        "  obsidian:\n"
        "    enabled: true\n"
        "    vault: myvault\n"
        "  post_merge:\n"
        "    parking_lot_path: internal/shared/parking-lot.md\n",
    )
    _write(
        project_root / ".fno" / "settings.yaml",
        "schema_version: 1\n"
        "config:\n"
        "  post_merge:\n"
        "    parking_lot_path: internal/web/backlog/parking-lot.md\n",
    )

    _pin(monkeypatch, project=project_root, global_file=global_file)

    from fno.config import load_settings

    s = load_settings()
    # Project wins for the key it sets.
    assert s.post_merge.parking_lot_path == "internal/web/backlog/parking-lot.md"
    # Global fills keys the project did not set.
    assert s.obsidian.enabled is True
    assert s.obsidian.vault == "myvault"


def test_corrupt_project_still_merges_global(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """A corrupt project file is skipped; global values still apply."""
    project_root = tmp_path / "repo"
    global_file = tmp_path / "global" / "settings.yaml"
    _write(
        global_file,
        "schema_version: 1\nconfig:\n  post_merge:\n    parking_lot_path: internal/etl/backlog/parking-lot.md\n",
    )
    _write(
        project_root / ".fno" / "settings.yaml",
        ":::bad yaml:::\n  - broken: [unterminated",
    )

    _pin(monkeypatch, project=project_root, global_file=global_file)

    from fno.config import load_settings

    assert load_settings().post_merge.parking_lot_path == "internal/etl/backlog/parking-lot.md"


def test_loaded_from_is_highest_priority_file(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """loaded_from() anchors to the project file when both exist."""
    project_root = tmp_path / "repo"
    global_file = tmp_path / "global" / "settings.yaml"
    _write(global_file, "schema_version: 1\nconfig:\n  plans_dir: g/\n")
    project_file = _write(
        project_root / ".fno" / "settings.yaml",
        "schema_version: 1\nconfig:\n  plans_dir: p/\n",
    )

    _pin(monkeypatch, project=project_root, global_file=global_file)

    from fno.config import load_settings, loaded_from

    load_settings()
    # Migrate converts the seeded settings.yaml to a flat config.toml sibling.
    assert loaded_from() == project_file.with_name("config.toml").resolve()


# ---------------------------------------------------------------------------
# _deep_merge unit behavior (no I/O)
# ---------------------------------------------------------------------------


def test_deep_merge_lists_replace_not_concatenate() -> None:
    from fno.config import _deep_merge

    out = _deep_merge({"reviewers": ["gemini", "codex"]}, {"reviewers": ["claude"]})
    assert out == {"reviewers": ["claude"]}


def test_deep_merge_none_does_not_overwrite_dict() -> None:
    """An empty override block (parses as None) must not null an existing dict.

    A bare `config:` line in a higher-priority file would otherwise overwrite
    the merged config with None and crash Pydantic validation (Gemini HIGH).
    """
    from fno.config import _deep_merge

    base = {"config": {"obsidian": {"enabled": True}}}
    override = {"config": None}
    assert _deep_merge(base, override) == {"config": {"obsidian": {"enabled": True}}}
