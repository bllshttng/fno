"""The role ladder is derived from the scope, and implementers get no role.

    0   several projects   scope names 2+ config projects (a portfolio)
    1   one project        scope names one config project
    2   a set of epics     every scope names a backlog node with type == "epic"

The rung itself is derived in Rust (crates/fno-agents/tests/role_ladder_parity.rs
pins it). These tests pin containment across the ladder, which stays in Python.
"""
from __future__ import annotations
from tests.fixtures.graph_seed import seed_graph

import json

import pytest

from fno.paths_testing import use_tmpdir
from fno.projects import resolve as proj_resolve


@pytest.fixture
def territory(tmp_path, monkeypatch):
    """A graph with two epics + one ordinary node, and two configured projects.

    The projects go through the REAL resolver against a temp config rather than a
    monkeypatched stub: "is this a project?" is half the ladder, so a stub would
    test the test.
    """
    use_tmpdir(monkeypatch, tmp_path)
    from fno import paths

    graph_path = paths.graph_json()
    graph_path.parent.mkdir(parents=True, exist_ok=True)
    seed_graph(graph_path, json.dumps(
            {
                "entries": [
                    {"id": "e-1", "type": "epic", "project": "alpha"},
                    {"id": "e-2", "type": "epic", "project": "beta"},
                    {"id": "n-1", "type": "feature", "project": "alpha"},
                ]
            }
        ))

    cfg = tmp_path / "config.toml"
    cfg.write_text(
        '[work.workspaces.ws1]\n'
        'projects = [{ name = "alpha", short_name = "a" }, { name = "beta" }]\n',
        encoding="utf-8",
    )
    monkeypatch.setattr(proj_resolve, "SETTINGS_PATH", cfg)
    proj_resolve._clear_cache()
    yield tmp_path
    proj_resolve._clear_cache()


# --- containment is now real, not an honor system ----------------------------


def test_containment_is_checkable_across_the_ladder(territory) -> None:
    """The rule this replaces could only check that two scopes DIFFERED, because
    scopes were opaque and project>epic containment was not derivable. It is now:
    a project sits in a portfolio by name, and an epic carries its project."""
    from fno.agents.role import scope_contains

    assert scope_contains("alpha,beta", "alpha") is True     # project in portfolio
    assert scope_contains("alpha", "e-1") is True            # epic in its project
    assert scope_contains("alpha,beta", "e-1") is True       # epic in the portfolio
    assert scope_contains("beta", "e-1") is False            # epic of another project
    assert scope_contains("alpha", "alpha") is False         # a peer, not below
    assert scope_contains("a,beta", "alpha") is True         # aliases normalize


def test_a_rung_2_set_is_a_union_for_containment(territory) -> None:
    """A role over two epics contains a role over either member: the lead
    over both epics can grant inside each. The set itself is a peer of its own
    spelling, and an epic outside the set is not contained."""
    from fno.agents.role import scope_contains

    assert scope_contains("e-1,e-2", "e-1") is True
    assert scope_contains("e-1,e-2", "e-2") is True
    assert scope_contains("e-2,e-1", "e-1") is True      # stored order-free
    assert scope_contains("e-1,e-2", "e-1,e-2") is False  # a peer, not below
    assert scope_contains("e-1", "e-1,e-2") is False       # narrower holds less


def test_an_epic_set_falls_under_the_role_holding_every_member(territory) -> None:
    """A set of epics is below whatever role holds EVERY member, not below a
    set of project names. The subset shortcut read epic ids against project
    names and always fell through, so no lead could grant two epics at once."""
    from fno import paths

    seed_graph(paths.graph_json(), json.dumps(
            {
                "entries": [
                    {"id": "e-1", "type": "epic", "project": "alpha"},
                    {"id": "e-2", "type": "epic", "project": "alpha"},
                    {"id": "e-3", "type": "epic", "project": "beta"},
                ]
            }
        ))
    from fno.agents.role import scope_contains

    assert scope_contains("alpha", "e-1,e-2") is True     # both members of alpha
    assert scope_contains("alpha", "e-1,e-3") is False    # spans two projects
    assert scope_contains("alpha,beta", "e-1,e-3") is True
    assert scope_contains("e-1,e-2", "e-1,e-2,e-3") is False  # peer sets


