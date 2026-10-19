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

    from fno.adapters.providers.runtime_state import write_usage_snapshot
    from fno.adapters.providers.usage import UsageSnapshot, UsageWindow

    now = time.time()
    write_usage_snapshot(
        UsageSnapshot("readyrule", (UsageWindow("5h", 100.0, now + 3600),), now, "t"),
        now=now,
    )
    write_usage_snapshot(
        UsageSnapshot("makers", (UsageWindow("5h", 32.0, now + 3600),), now, "t"),
        now=now,
    )
    return tmp_path


class TestPickAtLaunch:

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


class TestSpawnSeam:
    """AC8-CON: the picker is reached from every spawn path.

    A guard on one of N reachable paths is decorative. These call
    ``dispatch_spawn`` directly, bypassing the CLI's argument parsing entirely;
    ``TestPaneSeam`` below covers the other Python seam.
    """

    def _capture_create(self, monkeypatch: pytest.MonkeyPatch) -> dict:
        seen: dict = {}

        class _Created:
            short_id = "abc123"

        def _fake_create(**kwargs):
            seen.update(kwargs)
            return _Created()

        monkeypatch.setattr(dispatch_mod, "_claude_create_path", _fake_create)
        monkeypatch.setattr(dispatch_mod, "_emit_ev", lambda *a, **k: None)
        return seen

    def test_direct_call_applies_the_picked_overlay(
        self, armed: Path, monkeypatch: pytest.MonkeyPatch
    ) -> None:
        seen = self._capture_create(monkeypatch)
        dispatch_mod.dispatch_spawn(
            name="w1", message="hi", harness="claude", cwd=armed
        )
        assert seen["account_env"]["CLAUDE_CONFIG_DIR"] == str(armed / "claude-main")

    def test_explicit_account_is_never_overridden(
        self, armed: Path, monkeypatch: pytest.MonkeyPatch
    ) -> None:
        """AC10-ERR: an explicit --account wins even when it reads EXHAUSTED."""
        seen = self._capture_create(monkeypatch)

        def _never(*a, **k):
            raise AssertionError("picker consulted despite an explicit account")

        monkeypatch.setattr(dispatch_mod, "_pick_account_overlay", _never)
        explicit = {"CLAUDE_CONFIG_DIR": str(armed / "claude-alt")}
        dispatch_mod.dispatch_spawn(
            name="w2", message="hi", harness="claude", cwd=armed,
            account_env=explicit,
        )
        assert seen["account_env"] == explicit


class TestQuotaDeath:
    def test_a_quota_death_cools_the_account_down(
        self, armed: Path
    ) -> None:
        # The snapshot can be probe_ttl_seconds stale, so without this the next
        # pick would hand the successor the account that just died.
        from fno.adapters.providers.runtime_state import HeadroomState, headroom

        env = {"CLAUDE_CONFIG_DIR": str(armed / "claude-main")}
        assert headroom("makers").state is not HeadroomState.EXHAUSTED
        dispatch_mod.note_quota_death(env, "Error: usage limit reached for this account")
        assert headroom("makers").state is HeadroomState.EXHAUSTED


class TestRoutedSpawnsAreNotPicked:
    """`fno agents spawn` refuses --account together with --route/--role.

    The route's ANTHROPIC_* overrides the account's CLAUDE_CONFIG_DIR and
    silently mis-bills, which is exactly why the CLI refuses the combination.
    Auto-picking must not reassemble it behind that refusal.
    """


    def test_spawn_does_not_pick_for_a_routed_worker(
        self, armed: Path, monkeypatch: pytest.MonkeyPatch
    ) -> None:
        seen: dict = {}

        class _Created:
            short_id = "abc123"

        monkeypatch.setattr(
            dispatch_mod, "_claude_create_path",
            lambda **kw: (seen.update(kw), _Created())[1],
        )
        monkeypatch.setattr(dispatch_mod, "_emit_ev", lambda *a, **k: None)
        def _resolve(role, route_env, notice=None, **kwargs):
            kwargs["resolved_provider"]("zai")
            return route_env

        monkeypatch.setattr("fno.agents.model_routing.resolve_spawn_route", _resolve)
        monkeypatch.setenv("FNO_SPAWN_GATE", "0")
        from fno.agents.spawn_gate import run_gate

        gate = run_gate("w3", "bg", route_provider="zai")
        try:
            dispatch_mod.dispatch_spawn(
                name="w3", message="hi", harness="claude", cwd=armed,
                route_env={"ANTHROPIC_BASE_URL": "https://example.invalid"},
                provider_gate=gate,
            )
        finally:
            gate.release()
        assert seen["account_env"] is None


class TestPaneSeam:
    """`pane` is the DEFAULT substrate and `cmd_spawn` routes it straight to
    `dispatch_spawn_pane`, never through `dispatch_spawn`. A picker wired only
    at the latter would leave every default interactive spawn on the exhausted
    account while the option read enabled."""

    def test_the_pane_path_picks_too(
        self, armed: Path, monkeypatch: pytest.MonkeyPatch
    ) -> None:
        from fno.agents import mux_spawn

        seen: dict = {}

        def _capture(provider, model, *, role=None, route_env=None, account_env=None):
            seen["account_env"] = account_env
            raise RuntimeError("stop after the seam")

        monkeypatch.setattr(
            "fno.agents.model_routing.check_spawn_tier_remap", _capture
        )
        with pytest.raises(RuntimeError, match="stop after the seam"):
            mux_spawn.dispatch_spawn_pane(
                name="p1", message="hi", provider="claude", cwd=armed
            )
        assert seen["account_env"]["CLAUDE_CONFIG_DIR"] == str(armed / "claude-main")


class TestDeferDoesNotStallWhenPickingCanReroute:
    """Deferring is the floor, not the answer, once picking can reroute.

    With both knobs armed, holding work because the ACTIVE account is exhausted
    while another account has headroom is precisely the stall this feature
    exists to delete.
    """


    def test_no_healthy_alternate_leaves_the_defer_standing(
        self, tmp_path: Path, monkeypatch: pytest.MonkeyPatch
    ) -> None:
        import time as _time

        _write_config(tmp_path, pick_on_launch=True)
        _pin_config(tmp_path, monkeypatch)
        monkeypatch.setenv("FNO_RUNTIME_STATE_PATH", str(tmp_path / "rs.json"))
        from fno.adapters.providers.runtime_state import write_usage_snapshot
        from fno.adapters.providers.usage import UsageSnapshot, UsageWindow
        from fno.agents.autonomous_route import _healthy_alternate_exists

        now = _time.time()
        for rid in ("readyrule", "makers"):
            write_usage_snapshot(
                UsageSnapshot(rid, (UsageWindow("5h", 100.0, now + 3600),), now, "t"),
                now=now,
            )
        assert _healthy_alternate_exists() is False

