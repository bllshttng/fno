"""Tests for Codex PR #264 round 3 fixes (findings B, D).

Finding A's paths.sh tests moved to Rust with the emitter
(crates/fno-agents/src/paths_cli.rs, verify port).
Finding B: dead-line regression in health_monitor.py and collision.py; fail-open.
Finding D: plain-relative predicate in paths.py rejects env vars anywhere.
"""
from __future__ import annotations

from pathlib import Path
from typing import Generator

import pytest


# ---------------------------------------------------------------------------
# Autouse fixture: cache isolation (same pattern as test_paths.py)
# ---------------------------------------------------------------------------


@pytest.fixture(autouse=True)
def _isolate(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> Generator[None, None, None]:
    monkeypatch.setenv("FNO_REPO_ROOT", str(tmp_path))
    monkeypatch.delenv("FNO_CONFIG", raising=False)
    yield


# ===========================================================================
# Finding B: health_monitor.py and collision.py fail-open on settings error
# ===========================================================================


def test_collision_load_thresholds_failsopen_on_invalid_settings(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """AC2-HP (Finding B): _load_thresholds fails open when config_file() triggers validation error.

    When user_settings=None (default), _load_thresholds calls _paths.config_file()
    which triggers full Pydantic model validation. If settings.yaml is invalid,
    it raises ValidationError. _load_thresholds must catch that and return defaults.
    """
    from fno.graph.collision import _load_thresholds, _default_thresholds_loaded

    # Write an invalid settings.yaml - glob char in state_dir fails Pydantic validation
    bad_settings = tmp_path / "bad-collision-settings.yaml"
    bad_settings.write_text(
        "schema_version: 1\nconfig:\n  state_dir: '/home/*/fno'\n",
        encoding="utf-8",
    )
    # Wire FNO_CONFIG to point at the bad settings file
    monkeypatch.setenv("FNO_CONFIG", str(bad_settings))
    # Clear caches so the new bad settings are picked up

    # Call with user_settings=None (default) so _paths.config_file() is called
    # Should not raise; should return defaults
    result = _load_thresholds(
        project_settings=tmp_path / "nonexistent.yaml",
        user_settings=None,  # triggers _paths.config_file() call
    )
    assert isinstance(result, dict), "_load_thresholds must return a dict even on bad settings"
    assert result["high_count"] == _default_thresholds_loaded()["high_count"], (
        "result must contain default high_count when settings are invalid"
    )
