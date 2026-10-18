"""Setup-wizard config-write path for the node-ID scheme.

The `/setup` wizard collects the prefix (required) + hex width (default 4) and
persists them via ``fno config set`` -> ``set_config_value``. These tests cover
that write path's refusals: AC1-ERR (rejects an invalid prefix, file untouched)
and AC1-EDGE (rejects an out-of-range width).
"""
from __future__ import annotations

import pytest

from fno.config.writer import ConfigSetError, set_config_value


def test_setup_id_rejects_invalid_prefix(tmp_path):
    with pytest.raises(ConfigSetError) as exc:
        set_config_value("config.backlog.id_prefix", "CV-", scope="project", repo_root=tmp_path)
    assert exc.value.exit_code == 2
    # AC1-FR/ERR: nothing written on a rejected value.
    assert not (tmp_path / ".fno" / "config.toml").exists()


def test_setup_id_rejects_out_of_range_width(tmp_path):
    with pytest.raises(ConfigSetError) as exc:
        set_config_value("config.backlog.id_hex_width", "3", scope="project", repo_root=tmp_path)
    assert exc.value.exit_code == 2
    assert not (tmp_path / ".fno" / "config.toml").exists()
