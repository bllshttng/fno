"""Tests for fno.paths typed resolver.

Task 1.2: Create paths.py typed resolver with template substitution.
Task 1.4: Round out coverage for all AC items.

All tests use tmp_path + monkeypatch isolation. An autouse fixture pins
FNO_REPO_ROOT to tmp_path so resolve_repo_root() is isolated
(feedback_fno_repo_root_leaks_between_tests memory entry).
"""
from __future__ import annotations

from pathlib import Path
from typing import Generator

import pytest


# ---------------------------------------------------------------------------
# Autouse fixture: pin FNO_REPO_ROOT and clear caches before each test
# ---------------------------------------------------------------------------


@pytest.fixture(autouse=True)
def _isolate(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> Generator[None, None, None]:
    """Isolate each test: reset caches and pin repo root + settings."""
    # Pin FNO_REPO_ROOT so resolve_repo_root() doesn't wander
    monkeypatch.setenv("FNO_REPO_ROOT", str(tmp_path))
    # Clear settings cache so monkeypatched env takes effect
    monkeypatch.delenv("FNO_CONFIG", raising=False)
    # Clear paths caches (resolve_repo_root now @cached)
    yield
    # Clear again after test to avoid pollution
def _write_settings(tmp_path: Path, content: str) -> Path:
    """Write a settings.yaml to tmp_path and return its path."""
    f = tmp_path / "settings.yaml"
    f.write_text(content, encoding="utf-8")
    return f


def _set_settings(monkeypatch: pytest.MonkeyPatch, tmp_path: Path, content: str) -> None:
    """Write a settings.yaml and wire it via FNO_CONFIG."""
    settings_file = _write_settings(tmp_path, content)
    monkeypatch.setenv("FNO_CONFIG", str(settings_file))


# ---------------------------------------------------------------------------
# ab-fe825805 change 4: FNO_REPO_ROOT foreign-project overload warning
# ---------------------------------------------------------------------------


# ---------------------------------------------------------------------------
# AC1-FR: Cache lifetime (same object on second call)
# ---------------------------------------------------------------------------


# ---------------------------------------------------------------------------
# AC1-EDGE: Unknown {foo} variable rejected at resolve time
# ---------------------------------------------------------------------------


def test_unknown_template_variable_rejected(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """AC1-EDGE: {foo} in a path raises a hard error at resolve time."""
    _set_settings(
        monkeypatch,
        tmp_path,
        "schema_version: 1\nconfig:\n  state_dir: '/home/{foo}/fno'\n",
    )

    from fno.paths import state_dir

    with pytest.raises(Exception, match=r"\{foo\}|unknown.*variable|unrecognized"):
        state_dir()


# ---------------------------------------------------------------------------
# AC1-EDGE: postmortems_dir, memory_dir all derive from state_dir
# ---------------------------------------------------------------------------


# ---------------------------------------------------------------------------
# x-2e75: stable project-folder identity for internal/<project>/ paths.
# When config.project.id is unset, derive the folder name from the git remote
# (stable across worktrees/clones) instead of the checkout basename, which
# sprawls N stray internal/<name>/ folders in a shared vault. Sanitized so a
# derived or configured name can never escape internal/<project>/.
# ---------------------------------------------------------------------------


_VAULT_SETTINGS = (
    "schema_version: 1\nconfig:\n"
    "  obsidian:\n    enabled: true\n    vault: '{vault}'\n"
)


def test_traversal_project_id_rejected(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """Scenario 4: a traversal-bearing config.project.id is rejected (raises)."""
    vault = tmp_path / "vault"
    _set_settings(
        monkeypatch,
        tmp_path,
        _VAULT_SETTINGS.format(vault=vault) + "  project:\n    id: '../../etc'\n",
    )

    from fno.paths import handoffs_dir

    with pytest.raises(ValueError):
        handoffs_dir(project_root=tmp_path)


# ---------------------------------------------------------------------------
# x-fc25: FNO_STATE_DIR - the pinned carrier for the state root
# ---------------------------------------------------------------------------


def test_state_dir_honors_fno_state_dir_env(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """The env carrier moves the state root ahead of the config default.

    The state-root seal pins this var around a forwarded HOME, so a worker on a
    non-claude oauth_dir account resolves the same graph.json its parent did.
    """
    _set_settings(monkeypatch, tmp_path, "schema_version: 1\n")
    monkeypatch.setenv("FNO_STATE_DIR", str(tmp_path / "pinned"))

    from fno.paths import graph_json, state_dir

    assert state_dir() == (tmp_path / "pinned").resolve()
    assert graph_json() == (tmp_path / "pinned").resolve() / "db" / "graph.json"
