"""Unit tests for role-based model routing.

One table test per family; each row is a distinct branch of the resolver.
The resolver contract lives on config.model_routing (providers / roles /
extra_env): routed roles need a key (fail-safe None + notice), production
roles never route, and the tier map layers per provider.
"""

from __future__ import annotations

import pytest

from fno.agents import model_routing as mr
from fno.agents import spawn_overlay_client as soc
from fno.config import ConfigBlock, ModelRoutingBlock, SettingsModel


def _claude_route_answer(route: dict | None, notices: tuple[str, ...] = ()) -> dict:
    return {"route": route, "notices": list(notices)}


def _pin_claude_route(monkeypatch, answer: dict) -> None:
    """Pin the spawn-overlay answer the Rust route owner gives: pytest
    shards run without the binary, so the transport's payload contract and
    its answer passthrough are what these tests assert. The route bytes
    themselves are the parity goldens' contract."""
    monkeypatch.setattr(soc, "spawn_overlay_call", lambda payload: answer)


def _pinned_providers(monkeypatch, providers: dict) -> None:
    monkeypatch.setattr(
        soc, "spawn_overlay_call", lambda payload: {"providers": providers}
    )


def _settings(**block_kwargs: object) -> SettingsModel:
    return SettingsModel(
        config=ConfigBlock(model_routing=ModelRoutingBlock(**block_kwargs))
    )


def _collector() -> tuple[list[str], "object"]:
    notes: list[str] = []
    return notes, notes.append


# --- routed roles ---------------------------------------------------------------


# --- resolve_explicit_route --------------------------------------------------------

def test_explicit_route_rows(monkeypatch) -> None:
    captured: dict = {}

    def stub(payload):
        captured.update(payload)
        return _claude_route_answer(
            route={"ANTHROPIC_BASE_URL": "u", "ANTHROPIC_AUTH_TOKEN": "zk", "ANTHROPIC_MODEL": "glm-5.2"}
        )

    monkeypatch.setattr(soc, "spawn_overlay_call", stub)
    route = mr.resolve_explicit_route(
        "zai", "glm-5.2", settings=_settings(), env={"ZAI_API_KEY": "zk"}
    )
    assert route is not None and route["ANTHROPIC_MODEL"] == "glm-5.2"
    # The peer lane names its target directly: provider/model ride the
    # payload with the peer context and the env mapping.
    assert captured["provider"] == "zai"
    assert captured["model"] == "glm-5.2"
    assert captured["ctx"] == {"kind": "peer", "name": "zai"}
    assert captured["env"] == {"ZAI_API_KEY": "zk"}
    # An explicit peer opt-in is not role auto-routing: the disabled global
    # flag does not apply (there is no role), so the transport answers.
    route = mr.resolve_explicit_route(
        "zai", "glm-5.2", settings=_settings(enabled=False), env={"ZAI_API_KEY": "k"}
    )
    assert route is not None and route["ANTHROPIC_MODEL"] == "glm-5.2"


def test_explicit_route_no_key_fails_safe(monkeypatch) -> None:
    notes, sink = _collector()
    _pin_claude_route(
        monkeypatch,
        _claude_route_answer(
            route=None,
            notices=(
                "model-routing: no API key for provider 'zai' (peer 'zai'); skipping the peer",
            ),
        ),
    )
    route = mr.resolve_explicit_route(
        "zai", "glm-5.2", settings=_settings(), env={}, notice=sink
    )
    assert route is None
    assert any("skipping the peer" in n for n in notes)
    assert not any("primary" in n for n in notes)


def test_refuses_bare_tier_alias_on_foreign_endpoint() -> None:
    notes, sink = _collector()
    route = mr.resolve_explicit_route(
        "zai", "sonnet", settings=_settings(), env={"ZAI_API_KEY": "k"}, notice=sink
    )
    assert route is None
    assert any("sonnet" in n and "zai" in n for n in notes)


# --- protected roles ------------------------------------------------------------------


def _pin_codex_config(
    tmp_path, monkeypatch, provider: str, body: str, *, role: str = "tidy"
) -> None:
    cfg = tmp_path / "config.toml"
    cfg.write_text(
        "[model_routing]\n"
        f'[model_routing.roles]\n{role} = "{provider},glm-5.2"\n'
        f"[model_routing.providers.{provider}]\n" + body,
        encoding="utf-8",
    )
    monkeypatch.setenv("FNO_CONFIG", str(cfg))


requires_rust = pytest.mark.dev_build


# --- x-5cc5: a recorded overlay's provider-DEFAULT tier re-resolves on resume -----

def _recorded_route(
    haiku: str = "glm-4.5-air", base: str = "https://api.z.ai/api/anthropic"
) -> dict[str, str]:
    return {
        "ANTHROPIC_BASE_URL": base,
        "ANTHROPIC_AUTH_TOKEN": "zk-secret",
        "ANTHROPIC_MODEL": "glm-5.3",
        "ANTHROPIC_DEFAULT_OPUS_MODEL": "glm-5.3",
        "ANTHROPIC_DEFAULT_HAIKU_MODEL": haiku,
    }


