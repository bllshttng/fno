"""Business-role compatibility tests for the existing model-routing lanes."""

from __future__ import annotations

from collections.abc import Callable
import json
from pathlib import Path

import pytest

from fno import rust_binary
from fno.agents import model_routing as mr
from fno.agents import spawn_overlay_client as soc
from fno.company.contracts import FunctionRef, RoleRef
from fno.config import ConfigBlock, ModelRoutingBlock, SettingsModel
from fno.roles import (
    AuthorityCeiling,
    DeliveryPolicy,
    ReviewPolicy,
    RoleDefinitionSource,
    RoleLayer,
    RoleManifest,
    RoleResolutionReason,
    RoutingHint,
)

requires_rust = pytest.mark.dev_build


def _pin_oai_config(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    """Write the oai provider record to a real config.toml and pin it via
    FNO_CONFIG: the codex lane's provider half resolves in Rust, which reads
    the real candidates (x-3954), so a pydantic settings block cannot carry it."""
    cfg = tmp_path / "config.toml"
    cfg.write_text(
        "[model_routing.providers.oai]\n"
        'protocol = "openai"\n'
        'base_url = "https://example.test/v1"\n'
        'api_key_env = "OPENAI_API_KEY"\n',
        encoding="utf-8",
    )
    monkeypatch.setenv("FNO_CONFIG", str(cfg))
    monkeypatch.setenv("OPENAI_API_KEY", "openai-key")


def _call_resolver(resolver, role, *, settings=None, env=None, notice=None):
    """``resolve_codex_route`` no longer takes ``env=`` (the key resolves in
    the Rust command's process env); the claude signature keeps it."""
    kwargs: dict = {}
    if notice is not None:
        kwargs["notice"] = notice
    if resolver is mr.resolve_route:
        kwargs["settings"] = settings
        kwargs["env"] = env
    elif settings is not None:
        kwargs["settings"] = settings
    return resolver(role, **kwargs)


def _settings(**block_kwargs: object) -> SettingsModel:
    block = ModelRoutingBlock(**block_kwargs)  # type: ignore[arg-type]
    return SettingsModel(  # type: ignore[call-arg]
        config=ConfigBlock(model_routing=block)
    )


def _resolved(*, provider: str | None = None, model: str | None = None) -> dict:
    role = {"id": "publisher", "function_id": "communications"}
    projection = {"provider": provider, "model": model}
    return {"role": role, "source_digest": "0" * 64, "routing_projection": projection}


def _answer_business_role(monkeypatch: pytest.MonkeyPatch, answer: dict) -> list[str]:
    """Answer the business-role door with ``answer``; return the roles asked."""
    asked: list[str] = []
    real = rust_binary.command_call

    def door(command, payload, *args, **kwargs):
        if payload.get("kind") != "business-role":
            return real(command, payload, *args, **kwargs)
        asked.append(payload["role"])
        return answer

    monkeypatch.setattr(rust_binary, "command_call", door)
    return asked


OPENAI_PROVIDER = {
    "oai": {
        "protocol": "openai",
        "base_url": "https://example.test/v1",
        "api_key_env": "OPENAI_API_KEY",
    }
}


def _write_business_role(root: Path, *, provider: str | None, model: str | None) -> Path:
    role = RoleRef(id="publisher", function_id="communications")
    source = RoleDefinitionSource(
        layer=RoleLayer.COMPANY,
        source_id="company/publisher.json",
        snapshot_revision="snapshot-1",
        role=role,
        manifest=RoleManifest(
            role=role,
            function=FunctionRef(id=role.function_id),
            mission="Publish one bounded artifact.",
            deliverable_kinds=("brief",),
            authority_ceiling=AuthorityCeiling.INTERNAL,
            review_policy=ReviewPolicy(required=True, minimum_reviewers=1),
            delivery_policy=DeliveryPolicy(required_evidence=("artifact",)),
            default_topology="direct",
            routing_hint=(
                RoutingHint(provider=provider, model=model)
                if provider is not None or model is not None
                else None
            ),
        ),
    )
    path = root / "company" / "publisher.json"
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(source.model_dump(mode="json")), encoding="utf-8")
    return path


def _canned_route_stub(payload: dict, **_kwargs: object) -> dict:
    """Canned claude-route answers for the two providers these tests route
    to; pytest shards run without the binary. The bytes are the parity
    goldens' contract, and a caller-pinned block's extra_env merges into
    the route exactly as the Rust owner merges it."""
    canned = {
        "zai": "https://api.z.ai/api/anthropic",
        "deepseek": "https://deepseek.test/anthropic",
    }
    model = payload["model"]
    route = {
        "ANTHROPIC_BASE_URL": canned.get(payload["provider"], "u"),
        "ANTHROPIC_AUTH_TOKEN": "t",
        "ANTHROPIC_MODEL": model,
        "ANTHROPIC_DEFAULT_OPUS_MODEL": model,
        "ANTHROPIC_DEFAULT_SONNET_MODEL": model,
        "ANTHROPIC_DEFAULT_FABLE_MODEL": model,
        "ANTHROPIC_DEFAULT_HAIKU_MODEL": "glm-4.7",
    }
    route.update((payload.get("block") or {}).get("extra_env") or {})
    return {"route": route, "notices": []}


