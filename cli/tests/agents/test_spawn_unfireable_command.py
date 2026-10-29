"""A command-shaped codex seed the session cannot expand is refused, and the
receipt separates delivered from fired (x-aac2)."""
from __future__ import annotations

import pytest

from fno.agents import harness_map
from fno.agents.harness_map import (
    cannot_fire_refusal,
    render_seed,
)


@pytest.fixture(autouse=True)
def _plugin_state(monkeypatch):
    """Pin the codex plugin verdict the native door returns; tests set it via `state`."""
    holder = {"status": "fresh"}
    monkeypatch.setattr(
        "fno.rust_binary.call_binary_json",
        lambda *_a, **_k: (None, {"status": holder["status"]}),
    )
    yield holder


@pytest.fixture(autouse=True)
def _graphless(monkeypatch):
    """Default the seed-word resolver to a graphless machine: the marker and
    gate cases below pin shape, not resolution. Resolution tests stub their
    own rows via `graph_rows`."""
    def _unreadable(path=None, **_):
        raise ValueError("graph store unreadable")

    monkeypatch.setattr("fno.graph.load.load_graph", _unreadable)


@pytest.fixture
def graph_rows(monkeypatch):
    """Install the graph rows the seed-word resolver reads."""

    def _install(rows):
        monkeypatch.setattr(
            "fno.graph.load.load_graph", lambda path=None, **_: list(rows)
        )

    return _install


def test_a_codex_seed_without_an_enabled_plugin_is_refused(_plugin_state):
    _plugin_state["status"] = "missing"
    reason = cannot_fire_refusal("$fno:target x-1", "codex")
    assert reason is not None
    assert "'$fno:target'" in reason
    assert "fno config plugin install codex" in reason


def test_render_seed_refuses_and_normalizes(_plugin_state):
    _plugin_state["status"] = "missing"
    with pytest.raises(harness_map.DispatchResolveError):
        render_seed("/fno:target x-1", "codex")
    _plugin_state["status"] = "fresh"
    assert render_seed("/fno:target x-1", "codex") == "$fno:target x-1"
    assert render_seed("fix the login bug", "codex") == "fix the login bug"


#: One node the resolution tests resolve against (the reported spawn shape:
#: a quoted brief whose first word is prose, never a node).
_ROW = {
    "id": "x-bff0",
    "slug": "sideline-mux",
    "title": "Sideline mux one voice per pane",
    "status": "in_progress",
}


def test_the_gate_refuses_an_unresolvable_word_naming_the_nearest(graph_rows):
    graph_rows([dict(_ROW), dict(_ROW, id="x-1111", title="Sideline mux card")])
    with pytest.raises(harness_map.DispatchResolveError) as exc:
        render_seed('/fno:target "Sideline mux: ..."', "codex")
    assert "names no backlog node" in str(exc.value)
    with pytest.raises(harness_map.DispatchResolveError) as exc:
        render_seed("/fno:target sideline mux", "claude")
    assert "x-bff0" in str(exc.value)
    assert "x-1111" in str(exc.value)
