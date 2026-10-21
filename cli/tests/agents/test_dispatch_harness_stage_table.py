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
    command: str = "target",
    profile_route: Optional[str] = None,
) -> _Settings:
    from types import SimpleNamespace

    profiles = None
    if profile_provider or profile_route:
        profiles = {command: {"provider": profile_provider, "route": profile_route}}
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


def test_spawn_door_answers_the_same_harness(tmp_path, monkeypatch) -> None:
    """The measured failure this file pins: one node through both doors must
    not land on two vendors. The spawn-axis retirement moved the launch line
    to the lane template, so the composed argv no longer carries the harness
    axis; both doors read the one stage-table rung, and the two readers this
    file exercises must keep answering one harness for one command row."""
    from fno.dispatch_flags import configured_dispatch_harness

    config_path = tmp_path / "config.toml"
    config_path.write_text(
        '[agents.profiles.target]\nprovider = "codex"\n', encoding="utf-8"
    )
    monkeypatch.setenv("FNO_CONFIG", str(config_path))
    monkeypatch.setenv("FNO_NO_CANONICAL_CONFIG", "1")

    harness, _note = configured_dispatch_harness()
    assert harness == "codex"

    out = resolve_dispatch(settings=_settings(profile_provider="codex"))
    assert out["harness"] == "codex"


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