def test_resolved_business_role_changes_only_claude_provider_and_model(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    settings = _settings(
        providers={
            "deepseek": {
                "protocol": "anthropic",
                "base_url": "https://deepseek.test/anthropic",
                "api_key_env": "DEEPSEEK_API_KEY",
            }
        },
        roles={"build": "zai/legacy-model"},
        extra_env={"ROUTE_MARKER": "preserved"},
    )

    monkeypatch.setattr(soc, "spawn_overlay_call", _canned_route_stub)
    _answer_business_role(monkeypatch, _resolved(provider="deepseek", model="business-model"))
    route = mr.resolve_route(
        "build",
        settings=settings,
        env={"ZAI_API_KEY": "zai-key", "DEEPSEEK_API_KEY": "deepseek-key"},
    )

    assert route is not None
    assert route["ANTHROPIC_BASE_URL"] == "https://deepseek.test/anthropic"
    assert route["ANTHROPIC_MODEL"] == "business-model"
    assert route["ROUTE_MARKER"] == "preserved"
    assert not any(
        word in " ".join(route).lower()
        for word in ("manifest", "capability", "authority", "context", "approval", "delivery")
    )


def test_incomplete_projection_without_legacy_target_names_the_missing_field(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    _answer_business_role(monkeypatch, _resolved(provider="zai"))
    with pytest.raises(mr.BusinessRoleRoutingProjectionError, match="missing model"):
        mr.resolve_route("publisher", settings=_settings(), env={"ZAI_API_KEY": "zai-key"})


@pytest.mark.parametrize("resolver", [mr.resolve_route, mr.resolve_codex_route])
def test_disabled_routing_invalid_manifest_still_fails_closed(
    resolver: Callable[..., object],
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    blocked = {
        "role": {"id": "publisher", "function_id": "communications"},
        "reason": "invalid_manifest",
        "source_layer": None,
        "source_id": "company/roles.toml",
        "reference": "publisher",
        "detail": None,
    }
    _answer_business_role(monkeypatch, blocked)

    with pytest.raises(mr.BusinessRoleResolutionBlockedError) as caught:
        _call_resolver(resolver, "publisher", settings=_settings(enabled=False))

    assert caught.value.result.reason is RoleResolutionReason.INVALID_MANIFEST
    assert caught.value.result.source_id == "company/roles.toml"


def test_protected_business_names_validate_manifest_but_keep_primary_route(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    asked = _answer_business_role(monkeypatch, _resolved(provider="oai", model="unsafe"))

    assert (
        _call_resolver(
            mr.resolve_route,
            "implement",
            settings=_settings(providers=OPENAI_PROVIDER, roles={"implement": "oai/unsafe"}),
        )
        is None
    )
    assert asked == ["implement"]


@pytest.mark.dev_build
def test_default_production_lookup_projects_manifest_through_spawn_route(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    root = tmp_path / "roles"
    _write_business_role(root, provider="zai", model="business-model")
    _route_stub = {
        "route": {
            "ANTHROPIC_BASE_URL": "https://api.z.ai/api/anthropic",
            "ANTHROPIC_AUTH_TOKEN": "t",
            "ANTHROPIC_MODEL": "business-model",
            "ANTHROPIC_DEFAULT_OPUS_MODEL": "business-model",
            "ANTHROPIC_DEFAULT_SONNET_MODEL": "business-model",
            "ANTHROPIC_DEFAULT_FABLE_MODEL": "business-model",
            "ANTHROPIC_DEFAULT_HAIKU_MODEL": "glm-4.7",
        },
        "notices": [],
    }
    monkeypatch.setattr(soc, "spawn_overlay_call", lambda payload: _route_stub)
    monkeypatch.setenv("FNO_ROLES_ROOT", str(root))
    monkeypatch.setenv("ZAI_API_KEY", "zai-key")
    monkeypatch.setattr(mr, "_routing_block", lambda settings: _settings().model_routing)

    route = mr.resolve_spawn_route("publisher")

    assert route is not None
    assert route["ANTHROPIC_BASE_URL"] == "https://api.z.ai/api/anthropic"
    assert route["ANTHROPIC_MODEL"] == "business-model"


@requires_rust
def test_default_production_lookup_projects_manifest_through_codex_route(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    root = tmp_path / "roles"
    _write_business_role(root, provider="oai", model="gpt-business")
    monkeypatch.setenv("FNO_ROLES_ROOT", str(root))
    _pin_oai_config(tmp_path, monkeypatch)

    route = mr.resolve_codex_route("publisher")

    assert route is not None
    # The stamp rides with the codex lane too (x-c703): without it a routed
    # codex worker resolves provider "unknown" and ignores its subagent budget.
    assert route.env == {"OPENAI_API_KEY": "openai-key", "FNO_ROUTE_PROVIDER": "oai"}
    assert "model='gpt-business'" in " ".join(route.config_args)


@pytest.mark.parametrize(
    ("resolver", "settings", "env"),
    [
        (
            mr.resolve_route,
            _settings(roles={"publisher": "zai/legacy-model"}),
            {"ZAI_API_KEY": "zai-key"},
        ),
    ],
)
@pytest.mark.dev_build
def test_business_manifest_without_routing_hint_ignores_same_name_legacy_route(
    resolver: Callable[..., object],
    settings: SettingsModel,
    env: dict[str, str],
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    root = tmp_path / "roles"
    _write_business_role(root, provider=None, model=None)
    monkeypatch.setenv("FNO_ROLES_ROOT", str(root))

    assert _call_resolver(resolver, "publisher", settings=settings, env=env) is None


