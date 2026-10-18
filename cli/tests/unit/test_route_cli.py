"""Tests for the `fno route` verb family (ls / set / unset / env) - x-b0b4."""

from __future__ import annotations

from pathlib import Path

import pytest
from typer.testing import CliRunner

from fno.agents import model_routing as mr
from fno.config import ConfigBlock, ModelProvider, ModelRoutingBlock, SettingsModel
from fno.route_cli import route_app

requires_rust = pytest.mark.dev_build

runner = CliRunner()


def _settings(**block_kwargs: object) -> SettingsModel:
    return SettingsModel(config=ConfigBlock(model_routing=ModelRoutingBlock(**block_kwargs)))


@pytest.fixture
def project_scope(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> Path:
    """Isolate --local (project-scope) config writes into a tmp repo root."""
    monkeypatch.setattr("fno.paths.resolve_repo_root", lambda: tmp_path)
    return tmp_path


# ---------------------------------------------------------------------------
# ls / build_route_table (AC1-HP, AC1-UI)
# ---------------------------------------------------------------------------


def test_route_table_shows_builtins_config_build_and_protected() -> None:
    settings = _settings(
        providers={
            "zai-openai": ModelProvider(
                protocol="openai",
                base_url="https://api.z.ai/api/coding/paas/v4",
                api_key_env="ZAI_API_KEY",
            )
        },
        roles={"codex-verify": "zai-openai,glm-4.6"},
    )
    rows = mr.build_route_table(settings=settings, env={"ZAI_API_KEY": "k"})
    by_role = {r["role"]: r for r in rows}

    for r in ("coordinate", "tidy", "orient", "consolidate", "post-merge"):
        assert by_role[r]["provider_model"] == "zai/glm-5.3"
    assert by_role["codex-verify"]["provider_model"] == "zai-openai/glm-4.6"
    assert by_role["codex-verify"]["protocol"] == "openai"
    assert by_role["build"]["provider_model"] == "unconfigured"
    for p in ("implement", "review-verdict"):
        assert "never routed" in by_role[p]["provider_model"]
    # Every row carries the canonical 5-field shape (AC1-UI).
    for r in rows:
        assert set(r) == {"role", "provider_model", "protocol", "key", "assigned_by"}


# ---------------------------------------------------------------------------
# set (AC1-ERR, AC2-ERR, happy) - isolated to a tmp project scope
# ---------------------------------------------------------------------------


def _roles_on_disk(repo_root: Path) -> dict:
    from fno.config.writer import read_scope_value

    return read_scope_value("model_routing.roles", scope="project", repo_root=repo_root) or {}


def test_set_preserves_existing_roles(project_scope: Path) -> None:
    runner.invoke(route_app, ["set", "tidy", "zai,glm-4.7", "--local"])
    runner.invoke(route_app, ["set", "build", "zai/glm-5.2", "--local"])
    # Comma input is normalized to the canonical slash form on write.
    assert _roles_on_disk(project_scope) == {
        "tidy": "zai/glm-4.7",
        "build": "zai/glm-5.2",
    }


def test_set_protected_role_hint_names_the_owning_surface(project_scope: Path) -> None:
    """Refusing without naming the real surface sends the operator hunting for a
    knob this table does not have."""
    res = runner.invoke(route_app, ["set", "implement", "zai,glm-5.2", "--local"])
    assert res.exit_code == 2
    assert "hint:" in res.output
    assert "'build'" in res.output
    assert not (project_scope / ".fno" / "config.toml").exists()


def test_set_unknown_provider_refused_no_write(project_scope: Path) -> None:
    res = runner.invoke(route_app, ["set", "build", "zia,glm-5.2", "--local"])
    assert res.exit_code == 2
    assert "unknown provider" in res.output.lower()
    assert not (project_scope / ".fno" / "config.toml").exists()


# ---------------------------------------------------------------------------
# env (AC5-HP, AC2-FR) - built-in zai provider, no config needed
# ---------------------------------------------------------------------------


def test_env_unsets_parent_anthropic_creds_before_exports(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    # A parent ANTHROPIC_API_KEY / OAuth token in the invoking shell would
    # otherwise win over the routed AUTH_TOKEN and silently bill Anthropic. env
    # must clear them (parity with bg_create), BEFORE the exports.
    monkeypatch.setenv("ZAI_API_KEY", "zk-live")
    res = runner.invoke(route_app, ["env", "zai,glm-5.2"])
    assert res.exit_code == 0
    out = res.stdout
    assert "unset ANTHROPIC_API_KEY" in out
    assert "unset CLAUDE_CODE_OAUTH_TOKEN" in out
    assert out.index("unset ANTHROPIC_API_KEY") < out.index("export ANTHROPIC_AUTH_TOKEN")


def test_env_missing_key_emits_no_unset_or_export(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    # Fail-closed: nothing on stdout at all, not even the unset lines.
    monkeypatch.delenv("ZAI_API_KEY", raising=False)
    res = runner.invoke(route_app, ["env", "zai,glm-5.2"])
    assert res.exit_code == 1
    assert "unset " not in res.stdout
    assert "export " not in res.stdout
    assert "ZAI_API_KEY" in res.output


# ---------------------------------------------------------------------------
# inventory (the declared-model reachability read) + routing init
# ---------------------------------------------------------------------------


def _declare(monkeypatch, rows, objective="cheapest-that-clears"):
    from fno import route_resolve as rr

    inv = _inv(rows)
    monkeypatch.setattr(rr, "resolve_inventory", lambda **_kw: inv)


def _inv(rows):
    """An Inventory declaring exactly ``rows`` (the fixture builder the
    Python fold lost; construction is all that survives in Python)."""
    from fno import route_resolve as _rr

    built = {}
    for r in rows:
        r = dict(r)
        built[r.get("name", "")] = _rr.InventoryRow(
            name=r.get("name", ""),
            harness=r.get("harness", ""),
            model=r.get("model", ""),
            route=r.get("route", ""),
            account=r.get("account", ""),
            band=r.get("band", ""),
            effort=r.get("effort", ""),
            operator_view=r.get("operator_view", ""),
        )
    return _rr.Inventory(rows=built, declared=True)


@requires_rust
def test_inventory_lists_rows_bands_and_verdicts(monkeypatch) -> None:
    _declare(
        monkeypatch,
        [
            {"name": "glm-5.3", "harness": "claude", "model": "glm-5.3", "band": "medium"},
            # A harness fno will never know. It used to be "pi", which stopped
            # being uninstallable the day pi was onboarded.
            {"name": "ghost-x", "harness": "ghostharness", "model": "g", "band": "high"},
            {"name": "mystery", "harness": "claude", "model": "m"},
        ],
    )
    res = runner.invoke(route_app, ["inventory"])
    assert res.exit_code == 0
    assert "objective=cheapest-that-clears" in res.output
    assert "glm-5.3" in res.output and "ok" in res.output
    # AC3-ERR: the uninstalled harness refuses BY NAME on stderr
    assert "refused: ghost-x: harness 'ghostharness' is not installed" in res.output
    assert "unbanded" in res.output
