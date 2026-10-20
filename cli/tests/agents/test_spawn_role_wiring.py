"""dispatch_spawn / cmd_spawn thread the routing role to the create path (x-d2fe).

The provider boundary (bg_create) is covered by test_harness_role_routing.py;
these guards pin the wiring above it so a future refactor cannot silently drop
the ``role`` kwarg between the CLI flag and the claude create path.
"""

from __future__ import annotations

from pathlib import Path
from typing import Any, Dict

import pytest
from typer.testing import CliRunner

from fno.paths_testing import use_tmpdir


def _setup_tmp_home(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    use_tmpdir(monkeypatch, tmp_path)
    home = tmp_path / "home"
    home.mkdir(exist_ok=True)
    monkeypatch.setenv("HOME", str(home))
    for k in ("FNO_AGENT_SELF", "FNO_AGENT_HARNESS", "FNO_AGENT_SESSION"):
        monkeypatch.delenv(k, raising=False)


def _route_unit(model: str = "glm-5.2") -> dict[str, str]:
    """A resolved route as the real resolver emits it: endpoint, auth, and
    every model tier as one unit (resolve_spawn_route refuses less)."""
    from fno.agents.model_routing import MODEL_ENV_KEYS

    return {
        "ANTHROPIC_BASE_URL": "https://api.z.ai/api/anthropic",
        "ANTHROPIC_AUTH_TOKEN": "secret",
        **{k: model for k in MODEL_ENV_KEYS},
    }


def _stamped(route: dict[str, str], provider: str = "zai") -> dict[str, str]:
    """The route as it reaches the spawn, carrying its provider stamp.

    `bind_route_provider` writes `FNO_ROUTE_PROVIDER` into the route env so the
    provider resolution survives the fork (x-c703). The env a spawn receives is
    therefore the resolved route PLUS that key, and asserting the bare route
    would pass on a build that dropped the stamp.
    """
    from fno.agents.model_routing import ROUTE_PROVIDER_ENV

    return {**route, ROUTE_PROVIDER_ENV: provider}


def _resolved_zai(route: dict[str, str]):
    def resolve(*_args: Any, **kwargs: Any) -> dict[str, str]:
        callback = kwargs.get("resolved_provider")
        if callback is not None:
            callback("zai")
        return route

    return resolve


def _zai_admission(monkeypatch, name: str, substrate: str = "bg"):
    from fno.agents.spawn_gate import run_gate

    monkeypatch.setenv("FNO_SPAWN_GATE", "0")
    return run_gate(name, substrate, route_provider="zai")


def test_dispatch_spawn_threads_captured_role_route_to_create_path(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    _setup_tmp_home(tmp_path, monkeypatch)

    from fno.agents import dispatch as dispatch_mod, model_routing
    from fno.agents.dispatch import DispatchAskResult, dispatch_spawn

    captured: Dict[str, Any] = {}

    def fake_create(**kw: Any) -> DispatchAskResult:
        captured.update(kw)
        return DispatchAskResult(kind="create", short_id="abc12345")

    monkeypatch.setattr(dispatch_mod, "_claude_create_path", fake_create)
    route = _route_unit("business-model")
    monkeypatch.setattr(model_routing, "resolve_route", _resolved_zai(route))

    result = dispatch_spawn(
        name="dreamer",
        message="consolidate memory",
        harness="claude",
        cwd=tmp_path,
        role="consolidate",
        provider_gate=_zai_admission(monkeypatch, "dreamer"),
    )
    assert result.kind == "created"
    assert captured["role"] is None
    assert captured["route_env"] == _stamped(route)


@pytest.mark.parametrize("substrate", ["worker", "pane"])
def test_pre_resolved_route_without_provider_axis_refuses_before_launch(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
    substrate: str,
) -> None:
    _setup_tmp_home(tmp_path, monkeypatch)
    from fno.agents.dispatch import DispatchAskError, dispatch_spawn
    from fno.agents.mux_spawn import dispatch_spawn_pane

    harness = "claude"
    with pytest.raises(
        DispatchAskError, match="no bound model-provider identity"
    ) as exc_info:
        if substrate == "worker":
            dispatch_spawn(
                name="missing-provider-worker",
                message="work",
                harness=harness,
                cwd=tmp_path,
                route_env=_route_unit(),
            )
        else:
            dispatch_spawn_pane(
                name="missing-provider-pane",
                message="work",
                provider=harness,
                cwd=tmp_path,
                route_env=_route_unit(),
                runner=lambda *_args, **_kwargs: pytest.fail(
                    "refusal must precede pane launch"
                ),
            )

    assert exc_info.value.exit_code == 2
    assert "cap cannot be evaluated" in str(exc_info.value)


def test_resolved_provider_must_match_admitted_provider(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    _setup_tmp_home(tmp_path, monkeypatch)
    from fno.agents import dispatch as dispatch_mod, model_routing
    from fno.agents.dispatch import DispatchAskError, dispatch_spawn
    from fno.agents.spawn_gate import run_gate

    route = _route_unit()
    monkeypatch.setattr(model_routing, "resolve_route", _resolved_zai(route))
    monkeypatch.setattr(
        dispatch_mod,
        "_claude_create_path",
        lambda **_kwargs: pytest.fail("refusal must precede worker launch"),
    )
    monkeypatch.setenv("FNO_SPAWN_GATE", "0")
    gate = run_gate("wrong-provider", "bg", route_provider="openai")
    harness = "claude"

    with pytest.raises(DispatchAskError, match="resolved provider.*zai.*openai"):
        dispatch_spawn(
            name="wrong-provider-worker",
            message="work",
            harness=harness,
            cwd=tmp_path,
            role="tidy",
            route_provider="openai",
            provider_gate=gate,
        )


def test_provider_admission_is_single_use_and_name_bound(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    _setup_tmp_home(tmp_path, monkeypatch)
    from fno.agents import dispatch as dispatch_mod, model_routing
    from fno.agents.dispatch import DispatchAskError, DispatchAskResult, dispatch_spawn

    monkeypatch.setattr(
        model_routing, "resolve_route", _resolved_zai(_route_unit())
    )
    monkeypatch.setattr(
        dispatch_mod,
        "_claude_create_path",
        lambda **_kwargs: DispatchAskResult(kind="create", short_id="abc12345"),
    )
    gate = _zai_admission(monkeypatch, "single-use")
    harness = "claude"

    dispatch_spawn(
        name="single-use",
        message="first",
        harness=harness,
        cwd=tmp_path,
        role="tidy",
        provider_gate=gate,
    )
    with pytest.raises(DispatchAskError, match="no matching admission token"):
        dispatch_spawn(
            name="single-use",
            message="second",
            harness=harness,
            cwd=tmp_path,
            role="tidy",
            provider_gate=gate,
        )


def test_forged_admission_object_is_refused(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    _setup_tmp_home(tmp_path, monkeypatch)
    from fno.agents.dispatch import DispatchAskError, dispatch_spawn
    from fno.agents.model_routing import bind_route_provider

    class _ForgedGate:
        def consume_provider(self, *_args):
            return True

    harness = "claude"
    with pytest.raises(DispatchAskError, match="no matching admission token"):
        dispatch_spawn(
            name="forged-gate",
            message="work",
            harness=harness,
            cwd=tmp_path,
            route_env=bind_route_provider(_route_unit(), "zai"),
            route_provider="zai",
            provider_gate=_ForgedGate(),
        )


def test_routed_direct_spawn_without_admission_token_refuses(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    _setup_tmp_home(tmp_path, monkeypatch)
    from fno.agents import model_routing
    from fno.agents.dispatch import DispatchAskError, dispatch_spawn

    monkeypatch.setattr(
        model_routing, "resolve_route", _resolved_zai(_route_unit())
    )
    with pytest.raises(DispatchAskError, match="no matching admission token"):
        dispatch_spawn(
            name="ungated-worker",
            message="work",
            harness="claude",
            cwd=tmp_path,
            role="tidy",
        )


def test_tier_remap_preflight_normalizes_actual_conflict(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    _setup_tmp_home(tmp_path, monkeypatch)
    monkeypatch.setenv("ANTHROPIC_DEFAULT_OPUS_MODEL", "glm-5.2")

    from fno.agents.dispatch import DispatchAskError, dispatch_spawn

    with pytest.raises(DispatchAskError, match="model 'opus' is ambiguous") as exc_info:
        dispatch_spawn(
            name="remap-worker",
            message="work",
            harness="claude",
            cwd=tmp_path,
            model="opus",
        )

    assert exc_info.value.exit_code == 2


class _Gate:
    def release(self) -> None:
        pass


def test_cmd_spawn_resolves_role_route_once_before_substrate_fanout(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """Every routed CLI spawn carries one pre-resolved endpoint/auth/model unit."""
    from fno.agents import dispatch, model_routing, mux_spawn, spawn_gate

    route = _route_unit()
    resolutions: list[str | None] = []
    received: dict[str, Any] = {}

    gate_calls: list[dict[str, Any]] = []

    def resolve(role: str | None, **kwargs: Any) -> dict[str, str]:
        resolutions.append(role)
        kwargs["resolved_provider"]("zai")
        return route

    monkeypatch.setattr(model_routing, "resolve_route", resolve)
    monkeypatch.setattr(
        spawn_gate,
        "run_gate",
        lambda *a, **k: gate_calls.append(k) or _Gate(),
    )
    monkeypatch.setattr(mux_spawn, "resolve_provenance", lambda *a, **k: {})
    monkeypatch.setattr(
        dispatch,
        "dispatch_spawn",
        lambda **kwargs: received.update(kwargs)
        or dispatch.SpawnResult(
            kind="created",
            name=kwargs["name"],
            provider=kwargs["harness"],
            short_id="abcd1234",
        ),
    )

    from fno.agents.cli import agents_app

    result = CliRunner().invoke(
        agents_app,
        [
            "spawn",
            "--name",
            "route-bg",
            "--harness",
            "claude",
            "--role",
            "tidy",
            "--here",
            "work",
        ],
    )

    assert result.exit_code == 0, result.output
    assert resolutions == ["tidy"]
    assert received["route_env"] == _stamped(route)
    assert received["route_provider"] == "zai"
    assert gate_calls == [{
        "force": False,
        "no_wait": False,
        "route_provider": "zai",
        "account": None,
        "seed": "work",
        "session_phase": "",
        "succession_scope": None,
    }]


