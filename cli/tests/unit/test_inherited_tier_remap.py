"""A spawn must never inherit a foreign vendor's tier-alias remap.

Regression for the split-brain that produced a "live" receipt and a session
that died on turn one: a z.ai/GLM parent exports
``ANTHROPIC_DEFAULT_OPUS_MODEL=glm-5.2[1m]``, so ``spawn --model opus``
reached Anthropic asking for a GLM model id.
"""

import os
from types import SimpleNamespace

import pytest

from fno.adapters.providers.model import ProviderRecord
from fno.agents.account_env import SCRUB_AUTH_VARS
from fno.agents.model_routing import (
    ENV_SCRUB_VAR,
    MODEL_ENV_KEYS,
    TIER_ALIASES,
    TierRemapConflict,
    base_url_is_anthropic,
    check_spawn_tier_remap,
    env_scrub_warning,
    incoherent_model_env,
    incoherent_model_env_unset_args,
    overlay_restores_model_env,
    scrub_incoherent_model_env,
    scrub_incoherent_model_env_and_notify,
    unrouted_model_keys,
)
from fno.agents.rust_runtime import (
    _refuse_inherited_tier_remap,
    _scrub_account_auth_at_seam,
    _warn_env_scrub_spawn,
    env_scrub_spawn_warning,
    inherited_tier_remap,
)

ZAI_ENV = {
    "ANTHROPIC_BASE_URL": "https://api.z.ai/api/anthropic",
    "ANTHROPIC_DEFAULT_OPUS_MODEL": "glm-5.2[1m]",
    "ANTHROPIC_DEFAULT_SONNET_MODEL": "glm-5.2[1m]",
    "ANTHROPIC_DEFAULT_HAIKU_MODEL": "glm-4.5-air",
}

BASE = ["spawn", "--name", "w", "--substrate", "thread", "hi"]


def test_a_role_exempts_only_once_it_resolves_to_a_real_route(monkeypatch):
    # resolve_route is fail-SAFE: a protected role, a disabled block, an
    # unconfigured provider or a missing key all return None and leave the
    # ambient env untouched, so mentioning --role must not exempt on its own.
    args = [*BASE, "--role", "tidy", "--model", "opus"]

    monkeypatch.setattr(
        "fno.agents.model_routing.resolve_route", lambda *a, **k: None
    )
    assert inherited_tier_remap(args, ZAI_ENV) == ("opus", "glm-5.2[1m]")

    monkeypatch.setattr(
        "fno.agents.model_routing.resolve_route",
        lambda *a, **k: {"ANTHROPIC_MODEL": "glm-5.2"},
    )
    assert inherited_tier_remap(args, ZAI_ENV) is None


def test_cli_tier_preflight_normalizes_business_role_refusal(monkeypatch, capsys):
    from fno.agents.model_routing import BusinessRoleRoutingProjectionError

    for key, value in ZAI_ENV.items():
        monkeypatch.setenv(key, value)

    def blocked(*args, **kwargs):
        raise BusinessRoleRoutingProjectionError("invalid business role at CLI seam")

    monkeypatch.setattr("fno.agents.model_routing.resolve_route", blocked)

    with pytest.raises(SystemExit) as exc_info:
        _refuse_inherited_tier_remap(
            [*BASE, "--role", "publisher", "--model", "opus"]
        )

    assert exc_info.value.code == 2
    assert "invalid business role at CLI seam" in capsys.readouterr().err


def test_in_process_spawn_apis_enforce_the_same_invariant():
    # The CLI seam is one of several reachable paths; dispatch_spawn and
    # dispatch_spawn_pane accept `model` directly and must fail closed too.
    with pytest.raises(TierRemapConflict):
        check_spawn_tier_remap("claude", "opus", env=ZAI_ENV)
    # A resolved route or account overlay composes the whole thing: exempt.
    check_spawn_tier_remap("claude", "opus", env=ZAI_ENV, route_env={"A": "b"})
    check_spawn_tier_remap("claude", "opus", env=ZAI_ENV, account_env={"A": "b"})
    # Non-claude providers never read these vars.
    check_spawn_tier_remap("codex", "opus", env=ZAI_ENV)
    # A clean parent, and a vendor model id that is not a tier alias.
    check_spawn_tier_remap("claude", "opus", env={})
    check_spawn_tier_remap("claude", "glm-5.2[1m]", env=ZAI_ENV)


