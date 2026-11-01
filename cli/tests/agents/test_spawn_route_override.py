"""`fno agents spawn --route provider,model` explicit fail-closed override (x-b0b4).

Layers:
- cmd_spawn resolves + fails CLOSED before the gate (AC3-ERR).
- dispatch_spawn threads route_env to the claude create path.
- bg_create applies route_env, winning over --role.
"""
from __future__ import annotations

from pathlib import Path

import pytest
from typer.testing import CliRunner

from fno.paths_testing import use_tmpdir


def _receipt_line(output: str) -> str:
    """The line that IS the JSON receipt. CliRunner mixes stderr into output,
    so a stderr notice (the inherited tier-remap drop warning) can precede or
    follow the receipt on machines carrying ANTHROPIC_DEFAULT_*_MODEL."""
    for line in output.splitlines():
        line = line.strip()
        if line.startswith("{"):
            return line
    raise AssertionError(f"no JSON receipt line in output: {output!r}")

runner = CliRunner()


def _setup_tmp_home(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    use_tmpdir(monkeypatch, tmp_path)
    home = tmp_path / "home"
    home.mkdir(exist_ok=True)
    monkeypatch.setenv("HOME", str(home))
    for k in ("FNO_AGENT_SELF", "FNO_AGENT_HARNESS", "FNO_AGENT_SESSION"):
        monkeypatch.delenv(k, raising=False)


def _full_route_env(token: str = "t") -> dict[str, str]:
    """A hand-built foreign route carrying the WHOLE unit: endpoint, auth, and
    every model tier. resolve_spawn_route refuses an endpoint without the full
    model set, so any route passed through a guarded seam must look real."""
    from fno.agents.model_routing import MODEL_ENV_KEYS

    return {
        "ANTHROPIC_BASE_URL": "https://api.z.ai/api/anthropic",
        "ANTHROPIC_AUTH_TOKEN": token,
        **{k: "glm-5.2" for k in MODEL_ENV_KEYS},
    }


# ---------------------------------------------------------------------------
# cmd_spawn: fail CLOSED before the gate (AC3-ERR)
# ---------------------------------------------------------------------------


class _Gate:
    def release(self) -> None:  # noqa: D401
        pass


# ---------------------------------------------------------------------------
# cmd_spawn -> dispatch_spawn threads route_env (resolved) to the create path
# ---------------------------------------------------------------------------


# ---------------------------------------------------------------------------
# bg_create: route_env WINS over role; anthropic creds cleared (AC "--route wins")
# ---------------------------------------------------------------------------


# ---------------------------------------------------------------------------
# x-5ed4 / x-2af5: an account overlay + a vendor route COMPOSE. The route wins
# endpoint+auth+model as one unit (atomic); the account keeps CLAUDE_CONFIG_DIR.
# This is the regression for the split-brain the old "refuse" guard existed to
# prevent (overlay endpoint+auth, route model -> foreign model on Anthropic).
# ---------------------------------------------------------------------------


# ---------------------------------------------------------------------------
# x-6de8: routed spawn applies its route via a --settings file (survives the
# daemon fork that drops per-spawn env), on both bg and headless.
# ---------------------------------------------------------------------------


# ---------------------------------------------------------------------------
# An --account spawn expresses its auth/model scrub via a --settings file too:
# the env scrub is dropped at the claude --bg daemon fork, but a settings file is
# read by the forked session itself. CLAUDE_CONFIG_DIR rides the env overlay


# ---------------------------------------------------------------------------
# Harness axis: --harness/-H canonical, --provider/-p the older spelling.
# A model VENDOR is never a harness value -- that axis is --route.
# ---------------------------------------------------------------------------


# ---------------------------------------------------------------------------
# x-3db9 P1: the managed-OAuth refusal stays armed for a bare route (no account
# overlay), and only relaxes when an account overlay makes the composition atomic.
# ---------------------------------------------------------------------------


# ---------------------------------------------------------------------------
# x-3db9 P1 (codex), re-scoped by x-8552: a route that sets an endpoint must
# carry its own auth, on EVERY path, not just under an inherited managed
# marker. A direct dispatch_spawn caller can pass a hand-built partial
# route_env (base-URL-only); paired with an account's Keychain OAuth it would
# send that OAuth token to the foreign endpoint - the one split-brain the
# composition cannot express.
# ---------------------------------------------------------------------------


def test_partial_route_env_is_refused_with_or_without_account(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    from fno.agents.model_routing import (
        MODEL_ENV_KEYS,
        RouteCompositionError,
        resolve_spawn_route,
    )

    partial = {"ANTHROPIC_BASE_URL": "https://foreign.example/anthropic"}  # no auth token
    # No ambient marker needed: the refusal is a property of the route itself.
    monkeypatch.delenv("FNO_PROVIDER_AUTH", raising=False)
    with pytest.raises(RouteCompositionError, match="without its own credential"):
        resolve_spawn_route(None, partial)
    # Self-authed but model-less: the credentials compose scrubs every
    # inherited model var (they are all on its scrub list), so the worker would
    # ask the foreign endpoint for Claude's default model and fail its first
    # turn behind a "live" receipt (codex P2 on this PR).
    authed = {**partial, "ANTHROPIC_AUTH_TOKEN": "route-key"}
    with pytest.raises(RouteCompositionError, match="without ANTHROPIC_MODEL"):
        resolve_spawn_route(None, authed)
    # One missing tier is the same refusal: that tier keeps Claude's default
    # model id at a vendor that does not serve it.
    gap = {**authed, **{k: "glm-5.2" for k in MODEL_ENV_KEYS}}
    del gap["ANTHROPIC_DEFAULT_HAIKU_MODEL"]
    with pytest.raises(RouteCompositionError, match="ANTHROPIC_DEFAULT_HAIKU_MODEL"):
        resolve_spawn_route(None, gap)
    # A complete unit composes, ambient marker or not.
    complete = {**authed, **{k: "glm-5.2" for k in MODEL_ENV_KEYS}}
    assert resolve_spawn_route(None, complete) == complete
    monkeypatch.setenv("FNO_PROVIDER_AUTH", "managed")
    assert resolve_spawn_route(None, complete) == complete


