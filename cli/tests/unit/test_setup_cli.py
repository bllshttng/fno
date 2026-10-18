"""Tests for fno.setup_cli (fno setup migrate-paths).

migrate_paths_cmd() must pass the active state_dir as settings_root, or the
CLI surface writes to ~/.fno regardless of config.state_dir.
"""

from __future__ import annotations

from pathlib import Path
from typing import Generator
from unittest.mock import patch

import pytest


@pytest.fixture(autouse=True)
def _isolate(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> Generator[None, None, None]:
    """Clear caches and suppress auto-migration before each test."""
    monkeypatch.setenv("FNO_REPO_ROOT", str(tmp_path))
    monkeypatch.setenv("FNO_SKIP_MIGRATION", "1")
    monkeypatch.delenv("FNO_CONFIG", raising=False)
    from fno import config as config_mod

    if hasattr(config_mod, "_loaded_from"):
        config_mod._loaded_from = None
    yield
    if hasattr(config_mod, "_loaded_from"):
        config_mod._loaded_from = None


# ---------------------------------------------------------------------------
# AC-E-HP: fno setup migrate-paths honors active state_dir
# ---------------------------------------------------------------------------


def test_migrate_paths_cmd_passes_state_dir_to_run_migration(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """AC-E-HP: migrate_paths_cmd() passes settings_root=_paths.state_dir() to run_migration.

    Finding E: without this, `fno setup migrate-paths` ignores config.state_dir
    and always writes to ~/.fno.

    Strategy: configure a custom state_dir, invoke migrate_paths_cmd via
    typer CliRunner, intercept run_migration, verify settings_root kwarg.
    """
    custom_state = tmp_path / "custom-fno-e"
    custom_state.mkdir()

    # Write settings pointing to custom state_dir
    fno_dir = tmp_path / ".fno"
    fno_dir.mkdir()
    settings_file = fno_dir / "settings.yaml"
    settings_file.write_text(
        f"schema_version: 1\nconfig:\n  state_dir: '{custom_state}'\n",
        encoding="utf-8",
    )
    monkeypatch.setenv("FNO_CONFIG", str(settings_file))

    # Clear caches so new config is picked up
    from fno import config as config_mod

    if hasattr(config_mod, "_loaded_from"):
        config_mod._loaded_from = None
    captured_calls: list[dict] = []

    def _mock_run_migration(**kwargs: object) -> int:
        captured_calls.append(dict(kwargs))
        return 0

    from typer.testing import CliRunner
    from fno.setup_cli import app

    with patch("fno.setup.migrate_paths.run_migration", side_effect=_mock_run_migration):
        runner = CliRunner(env={"COLUMNS": "240", "NO_COLOR": "1"})
        # 'fno setup' now has multiple subcommands (migrate-paths, post-merge),
        # so the command under test must be named explicitly.
        result = runner.invoke(app, ["migrate-paths"])

    assert result.exit_code == 0, f"Expected exit 0; got {result.exit_code}\n{result.output}"
    assert captured_calls, "migrate_paths_cmd must call run_migration"
    call_kwargs = captured_calls[0]

    assert "settings_root" in call_kwargs, (
        f"run_migration must be called with settings_root kwarg; got: {call_kwargs}"
    )
    assert call_kwargs["settings_root"] == custom_state, (
        f"settings_root must equal _paths.state_dir() ({custom_state!s}); "
        f"got: {call_kwargs['settings_root']}"
    )


# ---------------------------------------------------------------------------
# Finding E (P2, round 5+6): migration print_summary writes to stderr, not stdout
# ---------------------------------------------------------------------------
