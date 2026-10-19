"""A verb-shaped codex seed the session cannot expand is refused, and the
receipt separates delivered from fired (x-aac2)."""
from __future__ import annotations

import pytest

from fno.agents import harness_map
from fno.agents.harness_map import (
    cannot_fire_refusal,
    render_seed,
    verb_fired_marker,
)
from fno.agents.rust_runtime import _refuse_unfireable_seed


@pytest.fixture(autouse=True)
def _plugin_state(monkeypatch):
    """Pin the codex plugin verdict; tests set it via `state`."""
    from fno.setup import codex_plugin

    holder = {"status": "fresh"}
    monkeypatch.setattr(
        codex_plugin, "inspect_freshness", lambda **_: {"status": holder["status"]}
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


def test_an_absolute_path_seed_is_never_judged(_plugin_state):
    """x-c976: a leading-slash path is prose, never a verb the plugin must
    expand - the fire test, not a prefix scan, decides."""
    _plugin_state["status"] = "missing"
    assert cannot_fire_refusal("/Users/bb16/plan.md go", "codex") is None
    _refuse_unfireable_seed(["spawn", "/Users/bb16/plan.md go", "-H", "codex"])


@pytest.mark.parametrize(
    ("seed", "expected"),
    [
        ("$fno:target x-1\ncontext lines follow", "fno agents claim status node:x-1"),
        ("/fno:target --no-merge x-1", None),
    ],
)
def test_the_marker_names_the_verbs_own_positive_proof(seed, expected):
    assert verb_fired_marker(seed) == expected


def test_the_seam_refuses_an_unfireable_codex_seed(_plugin_state, capsys):
    _plugin_state["status"] = "missing"
    with pytest.raises(SystemExit) as exc:
        _refuse_unfireable_seed(["spawn", "$fno:target x-1", "-H", "codex"])
    assert exc.value.code == 2
    assert "would not fire" in capsys.readouterr().err


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


def test_a_resolvable_word_resolves_the_marker_and_passes_the_gate(graph_rows):
    graph_rows([dict(_ROW)])
    assert (
        verb_fired_marker("/fno:target sideline-mux")
        == "fno agents claim status node:x-bff0"
    )
    assert verb_fired_marker("/fno:target sideline") == (
        "fno agents claim status node:x-bff0"
    )
    assert render_seed("/fno:target sideline-mux", "codex") == "$fno:target sideline-mux"


def test_the_gate_refuses_an_unresolvable_word_naming_the_nearest(graph_rows):
    graph_rows([dict(_ROW), dict(_ROW, id="x-1111", title="Sideline mux card")])
    with pytest.raises(harness_map.DispatchResolveError) as exc:
        render_seed('/fno:target "Sideline mux: ..."', "codex")
    assert "names no backlog node" in str(exc.value)
    with pytest.raises(harness_map.DispatchResolveError) as exc:
        render_seed("/fno:target sideline mux", "claude")
    assert "x-bff0" in str(exc.value)
    assert "x-1111" in str(exc.value)


