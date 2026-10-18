"""AC5-FR: the In-N-Out menu-cap ratchet (`fno doctor lint menu-caps`).

The ratchet keeps the advertised command surface small: promoting a verb past
the cap fails lint with a message that names the offender and both remedies,
and passes again once the verb is hidden or the cap constant is raised.
"""
from __future__ import annotations

import typer
import typer.main
import pytest
# click's runner: `fno doctor lint` is a plain-function registry entry that the live
# CLI resolves to a bare TyperCommand, which typer.testing.CliRunner rejects.
from click.testing import CliRunner

import fno.lint_cli as L


def _lint_command():
    """`fno doctor lint` in the shape the live CLI resolves it to (see _lazy_group)."""
    sub = typer.Typer(add_completion=False)
    sub.command(name="lint")(L.lint)
    return typer.main.get_command(sub)


runner = CliRunner()


def test_menu_caps_passes_on_the_shipped_registry():
    """The real curated menu is within caps (top-level <= 10)."""
    # This isolates the legacy advertised-menu assertion from the root
    # namespace ratchet, which intentionally starts red on this branch.
    monkeypatch = pytest.MonkeyPatch()
    monkeypatch.setattr(L, "MENU_CAP_ROOT_NAMESPACE", 50, raising=False)
    result = runner.invoke(_lint_command(), ["menu-caps"])
    monkeypatch.undo()
    assert result.exit_code == 0, result.output
    assert "menu-caps: ok" in result.output


def test_menu_caps_fails_naming_offender_and_both_remedies(monkeypatch):
    """AC5-FR: over-cap fails, naming the offending verb and both remedies."""
    import typer.main

    from fno.cli import app as root_app

    advertised = len(L._visible_command_names(typer.main.get_command(root_app)))
    # Lower the cap below the shipped advertised set - the same branch a real
    # 11th advertised verb would hit.
    monkeypatch.setattr(L, "MENU_CAP_TOP_LEVEL", advertised - 1)
    result = runner.invoke(_lint_command(), ["menu-caps"])
    assert result.exit_code == 1, result.output
    out = result.output
    # Names a concrete offending verb (the one over the cap).
    assert "over the cap:" in out
    # Both remedies, one of them naming the cap constant literally.
    assert "mark it hidden" in out
    assert "MENU_CAP_TOP_LEVEL" in out


def test_root_namespace_names_pin_today_by_name():
    """AC2-HP/AC4-HP: the folded surface is twelve, pinned by name.

    The pre-fold red state (20 real roots failing at cap 12) is the branch's
    own history: the gate commit landed first and the folds brought it green.
    This pin holds the TWELVE (d-b93d7754, plus the native uninstall) -
    agents, backlog, config, do, doctor, help, inbox, mux, uninstall, update,
    version, whoami - so the next root addition is a named event, not a drift.
    Twelve is the cap: the next root needs a fold first.
    """
    names = set(L._root_namespace_names())
    assert names == {
        "agents",
        "backlog",
        "config",
        "do",
        "doctor",
        "help",
        "inbox",
        "mux",
        "uninstall",
        "update",
        "version",
        "whoami",
    }


def test_root_namespace_excludes_a_move_and_counts_a_hidden_root(monkeypatch):
    """AC1-EDGE/ERR: visibility does not hide roots, and moves cost no slot."""
    import fno.cli as cli_mod

    monkeypatch.setitem(
        cli_mod.LAZY_SUBCOMMANDS,
        "hidden-fixture",
        ("fno.test_cmd:test_command", "fixture", {"hidden": True}),
    )
    names = set(L._root_namespace_names())
    assert "hidden-fixture" in names
    assert "annotate" not in names
    # One below the count WITH the fixture: the hidden registration pushed
    # the namespace over, which is the exact regression (a hidden verb that
    # moves no number) this gate exists to kill.
    monkeypatch.setattr(L, "MENU_CAP_ROOT_NAMESPACE", len(names) - 1)
    result = runner.invoke(_lint_command(), ["menu-caps"])
    assert result.exit_code == 1, result.output
    assert "root namespace has" in result.output