def test_routed_env_covers_every_tier_alias():
    # The set a route composes must equal the set of tiers, or a worker leaks
    # the uncovered tier back to Anthropic.
    assert set(MODEL_ENV_KEYS) == {"ANTHROPIC_MODEL"} | {
        f"ANTHROPIC_DEFAULT_{a.upper()}_MODEL" for a in TIER_ALIASES
    }


def test_account_spawn_scrubs_at_the_seam_so_rust_inherits_it(monkeypatch):
    # An --account spawn on --substrate bg auto-routes to the Rust client, which
    # has no ANTHROPIC_* handling; route_to_rust execs with os.environ, so the
    # seam is the only edit that lane sees.
    for k, v in ZAI_ENV.items():
        monkeypatch.setenv(k, v)
    monkeypatch.setattr(
        "fno.agents.account_env.resolve_account_overlay",
        lambda aid: SimpleNamespace(env={"CLAUDE_CONFIG_DIR": "/tmp/x"}),
    )
    _scrub_account_auth_at_seam([*BASE, "--account", "makers", "--model", "opus"])
    assert not [k for k in SCRUB_AUTH_VARS if k in os.environ]
    assert os.environ["CLAUDE_CONFIG_DIR"] == "/tmp/x"


def test_seam_clears_a_stale_launch_account_on_an_unaccounted_spawn(monkeypatch):
    # A worker spawned under --account carries FNO_LAUNCH_ACCOUNT in its env.
    # When THAT worker spawns a child with no account of its own, the seam must
    # clear the inherited id: the Rust mint reads it first, so a stale value
    # stamps the parent's account onto a row that pinned nothing.
    monkeypatch.setenv("FNO_LAUNCH_ACCOUNT", "parent-acct")
    _scrub_account_auth_at_seam([*BASE, "--model", "opus"])
    assert "FNO_LAUNCH_ACCOUNT" not in os.environ


def test_seam_resolves_the_account_before_scrubbing(monkeypatch):
    # An api_key record may reference the ambient env
    # (ANTHROPIC_API_KEY = "${ENV:ANTHROPIC_API_KEY}") and resolve_env_value
    # reads os.environ, so scrubbing first would delete the source value and
    # make a previously valid --account spawn unresolvable.
    monkeypatch.setenv("ANTHROPIC_API_KEY", "sk-from-parent")
    seen = {}

    def fake_resolve(account_id):
        seen["account"] = account_id
        seen["key_at_resolve"] = os.environ.get("ANTHROPIC_API_KEY")
        return SimpleNamespace(env={"ANTHROPIC_API_KEY": "sk-resolved"})

    monkeypatch.setattr(
        "fno.agents.account_env.resolve_account_overlay", fake_resolve
    )
    _scrub_account_auth_at_seam([*BASE, "--account=acct", "--model", "opus"])
    assert seen["account"] == "acct"
    assert seen["key_at_resolve"] == "sk-from-parent"
    assert os.environ["ANTHROPIC_API_KEY"] == "sk-resolved"


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


def test_env_scrub_warning_fires_when_var_set_and_a_mode_is_pinned():
    # --yolo is a stated permission intent; the var silently overrides it, and
    # the warning names both consequences plus the opt-out.
    msg = env_scrub_spawn_warning([*BASE, "--yolo"], env={ENV_SCRUB_VAR: "1"})
    assert msg is not None
    assert ENV_SCRUB_VAR in msg
    assert "ANTHROPIC_AUTH_TOKEN" in msg
    assert "ANTHROPIC_BASE_URL" in msg
    assert "--permission-mode" in msg
    assert f"{ENV_SCRUB_VAR}=0" in msg


