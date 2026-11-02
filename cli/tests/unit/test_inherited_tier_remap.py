"""A spawn must never inherit a foreign vendor's tier-alias remap.

Regression for the split-brain that produced a "live" receipt and a session
that died on turn one: a z.ai/GLM parent exports
``ANTHROPIC_DEFAULT_OPUS_MODEL=glm-5.2[1m]``, so ``spawn --model opus``
reached Anthropic asking for a GLM model id.
"""


from fno.adapters.providers.model import ProviderRecord

ZAI_ENV = {
    "ANTHROPIC_BASE_URL": "https://api.z.ai/api/anthropic",
    "ANTHROPIC_DEFAULT_OPUS_MODEL": "glm-5.2[1m]",
    "ANTHROPIC_DEFAULT_SONNET_MODEL": "glm-5.2[1m]",
    "ANTHROPIC_DEFAULT_HAIKU_MODEL": "glm-4.5-air",
}


# ---- auth=api_key recognizes ANTHROPIC_AUTH_TOKEN ----


def test_an_api_key_record_naming_an_anthropic_compatible_vendor_validates():
    # ANTHROPIC_AUTH_TOKEN is the credential Claude Code reads for a non-Anthropic
    # Anthropic-compatible endpoint (z.ai, DeepSeek), so an api_key record may
    # carry it instead of ANTHROPIC_API_KEY.
    record = ProviderRecord(
        id="zai",
        name="zai",
        cli="claude",
        auth="api_key",
        env={
            "ANTHROPIC_BASE_URL": "https://api.z.ai/api/anthropic",
            "ANTHROPIC_AUTH_TOKEN": "${ENV:ZAI_API_KEY}",
            "ANTHROPIC_DEFAULT_OPUS_MODEL": "glm-5.2[1m]",
        },
    )
    assert record.auth == "api_key"


# ---- warn (never refuse) when spawning under env scrub ----


# ---- strip an incoherent inherited model env at every spawn seam ----

#: The operator's measured daemon-carried env: GLM names, no base URL.
POISON_ENV = {
    "ANTHROPIC_MODEL": "glm-5.2[1m]",
    "ANTHROPIC_DEFAULT_HAIKU_MODEL": "glm-4.5-air",
}


if __name__ == "__main__":
    for name, fn in sorted(globals().items()):
        if name.startswith("test_"):
            fn()
            print(f"ok {name}")
