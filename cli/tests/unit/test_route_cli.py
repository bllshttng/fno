"""Tests for the `fno route` verb family (ls / set / unset / env) - x-b0b4."""
from __future__ import annotations
from pathlib import Path

import pytest
from typer.testing import CliRunner


from fno.agents import model_routing as mr
from fno.agents import spawn_overlay_client as soc
from fno.config import ConfigBlock, ModelProvider, ModelRoutingBlock, SettingsModel
from fno.route_cli import route_app

requires_rust = pytest.mark.dev_build

runner = CliRunner()


def _settings(**block_kwargs: object) -> SettingsModel:
    return SettingsModel(
        config=ConfigBlock(model_routing=ModelRoutingBlock(**block_kwargs))
    )


@pytest.fixture
def project_scope(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> Path:
    """Isolate --local (project-scope) config writes into a tmp repo root."""
    monkeypatch.setattr("fno.paths.resolve_repo_root", lambda: tmp_path)
    return tmp_path


_ZAI_ONLY_RECORDS = {
    "zai": {
        "protocol": "anthropic",
        "base_url": "https://api.z.ai/api/anthropic",
        "api_key_env": "ZAI_API_KEY",
        "api_key_file": "~/.fno/.env",
        "tier_models": {"haiku": "glm-4.7"},
    },
}


def _providers_stub(records: dict) -> object:
    """Canned spawn-overlay answers: pytest shards run without the binary,
    so the registry the render reads is pinned here. The key column derives
    from the payload env the transport forwards; the real answer is the
    Rust owner's, pinned by the parity goldens. A claude-route payload
    answers the first record's route when its key env is present, else the
    skip notice, mirroring the owner's env-beats-file read."""

    def _call(payload: dict) -> dict:
        kind = payload.get("kind")
        env = payload.get("env")
        if env is None:
            import os

            env = os.environ
        if kind == "providers":
            out = {}
            for name, record in records.items():
                record = dict(record)
                key_env = record.get("api_key_env") or ""
                satisfying = key_env if key_env and env.get(key_env) else None
                record["key"] = {
                    "satisfying": satisfying,
                    "checked": [key_env]
                    if satisfying
                    else [key_env, record.get("api_key_file")],
                }
                out[name] = record
            return {"providers": out}
        assert kind == "claude-route", payload
        record = records.get(payload.get("provider") or "")
        key_env = (record or {}).get("api_key_env") or ""
        if record is None or not (key_env and env.get(key_env)):
            return {
                "route": None,
                "notices": [
                    "model-routing: no API key for provider"
                    f" '{payload.get('provider')}' ({key_env or 'no key env'})"
                ],
            }
        model = payload.get("model") or ""
        route = {
            "ANTHROPIC_BASE_URL": record["base_url"],
            "ANTHROPIC_AUTH_TOKEN": env[key_env],
            "ANTHROPIC_MODEL": model,
            "ANTHROPIC_DEFAULT_OPUS_MODEL": model,
            "ANTHROPIC_DEFAULT_SONNET_MODEL": model,
            "ANTHROPIC_DEFAULT_FABLE_MODEL": model,
        }
        if record.get("tier_models"):
            for alias, tier_model in record["tier_models"].items():
                route[f"ANTHROPIC_DEFAULT_{alias.upper()}_MODEL"] = tier_model
        return {"route": route, "notices": []}

    return _call


# ---------------------------------------------------------------------------
# ls / build_route_table (AC1-HP, AC1-UI)
# ---------------------------------------------------------------------------


def test_route_table_shows_builtins_config_build_and_protected(monkeypatch) -> None:
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
    records = {
        "zai": {
            "protocol": "anthropic",
            "base_url": "https://api.z.ai/api/anthropic",
            "api_key_env": "ZAI_API_KEY",
            "api_key_file": "~/.fno/.env",
            "tier_models": {"haiku": "glm-4.7"},
        },
        "zai-openai": {
            "protocol": "openai",
            "base_url": "https://api.z.ai/api/coding/paas/v4",
            "api_key_env": "ZAI_API_KEY",
            "api_key_file": None,
            "tier_models": {},
        },
    }
    monkeypatch.setattr(soc, "spawn_overlay_call", _providers_stub(records))
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

    return read_scope_value(
        "model_routing.roles", scope="project", repo_root=repo_root
    ) or {}


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
    monkeypatch.setattr(
        soc, "spawn_overlay_call", _providers_stub(_ZAI_ONLY_RECORDS)
    )
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
    monkeypatch.setattr(
        soc, "spawn_overlay_call", _providers_stub(_ZAI_ONLY_RECORDS)
    )
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
    _declare(monkeypatch, [
        {"name": "glm-5.3", "harness": "claude", "model": "glm-5.3", "band": "medium"},
        # A harness fno will never know. It used to be "pi", which stopped
        # being uninstallable the day pi was onboarded.
        {"name": "ghost-x", "harness": "ghostharness", "model": "g", "band": "high"},
        {"name": "mystery", "harness": "claude", "model": "m"},
    ])
    res = runner.invoke(route_app, ["inventory"])
    assert res.exit_code == 0
    assert "objective=cheapest-that-clears" in res.output
    assert "glm-5.3" in res.output and "ok" in res.output
    # AC3-ERR: the uninstalled harness refuses BY NAME on stderr
    assert "refused: ghost-x: harness 'ghostharness' is not installed" in res.output
    assert "unbanded" in res.output
