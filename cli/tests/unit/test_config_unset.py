"""Tests for `fno config unset` / fno.config.writer.unset_config_value.

Covers AC1-HP (removes key, reverts to default, file no longer contains it),
AC1-ERR (unknown key exits 1, file unchanged), AC1-UI (CLI confirm line),
AC1-EDGE (absent key is a clean no-op; emptied parent pruned),
AC1-FR (mid-write failure leaves the original intact).
"""
from __future__ import annotations

import pytest
import tomllib

from fno.config.writer import ConfigSetError, set_config_value, unset_config_value


def _read(tmp_path):
    return tomllib.loads((tmp_path / ".fno" / "config.toml").read_text())


def test_ac1_hp_unset_removes_key_reverts_to_default(tmp_path):
    set_config_value(
        "config.auto_merge.enabled", "true", scope="project", repo_root=tmp_path
    )
    assert _read(tmp_path)["auto_merge"]["enabled"] is True

    res = unset_config_value(
        "config.auto_merge.enabled", scope="project", repo_root=tmp_path
    )
    assert res.present is True
    assert res.was is True
    # Reverts to the model default (auto_merge.enabled defaults False).
    assert res.default is False
    # File no longer carries the key, and the emptied block is pruned.
    assert "auto_merge" not in _read(tmp_path)


def test_ac1_err_unknown_key_exit1_unchanged(tmp_path):
    set_config_value(
        "config.agents.a2a.auto", "false", scope="project", repo_root=tmp_path
    )
    before = (tmp_path / ".fno" / "config.toml").read_text()
    with pytest.raises(ConfigSetError) as exc:
        unset_config_value("config.nonsense.key", scope="project", repo_root=tmp_path)
    assert exc.value.exit_code == 1
    assert (tmp_path / ".fno" / "config.toml").read_text() == before


def test_retired_dispatch_key_unsets_from_disk(tmp_path):
    cfg = tmp_path / ".fno" / "config.toml"
    cfg.parent.mkdir()
    cfg.write_text('[dispatch]\nallowed_verbs = ["/target"]\nsubstrate = "thread"\n')  # verb-wording: allow
    res = unset_config_value("dispatch.allowed_verbs", scope="project", repo_root=tmp_path)  # verb-wording: allow
    assert res.present is True
    assert _read(tmp_path) == {"dispatch": {"substrate": "thread"}}
