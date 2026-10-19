"""FNO_CONFIG_SEARCH_ROOT bounds the config candidate chain to the tmpdir.

The config candidate chain climbs to the canonical checkout via
``git worktree list``. During a local full-suite run that reached the
developer's real ~/code/footnote/footnote/.fno/config.toml (auto_install=false,
YAML-misparse warning) - green in CI, red locally. The ceiling drops any
candidate resolving outside the allowed test roots so a poisoned parent-dir
config can never leak in.
"""
from __future__ import annotations

from pathlib import Path

import pytest


@pytest.fixture(autouse=True)
def _isolate(monkeypatch: pytest.MonkeyPatch):
    monkeypatch.delenv("FNO_CONFIG", raising=False)
    monkeypatch.delenv("FNO_REPO_ROOT", raising=False)
    # preflight's hermetic runner exports FNO_NO_CANONICAL_CONFIG=1, which drops
    # the canonical candidate this module asserts is present.
    monkeypatch.delenv("FNO_NO_CANONICAL_CONFIG", raising=False)
    yield


def test_direct_reader_candidates_are_bounded(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """config_read_candidates (the _intake project<->path reader path) is bounded.

    The actual leak: a cwd-relative .fno/config.toml resolves through a
    worktree symlink to the canonical checkout, and _intake yaml-parses it,
    polluting --json stdout. That reader funnels through config_read_candidates,
    NOT _settings_yaml_locations, so the ceiling must bound this path too.
    """
    from fno.config import config_read_candidates

    inside = tmp_path / "wt" / ".fno" / "settings.yaml"
    outside = tmp_path / "canonical" / ".fno" / "settings.yaml"
    monkeypatch.setenv("FNO_CONFIG_SEARCH_ROOT", str(tmp_path / "wt"))

    cands = config_read_candidates([inside, outside])

    # config.toml-first sibling of the inside candidate survives...
    assert inside.with_name("config.toml") in cands
    # ...but nothing under the out-of-ceiling canonical root leaks in.
    assert all("canonical" not in str(c) for c in cands)
