"""Tests for config.post_merge - the /fno:ship pr merged skill config block.

Covers the mechanical, deterministic half of the post-merge ritual BDD:
per-project parking-lot-path resolution and the "missing config fails loud /
never guesses" guarantee, at the model level. The skill reads these values
through `fno config get`, whose resolution contract the native door owns and
the parity goldens freeze.
"""
from __future__ import annotations

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


def _load(tmp_path: Path, monkeypatch: pytest.MonkeyPatch, content: str):
    monkeypatch.delenv("FNO_CONFIG", raising=False)
    settings_file = _write_settings(tmp_path, content)
    monkeypatch.setenv("FNO_CONFIG", str(settings_file))

    from fno import config as config_mod

    return config_mod.load_settings()


# ---------------------------------------------------------------------------
# Schema defaults + overrides
# ---------------------------------------------------------------------------


def test_post_merge_defaults(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    """With no config.post_merge block: parking_lot_path unset, enabled True.

    parking_lot_path must default to None (not a guessed path) so the skill
    fails loud rather than writing to the wrong queue.
    """
    settings = _load(tmp_path, monkeypatch, "schema_version: 1\n")
    assert settings.post_merge.parking_lot_path is None
    assert settings.post_merge.enabled is True


def test_post_merge_parking_lot_path_rejects_absolute(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """An absolute parking_lot_path is rejected: it must be repo-relative."""
    with pytest.raises(Exception, match=r"repo-relative|/|~"):
        _load(
            tmp_path,
            monkeypatch,
            "schema_version: 1\nconfig:\n  post_merge:\n"
            "    parking_lot_path: /etc/evil.md\n",
        )


def test_post_merge_parking_lot_path_rejects_dotdot(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """A '..' segment is rejected so the skill cannot write outside the repo."""
    with pytest.raises(Exception, match=r"\.\.|escape"):
        _load(
            tmp_path,
            monkeypatch,
            "schema_version: 1\nconfig:\n  post_merge:\n"
            "    parking_lot_path: ../../outside/parking-lot.md\n",
        )


# ---------------------------------------------------------------------------
# self_reap - opt-in agent-view row self-removal (Step 8)
# ---------------------------------------------------------------------------


def test_post_merge_self_reap_typo_coerces_false(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """A malformed value fails safe to False (auto-removing a wanted row is the
    costly direction), and never breaks load_settings() for other consumers."""
    settings = _load(
        tmp_path,
        monkeypatch,
        "schema_version: 1\nconfig:\n  post_merge:\n    self_reap: banana\n",
    )
    assert settings.post_merge.self_reap is False


# ---------------------------------------------------------------------------
# model - the post-merge worker tier knob
# ---------------------------------------------------------------------------


def test_post_merge_model_empty_coerces_default(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """A blank model must never reach `--model ""` - it coerces to the field default."""
    from fno.config import PostMergeBlock

    settings = _load(
        tmp_path,
        monkeypatch,
        'schema_version: 1\nconfig:\n  post_merge:\n    model: ""\n',
    )
    assert settings.post_merge.model == "claude-opus-5"
    # The coercer has no literal of its own: changing the field default moves it.
    assert PostMergeBlock.model_fields["model"].default == settings.post_merge.model
    assert PostMergeBlock(model="   ").model == settings.post_merge.model
    assert PostMergeBlock.model_validate({"model": None}).model == settings.post_merge.model


def test_maintainer_marker_default_empty_and_validated() -> None:
    """maintainer_marker defaults to None (the OSS-safe 'omit the tag' state) and
    validates to a single whitespace-free token. The default is the load-bearing
    part: a fresh install must ship no one's initials."""
    from fno.config import PostMergeBlock

    assert PostMergeBlock().maintainer_marker is None
    assert PostMergeBlock(maintainer_marker="#maintainer").maintainer_marker == "#maintainer"
    # whitespace-only / empty / non-str coerce to None (safe direction = no marker)
    assert PostMergeBlock(maintainer_marker="   ").maintainer_marker is None
    assert PostMergeBlock(maintainer_marker="").maintainer_marker is None
    assert PostMergeBlock.model_validate({"maintainer_marker": 123}).maintainer_marker is None
    # leading/trailing whitespace is stripped, not rejected
    assert PostMergeBlock(maintainer_marker="  #maintainer  ").maintainer_marker == "#maintainer"
    # an internal space is a real config error: a tag is one token
    with pytest.raises(Exception):
        PostMergeBlock(maintainer_marker="#two words")
