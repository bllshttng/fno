"""Role-based per-spawn model routing for fno agents.

Auxiliary coordination work (backlog tidying, node orientation, memory
consolidation) is routed to a *secondary* model provider (z.ai GLM, DeepSeek,
...) via the worker's environment at spawn time; production work (writing the
diff, the correctness verdict) stays on the primary Anthropic model,
byte-for-byte as today.

The ``build`` lane extends the same mechanism to *delivery* spawns (``/target
bg`` + the ordered ``fno backlog advance`` drain). It is opt-in by config presence: unconfigured it
routes nothing (fail-safe ``None``); writing ``model_routing.roles.build`` is
the consent. ``build`` is not in :data:`DEFAULT_ROUTED_ROLES` but is named in
:data:`KNOWN_LANE_ROLES` so ``fno config route ls`` renders it even before it is set.

Mechanism (Locked Decision 2): a spawn stamps ``ANTHROPIC_BASE_URL`` +
``ANTHROPIC_AUTH_TOKEN`` + the model env vars into the worker env. No proxy in
the critical path; each worker is a fresh process, so switching base_url per
spawn is safe (never switch base_url mid-session).

A spawned worker is ``claude --bg``, which speaks the **Anthropic** Messages
API. So a provider is usable here only if it exposes an Anthropic-compatible
endpoint (z.ai: ``https://api.z.ai/api/anthropic``; DeepSeek:
``https://api.deepseek.com/anthropic``). The OpenAI-protocol endpoints those
same vendors publish (z.ai's ``/api/coding/paas/v4``) are for OpenAI-SDK
consumers and a future codex/openai lane, not for a claude worker. A provider
whose ``protocol`` is not ``anthropic`` is skipped here with a notice.

Claude Code internally requests the opus/sonnet/haiku/fable tiers (background
tasks use haiku). Setting ``ANTHROPIC_MODEL`` + EVERY ``ANTHROPIC_DEFAULT_*`` tier
var to the routed model sends the WHOLE worker to the secondary provider, so no
Anthropic usage is recorded (AC1-HP). The background (haiku) tier defaults to
the provider's cheaper ``haiku_model`` (zai -> ``glm-4.7``) so judgment-light
background traffic runs cheap on the SAME secondary provider; opus/sonnet stay
on the role model. A provider with no ``haiku_model`` keeps the role model on
every tier. Operators can further differentiate any tier via ``extra_env``.

A routed model name carrying the ``[1m]`` suffix also gets an
``CLAUDE_CODE_AUTO_COMPACT_WINDOW`` backstop injected. The ``[1m]`` variant
already selects the 1M context; this var is the compaction THRESHOLD (how full
the window gets before Claude Code compacts), capped at the model's window, so
it cannot extend anything. It takes precedence over ``/autocompact``,
``--autocompact``, and the saved setting, so the injected value is what the
worker runs with; an explicit ``extra_env`` value still wins.

Two non-negotiable invariants:

- **Fail safe, not fail closed** (US4): if no key is configured for the role's
  provider, the role falls back to the primary Anthropic model with a one-line
  notice and the spawn still succeeds. The legacy :func:`resolve_route` path
  never raises; the business-role lookup fails closed on typed resolution
  errors so they cannot be mistaken for legacy fallback.
- **Hard quality guard**: ``implement`` / ``review-verdict`` are in
  ``PROTECTED_ROLES`` and short-circuit to ``None`` *before* any config is read.
  No settings edit can route the diff or verdict to a secondary provider.
"""

from __future__ import annotations

from typing import (
    TYPE_CHECKING,
    Callable,
    Mapping,
    Optional,
)

from fno.config._tiers import TIER_ALIASES

if TYPE_CHECKING:
    from fno.config import ModelRoutingBlock, SettingsModel


# Default secondary model for routed roles. glm-4.5-air is too weak for
# reasoning-bearing work; a current flagship GLM does real work. Pin a cheaper
# model per role via the roles map.
DEFAULT_SECONDARY_MODEL = "glm-5.3"


def is_anthropic_model(model: str) -> bool:
    """True for an Anthropic model id or tier alias.

    Pinning a tier to a specific Anthropic model
    (``ANTHROPIC_DEFAULT_OPUS_MODEL=claude-opus-4-1``) is a supported Claude
    Code customization, not a vendor conflict: endpoint and model still agree.
    Only a FOREIGN vendor's id (``glm-5.3[1m]``, ``deepseek-...``) creates the
    split this module guards against.
    """
    name = model.strip().lower()
    return name.startswith("claude-") or name in TIER_ALIASES


# The inherited-env carrier: a long-lived background daemon stamps the
# environment of the shell that started it into every session it spawns. When
# that shell held a foreign vendor's model exports with no base URL, every
# child asks Anthropic's endpoint for a model it does not serve and the whole
# tier ERRORS rather than degrading - far from the cause, because no config
# edit reaches a running daemon. The four spawn seams strip the vars below; a
# real route (base URL + token + model as one unit) is never stripped because
# the coherence question never fires under a foreign base URL.


def _emit(notice: Optional[Callable[[str], object]], message: str) -> None:
    """Surface a one-line fail-safe notice; quiet when no sink is supplied."""
    if notice is not None:
        notice(message)


def _normalize(role: Optional[str]) -> str:
    return (role or "").strip().lower()


