"""`fno agents spawn --route provider,model` explicit fail-closed override (x-b0b4).

Layers:
- rust_runtime detector keeps --route Python-only (parity with --role).
- cmd_spawn resolves + fails CLOSED before the gate (AC3-ERR).
- dispatch_spawn threads route_env to the claude create path.
- bg_create applies route_env, winning over --role.
"""
from __future__ import annotations

import os
import json
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


def test_route_missing_key_refused_before_gate(monkeypatch: pytest.MonkeyPatch) -> None:
    from fno.agents import dispatch, spawn_gate

    monkeypatch.delenv("ZAI_API_KEY", raising=False)

    gate_calls: list = []
    monkeypatch.setattr(
        spawn_gate, "run_gate", lambda *a, **k: gate_calls.append(1) or _Gate()
    )
    # If the refusal fails to fire, this stub prevents a real spawn.
    monkeypatch.setattr(
        "fno.agents.dispatch.dispatch_spawn",
        lambda **kw: dispatch.SpawnResult(
            kind="created", name=kw["name"], provider="claude", short_id="x"
        ),
    )
    from fno.agents.cli import agents_app

    result = runner.invoke(
        agents_app,
        ["spawn", "--name", "w1", "hi", "--harness", "claude",
         "--route", "zai,glm-5.2"],
    )
    assert result.exit_code == 2, result.output
    assert "refused" in result.output.lower()
    # Fail-closed BEFORE the gate: no slot acquired, no worker launched.
    assert gate_calls == []


class _Gate:
    def release(self) -> None:  # noqa: D401
        pass


