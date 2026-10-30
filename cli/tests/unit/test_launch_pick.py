"""Launch-time headroom picking at the spawn seam.

No in-session credential swap is possible, so the only moment footnote can
choose an account is just before a process starts. These cover both Python
spawn seams - ``dispatch_spawn`` for bg/headless and ``dispatch_spawn_pane``
for the default pane substrate, which never reaches the former - plus the two
rules that keep picking honest: an explicit ``--account`` is never
second-guessed, a routed spawn is never picked for, and a worker that dies on
quota poisons the next pick before its snapshot would refresh.

Run: cd cli && uv run pytest tests/unit/test_launch_pick.py -v
"""
from __future__ import annotations

import json
import time
from pathlib import Path

import pytest
import tomli_w

from fno.agents import dispatch as dispatch_mod


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


def _seed_usage(path: Path, **used_pct: float) -> None:
    """A fresh 5h usage row per account, as the runtime-state writer leaves it."""
    now = time.time()
    path.write_text(json.dumps({"usage": {
        rid: {"windows": [{"label": "5h", "used_pct": pct, "resets_at": now + 3600}],
              "probed_at": now, "source": "t"}
        for rid, pct in used_pct.items()
    }}))


def _pin_config(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setenv("HOME", str(tmp_path))
    monkeypatch.setenv("PWD", str(tmp_path))
    monkeypatch.setenv("FNO_STATE_DIR", str(tmp_path / ".fno"))
    monkeypatch.setenv("FNO_CONFIG", str(tmp_path / ".fno" / "config.toml"))


@pytest.fixture()
def armed(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> Path:
    """readyrule EXHAUSTED, makers OK, picking armed."""
    _write_config(tmp_path, pick_on_launch=True)
    _pin_config(tmp_path, monkeypatch)
    monkeypatch.chdir(tmp_path)
    monkeypatch.setenv("FNO_RUNTIME_STATE_PATH", str(tmp_path / "runtime-state.json"))

    _seed_usage(tmp_path / "runtime-state.json", readyrule=100.0, makers=32.0)
    return tmp_path


class TestPickAtLaunch:

    @pytest.mark.dev_build  # the pick is the fno-agents command
    def test_receipt_names_the_picked_account_and_its_headroom(
        self, armed: Path, capsys: pytest.CaptureFixture[str]
    ) -> None:
        # A launch landing on a different account than expected is a billing
        # surprise; the receipt is the whole mitigation.
        dispatch_mod._pick_account_overlay()
        err = capsys.readouterr().err
        assert "account: makers (picked by accounts.quota.pick_on_launch," in err
        assert "32%" in err

    def test_off_by_default(self, tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
        _write_config(tmp_path, pick_on_launch=False)
        _pin_config(tmp_path, monkeypatch)
        assert dispatch_mod._pick_account_overlay() is None


class TestQuotaDeath:
    @pytest.mark.dev_build  # the verdict reads through the native list command
    def test_a_quota_death_cools_the_account_down(
        self, armed: Path
    ) -> None:
        # The snapshot can be probe_ttl_seconds stale, so without this the next
        # pick would hand the successor the account that just died. The verdict
        # reads through the native list command, whose `headroom` column the
        # account rows carry.
        def _makers_headroom() -> str:
            from fno.rust_binary import call_binary_json

            err, rows = call_binary_json(
                "spawn-overlay", ("accounts", "list", "--json")
            )
            assert err is None, err
            return next(r["headroom"] for r in rows if r["id"] == "makers")

        env = {"CLAUDE_CONFIG_DIR": str(armed / "claude-main")}
        assert not _makers_headroom().startswith("exhausted")
        dispatch_mod.note_quota_death(env, "Error: usage limit reached for this account")
        assert _makers_headroom().startswith("exhausted")