def _parse_target(raw: str) -> Optional[tuple[str, str]]:
    """Parse a ``"provider/model"`` (or legacy ``"provider,model"``) target into
    (provider, model).

    Slash is the canonical, ecosystem-standard form (``zai/glm-5.3[1m]``); comma
    is still accepted for the existing peer-lane / built-in values and any config
    written before the switch, so nothing already configured breaks. A comma, if
    present, wins as the separator (it never appears inside a model id, so it is
    unambiguous); otherwise the FIRST slash splits, which keeps a namespaced model
    id intact (``zai/z-ai/glm-5.3`` -> provider ``zai``, model ``z-ai/glm-5.3``).

    Returns None for a malformed value (fail-safe; caller degrades to primary)."""
    raw = raw.strip()
    if "," in raw:
        parts = [p.strip() for p in raw.split(",")]
        if len(parts) != 2:
            return None
        provider, model = parts
    else:
        provider, found, model = raw.partition("/")
        if not found:
            return None
        provider, model = provider.strip(), model.strip()
    if not provider or not model:
        return None
    # Reject INTERNAL whitespace (a space or newline inside a token): the
    # contract is one non-whitespace model token, an embedded space yields an
    # invalid model id and a newline would corrupt the line-oriented dispatch
    # receipt. Outer whitespace was already stripped above.
    if any(c.isspace() for c in provider) or any(c.isspace() for c in model):
        return None
    return provider.lower(), model


def _block_payload(block: "ModelRoutingBlock") -> dict[str, object]:
    """The route builder's slice of a caller-pinned routing block: the
    provider overlay table and extra_env, the only fields the Rust owner
    reads. Each record dumps ONLY explicitly-set fields and no nulls, so a
    partial override keeps the built-in preset's other fields exactly as the
    deleted Python merge did (exclude_unset) and the JSON block converts to
    TOML cleanly (no nulls)."""
    providers: dict[str, object] = {}
    for name, record in (getattr(block, "providers", None) or {}).items():
        fields = (
            record.model_dump(exclude_unset=True)
            if hasattr(record, "model_dump")
            else dict(record)
        )
        providers[str(name)] = {str(k): v for k, v in fields.items() if v is not None}
    extra = getattr(block, "extra_env", None) or {}
    return {
        "providers": providers,
        "extra_env": {str(k): str(v) for k, v in extra.items()},
    }


def _claude_route_call(
    pname: str,
    model: str,
    env: Mapping[str, str],
    notice: Optional[Callable[[str], object]],
    *,
    ctx_kind: str,
    ctx_name: str,
    block: Optional[dict] = None,
) -> Optional[dict[str, str]]:
    """The route builder's transport: one spawn-overlay round trip to the one
    Rust owner (`provider_route.rs`). The role-to-target lookup above this
    call stays here; the preset, the merge, the env build and the notices
    answer from the binary. Returns None (fail-safe, never raises) when the
    binary is missing or skips the route; its notices ride ``notice``. A
    caller-pinned ``block`` (the settings a repository settings file owns)
    rides the payload so the one owner merges it instead of the config
    stack."""
    import os

    from fno.agents.spawn_overlay_client import SpawnOverlayUnavailable, spawn_overlay_call

    payload: dict = {
        "kind": "claude-route",
        "provider": pname,
        "model": model,
        "cwd": os.getcwd(),
        "ctx": {"kind": ctx_kind, "name": ctx_name},
    }
    if env is not os.environ:
        payload["env"] = dict(env)
    if block is not None:
        payload["block"] = block
    try:
        answer = spawn_overlay_call(payload)
    except SpawnOverlayUnavailable as exc:
        fallback = "skipping the peer" if ctx_kind == "peer" else "using the primary model"
        _emit(
            notice,
            f"model-routing: route builder unavailable ({exc}); {fallback}",
        )
        return None
    for note in answer.get("notices") or []:
        _emit(notice, str(note))
    route = answer.get("route")
    if not route:
        return None
    return {str(k): str(v) for k, v in route.items()}


def resolve_explicit_route(
    provider: str,
    model: str,
    *,
    settings: "Optional[SettingsModel]" = None,
    env: Optional[Mapping[str, str]] = None,
    notice: Optional[Callable[[str], object]] = None,
) -> Optional[dict[str, str]]:
    """Resolve the routed ``ANTHROPIC_*`` env for an explicit ``provider,model``.

    The peer lane (``config.review.peers`` entry ``{provider: claude, model:
    "zai,glm-5.3"}``) names its route directly rather than via a role, so it
    bypasses both the role->target lookup AND the ``PROTECTED_ROLES`` / global
    ``enabled`` role-auto-routing policy (an explicit peer opt-in is not
    auto-routing). It reuses the SAME provider-registry key/env logic as
    ``resolve_route`` so the z.ai env-var contract lives in exactly one place.

    Returns None (fail-safe, never raises) when the provider is unknown,
    non-anthropic, or keyless - the caller (peer skill) then skips the GLM peer
    rather than silently falling back to the Anthropic-billed author model.
    """
    pname = _normalize(provider)
    if not pname or not (model or "").strip():
        return None
    if env is None:
        import os

        env = os.environ
    return _claude_route_call(
        pname,
        model.strip(),
        env,
        notice,
        ctx_kind="peer",
        ctx_name=pname,
        block=_block_payload(_routing_block(settings)) if settings is not None else None,
    )


def _routing_block(settings: "Optional[SettingsModel]") -> "ModelRoutingBlock":
    """Return the model_routing config block, loading settings if needed."""
    if settings is None:
        from fno.config import load_settings

        settings = load_settings()
    return settings.model_routing
