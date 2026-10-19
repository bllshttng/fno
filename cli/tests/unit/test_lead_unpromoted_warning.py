"""`lead init`'s unpromoted-row warning and the scope predicate it keys on.

The warning exists because three row-keyed readers (`lead done`,
`lead manifest-path`, `lead-postcompact-reinject.sh`) all fail CLOSED and
SILENTLY when the row carries no role for the armed scope. It never
refuses: the manifest is written and the loop arms on the FILE.

Review 2026-09-03 found the first cut gated on "does this row carry ANY
role", which `role_label` answers from `role_level` alone. A session
promoted over other territory therefore armed a second scope in silence,
which is the exact state the warning was added to make loud. Each branch
asserts the MESSAGE it produces, never merely that something was printed.
"""
from __future__ import annotations

from dataclasses import dataclass
from typing import Optional

import pytest


@dataclass
class _Row:
    """Enough of an ``AgentEntry`` for the warning, including its property.

    ``role_label`` is a real property on the registry model and derives from
    ``role_level`` alone, rendering ``"L2 ?"`` when the scope is unset. That
    asymmetry is what the reviewed defect turned on, so the double is faithful
    to it rather than carrying a plain label field.
    """

    name: str = "a5cdfd52"
    role_level: Optional[int] = None
    role_scope: Optional[str] = None
    role_grantor: Optional[str] = None

    @property
    def role_label(self) -> Optional[str]:
        if self.role_level is None:
            return None
        return f"L{self.role_level} {self.role_scope or '?'}"


def _warn(monkeypatch, capsys, row, scope="x-b76b"):
    from fno.lead import cli as lead_cli

    monkeypatch.setattr("fno.agents.role.calling_agent_row", lambda: row)
    lead_cli._warn_unpromoted_row(scope)
    return capsys.readouterr().err


def test_no_role_names_the_grant_command(monkeypatch, capsys):
    err = _warn(monkeypatch, capsys, _Row())
    assert "NO role" in err
    assert "fno agents org promote a5cdfd52 --scope x-b76b" in err
    # The consequence must be stated truthfully: manifest-path raises
    # typer.Exit(1) and prints nothing, it does not answer empty at exit 0.
    assert "exit non-zero without printing a path" in err


@pytest.mark.parametrize(
    "held,requested,matches",
    [
        (None, "x-b76b", False),
        # A rung-2 set: same set spelled in any order is one territory, but a
        # MEMBER is not the whole scope - the readers key on the stored scope,
        # so a set role must not satisfy a single-member manifest.
        ("x-1,x-2", "x-2,x-1", True),
        ("x-1,x-2", "x-1", False),
    ],
)
def test_role_scope_matches_answers_equality_and_absence(held, requested, matches):
    """Territory equality, which is the question the row-keyed readers ask."""
    from fno.agents.role import role_scope_matches

    assert role_scope_matches(held, requested) is matches


def test_a_containing_role_does_not_match_a_narrower_manifest(monkeypatch, capsys):
    """The second defect this gate had, and the opposite of the first.

    A first fix accepted ``scope_contains``, reasoning that the grant path
    accepts a strict container. The readers do not: ``lead_manifest_path``
    builds ``leads/{role_scope}.md`` from the stored scope verbatim and
    ``done_cmd`` refuses on ``own != scope``. So a project-level role over
    an epic manifest must still warn, or the warning goes quiet on exactly
    the state it exists to expose.
    """
    from fno.agents.role import role_scope_matches

    assert role_scope_matches("fno", "x-b76b") is False

    row = _Row(role_level=3, role_scope="fno")
    err = _warn(monkeypatch, capsys, row, scope="x-b76b")
    assert "is not that scope" in err
    assert "L3 fno" in err
    # The message must not DENY the containment, because it is real here.
    # Saying "neither equals nor contains it" to a role that does contain it
    # is runtime text drifting from behavior.
    assert "neither equals nor contains" not in err
    assert "EXACT role_scope" in err
