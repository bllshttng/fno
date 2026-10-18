"""The role ladder is derived from the scope, and implementers get no role.

    0   several projects   scope names 2+ config projects (a portfolio)
    1   one project        scope names one config project
    2   a set of epics     every scope names a backlog node with type == "epic"

There is no operator-supplied level anywhere in the surface. That is the point of
these tests: the old grammar made callers hand-type an altitude on a ladder whose
direction reads backwards (0 is the TOP), and a wrong guess minted authority at
the wrong rung with no error. The rung is now a fact about the territory, so the
only way to get it wrong is to name the wrong territory - which is checkable, and
checked here.
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


def _derive(scopes):
    from fno.agents.role import derive_role_level

    return derive_role_level(scopes)


def _refusal(scopes) -> str:
    from fno.agents.role import RoleScopeError, derive_role_level

    with pytest.raises(RoleScopeError) as exc:
        derive_role_level(scopes)
    return str(exc.value)


# --- the three rungs ---------------------------------------------------------


# --- implementers get no role ----------------------------------------------


def test_a_non_epic_node_is_refused_not_stamped_at_a_bottom_rung(territory) -> None:
    """The old ladder had an IC rung, so any node minted a role. A single task is
    work, not a territory: nobody terms for a day over it."""
    msg = _refusal(["n-1"])
    assert "not an epic" in msg
    assert "Implementers get no roles" in msg
    # AC2-HP: the refusal names the exact fix when the node IS meant to be an
    # epic - one command away, not a dead end.
    assert "fno backlog update n-1 type=epic" in msg


def test_an_unresolvable_scope_is_refused(territory) -> None:
    """A typo used to fall through to level 0, minting a VP over a scope nobody
    could find. Refusing is the whole reason derivation returns no default."""
    assert "neither a configured project nor a backlog node" in _refusal(["banana"])


def test_an_empty_scope_is_refused(territory) -> None:
    assert "needs a scope" in _refusal([])


# --- mixed scopes are a mistake about what is being ruled --------------------


def test_a_mixed_scope_names_each_scopes_rung(territory) -> None:
    """The refusal names what each scope IS, so the operator can see the
    mistake (a project beside an epic) rather than just that one exists."""
    msg = _refusal(["alpha", "e-1"])
    assert "rules PROJECTS" in msg
    assert "alpha (a project)" in msg
    assert "e-1 (an epic)" in msg


# --- rung 2 rules a SET of epics ----------------------------------------------
#
# The operator's mux lead oversees two epics at once; before the set grammar
# that lead had no legal role: two epics was read as a failed portfolio.


def test_the_epic_set_is_canonical_order_and_dedup(territory) -> None:
    """The stored form is the canonical join, so two spellings of one set are
    one role - the one-live-role guard compares stored scopes."""
    from fno.agents.role import resolve_role

    assert resolve_role(["e-2", "e-1", "e-2"]) == (2, "e-1,e-2")


# --- containment is now real, not an honor system ----------------------------


# --- an alias is the same territory, not a second one ------------------------
#
# `resolve_project_name` accepts a project's `short_name`, so one project can be
# spelled two ways. The stored scope must be the CANONICAL name, or the
# equality-based one-live-role guard sees two territories where there is one.


def test_one_project_named_twice_is_not_a_portfolio(territory) -> None:
    """`-k alpha -k a` is one project spelled two ways. Deduping the raw strings
    would count two members and mint portfolio authority over a single project."""
    from fno.agents.role import resolve_role

    assert resolve_role(["alpha", "a"]) == (1, "alpha")


def test_a_real_portfolio_still_derives_level_0_through_aliases(territory) -> None:
    from fno.agents.role import resolve_role

    assert resolve_role(["a", "beta"]) == (0, "alpha,beta")


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


