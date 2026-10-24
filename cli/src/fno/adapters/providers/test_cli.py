"""Tests for fno providers CLI surface (Phase 02).

Run: cd cli && uv run pytest src/fno/adapters/providers/test_cli.py -v
"""
from __future__ import annotations

from pathlib import Path

import pytest
import tomli_w


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------

def _flatten(d: dict) -> dict:
    cfg = d.get("config")
    if not isinstance(cfg, dict):
        return d
    out = {k: v for k, v in d.items() if k != "config"}
    out.update(cfg)
    return out


def _strip_none(x):
    if isinstance(x, dict):
        return {k: _strip_none(v) for k, v in x.items() if v is not None}
    if isinstance(x, list):
        return [_strip_none(v) for v in x]
    return x


def _write_settings(path: Path, content: dict) -> None:
    """Write a flat config.toml at path (lifts any legacy config: wrapper)."""
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(tomli_w.dumps(_strip_none(_flatten(content))), encoding="utf-8")


def _two_record_config(active: str = "claude-primary") -> dict:
    return {
        "config": {
            "providers": {
                "active": active,
                "records": [
                    {
                        "id": "claude-primary",
                        "name": "Claude Primary",
                        "harness": "claude",
                        "auth": "oauth_dir",
                        "credentials_source": str(Path.home() / ".claude"),
                        "priority": 10,
                    },
                    {
                        "id": "gemini-backup",
                        "name": "Gemini Backup",
                        "harness": "gemini",
                        "auth": "api_key",
                        "env": {"GEMINI_API_KEY": "test-key"},
                        "priority": 20,
                    },
                ],
            }
        }
    }


# ---------------------------------------------------------------------------
# AC5-CON: routing-active and slot-active resolve through ONE function
# ---------------------------------------------------------------------------


def _managed_pair_config(active: str) -> dict:
    return {
        "config": {
            "providers": {
                "active": active,
                "records": [
                    {"id": "readyrule", "name": "readyrule", "harness": "claude",
                     "auth": "managed", "priority": 10},
                    {"id": "makers", "name": "makers", "harness": "claude",
                     "auth": "managed", "priority": 20},
                ],
            }
        }
    }


class TestEffectiveActive:
    """`config.providers.active` and the slot occupant can disagree.

    Live on the machine that motivated this: config said `readyrule` while the
    shared slot held `makers`, so the display path marked one account and the
    dispatch path evaluated the other's headroom.
    """

    @pytest.fixture()
    def diverged(self, tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> Path:
        _write_settings(tmp_path / ".fno" / "config.toml", _managed_pair_config("readyrule"))
        monkeypatch.setenv("HOME", str(tmp_path))
        monkeypatch.setenv("PWD", str(tmp_path))
        monkeypatch.setenv("FNO_STATE_DIR", str(tmp_path / ".fno"))
        from fno.adapters.providers import managed

        managed.stamp_active_slot("claude", "makers")
        return tmp_path

    def test_resolver_returns_the_slot_occupant(self, diverged: Path) -> None:
        from fno.adapters.providers.loader import effective_active

        assert effective_active(repo_root=diverged) == "makers"

    def test_non_managed_active_still_reads_routing_active(
        self, tmp_path: Path, monkeypatch: pytest.MonkeyPatch
    ) -> None:
        # A record whose credential does NOT come from the shared slot keeps
        # routing-active as its authority.
        _write_settings(tmp_path / ".fno" / "config.toml", _two_record_config())
        monkeypatch.setenv("HOME", str(tmp_path))
        monkeypatch.setenv("PWD", str(tmp_path))
        monkeypatch.setenv("FNO_STATE_DIR", str(tmp_path / ".fno"))
        from fno.adapters.providers.loader import effective_active

        assert effective_active(repo_root=tmp_path) == "claude-primary"

    def test_managed_active_with_no_stamp_is_nobody(
        self, tmp_path: Path, monkeypatch: pytest.MonkeyPatch
    ) -> None:
        # No stamp means nothing is known to occupy the slot. Guessing
        # routing-active here is how a dispatch ends up billing an account whose
        # credential is not actually loaded.
        _write_settings(tmp_path / ".fno" / "config.toml", _managed_pair_config("readyrule"))
        monkeypatch.setenv("HOME", str(tmp_path))
        monkeypatch.setenv("PWD", str(tmp_path))
        monkeypatch.setenv("FNO_STATE_DIR", str(tmp_path / ".fno"))
        from fno.adapters.providers.loader import effective_active

        assert effective_active(repo_root=tmp_path) is None

