"""config.routing: the declared inventory block + the shipped sample.

Config stays a leaf module (x-7fdd): no validation against the harness map at
load time, an unknown objective degrades to the default, and the sample file
no code path reads must still parse and carry the fields the schema names.
"""
from __future__ import annotations

from types import SimpleNamespace

from fno.config import SettingsModel


def _settings(payload: dict) -> SettingsModel:
    return SettingsModel.model_validate(payload)


def test_auto_merge_grant_is_true_only_for_dispatch():
    from fno.config.grant import auto_merge_grant

    assert auto_merge_grant(_settings({"auto_merge": {"grant": "dispatch"}}))
    assert not auto_merge_grant(_settings({"auto_merge": {"grant": "none"}}))
    assert not auto_merge_grant(SimpleNamespace(auto_merge=SimpleNamespace(grant=True)))
    assert not auto_merge_grant(None)


# --- config.sideline.colors (x-1b35) ----------------------------------------


def test_sideline_colors_bare_key_is_refused():
    """A bare key is ambiguous between account, route and model - the config
    layer refuses it by name instead of guessing an axis."""
    import pytest
    from pydantic import ValidationError

    with pytest.raises(ValidationError) as err:
        _settings({"sideline": {"colors": {"zai": "green"}}})
    assert "forbid" in str(err.value).lower() or "extra" in str(err.value).lower()


def test_spawn_defaults_carry_a_harness_overlay():
    """AC1-HP: the per-harness overlay loads beside an unchanged base scalar.

    The base that works for most stays put; a harness whose flag vocabulary
    differs gets its own answer keyed by harness."""
    s = _settings({
        "agents": {
            "defaults": {
                "permission_mode": "bypassPermissions",
                "harness": {
                    "codex": {
                        "permission_mode": "yolo",
                        "args": ["--profile", "fno"],
                    },
                },
            },
            "profiles": {
                "target": {
                    "effort": "high",
                    "harness": {"codex": {"effort": "xhigh"}},
                },
            },
        },
    })
    d = s.agents.defaults
    assert d.permission_mode == "bypassPermissions"
    assert d.harness["codex"].permission_mode == "yolo"
    assert d.harness["codex"].args == ["--profile", "fno"]
    prof = s.agents.profiles["target"]
    assert prof.effort == "high"
    assert prof.harness["codex"].effort == "xhigh"


def test_provider_tier_models_rejects_unknown_tier_key():
    """AC5-ERR: a tier_models key outside the Claude tier aliases is refused at
    load, naming the bad key and the legal set - a typo is a refusal, never a
    silently ignored tier."""
    import pytest
    from pydantic import ValidationError

    from fno.config import ModelProvider

    with pytest.raises(ValidationError) as excinfo:
        ModelProvider(tier_models={"bogus": "glm-5.3[1m]"})
    message = str(excinfo.value)
    assert "bogus" in message
    assert "opus" in message
