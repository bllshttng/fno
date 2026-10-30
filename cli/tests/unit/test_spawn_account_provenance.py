"""Launch-account provenance: the receipt answers WHO chose the account.

The defect this pins: a spawn receipt named the account a worker launched on
but not WHO chose it, and a config injection (accounts.quota.pick_on_launch)
printed identically to a caller decision. The fix is one shared vocabulary -
"caller" / "config" (plus "env" / "default" where an axis realizes them) -
defined once in ``fno.agents.spawn_flag_owners`` and stamped by every spawn
seam onto the row (`launch_account_source`, registry v26) and the receipt
(`account_source`).

Run: cd cli && uv run pytest tests/unit/test_spawn_account_provenance.py -v
"""
from __future__ import annotations

import json
import time
from pathlib import Path

import pytest
import tomli_w



@pytest.fixture(autouse=True)
def _isolate_config_override(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """The sanctioned test runner exports FNO_CONFIG; these fixtures own config."""
    monkeypatch.setenv("FNO_CONFIG", str(tmp_path / ".fno" / "config.toml"))


def _write_config(tmp_path: Path, *, pick_on_launch: bool) -> None:
    alt, main = tmp_path / "claude-alt", tmp_path / "claude-main"
    for d in (alt, main):
        d.mkdir(exist_ok=True)
        (d / ".credentials.json").write_text(
            '{"claudeAiOauth":{"accessToken":"test-token","refreshToken":"test-refresh"}}'
        )
    (tmp_path / ".fno").mkdir(exist_ok=True)
    (tmp_path / ".fno" / "config.toml").write_text(
        tomli_w.dumps({
            "providers": {
                "active": "readyrule",
                "quota": {"pick_on_launch": pick_on_launch},
                "records": [
                    {"id": "readyrule", "name": "readyrule", "harness": "claude",
                     "auth": "managed", "config_dir": str(alt)},
                    {"id": "makers", "name": "makers", "harness": "claude",
                     "auth": "managed", "config_dir": str(main)},
                ],
            }
        }),
        encoding="utf-8",
    )


def _pin_config(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setenv("HOME", str(tmp_path))
    monkeypatch.setenv("PWD", str(tmp_path))
    monkeypatch.setenv("FNO_STATE_DIR", str(tmp_path / ".fno"))
    monkeypatch.setenv("FNO_CONFIG", str(tmp_path / ".fno" / "config.toml"))


@pytest.fixture()
def armed(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> Path:
    """readyrule exhausted, makers healthy, picking armed."""
    _write_config(tmp_path, pick_on_launch=True)
    _pin_config(tmp_path, monkeypatch)
    monkeypatch.chdir(tmp_path)
    monkeypatch.setenv("FNO_RUNTIME_STATE_PATH", str(tmp_path / "runtime-state.json"))

    now = time.time()

    def snap(used: float) -> dict:
        return {"windows": [{"label": "5h", "used_pct": used, "resets_at": now + 3600}],
                "probed_at": now, "source": "t"}

    (tmp_path / "runtime-state.json").write_text(
        json.dumps({"usage": {"readyrule": snap(100.0), "makers": snap(32.0)}})
    )
    return tmp_path


class TestVocabulary:
    def test_the_carrier_word_is_spelled_identically_across_runtimes(self) -> None:
        """The Python seam sets the provenance carrier; the Rust mint reads it.
        Two spellings of one env key silently cut one side off the wire."""
        from fno.agents import launch_provenance

        rs = (
            Path(__file__).resolve().parents[3]
            / "crates" / "fno-agents" / "src" / "state.rs"
        )
        if not rs.is_file():
            pytest.skip("rust crate not present in this checkout")
        src = rs.read_text(encoding="utf-8")
        assert launch_provenance.LAUNCH_ACCOUNT_SOURCE_ENV in src
        # The mint speaks exactly the wire vocabulary, in Python's own words.
        assert 'src == "caller" || src == "config"' in src