def test_env_scrub_warning_is_not_a_refusal(monkeypatch, capsys):
    # A warning, never a refusal: the seam prints and returns normally even when
    # the message fires (unlike _refuse_inherited_tier_remap, which exits 2).
    monkeypatch.setenv(ENV_SCRUB_VAR, "1")
    _warn_env_scrub_spawn([*BASE, "--yolo"])  # must not raise
    captured = capsys.readouterr()
    assert ENV_SCRUB_VAR in captured.err


def test_env_scrub_warning_structured_form_uses_the_resolved_provider():
    # The in-process spawn APIs hand the detector the provider they resolved, so
    # it judges the spawn on what it actually is, not a guessed claude.
    msg = env_scrub_warning("claude", permission_pinned=True, env={ENV_SCRUB_VAR: "1"})
    assert msg is not None
    assert (
        env_scrub_warning("codex", permission_pinned=True, env={ENV_SCRUB_VAR: "1"})
        is None
    )
    assert env_scrub_warning("claude", permission_pinned=False, env={ENV_SCRUB_VAR: "1"}) is None
    assert env_scrub_warning("claude", permission_pinned=True, env={}) is None


# ---- strip an incoherent inherited model env at every spawn seam ----

#: The operator's measured daemon-carried env: GLM names, no base URL.
POISON_ENV = {
    "ANTHROPIC_MODEL": "glm-5.2[1m]",
    "ANTHROPIC_DEFAULT_HAIKU_MODEL": "glm-4.5-air",
}


def test_incoherent_model_env_returns_every_poisoned_var():
    # Every offender, not the first: the measured carrier poisoned more than
    # one var, and a repair that drops half of them ships the same bug on the
    # tiers it missed.
    assert dict(incoherent_model_env(POISON_ENV)) == POISON_ENV


def test_a_real_route_is_never_stripped():
    # A foreign base URL serves those model ids, so a composed route must reach
    # the child unchanged (AC3). The route case never even enters the strip.
    routed = {**POISON_ENV, "ANTHROPIC_BASE_URL": "https://api.z.ai/api/anthropic"}
    assert incoherent_model_env(routed) == ()
    assert base_url_is_anthropic(routed) is False


def test_bedrock_and_vertex_lanes_are_coherent():
    # Those lanes serve Anthropic models under ids that do not start with
    # "claude-" and leave the base URL unset, so the question does not apply
    # (AC4). A pre-existing false positive on the single-var detector.
    bedrock_env = {
        "CLAUDE_CODE_USE_BEDROCK": "1",
        "ANTHROPIC_MODEL": "us.anthropic.claude-sonnet-4-20250514-v1:0",
    }
    assert incoherent_model_env(bedrock_env) == ()
    assert incoherent_model_env({**bedrock_env, "CLAUDE_CODE_USE_VERTEX": "1"}) == ()
    # An explicit off-word is not a lane opt-in.
    off = {"CLAUDE_CODE_USE_BEDROCK": "0", **POISON_ENV}
    assert dict(incoherent_model_env(off)) == POISON_ENV


def test_dict_scrub_and_argv_args_drop_the_same_names():
    # Seam parity asserted on the resulting NAME SETS, not on both calling one
    # helper: asserting shared plumbing pins the call, not the destination.
    env = {**POISON_ENV, "ANTHROPIC_DEFAULT_SONNET_MODEL": "glm-5.2[1m]"}
    scrubbed = dict(env)
    dropped_by_dict = scrub_incoherent_model_env(scrubbed)
    argv = incoherent_model_env_unset_args(env)
    dropped_by_argv = {name for i, name in enumerate(argv) if i % 2 == 1}
    assert set(dropped_by_dict) == dropped_by_argv == {
        "ANTHROPIC_MODEL",
        "ANTHROPIC_DEFAULT_HAIKU_MODEL",
        "ANTHROPIC_DEFAULT_SONNET_MODEL",
    }
    assert all(flag == "-u" for i, flag in enumerate(argv) if i % 2 == 0)
    assert not [k for k in dropped_by_dict if k in scrubbed]
    # A coherent env scrubs nothing and builds a stable empty argv.
    assert scrub_incoherent_model_env(dict(ZAI_ENV)) == ()
    assert incoherent_model_env_unset_args(ZAI_ENV) == []


