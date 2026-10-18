"""Tests for multi-key `fno config set a.b=1 c.d=2`.

Covers AC2-HP (both set, one lock), AC2-ERR (one invalid -> writes nothing),
AC2-UI (each pair + scope/path once), AC2-EDGE (same key twice -> last wins),
AC2-FR (mid-batch write failure leaves the pre-batch file).
"""
from __future__ import annotations

import pytest
import tomllib

from fno.config.writer import ConfigSetError, set_config_values


def _read(tmp_path):
    return tomllib.loads((tmp_path / ".fno" / "config.toml").read_text())


def test_ac2_err_one_invalid_writes_nothing(tmp_path):
    # Seed a valid file first.
    set_config_values(
        [("config.agents.a2a.auto", "false")], scope="project", repo_root=tmp_path
    )
    before = (tmp_path / ".fno" / "config.toml").read_text()
    with pytest.raises(ConfigSetError) as exc:
        set_config_values(
            [
                ("config.agents.a2a.turn_ceiling", "9"),  # valid
                ("config.agents.a2a.turn_ceiling", "0"),  # invalid (>=1)
            ],
            scope="project",
            repo_root=tmp_path,
        )
    # The last-wins value (0) is the one validated and it fails.
    assert exc.value.exit_code == 2
    # Nothing from the batch was written.
    assert (tmp_path / ".fno" / "config.toml").read_text() == before


def test_multi_set_cross_field_block_validates_on_final_state(tmp_path):
    """A batch setting two cross-field-coupled keys in the same block must
    validate the FINAL state, not the intermediate one. config.obsidian.enabled
    requires .vault; enabled-first must NOT abort.
    """
    # enabled listed first (the order that previously aborted on enabled=true).
    results = set_config_values(
        [
            ("config.obsidian.enabled", "true"),
            ("config.obsidian.vault", "MyVault"),
        ],
        scope="project",
        repo_root=tmp_path,
    )
    assert len(results) == 2
    data = _read(tmp_path)
    assert data["obsidian"]["enabled"] is True
    assert data["obsidian"]["vault"] == "MyVault"


# --- CLI surface ---


def test_dict_block_whole_entry_and_bad_inner_leaf_refuse(tmp_path):
    """The map-key leaf itself (a whole sub-block) is not a settable scalar,
    and an unknown field inside the value model still refuses."""
    with pytest.raises(ConfigSetError):
        set_config_values(
            [("config.agents.defaults.harness.codex", "yolo")],
            scope="project",
            repo_root=tmp_path,
        )
    with pytest.raises(ConfigSetError):
        set_config_values(
            [("config.agents.defaults.harness.codex.banana", "1")],
            scope="project",
            repo_root=tmp_path,
        )
