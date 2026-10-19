"""Tests for fno.config - Pydantic settings schema.

Task 1.1: Extend Pydantic config schema with state_dir, plans_dir,
paths block, obsidian block, validators.
"""
from __future__ import annotations

import logging
from pathlib import Path

import pytest


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------


def _write_settings(tmp_path: Path, content: str) -> Path:
    """Write a settings.yaml to tmp_path/.fno/ and return the path."""
    settings_dir = tmp_path / ".fno"
    settings_dir.mkdir(parents=True, exist_ok=True)
    settings_file = settings_dir / "settings.yaml"
    settings_file.write_text(content, encoding="utf-8")
    return settings_file


# ---------------------------------------------------------------------------
# AC1-ERR: Glob characters rejected
# ---------------------------------------------------------------------------


def test_glob_star_in_state_dir_rejected(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    """AC1-ERR: state_dir with '*' raises ValidationError at load time."""
    monkeypatch.delenv("FNO_CONFIG", raising=False)
    settings_file = _write_settings(
        tmp_path, "schema_version: 1\nconfig:\n  state_dir: '~/.fno/*'\n"
    )
    monkeypatch.setenv("FNO_CONFIG", str(settings_file))


    from fno.config import load_settings

    with pytest.raises(Exception, match=r"\*|\?|\[|glob"):
        load_settings()


# ---------------------------------------------------------------------------
# AC1-EDGE: {vault} with obsidian disabled rejected at load
# ---------------------------------------------------------------------------


def test_vault_template_with_obsidian_disabled_rejected(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """AC1-EDGE: {vault} in plans_dir with obsidian.enabled=false raises at load."""
    monkeypatch.delenv("FNO_CONFIG", raising=False)
    settings_file = _write_settings(
        tmp_path,
        "schema_version: 1\nconfig:\n  plans_dir: '{vault}/plans'\n  obsidian:\n    enabled: false\n",
    )
    monkeypatch.setenv("FNO_CONFIG", str(settings_file))


    from fno.config import load_settings

    with pytest.raises(Exception, match=r"vault|obsidian"):
        load_settings()


# ---------------------------------------------------------------------------
# Fix 6: Dual ProjectBlock - top-level project is deprecated alias for config.project
# ---------------------------------------------------------------------------


# ---------------------------------------------------------------------------
# Fix 5: ObsidianBlock rejects enabled=True with no vault at construction time
# ---------------------------------------------------------------------------


# ---------------------------------------------------------------------------
# Fix 1: {{ escape }} must not trigger vault/obsidian validation error
# ---------------------------------------------------------------------------


# ---------------------------------------------------------------------------
# Finding C (P1): load_settings falls through to next candidate on parse failure
# ---------------------------------------------------------------------------


# ---------------------------------------------------------------------------
# PATH_MAX validation
# ---------------------------------------------------------------------------


# ---------------------------------------------------------------------------
# Fix 2: _load_raw warns on YAML parse failure instead of silent fallback
# ---------------------------------------------------------------------------


def test_corrupt_yaml_returns_defaults_and_logs_warning(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch, caplog: pytest.LogCaptureFixture
) -> None:
    """AC2-ERR: corrupt settings.yaml returns defaults AND emits a WARNING.

    A user with a broken settings file should see a warning, not silent defaults.
    """
    monkeypatch.delenv("FNO_CONFIG", raising=False)
    settings_file = _write_settings(tmp_path, ":::bad yaml:::\n  - broken: [unterminated")
    monkeypatch.setenv("FNO_CONFIG", str(settings_file))


    with caplog.at_level(logging.WARNING, logger="fno.config"):
        from fno.config import load_settings

        result = load_settings()

    # Returns defaults (not raising)
    assert result.state_dir == "~/.fno/"
    # Must have logged a warning
    assert any(
        "failed to parse" in record.message or "YAMLError" in record.message or "parse" in record.message
        for record in caplog.records
    ), f"Expected parse warning, got: {[r.message for r in caplog.records]}"


# ---------------------------------------------------------------------------
# Lookup walk: FNO_CONFIG env -> ./.fno/settings.yaml -> ~/.fno/settings.yaml -> defaults
# ---------------------------------------------------------------------------


# ---------------------------------------------------------------------------
# Unknown-key warnings: stderr on every load, capped, no duplicate emission
# ---------------------------------------------------------------------------


def test_unknown_key_warns_exactly_once_per_load(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch, capsys: pytest.CaptureFixture
) -> None:
    """One load, one line per unknown key: the per-layer walk hands nested
    keys up, so a key at any depth prints once, not once per level."""
    monkeypatch.delenv("FNO_CONFIG", raising=False)
    settings_file = _write_settings(
        tmp_path,
        "schema_version: 1\nconfig:\n  future_thing: true\n  target:\n    future_leaf: true\n",
    )
    monkeypatch.setenv("FNO_CONFIG", str(settings_file))

    from fno.config import load_settings

    load_settings()
    err = capsys.readouterr().err

    assert err.count("future_thing") == 1, f"Expected exactly one line: {err!r}"
    assert err.count("target.future_leaf") == 1, f"Expected exactly one line: {err!r}"


# ---------------------------------------------------------------------------
# config.blueprint.max_prs_per_epic (ab-e9c81ed3, C1)
# ---------------------------------------------------------------------------