def test_route_on_non_claude_harness_names_harness_not_provider(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    # AC5-HP: the refusal says "harness 'codex'", never "provider 'codex'",
    # and names the caller's own --route flag.
    from fno.agents import dispatch, spawn_gate

    monkeypatch.setattr(spawn_gate, "run_gate", lambda *a, **k: _Gate())
    monkeypatch.setattr(
        "fno.agents.dispatch.dispatch_spawn",
        lambda **kw: dispatch.SpawnResult(
            kind="created", name=kw["name"], provider="claude", short_id="x"
        ),
    )
    from fno.agents.cli import agents_app

    result = runner.invoke(
        agents_app,
        ["spawn", "--name", "w1", "hi", "--harness", "codex",
         "--route", "zai,glm-5.2"],
    )
    assert result.exit_code == 2, result.output
    assert "harness 'codex'" in result.output
    assert "provider 'codex'" not in result.output
    assert "--route zai,glm-5.2" in result.output


def test_route_allowed_on_capability_enabled_pane(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    from fno.agents import mux_spawn, spawn_gate
    from fno.agents.cli import agents_app
    from fno.agents.model_routing import MODEL_ENV_KEYS
    from fno.agents.mux_spawn import MuxSpawnResult
    from tests.agents._route_overlay_stub import claude_route_overlay_stub

    monkeypatch.setattr(
        "fno.agents.spawn_overlay_client.spawn_overlay_call", claude_route_overlay_stub
    )

    _setup_tmp_home(tmp_path, monkeypatch)
    monkeypatch.setenv("FNO_AGENTS_RUNTIME", "python")
    monkeypatch.setenv("FNO_REPO_ROOT", os.getcwd())
    monkeypatch.setenv("ZAI_API_KEY", "zk-live")
    gate_call: Dict[str, Any] = {}

    def fake_gate(*_args: Any, **kwargs: Any) -> _Gate:
        gate_call.update(kwargs)
        return _Gate()

    monkeypatch.setattr(spawn_gate, "run_gate", fake_gate)
    captured: Dict[str, Any] = {}

    def fake_dispatch(**kwargs: Any) -> MuxSpawnResult:
        captured.update(kwargs)
        return MuxSpawnResult(
            name=kwargs["name"],
            provider=kwargs["provider"],
            session="main",
            pane_id=1,
            child_pid=None,
            session_uuid="u",
        )

    monkeypatch.setattr(mux_spawn, "dispatch_spawn_bounded_pane", fake_dispatch)
    result = runner.invoke(
        agents_app,
        ["spawn", "--name", "w1", "hi", "--harness", "claude", "--as", "pane", "--split", "left", "--route", "zai,glm-5.2"],
    )
    assert result.exit_code == 0, result.output
    assert captured["provider"] == "claude"
    assert captured["route_provider"] == "zai"
    assert gate_call["route_provider"] == "zai"
    route_env = captured["route_env"]
    assert route_env["ANTHROPIC_BASE_URL"] == "https://api.z.ai/api/anthropic"
    assert route_env["ANTHROPIC_AUTH_TOKEN"] == "zk-live"
    for key in MODEL_ENV_KEYS:
        if key == "ANTHROPIC_DEFAULT_HAIKU_MODEL":
            continue
        assert route_env[key] == "glm-5.2"
    assert route_env["ANTHROPIC_DEFAULT_HAIKU_MODEL"] == "glm-4.7"
    # AC4 + the live specimen (2026-08-06): a routed spawn's receipt carries
    # three axes in three keys - harness (claude), provider (the vendor zai),
    # model (glm-5.2) - and NO key holds a harness literal under `provider`.
    # The defect was a receipt reading {"provider": "claude"} for this exact
    # invocation while the worker ran on glm-5.2: the flags took, the receipt lied.
    receipt = json.loads(_receipt_line(result.output))
    assert receipt["harness"] == "claude"
    assert receipt["provider"] == "zai"
    assert receipt["model"] == "glm-5.2"


def test_bg_receipt_carries_route_provider_and_effective_model(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """A routed bg spawn's receipt carries the same three axes as the pane path.

    The bg receipt branch builds provider_field and model_field with the same
    ``model or route_model`` logic as the pane branch. Without this test, a
    regression in the bg branch's receipt (dropping provider/model, or reporting
    route_model instead of the effective model) would pass CI.
    """
    from fno.agents import dispatch, spawn_gate
    from fno.agents.cli import agents_app

    _setup_tmp_home(tmp_path, monkeypatch)
    monkeypatch.setenv("FNO_AGENTS_RUNTIME", "python")
    monkeypatch.setenv("FNO_REPO_ROOT", os.getcwd())
    monkeypatch.setenv("ZAI_API_KEY", "zk-live")
    monkeypatch.setattr(spawn_gate, "run_gate", lambda *a, **k: _Gate())
    captured: Dict[str, Any] = {}

    def fake_dispatch(**kwargs: Any) -> Any:
        captured.update(kwargs)
        return dispatch.SpawnResult(
            kind="created", name=kwargs["name"], provider="claude", short_id="abcd1234"
        )

    monkeypatch.setattr("fno.agents.dispatch.dispatch_spawn", fake_dispatch)
    result = runner.invoke(
        agents_app,
        ["spawn", "--name", "w1", "hi", "--harness", "claude", "--route", "zai,glm-5.2"],
    )
    assert result.exit_code == 0, result.output
    assert captured.get("model") is None
    assert captured["route_provider"] == "zai"
    assert captured["route_env"]["ANTHROPIC_MODEL"] == "glm-5.2"
    receipt_line = next(
        line for line in result.output.strip().splitlines() if '"short_id"' in line
    )
    receipt = json.loads(receipt_line)
    assert receipt["harness"] == "claude"
    assert receipt["provider"] == "zai"
    assert receipt["model"] == "glm-5.2"


def test_route_on_pane_capability_fails_closed_before_gate(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    import fno.agents.harness_map as harness_map
    from fno.agents import mux_spawn, spawn_gate
    from fno.agents.cli import agents_app

    _setup_tmp_home(tmp_path, monkeypatch)
    monkeypatch.setenv("FNO_AGENTS_RUNTIME", "python")
    monkeypatch.setenv("FNO_REPO_ROOT", os.getcwd())
    monkeypatch.setenv("ZAI_API_KEY", "zk-live")
    monkeypatch.delitem(harness_map._HARNESS_CAPS["claude"], "route_on_pane")
    gate_calls: list[int] = []
    monkeypatch.setattr(
        spawn_gate,
        "run_gate",
        lambda *a, **k: gate_calls.append(1) or _Gate(),
    )
    monkeypatch.setattr(
        mux_spawn,
        "dispatch_spawn_bounded_pane",
        lambda **kwargs: pytest.fail("pane dispatch called"),
    )

    result = runner.invoke(
        agents_app,
        ["spawn", "--name", "w1", "hi", "--harness", "claude", "--as", "pane", "--split", "left", "--route", "zai,glm-5.2"],
    )

    assert result.exit_code == 2, result.output
    assert "route_on_pane" in result.output
    assert gate_calls == []


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


def test_print_renders_the_lane_native_line(tmp_path: Path, monkeypatch) -> None:
    """--print resolves the lane and prints the native line it would exec,
    then exits. Nothing is gated, registered or launched. The lane fold, the
    compose and the render all live behind the spawn-overlay print kind; the
    Rust goldens pin the tokens, so this test pins the transport: the ask
    carries the RAW overlays and stdout is the shell-quoted join of the
    answer."""
    from fno.agents import spawn_gate
    from fno.agents.cli import agents_app

    gate_calls: list[Any] = []
    monkeypatch.setattr(
        spawn_gate, "run_gate", lambda *a, **k: gate_calls.append(a) or _Gate()
    )
    monkeypatch.setenv("ZAI_API_KEY", "zk-live")
    from fno.agents import spawn_overlay_client

    asks: list[dict] = []
    from fno.agents.spawn_overlay_client import SpawnOverlayUnavailable

    def fake_overlay(ask, **k):
        asks.append(ask)
        if ask.get("kind") == "compose":
            # Config defaults compose at the seam before the print leg; a
            # no-op answer keeps the stderr skip line out of the render.
            return {"exit": 0}
        if ask.get("kind") == "claude-route":
            # The transport owns route resolution: the zai preset under the
            # caller env, all four tiers, or the composition refuses.
            env = ask.get("env") or os.environ
            model = ask.get("model", "")
            return {
                "route": {
                    "ANTHROPIC_BASE_URL": "https://api.z.ai/api/anthropic",
                    "ANTHROPIC_AUTH_TOKEN": env.get("ZAI_API_KEY", ""),
                    "ANTHROPIC_MODEL": model,
                    "ANTHROPIC_DEFAULT_OPUS_MODEL": model,
                    "ANTHROPIC_DEFAULT_SONNET_MODEL": model,
                    "ANTHROPIC_DEFAULT_FABLE_MODEL": model,
                    "ANTHROPIC_DEFAULT_HAIKU_MODEL": "glm-4.7",
                },
                "notices": [],
            }
        if ask.get("harness") != "claude":
            # The door refuses a harness with no native line; this stubs the
            # transport so the CLI's exit mapping is what runs.
            raise SpawnOverlayUnavailable(
                "--print renders claude --bg lines; harness 'codex' has no native line yet."
            )
        return {"tokens": ["claude", "--bg", "--settings", "/state/route-settings/lane-zai-flash.json",
                           "--model", "glm-5.2", "--name", "w", "seed work"]}

    monkeypatch.setattr(spawn_overlay_client, "spawn_overlay_call", fake_overlay)
    result = runner.invoke(
        agents_app,
        ["spawn", "--name", "w", "seed work", "--route", "zai,glm-5.2", "--print"],
    )
    assert result.exit_code == 0, result.output
    line = result.output.strip()
    assert line.startswith("claude --bg --settings /state/route-settings/lane-zai-flash.json"), line
    assert "--model glm-5.2" in line
    assert "--name w" in line
    assert "'seed work'" in line
    [ask] = [a for a in asks if a.get("kind") == "print"]
    assert ask["harness"] == "claude"
    assert ask["route_env"].get("ANTHROPIC_AUTH_TOKEN") == "zk-live"
    assert ask["account_env"] == {}
    # The effort axis is retired and no lane row names one here.
    assert ask["model"] == "glm-5.2" and ask["effort"] == ""
    assert ask["cwd"], "the print ask carries the spawn's own cwd"
    assert gate_calls == [], "--print must not touch the gate"

    # A harness the lane has no native line for refuses through the door; the
    # CLI maps the door refusal to a clean exit.
    refused = runner.invoke(
        agents_app,
        ["spawn", "--name", "w", "hi", "-H", "codex", "--print"],
    )
    assert refused.exit_code == 2, refused.output
    assert "no native line yet" in refused.output


def test_route_lane_name_resolves_the_row_axes(monkeypatch) -> None:
    """The lane pin spelling: ``--route zai-flash`` resolves the declared row
    and the row's own axes answer the spawn - the door's vendor split names
    the provider and effort seeds when the spawn named none of its own. A
    pair-shaped value never reaches the name arm: the pair parse runs
    first."""
    from fno.agents import spawn_gate
    from fno.agents.cli import agents_app

    gate_calls: list[Any] = []
    monkeypatch.setattr(
        spawn_gate, "run_gate", lambda *a, **k: gate_calls.append(a) or _Gate()
    )
    monkeypatch.setenv("ZAI_API_KEY", "zk-live")
    row = {
        "name": "zai-flash", "harness": "claude", "model": "glm-5.3-flash[1m]",
        "route": "zai/glm-5.3-flash[1m]", "effort": "xhigh", "vendor": "zai",
    }
    monkeypatch.setattr(
        "fno.route_slot_client.route_slot_call",
        lambda ask: dict(row, found=True) if ask.get("mode") == "lane" else {"found": False},
    )
    from fno.agents import spawn_overlay_client

    asks: list[dict] = []

    def lane_stub(ask: dict, **_kwargs: object) -> dict:
        """The transport owns both asks the spawn path makes: the --print
        tokens ask and the claude-route ask (the Rust owner answers the
        zai preset under the caller env, all four tiers, or the composition
        refuses)."""
        asks.append(ask)
        if ask.get("kind") == "claude-route":
            env = ask.get("env") or os.environ
            model = ask.get("model", "")
            return {
                "route": {
                    "ANTHROPIC_BASE_URL": "https://api.z.ai/api/anthropic",
                    "ANTHROPIC_AUTH_TOKEN": env.get("ZAI_API_KEY", ""),
                    "ANTHROPIC_MODEL": model,
                    "ANTHROPIC_DEFAULT_OPUS_MODEL": model,
                    "ANTHROPIC_DEFAULT_SONNET_MODEL": model,
                    "ANTHROPIC_DEFAULT_FABLE_MODEL": model,
                    "ANTHROPIC_DEFAULT_HAIKU_MODEL": "glm-4.7",
                },
                "notices": [],
            }
        return {"tokens": ["claude", "--bg", "--name", "w", "go"]}

    monkeypatch.setattr(spawn_overlay_client, "spawn_overlay_call", lane_stub)
    result = runner.invoke(
        agents_app,
        ["spawn", "--name", "w", "go", "--harness", "claude",
         "--route", "zai-flash", "--print"],
    )
    assert result.exit_code == 0, result.output
    ask = next(a for a in asks if a.get("kind") == "print")
    assert ask["model"] == "glm-5.3-flash[1m]"
    assert ask["effort"] == "xhigh", "the row's effort seeded the spawn"
    assert ask["route_env"].get("FNO_ROUTE_PROVIDER") == "zai"


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


def test_bare_route_vendor_is_refused(monkeypatch: pytest.MonkeyPatch) -> None:
    """A bare `--route zai` names no model. It is refused rather than expanded from
    a hardcoded default: the vendor/model pair is the caller's choice."""
    from fno.agents import spawn_gate

    monkeypatch.setenv("ZAI_API_KEY", "zk-live")
    monkeypatch.setattr(spawn_gate, "run_gate", lambda *a, **k: _Gate())
    from fno.agents.cli import agents_app

    result = runner.invoke(
        agents_app,
        ["spawn", "--name", "w", "hi", "--once", "--route", "zai"],
    )
    assert result.exit_code == 2, result.output
    assert "provider,model" in result.output


def test_harness_name_on_the_provider_axis_is_refused_by_name(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """`--provider claude` used to select the CLI binary. Now that --provider is
    the model-vendor axis it must refuse a harness name and NAME the fix, rather
    than launching a worker with a nonsense route."""
    from fno.agents import spawn_gate

    monkeypatch.setattr(spawn_gate, "run_gate", lambda *a, **k: _Gate())
    from fno.agents.cli import agents_app

    result = runner.invoke(agents_app, ["spawn", "--name", "w1", "hi", "--provider", "codex"])
    assert result.exit_code == 2, result.output
    assert "--provider is retired" in result.output


def test_provider_and_route_together_are_refused(monkeypatch: pytest.MonkeyPatch) -> None:
    """Two spellings of one route; taking both would silently drop one."""
    from fno.agents import spawn_gate

    monkeypatch.setenv("ZAI_API_KEY", "zk-live")
    monkeypatch.setattr(spawn_gate, "run_gate", lambda *a, **k: _Gate())
    from fno.agents.cli import agents_app

    result = runner.invoke(
        agents_app,
        ["spawn", "--name", "w1", "hi", "--provider", "zai",
         "--model", "glm-5.2", "--route", "zai,glm-5.2"],
    )
    assert result.exit_code == 2, result.output
    assert "--provider is retired" in result.output


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
    # Self-authed but model-less: compose_worker_credentials scrubs every
    # inherited model var (they are all SCRUB_AUTH_VARS), so the worker would
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


# ---------------------------------------------------------------------------
# x-3db9 P2: a route-bearing --account spawn must not scrub the route key at the
# seam. A vendor may name a SCRUB_AUTH_VARS member (e.g. ANTHROPIC_AUTH_TOKEN) as
# its api_key_env; resolve_explicit_route reads it from os.environ, so scrubbing
# first reports a valid route keyless only when --account is also present.
# Routed spawns are Python-only and the create lane scrubs + applies route-wins
# itself, so the seam scrub is skipped entirely for them.
# ---------------------------------------------------------------------------


def test_seam_scrub_skips_route_bearing_account_spawn(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    import os

    from fno.agents import rust_runtime

    monkeypatch.setenv("ANTHROPIC_AUTH_TOKEN", "route-key")
    args = ["spawn", "--name", "w", "hi", "--account", "acct", "--route", "zai,glm-5.2"]
    rust_runtime._scrub_account_auth_at_seam(args)
    assert os.environ.get("ANTHROPIC_AUTH_TOKEN") == "route-key"  # not scrubbed


def test_dispatch_account_refuses_a_harness_mismatch(monkeypatch: pytest.MonkeyPatch) -> None:
    """The cutover carrier must agree with the binary it spawns.

    A codex record's CODEX_HOME handed to a claude spawn authenticates nothing
    and launches the wrong binary - the exact miss this flag exists to prevent,
    so the record's harness is verified rather than trusted from the caller.
    """
    from types import SimpleNamespace

    from fno.agents import spawn_gate

    monkeypatch.setattr(spawn_gate, "run_gate", lambda *a, **k: _Gate())
    monkeypatch.setattr(
        "fno.adapters.providers.loader.load_providers",
        lambda *a, **k: SimpleNamespace(
            by_id={"codex-acct": SimpleNamespace(harness="codex")}
        ),
    )
    monkeypatch.setattr(
        "fno.adapters.providers.dispatch.dispatch_env",
        lambda *a, **k: pytest.fail("staged an overlay for the wrong harness"),
    )
    from fno.agents.cli import agents_app

    result = runner.invoke(
        agents_app,
        ["spawn", "--name", "w", "hi", "--harness", "claude", "--once",
         "--dispatch-account", "codex-acct"],
    )
    assert result.exit_code == 2, result.output
    assert "codex account but the spawn resolves claude" in result.output


def test_every_spawn_seam_uses_the_one_composition_function(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """AC5-INV: bg_create, headless_create, and _mesh_env_wrapper all produce
    exactly what compose_worker_credentials produces - asserted by enumerating
    the seams against the function, not by three independent orderings.

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


def test_provider_without_model_is_refused(monkeypatch: pytest.MonkeyPatch) -> None:
    from fno.agents import spawn_gate

    monkeypatch.setenv("ZAI_API_KEY", "zk-live")
    monkeypatch.setattr(spawn_gate, "run_gate", lambda *a, **k: _Gate())
    from fno.agents.cli import agents_app

    result = runner.invoke(agents_app, ["spawn", "--name", "w1", "hi", "--provider", "zai"])
    assert result.exit_code == 2, result.output
    assert "--provider is retired" in result.output


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


def _stub_zai_claude_route(monkeypatch) -> None:
    """Answer the claude-route ask the Rust owner serves (the zai preset); a
    fake binary cannot. Every other overlay ask reaches the real transport."""
    from fno.agents import spawn_overlay_client

    real_overlay_call = spawn_overlay_client.spawn_overlay_call

    def route_stub(ask: dict, **kwargs: object) -> dict:
        if ask.get("kind") == "claude-route":
            model = ask.get("model", "")
            return {
                "route": {
                    "ANTHROPIC_BASE_URL": "https://api.z.ai/api/anthropic",
                    "ANTHROPIC_AUTH_TOKEN": "zk-live",
                    "ANTHROPIC_MODEL": model,
                    "ANTHROPIC_DEFAULT_OPUS_MODEL": model,
                    "ANTHROPIC_DEFAULT_SONNET_MODEL": model,
                    "ANTHROPIC_DEFAULT_FABLE_MODEL": model,
                    "ANTHROPIC_DEFAULT_HAIKU_MODEL": "glm-4.7",
                },
                "notices": [],
            }
        return real_overlay_call(ask, **kwargs)

    monkeypatch.setattr(spawn_overlay_client, "spawn_overlay_call", route_stub)


def test_native_lane_execs_through_the_door(tmp_path: Path, monkeypatch) -> None:
    """A fresh routed claude thread spawn execs the lane's own bg line through
    the launch kind: the ask carries the RAW overlays and the record
    context (the compose happens in the door), the dispatcher path never
    runs, and the gate still releases."""
    from fno.agents import spawn_gate
    from fno.agents.cli import agents_app

    gate_calls: list[Any] = []

    class RecordingGate(_Gate):
        release_calls: list[bool] = []

        def release(self) -> None:
            RecordingGate.release_calls.append(True)

    monkeypatch.setattr(
        spawn_gate, "run_gate", lambda *a, **k: gate_calls.append(a) or RecordingGate()
    )
    monkeypatch.setenv("ZAI_API_KEY", "zk-live")
    row = {
        "name": "zai-flash", "harness": "claude", "model": "glm-5.3-flash[1m]",
        "route": "zai/glm-5.3-flash[1m]", "effort": "xhigh", "vendor": "zai",
    }
    monkeypatch.setattr(
        "fno.route_slot_client.route_slot_call",
        lambda ask: dict(row, found=True) if ask.get("mode") == "lane" else {"found": False},
    )
    from fno.agents import spawn_lineage

    monkeypatch.setattr(
        spawn_lineage,
        "_capture_parent_edge",
        lambda: ("lead-1", "claude", str(tmp_path)),
    )
    ask_file = tmp_path / "door-ask.json"
    fake = tmp_path / "fake-agents"
    fake.write_text(
        "#!/bin/sh\n"
        'echo "$@" >> ' + str(tmp_path / "calls.log") + "\n"
        "tee " + str(tmp_path / "stdin.log") + " > /dev/null\n"
        'if [ "$1" = "spawn-axes" ]; then echo \'{"action":"pass"}\'; exit 0; fi\n'
        'if [ "$1" = "spawn-overlay" ]; then cp '
        + str(tmp_path / "stdin.log") + " " + str(ask_file) + "; fi\n"
        'echo \'{"exit":0}\'\n',
        encoding="utf-8",
    )
    fake.chmod(0o755)
    monkeypatch.setattr("fno.rust_binary.resolve_binary", lambda: fake)
    _stub_zai_claude_route(monkeypatch)
    result = runner.invoke(
        agents_app,
        ["spawn", "--name", "w", "go", "--route", "zai-flash"],
    )
    assert result.exit_code == 0, result.output
    ask = json.loads(ask_file.read_text(encoding="utf-8"))
    assert ask["harness"] == "claude"
    assert ask["name"] == "w"
    assert ask["payload"] == "go"
    assert ask["route_env"].get("ANTHROPIC_AUTH_TOKEN") == "zk-live"
    assert ask["account_env"] == {}
    assert ask["model"] == "glm-5.3-flash[1m]"
    # The launch template owns effort; the door ask leaves the hole empty.
    assert ask["effort"] == ""
    assert ask["provider"] == "zai"
    assert ask["launcher_session"] == "lead-1"
    assert RecordingGate.release_calls, "the gate still releases"


def test_native_lane_keeps_the_dispatcher_outside_a_session(tmp_path: Path, monkeypatch) -> None:
    """A routed claude thread spawn with no launcher identity keeps the
    dispatcher, never the launch kind: the launch record must name a parent,
    and the dispatcher writes its row synchronously."""
    from fno.agents import spawn_gate, spawn_lineage
    from fno.agents.cli import agents_app

    monkeypatch.setattr(spawn_lineage, "_capture_parent_edge", lambda: (None, None, None))

    gate_calls: list[Any] = []
    monkeypatch.setattr(
        spawn_gate, "run_gate", lambda *a, **k: gate_calls.append(a) or _Gate()
    )
    monkeypatch.setenv("ZAI_API_KEY", "zk-live")
    row = {
        "name": "zai-flash", "harness": "claude", "model": "glm-5.3-flash[1m]",
        "route": "zai/glm-5.3-flash[1m]", "effort": "xhigh", "vendor": "zai",
    }
    monkeypatch.setattr(
        "fno.route_slot_client.route_slot_call",
        lambda ask: dict(row, found=True) if ask.get("mode") == "lane" else {"found": False},
    )
    dispatched: list[dict] = []

    def fake_dispatch(**kw):
        dispatched.append(kw)
        from fno.agents.dispatch import SpawnResult

        return SpawnResult(kind="created", name=kw["name"], provider="claude", short_id="c-1")

    monkeypatch.setattr("fno.agents.dispatch.dispatch_spawn", fake_dispatch)
    # spawn-overlay is legitimately called for OTHER kinds during normal
    # resolution (e.g. `compose`, to read the lane row); the fix is about the
    # `launch` kind specifically, so every call's payload is appended and
    # checked rather than asserting spawn-overlay is never invoked at all.
    overlay_log = tmp_path / "overlay-stdin.log"
    fake = tmp_path / "fake-agents"
    fake.write_text(
        "#!/bin/sh\n"
        'if [ "$1" = "spawn-axes" ]; then echo \'{"action":"pass"}\'; exit 0; fi\n'
        'if [ "$1" = "spawn-overlay" ]; then cat >> '
        + str(overlay_log) + "; fi\n"
        'echo \'{"exit":0}\'\n',
        encoding="utf-8",
    )
    fake.chmod(0o755)
    monkeypatch.setattr("fno.rust_binary.resolve_binary", lambda: fake)
    _stub_zai_claude_route(monkeypatch)
    result = runner.invoke(
        agents_app,
        ["spawn", "--name", "w", "go", "--route", "zai-flash"],
    )
    assert result.exit_code == 0, result.output
    assert dispatched, "a sessionless routed spawn reaches dispatch_spawn"
    assert dispatched[0]["name"] == "w"
    overlay_calls = overlay_log.read_text(encoding="utf-8") if overlay_log.exists() else ""
    assert '"kind": "launch"' not in overlay_calls, "the spawn-overlay launch kind is never reached"