def test_unrouted_keys_name_every_claim_coherent_or_not():
    # The unrouted clear is not the incoherence predicate: a coherent pin is
    # still a claim the launching shell made, and an unrouted child must carry
    # no claim at all. Whitespace-only is no claim.
    env = {
        "ANTHROPIC_MODEL": "claude-opus-4-8",
        "ANTHROPIC_DEFAULT_HAIKU_MODEL": "  ",
        "ANTHROPIC_DEFAULT_OPUS_MODEL": "glm-5.2[1m]",
    }
    assert unrouted_model_keys(env) == (
        "ANTHROPIC_MODEL",
        "ANTHROPIC_DEFAULT_OPUS_MODEL",
    )


def test_an_unrouted_scrub_clears_a_coherent_claim_and_says_so(capsys):
    # A spawn composing no route inherits the shell's pin otherwise; the
    # notice is true whether or not --model was typed: it never
    # calls a pinned child unrouted nor prescribes the flag it already used.
    env = {"ANTHROPIC_MODEL": "claude-opus-4-8"}
    dropped = scrub_incoherent_model_env_and_notify(env, routed=False)
    assert dropped == ()
    assert "ANTHROPIC_MODEL" not in env
    notice = capsys.readouterr().err
    assert "fno: cleared ANTHROPIC_MODEL" in notice
    assert "The child's model comes from its --model flag" in notice
    assert "unrouted" not in notice
    assert "Select one with --model" not in notice


def test_overlay_restores_model_env_requires_an_actual_model_var():
    # A config-dir/managed-active account overlay (resolve_account_overlay's
    # "config-dir"/"active-slot" lanes) sets only CLAUDE_CONFIG_DIR - no
    # MODEL_ENV_KEYS entry - so the dropped var is never resupplied and the
    # child genuinely falls back to its account's own default. A caller that
    # treated "account_env is truthy" as "routed" printed a false claim.
    config_dir_only = {"CLAUDE_CONFIG_DIR": "/Users/x/.claude-alt"}
    assert overlay_restores_model_env(config_dir_only) is False
    assert overlay_restores_model_env(None, config_dir_only) is False

    # A real route, or an own-dir/api-key account overlay carrying its own
    # model pins, DOES resupply the tier.
    routed = {"ANTHROPIC_MODEL": "glm-5.3", "ANTHROPIC_BASE_URL": "https://api.z.ai"}
    assert overlay_restores_model_env(routed) is True
    assert overlay_restores_model_env(config_dir_only, routed) is True
    assert overlay_restores_model_env(None, None) is False


def test_hook_var_list_matches_model_env_keys():
    # The var list lives in two languages (the hook cannot import Python
    # without paying a subprocess at every SessionStart), so pin the hook's
    # list to MODEL_ENV_KEYS: a future tier added to TIER_ALIASES without
    # updating the hook fails here naming the missing var (AC7).
    import re
    from pathlib import Path

    hook = Path(__file__).resolve().parents[3] / "hooks" / "attest-model.sh"
    block = re.search(r"MODEL_ENV_VARS=\((.*?)\)", hook.read_text(), re.S)
    assert block is not None, "hooks/attest-model.sh has no MODEL_ENV_VARS list"
    hook_vars = set(re.findall(r"\b(ANTHROPIC_[A-Z_]+)\b", block.group(1)))
    assert hook_vars == set(MODEL_ENV_KEYS), (
        f"hook/model var drift: hook-only={sorted(hook_vars - set(MODEL_ENV_KEYS))} "
        f"model-only={sorted(set(MODEL_ENV_KEYS) - hook_vars)}"
    )


if __name__ == "__main__":
    for name, fn in sorted(globals().items()):
        if name.startswith("test_"):
            fn()
            print(f"ok {name}")
