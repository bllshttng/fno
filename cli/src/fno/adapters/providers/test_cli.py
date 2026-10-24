"""Tests for fno providers CLI surface (Phase 02).

Run: cd cli && uv run pytest src/fno/adapters/providers/test_cli.py -v
"""
from __future__ import annotations

import json
from pathlib import Path

import pytest
import tomli_w
from typer.testing import CliRunner

from fno.adapters.providers.cli import cli as providers_app


runner = CliRunner()


def _seed_state(path: Path, **blocks: dict) -> None:
    """Write a runtime-state file as the Rust writer leaves it."""
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(blocks), encoding="utf-8")


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


def _invoke(args: list[str], cwd: Path, home: Path):
    """Invoke the CLI with environment isolation."""
    return runner.invoke(
        providers_app,
        args,
        env={
            "HOME": str(home),
            "PWD": str(cwd),
        },
        catch_exceptions=False,
    )


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


# ---------------------------------------------------------------------------
# window: the usage env the projection reads
# ---------------------------------------------------------------------------


def _config_dir_pair(tmp_path: Path, active: str = "readyrule") -> dict:
    for name in ("claude-alt", "claude-main"):
        d = tmp_path / name
        d.mkdir(exist_ok=True)
        (d / ".credentials.json").write_text("{}")
    return {
        "config": {
            "providers": {
                "active": active,
                "records": [
                    {"id": "readyrule", "name": "readyrule", "harness": "claude",
                     "auth": "managed", "config_dir": str(tmp_path / "claude-alt")},
                    {"id": "makers", "name": "makers", "harness": "claude",
                     "auth": "managed", "config_dir": str(tmp_path / "claude-main")},
                ],
            }
        }
    }


def _usage_env(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> Path:
    _write_settings(tmp_path / ".fno" / "config.toml", _config_dir_pair(tmp_path))
    monkeypatch.setenv("HOME", str(tmp_path))
    monkeypatch.setenv("PWD", str(tmp_path))
    monkeypatch.setenv("FNO_STATE_DIR", str(tmp_path / ".fno"))
    monkeypatch.setenv("FNO_RUNTIME_STATE_PATH", str(tmp_path / "runtime-state.json"))
    return tmp_path


class TestWindowCommand:
    """AC3 at the CLI boundary: a projection, and an honest unknown."""

    def test_ac3_edge_a_probeless_record_prints_unknown_and_keeps_the_key(
        self, tmp_path: Path, monkeypatch: pytest.MonkeyPatch
    ) -> None:
        # done_probe 2 greps for "closes_in_s". The key must be present even
        # when the answer is unknown, or a shapeless response passes the gate.
        import json as _json

        _usage_env(tmp_path, monkeypatch)
        result = _invoke(["window", "--json"], cwd=tmp_path, home=tmp_path)
        assert result.exit_code == 0, result.output
        payload = _json.loads(result.output.strip())
        assert payload
        for row in payload.values():
            assert "closes_in_s" in row
            assert row["closes_in_s"] is None
            assert row["headroom"] == "unknown"

    def test_ac3_hp_a_recorded_open_prints_seconds_to_close(
        self, tmp_path: Path, monkeypatch: pytest.MonkeyPatch
    ) -> None:
        import json as _json
        import time as _time

        import fno.adapters.providers.runtime_state as rs

        _usage_env(tmp_path, monkeypatch)
        monkeypatch.setattr(
            rs, "_record_window_seconds", lambda _pid: 300 * 60, raising=True
        )
        _seed_state(tmp_path / "runtime-state.json", windows_opened={
            "readyrule": {"window": {"opened_at": _time.time() - 2 * 3600, "warned": False}},
        })

        result = _invoke(["window", "--json"], cwd=tmp_path, home=tmp_path)
        assert result.exit_code == 0, result.output
        row = _json.loads(result.output.strip())["readyrule"]
        assert row["closes_in_s"] == pytest.approx(3 * 3600, abs=5)
        assert row["closes_at"] is not None
        # The projection must not have taught headroom a percentage.
        assert row["headroom"] == "unknown"

    def test_the_human_row_says_unknown_rather_than_a_number(
        self, tmp_path: Path, monkeypatch: pytest.MonkeyPatch
    ) -> None:
        _usage_env(tmp_path, monkeypatch)
        result = _invoke(["window"], cwd=tmp_path, home=tmp_path)
        assert result.exit_code == 0, result.output
        assert "unknown" in result.output
