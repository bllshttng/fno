"""Canned spawn-overlay answers for tests that drive the route transports.

pytest shards run without the fno-agents binary, so a test pins the answer
the Rust route owner (`provider_route.rs`) gives. The canned shape mirrors
the zai preset; the bytes themselves are the parity goldens' contract, not
this stub's.
"""

import os

# accounts::credentials::SCRUB_AUTH_VARS, which the door removes from the
# base before layering; the tail-pinning test in Rust keeps this in step.
_SCRUB_AUTH_VARS = (
    "ANTHROPIC_API_KEY",
    "CLAUDE_CODE_OAUTH_TOKEN",
    "ANTHROPIC_BASE_URL",
    "ANTHROPIC_AUTH_TOKEN",
    "FNO_ROUTE_PROVIDER",
    "ANTHROPIC_MODEL",
    "ANTHROPIC_DEFAULT_OPUS_MODEL",
    "ANTHROPIC_DEFAULT_SONNET_MODEL",
    "ANTHROPIC_DEFAULT_HAIKU_MODEL",
    "ANTHROPIC_DEFAULT_FABLE_MODEL",
)


def claude_route_overlay_stub(payload: dict, **_kwargs: object) -> dict:
    """Answer the zai preset shape for a claude-route payload; providers
    answers empty. The key comes from the payload env, else the process
    env, exactly the exposure the kind has. Extra transport kwargs (the
    spawn path's explicit timeout) ride in and are ignored."""
    if payload.get("kind") == "providers":
        return {"providers": {}}
    if payload.get("kind") == "credentials":
        # The door's layering: base minus the auth scrub vars, then the
        # account layer, then the route last so it wins as one unit.
        env = {
            k: v
            for k, v in (payload.get("base") or {}).items()
            if k not in _SCRUB_AUTH_VARS
        }
        env.update(payload.get("account_env") or {})
        env.update(payload.get("route_env") or {})
        return {
            "env": env,
            "decision": {},
            "scrub": list(_SCRUB_AUTH_VARS),
            "secret": [],
        }
    env = payload.get("env") or os.environ
    model = payload.get("model", "")
    key = env.get("ZAI_API_KEY", "zk-stub")
    return {
        "route": {
            "ANTHROPIC_BASE_URL": "https://api.z.ai/api/anthropic",
            "ANTHROPIC_AUTH_TOKEN": key,
            "ANTHROPIC_MODEL": model,
            "ANTHROPIC_DEFAULT_OPUS_MODEL": model,
            "ANTHROPIC_DEFAULT_SONNET_MODEL": model,
            "ANTHROPIC_DEFAULT_FABLE_MODEL": model,
            "ANTHROPIC_DEFAULT_HAIKU_MODEL": "glm-4.7",
        },
        "notices": [],
    }
