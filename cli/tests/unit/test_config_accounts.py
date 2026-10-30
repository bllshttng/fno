"""Tests for accounts configuration resolution.

The doctor's accounts findings and the `fno config get` answers, including
the pre-rename `providers` alias, are the native door's contract, frozen by
the parity goldens.
"""
from __future__ import annotations

from pathlib import Path

import pytest


def _write_config(tmp_path: Path, content: str) -> Path:
    d = tmp_path / ".fno"
    d.mkdir(parents=True, exist_ok=True)
    f = d / "config.toml"
    f.write_text(content, encoding="utf-8")
    return f


def test_load_providers_preserves_global_records_with_local_leaf_override(tmp_path: Path, monkeypatch: pytest.MonkeyPatch):
    from fno.adapters.providers.loader import load_providers

    global_dir = tmp_path / "global"
    global_cfg = _write_config(global_dir, '''
[accounts]
active = "global-acc"

[[accounts.records]]
id = "global-acc"
name = "global-acc"
harness = "claude"
auth = "oauth_dir"
credentials_source = "~/.claude"
priority = 100
''')
    local_dir = tmp_path / "local"
    _write_config(local_dir, '''
[accounts.quota]
defer_dispatch = true
''')
    monkeypatch.delenv("FNO_CONFIG", raising=False)
    monkeypatch.setenv("FNO_GLOBAL_SETTINGS_PATH", str(global_cfg))
    monkeypatch.setenv("PWD", str(local_dir))

    cfg = load_providers(repo_root=local_dir)
    assert cfg.active == "global-acc"
    assert len(cfg.records) == 1
    assert cfg.records[0].id == "global-acc"


def test_load_combos_honors_fno_config(tmp_path: Path, monkeypatch: pytest.MonkeyPatch):
    from fno.adapters.providers.loader import load_combos

    cfg = _write_config(tmp_path, '''
[accounts]
active = "acc1"

[[accounts.records]]
id = "acc1"
name = "acc1"
harness = "claude"
auth = "oauth_dir"
credentials_source = "~/.claude"

[accounts.combos.my_combo]
strategy = "fallback"
providers = ["acc1"]
''')
    monkeypatch.setenv("FNO_CONFIG", str(cfg))
    combos = load_combos()
    assert "my_combo" in combos
    assert combos["my_combo"].providers == ("acc1",)
