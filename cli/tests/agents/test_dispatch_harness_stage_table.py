"""One home for the autonomous-dispatch harness axis.

``agents.profiles.<command>.provider`` (the stage table) is the home; the
deprecated ``dispatch.harness`` reads as the fallback rung beneath it for one
release. Both dispatch doors - ``resolve_dispatch`` (dispatch-node.sh /
``resolve_node_spawn``) and ``fno agents spawn``
(compose_spawn_argv) - must answer one harness for one node.
"""
from __future__ import annotations

from typing import Optional

from fno.agents.harness_map import resolve_dispatch


class _Defaults:
    def __init__(self, provider="", model="", effort="", substrate="", permission_mode="",
                 route="", account="", pane_group="", lanes=None):
        self.provider = provider
        self.model = model
        self.effort = effort
        self.substrate = substrate
        self.permission_mode = permission_mode
        self.route = route
        self.account = account
        self.pane_group = pane_group
        self.lanes = [
            _Defaults(**lane) if isinstance(lane, dict) else lane
            for lane in (lanes or [])
        ]


class _Settings:
    def __init__(self, profiles=None, dispatch=None, **kw):
        prof = {k: _Defaults(**v) for k, v in (profiles or {}).items()}
        self.agents = type(
            "A",
            (),
            {
                "defaults": _Defaults(**kw),
                "profiles": prof,
                "max_lanes": {},
            },
        )()
        self.dispatch = dispatch


def _settings(
    profile_provider: Optional[str] = None,
    legacy_harness: Optional[str] = None,
    command_name: str = "target",
    profile_route: Optional[str] = None,
) -> _Settings:
    from types import SimpleNamespace

    profiles = None
    if profile_provider or profile_route:
        profiles = {command_name: {"provider": profile_provider, "route": profile_route}}
    # Attribute access, like the typed DispatchBlock the real model carries.
    dispatch = (
        SimpleNamespace(harness=legacy_harness, substrate="", command="")
        if legacy_harness
        else None
    )
    return _Settings(profiles=profiles, dispatch=dispatch)


def test_dispatch_door_names_the_losing_spelling_in_its_decision() -> None:
    out = resolve_dispatch(
        settings=_settings(profile_provider="codex", legacy_harness="claude")
    )
    assert out["harness"] == "codex"
    assert any("dispatch.harness" in entry and "claude" in entry for entry in out["decision"])


def test_dispatch_door_keeps_the_legacy_key_alone() -> None:
    """A legacy-only installation is unchanged: the key still wins its rung,
    and nothing is named as shadowed (nothing was)."""
    out = resolve_dispatch(settings=_settings(legacy_harness="codex"))
    assert out["harness"] == "codex"
    assert not any("ignored" in entry for entry in out["decision"])


def test_spawn_door_answers_the_same_harness(monkeypatch) -> None:
    """The measured failure this file pins: one node through both doors must
    not land on two vendors. The config rung moved native, so the invariant
    at this seam is the wiring: the dispatch fold asks the ONE door with the
    same command key the spawn path asks, and the settings fold agrees with
    the rung the door answered. The door's own config semantics are
    golden-frozen on the Rust side."""
    from fno import rust_binary
    from fno.agents.harness_map import _load_dispatch_cfg

    asks: list[tuple[str, dict]] = []

    def door(command, payload, *a, **k):
        asks.append((command, payload))
        if payload.get("mode") == "configured":
            return {"harness": "codex", "note": None, "route": None}
        raise rust_binary.CommandUnavailable(f"{command} hermetic")

    monkeypatch.setattr(rust_binary, "command_call", door)

    cfg = _load_dispatch_cfg(None, command="target")
    assert cfg.get("harness") == "codex"

    out = resolve_dispatch(settings=_settings(profile_provider="codex"))
    assert out["harness"] == "codex"

    assert asks, "the config rung must ride the door"
    assert all(name == "route-slot" for name, _ in asks)
    assert all(
        payload.get("mode") == "configured" and payload.get("command") == "target"
        for _, payload in asks
    )


def test_dispatch_door_carries_the_stage_table_route() -> None:
    """The route rides the same stage-table row the harness does: a dispatch
    that forwards the harness but not the route sends a routed model to the
    default endpoint, where it dies on first inference. The tuple must carry
    both, so a caller can forward both."""
    out = resolve_dispatch(
        settings=_settings(profile_provider="claude",
                           profile_route="zai,glm-5.3-flash[1m]")
    )
    assert out["harness"] == "claude"
    assert out["route"] == "zai,glm-5.3-flash[1m]"
    assert any("route=config(" in entry for entry in out["decision"])


