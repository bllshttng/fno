"""`fno agents spawn --route provider,model` explicit fail-closed override (x-b0b4).

Layers:
- rust_runtime detector keeps --route Python-only (parity with --role).
- cmd_spawn resolves + fails CLOSED before the gate (AC3-ERR).
- dispatch_spawn threads route_env to the claude create path.
- bg_create applies route_env, winning over --role.
"""
from __future__ import annotations

from pathlib import Path
from typing import Any, Dict

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
# rust_runtime: --route is Python-only, exactly like --role
# ---------------------------------------------------------------------------


def test_codex_thread_lane_refuses_resume_instead_of_spawning_fresh(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    """A --resume codex spawn used to drop the id silently and mint a FRESH
    session; it must refuse and name the durable resume path instead."""
    from fno.agents import dispatch

    def _never(*args: Any, **kwargs: Any) -> str:
        raise AssertionError("a resume request must not reach the thread-lane spawn")

    monkeypatch.setattr(dispatch, "_codex_thread_spawn", _never)
    monkeypatch.setattr(dispatch, "_emit_ev", lambda *a, **k: None)
    with pytest.raises(dispatch.DispatchAskError) as exc:
        dispatch.dispatch_spawn(
            name="codex-thread",
            message="hi",
            harness="codex",
            cwd=tmp_path,
            resume_session_id="019f0000-0000-7000-8000-000000000003",
        )
    assert "not supported on the codex thread lane" in str(exc.value)


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


def test_composed_route_and_account_spawn_scrubs_inherited_auth(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """A --route + --account spawn writes ONLY the route settings file (route-wins
    atomicity), so that file has to carry the auth scrub itself: the env scrub is
    discarded at the daemon fork, and without the floor the worker could still
    authenticate with an inherited key while its endpoint points elsewhere."""
    import json

    _setup_tmp_home(tmp_path, monkeypatch)
    monkeypatch.setenv("HOME", str(tmp_path / "home"))
    monkeypatch.setenv("ANTHROPIC_API_KEY", "stale-primary-key")
    from fno.agents.harnesses import claude as claude_mod

    seen: Dict[str, Any] = {}

    def fake_run(argv, **kwargs):  # type: ignore[no-untyped-def]
        seen["argv"] = argv
        from subprocess import CompletedProcess

        return CompletedProcess(argv, 0, stdout="backgrounded \xb7 abcd1234 \xb7 ok\n", stderr="")

    monkeypatch.setattr(claude_mod, "_subprocess_run", fake_run)
    claude_mod.bg_create(
        name="w",
        message="hi",
        cwd=tmp_path,
        account_env={"CLAUDE_CONFIG_DIR": str(tmp_path / "acct")},
        route_env=_full_route_env(),
    )
    argv = seen["argv"]
    blob = json.load(open(argv[argv.index("--settings") + 1]))["env"]
    assert "ANTHROPIC_API_KEY" not in blob, (
        "a self-authed route drops the credential it does not name"
    )
    assert blob["CLAUDE_CODE_OAUTH_TOKEN"] == ""
    assert blob["ANTHROPIC_BASE_URL"] == "https://api.z.ai/api/anthropic"


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


@pytest.mark.dev_build
def test_every_spawn_seam_uses_the_one_composition_function(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """AC5-INV: bg_create, headless_create, and _mesh_env_wrapper all produce
    exactly what the `accounts::credentials` compose produces - asserted by
    enumerating the seams against it, not by three independent orderings.

    An account overlay + a vendor route COMPOSE. The route wins
    endpoint+auth+model as one unit; the account keeps CLAUDE_CONFIG_DIR, and
    its competing login never reaches the env or the pane argv (where `ps`
    would show it)."""
    from fno.agents.harnesses import claude as claude_mod
    from fno.agents.mux_spawn import _mesh_env_wrapper

    _setup_tmp_home(tmp_path, monkeypatch)
    monkeypatch.setenv("ANTHROPIC_API_KEY", "ambient-stale")
    monkeypatch.setenv("ANTHROPIC_BASE_URL", "https://stale.example")
    account = {"CLAUDE_CONFIG_DIR": "/x/.claude", "ANTHROPIC_AUTH_TOKEN": "acct"}
    route = _full_route_env()

    class _Result:
        returncode = 0
        stdout = "backgrounded \xb7 abcd1234 \xb7 ok\n"
        stderr = ""

    seen: Dict[str, Any] = {}

    def fake_run(argv, **kwargs):  # type: ignore[no-untyped-def]
        seen["argv"] = argv
        seen["env"] = kwargs.get("env")
        return _Result()

    monkeypatch.setattr(claude_mod, "_subprocess_run", fake_run)
    claude_mod.bg_create(
        name="seam-bg", message="hi", cwd=tmp_path, route_env=route,
        account_env=account,
    )
    assert seen["env"]["CLAUDE_CONFIG_DIR"] == "/x/.claude"
    assert seen["env"]["ANTHROPIC_BASE_URL"] == "https://api.z.ai/api/anthropic"
    assert seen["env"]["ANTHROPIC_AUTH_TOKEN"] == "t"
    assert "ANTHROPIC_API_KEY" not in seen["env"]

    claude_mod.headless_create(
        message="hi", cwd=tmp_path, route_env=route, account_env=account,
    )
    assert seen["env"]["CLAUDE_CONFIG_DIR"] == "/x/.claude"
    assert seen["env"]["ANTHROPIC_BASE_URL"] == "https://api.z.ai/api/anthropic"
    assert seen["env"]["ANTHROPIC_AUTH_TOKEN"] == "t"
    assert "ANTHROPIC_API_KEY" not in seen["env"]

    # The pane seam renders the same composition as an env(1) argv: scrub via
    # -u, then the composed pairs (route last, left-to-right last-wins).
    argv = _mesh_env_wrapper(
        "seam-pane", "claude", None, ["claude"], account_env=dict(account),
        route_env=dict(route),
    )
    assignments = {
        a.split("=", 1)[0]: a.split("=", 1)[1]
        for a in argv
        if "=" in a
    }
    assert assignments.get("CLAUDE_CONFIG_DIR") == "/x/.claude"
    assert assignments.get("ANTHROPIC_BASE_URL") == "https://api.z.ai/api/anthropic"
    assert assignments.get("ANTHROPIC_AUTH_TOKEN") == "t"
    assert "ANTHROPIC_AUTH_TOKEN=acct" not in argv
    assert argv.count("ANTHROPIC_API_KEY=") == 0


def test_bg_and_headless_refuse_modelless_route_env(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """The model-completeness refusal must fire on the in-process seams (bg,
    headless), not only on the CLI-resolved lane - the guard lives in
    resolve_spawn_route, which every seam routes through."""
    from fno.agents.harnesses import claude as claude_mod
    from fno.agents.model_routing import RouteCompositionError

    def fake_run(argv, **kwargs):  # type: ignore[no-untyped-def]
        from subprocess import CompletedProcess

        return CompletedProcess(argv, 0, stdout="ok", stderr="")

    monkeypatch.setattr(claude_mod, "_subprocess_run", fake_run)
    modelless = {
        "ANTHROPIC_BASE_URL": "https://foreign.example/anthropic",
        "ANTHROPIC_AUTH_TOKEN": "t",
    }
    with pytest.raises(RouteCompositionError, match="without ANTHROPIC_MODEL"):
        claude_mod.bg_create(name="w", message="hi", cwd=tmp_path, route_env=modelless)
    with pytest.raises(RouteCompositionError, match="without ANTHROPIC_MODEL"):
        claude_mod.headless_create(message="hi", cwd=tmp_path, route_env=modelless)

