"""Tests for the config.auto_continue schema block.

"Disabled by default" (AC2-HP) and "malformed fails safe" (AC2-ERR) are the
load-bearing invariants. The arming resolver lives in the native advance command
and carries its own tests there.
"""
from __future__ import annotations

from pathlib import Path

import pytest


# ---------------------------------------------------------------------------
# Helpers (mirrors test_config_handoff.py)
# ---------------------------------------------------------------------------


def _write_settings(tmp_path: Path, content: str) -> Path:
    settings_dir = tmp_path / ".fno"
    settings_dir.mkdir(parents=True, exist_ok=True)
    settings_file = settings_dir / "settings.yaml"
    settings_file.write_text(content, encoding="utf-8")
    return settings_file


def _load(tmp_path: Path, monkeypatch: pytest.MonkeyPatch, content: str):
    monkeypatch.delenv("FNO_CONFIG", raising=False)
    settings_file = _write_settings(tmp_path, content)
    monkeypatch.setenv("FNO_CONFIG", str(settings_file))

    from fno import config as config_mod

    return config_mod.load_settings()


# ---------------------------------------------------------------------------
# Schema: default + malformed fail-safe
# ---------------------------------------------------------------------------


def test_auto_continue_default_disabled(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    """AC2-HP: with no config.auto_continue block, enabled is False."""
    settings = _load(tmp_path, monkeypatch, "schema_version: 1\n")
    assert settings.auto_continue.enabled is False


def test_auto_continue_malformed_block_fails_safe(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """AC2-ERR: a non-boolean enabled degrades to disabled, never raises."""
    settings = _load(
        tmp_path,
        monkeypatch,
        "config:\n  auto_continue:\n    enabled: not-a-bool\n",
    )
    assert settings.auto_continue.enabled is False
